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
    AccessibleObjectFromPoint, CUIAutomation, IAccessible, IUIAutomation, IUIAutomationElement,
    IUIAutomationLegacyIAccessiblePattern, IUIAutomationTextPattern, IUIAutomationTextPattern2,
    IUIAutomationValuePattern, TextUnit_Line, UIA_LegacyIAccessiblePatternId, UIA_TextPattern2Id,
    UIA_TextPatternId, UIA_ValuePatternId, WindowFromAccessibleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetWindowThreadProcessId};

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

#[derive(Clone, Copy, Debug)]
pub struct CaptureRequest {
    pub point: POINT,
    pub method: RetrievalMethod,
    pub menu: bool,
}

impl CaptureRequest {
    pub fn execute(self) -> Option<Capture> {
        self.execute_with(crate::menu::capture_menu_at, capture_at)
    }

    fn execute_with(
        self,
        menu: impl FnOnce(POINT) -> Option<Capture>,
        normal: impl FnOnce(POINT, RetrievalMethod) -> Option<Capture>,
    ) -> Option<Capture> {
        if self.menu {
            menu(self.point).or_else(|| normal(self.point, self.method))
        } else {
            normal(self.point, self.method)
        }
    }
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
    win32::get_text(hwnd)
        .replace('\0', "")
        .replace(['\r', '\n'], " ")
}

// ---------------------------------------------------------------------------
// UI Automation
// ---------------------------------------------------------------------------

fn is_chromium_window(hwnd: HWND) -> bool {
    if win32::hwnd_is_null(hwnd) {
        return false;
    }
    let class = win32::get_class(hwnd).to_ascii_lowercase();
    if class.starts_with("chrome_")
        || class.contains("renderwidgethost")
        || class.contains("intermediate d3d window")
    {
        return true;
    }
    let root = win32::get_root(hwnd);
    if root != hwnd {
        let root_class = win32::get_class(root).to_ascii_lowercase();
        return root_class.starts_with("chrome_")
            || root_class.contains("renderwidgethost")
            || root_class.contains("intermediate d3d window");
    }
    false
}

fn is_generic_control_name(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "text editor"
            | "редактор текста"
            | "richedit control"
            | "richedit"
            | "rich edit"
            | "edit"
            | "document"
            | "документ"
            | "editor"
            | "редактор"
            | "text area"
            | "поле ввода"
            | "текстовое поле"
            | "область текста"
            | "текстовая область"
            | "терминал"
            | "terminal"
            | "консоль"
            | "console"
    )
}

unsafe fn capture_uia(point: POINT) -> Option<Capture> {
    let automation: IUIAutomation =
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;

    let hwnd_at_point = windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(point);
    let is_chromium = is_chromium_window(hwnd_at_point);

    let mut element = match automation.ElementFromPoint(point) {
        Ok(elem) => elem,
        Err(_) if is_chromium => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            automation.ElementFromPoint(point).ok()?
        }
        Err(_) => return None,
    };

    let is_chromium =
        is_chromium || is_chromium_window(element.CurrentNativeWindowHandle().unwrap_or_default());

    // Chromium lazily initializes its accessibility bridge upon the first
    // query; a brief delay allows the AXTree to populate.
    if is_chromium {
        std::thread::sleep(std::time::Duration::from_millis(35));
        if let Ok(second) = automation.ElementFromPoint(point) {
            element = second;
        }
    } else if let Ok(second) = automation.ElementFromPoint(point) {
        element = second;
    }

    let mut result = walk_uia_tree(&automation, &element, point);

    if result.is_none() && is_chromium {
        // If the accessibility tree was still populating on the first pass,
        // retry once more after another brief pause.
        std::thread::sleep(std::time::Duration::from_millis(60));
        if let Ok(retry_element) = automation.ElementFromPoint(point) {
            result = walk_uia_tree(&automation, &retry_element, point);
        }
    }

    result
}

unsafe fn walk_uia_tree(
    automation: &IUIAutomation,
    start: &IUIAutomationElement,
    point: POINT,
) -> Option<Capture> {
    let walker = automation.RawViewWalker().ok()?;
    let process_id = start.CurrentProcessId().unwrap_or(0);

    let mut current = start.clone();
    for _ in 0..MAX_TREE_DEPTH {
        let segments = uia_segments(&current, point);
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

unsafe fn extract_text_pattern(
    element: &IUIAutomationElement,
    point: POINT,
    segments: &mut Vec<String>,
) {
    let pattern: Option<IUIAutomationTextPattern> = element
        .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
        .ok()
        .or_else(|| {
            element
                .GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id)
                .ok()
                .and_then(|p2| p2.cast().ok())
        })
        .or_else(|| {
            element
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPattern2Id)
                .ok()
        });

    let Some(pattern) = pattern else {
        return;
    };

    let doc_text = pattern
        .DocumentRange()
        .ok()
        .and_then(|r| r.GetText(-1).ok())
        .map(|s| clean_segment(&s.to_string()))
        .filter(|s| !s.is_empty());

    // If the document is small/medium (<= 60 lines and <= 8192 chars), use it as the
    // complete text of the control. This avoids truncating multi-line notes or error dialogs,
    // and avoids duplicating the line at point.
    if let Some(ref text) = doc_text {
        if text.lines().count() <= 60 && text.len() <= 8192 {
            push_fragment(segments, text);
            return;
        }
    }

    // For large documents (e.g. terminal scrollback, large source code, or long web documents),
    // extract the line under the mouse cursor.
    if let Ok(range) = pattern.RangeFromPoint(point) {
        let _ = range.ExpandToEnclosingUnit(TextUnit_Line);
        if let Ok(line_text) = range.GetText(-1) {
            let cleaned = clean_segment(&line_text.to_string());
            if !cleaned.is_empty() {
                push_fragment(segments, &cleaned);
                return;
            }
        }
    }

    // If RangeFromPoint did not return text, fall back to the document text if available.
    if let Some(text) = doc_text {
        push_fragment(segments, &text);
    }
}

unsafe fn uia_segments(element: &IUIAutomationElement, point: POINT) -> Vec<String> {
    let mut segments = Vec::new();

    // 1. Text Pattern (IUIAutomationTextPattern / IUIAutomationTextPattern2)
    extract_text_pattern(element, point, &mut segments);

    // 2. Value Pattern (if Text Pattern did not provide text)
    if segments.is_empty() {
        if let Ok(pattern) =
            element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        {
            if let Ok(value) = pattern.CurrentValue() {
                push_fragment(&mut segments, &value.to_string());
            }
        }
    }

    // 3. CurrentName
    let name = element
        .CurrentName()
        .map(|name| name.to_string())
        .unwrap_or_default();
    let name_trimmed = name.trim();
    if segments.is_empty() {
        push_fragment(&mut segments, name_trimmed);
    } else if !is_generic_control_name(name_trimmed) {
        push_fragment(&mut segments, name_trimmed);
    }

    // 4. Legacy IAccessible Pattern
    if let Ok(pattern) = element.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
        UIA_LegacyIAccessiblePatternId,
    ) {
        if let Ok(name) = pattern.CurrentName() {
            let name_str = name.to_string();
            let trimmed = name_str.trim();
            if segments.is_empty() || !is_generic_control_name(trimmed) {
                push_fragment(&mut segments, trimmed);
            }
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
    let hwnd_at_point = windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(point);
    let is_chromium = is_chromium_window(hwnd_at_point);

    let mut accessible: Option<IAccessible> = None;
    let mut child = VARIANT::new();
    if AccessibleObjectFromPoint(point, &mut accessible, &mut child).is_err()
        || accessible.is_none()
    {
        if is_chromium {
            std::thread::sleep(std::time::Duration::from_millis(40));
            AccessibleObjectFromPoint(point, &mut accessible, &mut child).ok()?;
        } else {
            return None;
        }
    }
    let mut accessible = accessible?;

    // Chromium occasionally returns the correct element only on the second
    // query; allow a brief delay for its accessibility tree to initialize.
    if is_chromium {
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
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

    let interesting =
        class == "static" || class == "edit" || class.starts_with("richedit") || class == "syslink";
    if interesting {
        let text = win32::get_text(hwnd);
        if !text.trim().is_empty() {
            collected.push(text);
        }
    }

    BOOL(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_preserves_normal_and_menu_fallback_selection() {
        let point = POINT { x: 12, y: -3 };
        let request = CaptureRequest {
            point,
            method: RetrievalMethod::Msaa,
            menu: false,
        };
        request.execute_with(
            |_| panic!("normal capture must not try the menu provider"),
            |actual, method| {
                assert_eq!(
                    (actual.x, actual.y, method),
                    (12, -3, RetrievalMethod::Msaa)
                );
                None
            },
        );
        CaptureRequest {
            menu: true,
            ..request
        }
        .execute_with(
            |_| None,
            |actual, method| {
                assert_eq!(
                    (actual.x, actual.y, method),
                    (12, -3, RetrievalMethod::Msaa)
                );
                None
            },
        );
        let captured = Capture {
            text: "menu".into(),
            segments: vec!["menu".into()],
            rect: RECT::default(),
            hwnd: HWND::default(),
            title: String::new(),
            method: RetrievalMethod::Uia,
            menu: true,
        };
        let result = CaptureRequest {
            menu: true,
            ..request
        }
        .execute_with(
            |_| Some(captured),
            |_, _| panic!("successful menu capture must not fall back"),
        );
        assert!(result.unwrap().menu);
    }

    #[test]
    fn identifies_generic_control_names() {
        assert!(is_generic_control_name("Text Editor"));
        assert!(is_generic_control_name("Редактор текста"));
        assert!(is_generic_control_name("РЕДАКТОР ТЕКСТА"));
        assert!(is_generic_control_name("richedit control"));
        assert!(is_generic_control_name("Edit"));
        assert!(is_generic_control_name("Document"));
        assert!(is_generic_control_name("Документ"));
        assert!(is_generic_control_name("Поле ввода"));
        assert!(is_generic_control_name("Текстовое поле"));
        assert!(is_generic_control_name("Терминал"));
        assert!(is_generic_control_name("Console"));

        assert!(!is_generic_control_name("Текст заметки"));
        assert!(!is_generic_control_name("My Document.txt"));
        assert!(!is_generic_control_name("Имя пользователя:"));
    }
}
