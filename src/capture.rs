//! Text extraction from the UI element under a screen point.
//!
//! Three independent providers are implemented:
//!
//! 1. UI Automation (the primary one, used by Textify itself),
//! 2. MSAA / IAccessible (useful for older applications and for controls that
//!    expose a more deeply nested element through MSAA),
//! 3. plain `WM_GETTEXT` (a last-resort fallback for classic controls).
//!
//! Both accessibility providers walk up the element tree until a non-empty
//! name/value is found, exactly like Textify: hovering a dialog's body yields
//! the body text rather than the whole window.

use windows::core::{Interface, VARIANT};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Accessibility::{
    AccessibleObjectFromPoint, CUIAutomation, IAccessible, IUIAutomation,
    IUIAutomationLegacyIAccessiblePattern, IUIAutomationValuePattern, WindowFromAccessibleObject,
    UIA_LegacyIAccessiblePatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetWindowThreadProcessId,
};

use crate::config::RetrievalMethod;
use crate::text::{clean_segment, dedupe_segments, join_segments};
use crate::win32;

const MAX_TREE_DEPTH: usize = 48;
const ROLE_SYSTEM_TITLEBAR: i32 = 1;

/// Text captured from one screen point.
#[derive(Clone, Debug)]
pub struct Capture {
    /// Text with CRLF line endings, ready for a Win32 edit control.
    pub text: String,
    /// Individual fragments (used by Tab navigation in the result window).
    pub segments: Vec<String>,
    /// Screen rectangle of the source element, if known.
    pub rect: RECT,
    /// Native window the text came from.
    pub hwnd: HWND,
    /// Title of the top-level window.
    pub title: String,
    /// Provider that produced the result.
    pub method: RetrievalMethod,
    /// True when the whole menu was captured (special Ctrl+Shift+click mode)
    /// rather than the single element under the cursor.
    pub menu: bool,
}

/// Captures the text at `point` using the requested retrieval strategy.
pub fn capture_at(point: POINT, method: RetrievalMethod) -> Option<Capture> {
    unsafe {
        let capture = match method {
            RetrievalMethod::Uia => capture_uia(point),
            RetrievalMethod::Msaa => capture_msaa(point),
            RetrievalMethod::WindowText => capture_window_text(point),
            RetrievalMethod::Auto => {
                let uia = capture_uia(point);
                let msaa = capture_msaa(point);

                match (uia, msaa) {
                    (Some(uia), Some(msaa)) => {
                        // MSAA sometimes returns a more deeply nested element
                        // than UIA (notably in Chromium). Prefer it when its
                        // rectangle is strictly inside UIA's rectangle.
                        if strictly_inside(&uia.rect, &msaa.rect) {
                            Some(msaa)
                        } else {
                            Some(uia)
                        }
                    }
                    (Some(capture), None) | (None, Some(capture)) => Some(capture),
                    (None, None) => capture_window_text(point),
                }
            }
        }?;

        if capture.text.trim().is_empty() {
            return None;
        }
        Some(capture)
    }
}

fn strictly_inside(outer: &RECT, inner: &RECT) -> bool {
    if win32::rect_is_empty(inner) {
        return false;
    }
    inner.left >= outer.left
        && inner.top >= outer.top
        && inner.right <= outer.right
        && inner.bottom <= outer.bottom
        && (inner.left > outer.left
            || inner.top > outer.top
            || inner.right < outer.right
            || inner.bottom < outer.bottom)
}

fn finish(
    segments: Vec<String>,
    rect: RECT,
    hwnd: HWND,
    method: RetrievalMethod,
) -> Option<Capture> {
    finish_with(segments, rect, hwnd, method, false)
}

/// Builds a [`Capture`] from already collected fragments. `menu` marks results
/// of the special whole-menu mode so the result window can label them.
pub(crate) fn finish_with(
    segments: Vec<String>,
    rect: RECT,
    hwnd: HWND,
    method: RetrievalMethod,
    menu: bool,
) -> Option<Capture> {
    finish_with_title(segments, rect, hwnd, method, menu, None)
}

/// Same as [`finish_with`], but with an explicit title. The whole-menu mode
/// passes an empty title on purpose: while a menu is modal, reading the caption
/// of its owner would have to go through a cross-thread WM_GETTEXT that the
/// menu loop may never answer.
pub(crate) fn finish_with_title(
    segments: Vec<String>,
    rect: RECT,
    hwnd: HWND,
    method: RetrievalMethod,
    menu: bool,
    title: Option<String>,
) -> Option<Capture> {
    let segments = dedupe_segments(segments);
    if segments.is_empty() {
        return None;
    }

    let (text, _) = join_segments(&segments);
    let rect = if win32::rect_is_empty(&rect) {
        win32::get_window_rect(hwnd)
    } else {
        rect
    };

    Some(Capture {
        text,
        segments,
        rect,
        hwnd,
        title: title.unwrap_or_else(|| window_title(hwnd)),
        method,
        menu,
    })
}

pub(crate) fn window_title(hwnd: HWND) -> String {
    let root = win32::get_root(hwnd);
    let mut title = caption_of(root);
    if title.trim().is_empty() && root != hwnd {
        title = caption_of(hwnd);
    }
    title.trim().to_string()
}

/// Window caption with line breaks flattened and NUL characters dropped: some
/// hosted windows answer WM_GETTEXT with a lone NUL, which would otherwise
/// look like a non-empty title.
fn caption_of(hwnd: HWND) -> String {
    win32::get_text(hwnd).replace('\0', "").replace(['\r', '\n'], " ")
}

// ---------------------------------------------------------------------------
// UI Automation
// ---------------------------------------------------------------------------

unsafe fn capture_uia(point: POINT) -> Option<Capture> {
    let automation: IUIAutomation =
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;

    let mut element = automation.ElementFromPoint(point).ok()?;
    // Chromium occasionally returns the correct element only on the second
    // query; Textify does the same double query.
    if let Ok(second) = automation.ElementFromPoint(point) {
        element = second;
    }

    let walker = automation.RawViewWalker().ok()?;
    let process_id = element.CurrentProcessId().unwrap_or(0);

    let mut current = element;
    for _ in 0..MAX_TREE_DEPTH {
        let segments = uia_segments(&current);
        if !segments.is_empty() {
            let rect = current.CurrentBoundingRectangle().unwrap_or_default();
            let hwnd = current.CurrentNativeWindowHandle().unwrap_or_default();
            return finish(segments, rect, hwnd, RetrievalMethod::Uia);
        }

        let Ok(parent) = walker.GetParentElement(&current) else {
            break;
        };
        if parent.CurrentProcessId().unwrap_or(-1) != process_id {
            break;
        }
        current = parent;
    }

    None
}

unsafe fn uia_segments(element: &windows::Win32::UI::Accessibility::IUIAutomationElement) -> Vec<String> {
    let mut segments = Vec::new();

    let name = element
        .CurrentName()
        .map(|name| name.to_string())
        .unwrap_or_default();
    push_fragment(&mut segments, &name);

    if let Ok(pattern) =
        element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
    {
        if let Ok(value) = pattern.CurrentValue() {
            push_fragment(&mut segments, &value.to_string());
        }
    }

    if let Ok(pattern) = element
        .GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(UIA_LegacyIAccessiblePatternId)
    {
        if let Ok(name) = pattern.CurrentName() {
            push_fragment(&mut segments, &name.to_string());
        }
        if let Ok(value) = pattern.CurrentValue() {
            push_fragment(&mut segments, &value.to_string());
        }
        if let Ok(description) = pattern.CurrentDescription() {
            push_fragment(&mut segments, &description.to_string());
        }
    }

    segments
}

pub(crate) fn push_fragment(segments: &mut Vec<String>, value: &str) {
    let cleaned = clean_segment(value);
    if !cleaned.is_empty() && !segments.iter().any(|existing| existing == &cleaned) {
        segments.push(cleaned);
    }
}

// ---------------------------------------------------------------------------
// MSAA / IAccessible
// ---------------------------------------------------------------------------

unsafe fn capture_msaa(point: POINT) -> Option<Capture> {
    let mut accessible: Option<IAccessible> = None;
    let mut child = VARIANT::new();
    AccessibleObjectFromPoint(point, &mut accessible, &mut child).ok()?;
    let mut accessible = accessible?;

    // The same Chromium quirk as above.
    let mut second_accessible: Option<IAccessible> = None;
    let mut second_child = VARIANT::new();
    if AccessibleObjectFromPoint(point, &mut second_accessible, &mut second_child).is_ok() {
        if let Some(second) = second_accessible {
            accessible = second;
            child = second_child;
        }
    }

    let mut hwnd = HWND::default();
    WindowFromAccessibleObject(&accessible, Some(&mut hwnd)).ok()?;

    let mut process_id = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut process_id));

    let mut variant = child;
    for _ in 0..MAX_TREE_DEPTH {
        let mut segments = msaa_segments(&accessible, &variant);

        let mut child_id = i32::try_from(&variant).unwrap_or(0);
        if segments.is_empty() && child_id != 0 {
            // Sometimes the object itself has text while the child element
            // returned for the point does not.
            variant = VARIANT::from(0i32);
            child_id = 0;
            segments = msaa_segments(&accessible, &variant);
        }

        if !segments.is_empty() {
            let mut left = 0i32;
            let mut top = 0i32;
            let mut width = 0i32;
            let mut height = 0i32;
            let rect = if accessible
                .accLocation(&mut left, &mut top, &mut width, &mut height, &variant)
                .is_ok()
                && width > 0
                && height > 0
            {
                RECT {
                    left,
                    top,
                    right: left + width,
                    bottom: top + height,
                }
            } else {
                win32::get_window_rect(hwnd)
            };

            return finish(segments, rect, hwnd, RetrievalMethod::Msaa);
        }

        if child_id != 0 {
            variant = VARIANT::from(0i32);
            continue;
        }

        let parent_dispatch = accessible.accParent().ok()?;
        let parent: IAccessible = parent_dispatch.cast().ok()?;

        let mut parent_hwnd = HWND::default();
        WindowFromAccessibleObject(&parent, Some(&mut parent_hwnd)).ok()?;
        let mut parent_process_id = 0u32;
        GetWindowThreadProcessId(parent_hwnd, Some(&mut parent_process_id));
        if parent_process_id != process_id {
            break;
        }

        accessible = parent;
        variant = VARIANT::from(0i32);
    }

    None
}

unsafe fn msaa_segments(accessible: &IAccessible, variant: &VARIANT) -> Vec<String> {
    let mut segments = Vec::new();

    let name = accessible
        .get_accName(variant)
        .map(|value| value.to_string())
        .unwrap_or_default();
    push_fragment(&mut segments, &name);

    let value = accessible
        .get_accValue(variant)
        .map(|value| value.to_string())
        .unwrap_or_default();
    push_fragment(&mut segments, &value);

    let self_variant = VARIANT::from(0i32);
    let role = accessible
        .get_accRole(&self_variant)
        .ok()
        .and_then(|role| i32::try_from(&role).ok())
        .unwrap_or(0);

    if role != ROLE_SYSTEM_TITLEBAR {
        let description = accessible
            .get_accDescription(variant)
            .map(|value| value.to_string())
            .unwrap_or_default();
        push_fragment(&mut segments, &description);
    }

    segments
}

// ---------------------------------------------------------------------------
// WM_GETTEXT fallback
// ---------------------------------------------------------------------------

unsafe fn capture_window_text(point: POINT) -> Option<Capture> {
    let hwnd = windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(point);
    if win32::hwnd_is_null(hwnd) {
        return None;
    }

    let root = win32::get_root(hwnd);
    let mut segments = Vec::new();

    if hwnd != root {
        let text = win32::get_text(hwnd);
        if !text.trim().is_empty() {
            segments.push(text);
        }
    }

    if segments.is_empty() {
        let mut collected: Vec<String> = Vec::new();
        let _ = EnumChildWindows(
            root,
            Some(collect_child_text),
            LPARAM(&mut collected as *mut Vec<String> as isize),
        );

        let joined = collected
            .into_iter()
            .map(|text| clean_segment(&text))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\r\n");
        if !joined.trim().is_empty() {
            segments.push(joined);
        }
    }

    if segments.is_empty() {
        let title = win32::get_text(root);
        if !title.trim().is_empty() {
            segments.push(title);
        }
    }

    if segments.is_empty() {
        return None;
    }

    let rect = win32::get_window_rect(if hwnd != root { hwnd } else { root });
    finish(segments, rect, hwnd, RetrievalMethod::WindowText)
}

unsafe extern "system" fn collect_child_text(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let collected = &mut *(lparam.0 as *mut Vec<String>);
    let class = win32::get_class(hwnd).to_ascii_lowercase();

    let interesting = class == "static"
        || class == "edit"
        || class.starts_with("richedit")
        || class == "syslink";
    if interesting {
        let text = win32::get_text(hwnd);
        if !text.trim().is_empty() {
            collected.push(text);
        }
    }

    BOOL(1)
}