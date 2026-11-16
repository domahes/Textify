//! Special "whole menu" capture mode.
//!
//! The ordinary shortcut copies the text of the element under the cursor; for
//! a menu that is a single item. This module implements the dedicated mode
//! bound to Ctrl+Shift+middle-click: it collects every item of the open menu
//! window instead.
//!
//! Three providers are tried, in the same spirit as [`crate::capture`]:
//!
//! 1. Classic `#32768` popup windows — ask the menu window for its `HMENU`
//!    (`MN_GETHMENU`) and read the item strings. Other visible popups of the
//!    same process (open submenus) are appended too. This path only uses
//!    window messages, so it cannot stall behind an automation provider.
//! 2. UI Automation — climb to the outermost Menu/MenuBar element and read its
//!    MenuItem descendants in tree order. This covers modern surfaces (Office,
//!    Chromium, WinUI) and hosted dropdowns.
//! 3. MSAA / IAccessible as a last resort for old applications.

use std::ffi::c_void;

use windows::core::{Interface, VARIANT};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Accessibility::{
    AccessibleObjectFromPoint, CUIAutomation, IAccessible, IUIAutomation, IUIAutomationElement,
    IUIAutomationTreeWalker, TreeScope_Descendants, UIA_ControlTypePropertyId,
    UIA_MenuBarControlTypeId, UIA_MenuControlTypeId, UIA_MenuItemControlTypeId,
    WindowFromAccessibleObject, ROLE_SYSTEM_MENUBAR, ROLE_SYSTEM_MENUITEM, ROLE_SYSTEM_MENUPOPUP,
    ROLE_SYSTEM_SEPARATOR, ROLE_SYSTEM_TITLEBAR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetMenuStringW, GetMenuItemCount, GetParent, GetWindowThreadProcessId,
    IsWindowVisible, SendMessageTimeoutW, WindowFromPoint, HMENU, MF_BYPOSITION, MN_GETHMENU,
    SMTO_ABORTIFHUNG, SMTO_BLOCK,
};

use crate::capture::{finish_with_title, push_fragment, Capture};
use crate::config::RetrievalMethod;
use crate::text::dedupe_segments;
use crate::win32;

/// Window class of a classic Win32 popup menu.
const CLASSIC_MENU_CLASS: &str = "#32768";
/// A menu dump is meant for humans: stop before it turns into a directory
/// listing, and never let an unlucky tree walk run away.
const MAX_MENU_ITEMS: usize = 400;
const MAX_TREE_DEPTH: usize = 32;
const MENU_ITEM_BUFFER: usize = 512;
/// Same order of magnitude as the WM_GETTEXT timeout used elsewhere: a menu
/// that cannot answer this fast is treated as unavailable.
const MENU_REPLY_TIMEOUT_MS: u32 = 150;

/// Captures every item of the menu under `point`.
///
/// Returns `None` when the point is not over a menu at all, so the caller can
/// fall back to the ordinary single-element capture.
pub fn capture_menu_at(point: POINT) -> Option<Capture> {
    // A classic Win32 popup is answered purely by window messages: no COM, no
    // cross-process automation call that could stall behind a busy menu loop.
    // Modern surfaces (Office, Chromium, WinUI) are not #32768 windows, so they
    // fall through to UI Automation.
    if let Some(capture) = unsafe { classic_menu_capture(point) } {
        return Some(capture);
    }

    unsafe {
        uia_menu_capture(point).or_else(|| msaa_menu_capture(point))
    }
}

/// Removes the access-key marker (`&`) from a menu string.
pub fn strip_menu_markers(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '&' {
            // `&&` is a literal ampersand; a single one marks the access key.
            if chars.peek() == Some(&'&') {
                chars.next();
                result.push('&');
            }
        } else {
            result.push(character);
        }
    }
    result
}

/// Pushes a classic menu string, removing its access-key marker (`&`).
/// Accessible names from UIA/MSAA have no marker and are pushed unchanged, so
/// a literal ampersand in them is never lost.
fn push_menu_fragment(segments: &mut Vec<String>, text: &str) {
    let text = strip_menu_markers(text);
    if text.is_empty() {
        return;
    }
    push_fragment(segments, &text);
}

/// A menu is a single list, not a set of unrelated fragments: the collected
/// items become one block separated by plain line breaks instead of the blank
/// lines used between fragments of a dialog.
fn collapse_menu_segments(items: Vec<String>) -> Vec<String> {
    let items = dedupe_segments(items);
    if items.is_empty() {
        return Vec::new();
    }
    vec![items.join("\n")]
}

// ---------------------------------------------------------------------------
// UI Automation
// ---------------------------------------------------------------------------

unsafe fn uia_menu_capture(point: POINT) -> Option<Capture> {
    let automation: IUIAutomation =
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
    let mut element = automation.ElementFromPoint(point).ok()?;
    // The first query of a fresh UIA client sometimes returns the window
    // behind the popup (the same quirk the ordinary capture compensates for).
    if let Ok(second) = automation.ElementFromPoint(point) {
        element = second;
    }
    let walker = automation.RawViewWalker().ok()?;
    let process_id = element.CurrentProcessId().unwrap_or(0);

    // Climb to the outermost menu of the open chain: a submenu is nested in
    // its parent menu, and only the parent covers the whole structure.
    let mut menu: Option<IUIAutomationElement> = None;
    let mut current = element;
    for _ in 0..MAX_TREE_DEPTH {
        let control_type = current.CurrentControlType().map(|value| value.0).unwrap_or(0);
        if control_type == UIA_MenuControlTypeId.0 {
            menu = Some(current.clone());
        } else if control_type == UIA_MenuBarControlTypeId.0 {
            // Hovering a menu-bar item without an open popup: the bar itself
            // is the best root we have.
            if menu.is_none() {
                menu = Some(current.clone());
            }
            break;
        } else if menu.is_some() {
            break;
        }

        let Ok(parent) = walker.GetParentElement(&current) else {
            break;
        };
        if parent.CurrentProcessId().unwrap_or(-1) != process_id {
            break;
        }
        current = parent;
    }

    let menu = menu?;
    let segments = uia_menu_items(&automation, &menu);
    if segments.is_empty() {
        return None;
    }

    let rect = menu.CurrentBoundingRectangle().unwrap_or_default();
    let hwnd = menu.CurrentNativeWindowHandle().unwrap_or_default();
    finish_with_title(
        collapse_menu_segments(segments),
        rect,
        hwnd,
        RetrievalMethod::Uia,
        true,
        // No caption: while a menu is modal, the caption of its owner can only
        // be read through a cross-thread message the menu loop may not answer.
        Some(String::new()),
    )
}

unsafe fn uia_menu_items(automation: &IUIAutomation, root: &IUIAutomationElement) -> Vec<String> {
    let mut segments = Vec::new();

    if let Ok(condition) = automation.CreatePropertyCondition(
        UIA_ControlTypePropertyId,
        &VARIANT::from(UIA_MenuItemControlTypeId.0),
    ) {
        if let Ok(array) = root.FindAll(TreeScope_Descendants, &condition) {
            let length = array.Length().unwrap_or(0);
            for index in 0..length {
                if segments.len() >= MAX_MENU_ITEMS {
                    break;
                }
                let Ok(item) = array.GetElement(index) else {
                    continue;
                };
                if let Ok(name) = item.CurrentName() {
                    push_fragment(&mut segments, &name.to_string());
                }
            }
        }
    }

    if segments.len() < 2 {
        // Toolbars and owner-drawn dropdowns expose their entries as buttons
        // or custom elements; fall back to a plain name walk in tree order.
        if let Ok(walker) = automation.RawViewWalker() {
            let mut names = Vec::new();
            walk_names(&walker, root, &mut names, 0);
            if names.len() > segments.len() {
                segments = names;
            }
        }
    }

    segments
}

unsafe fn walk_names(
    walker: &IUIAutomationTreeWalker,
    parent: &IUIAutomationElement,
    segments: &mut Vec<String>,
    depth: usize,
) {
    if depth > MAX_TREE_DEPTH || segments.len() >= MAX_MENU_ITEMS {
        return;
    }

    let mut child = walker.GetFirstChildElement(parent).ok();
    while let Some(element) = child {
        if segments.len() >= MAX_MENU_ITEMS {
            return;
        }
        if let Ok(name) = element.CurrentName() {
            push_fragment(segments, &name.to_string());
        }
        walk_names(walker, &element, segments, depth + 1);
        child = walker.GetNextSiblingElement(&element).ok();
    }
}

// ---------------------------------------------------------------------------
// Classic Win32 popup menus (#32768)
// ---------------------------------------------------------------------------

unsafe fn classic_menu_capture(point: POINT) -> Option<Capture> {
    let popup = classic_menu_window(WindowFromPoint(point))?;

    let mut segments = Vec::new();
    collect_popup_items(&mut segments, popup);

    // Open submenus live in popup windows of their own; collect them too so
    // the dump covers everything the user can see right now.
    let mut process_id = 0u32;
    GetWindowThreadProcessId(popup, Some(&mut process_id));
    let mut search = PopupSearch {
        process_id,
        skip: popup,
        found: Vec::new(),
    };
    let _ = EnumWindows(
        Some(collect_visible_popup),
        LPARAM(&mut search as *mut PopupSearch as isize),
    );
    // EnumWindows walks the z-order from topmost down: reversing it lists the
    // parent popup before the submenus that were opened from it.
    for other in search.found.into_iter().rev() {
        collect_popup_items(&mut segments, other);
    }

    if segments.is_empty() {
        return None;
    }

    finish_with_title(
        collapse_menu_segments(segments),
        win32::get_window_rect(popup),
        popup,
        RetrievalMethod::WindowText,
        true,
        Some(String::new()),
    )
}

/// Walks up to a classic menu window, so that a click on a hosted/child
/// window still resolves to the popup that actually owns it.
unsafe fn classic_menu_window(hwnd: HWND) -> Option<HWND> {
    let mut current = hwnd;
    for _ in 0..8 {
        if win32::hwnd_is_null(current) {
            return None;
        }
        if win32::get_class(current) == CLASSIC_MENU_CLASS {
            return Some(current);
        }
        current = GetParent(current).unwrap_or_default();
    }
    None
}

struct PopupSearch {
    process_id: u32,
    skip: HWND,
    found: Vec<HWND>,
}

unsafe extern "system" fn collect_visible_popup(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = &mut *(lparam.0 as *mut PopupSearch);
    if hwnd != search.skip
        && win32::get_class(hwnd) == CLASSIC_MENU_CLASS
        && IsWindowVisible(hwnd).as_bool()
    {
        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        if process_id == search.process_id {
            search.found.push(hwnd);
        }
    }
    BOOL(1)
}

unsafe fn collect_popup_items(segments: &mut Vec<String>, popup: HWND) {
    if segments.len() >= MAX_MENU_ITEMS {
        return;
    }

    // A menu whose owning thread is stuck must not freeze the whole app, so
    // every cross-thread call here is bounded.
    let mut handle: usize = 0;
    let sent = SendMessageTimeoutW(
        popup,
        MN_GETHMENU,
        WPARAM(0),
        LPARAM(0),
        SMTO_ABORTIFHUNG | SMTO_BLOCK,
        MENU_REPLY_TIMEOUT_MS,
        Some(&mut handle),
    );
    let menu = if sent.0 != 0 {
        HMENU(handle as *mut c_void)
    } else {
        HMENU::default()
    };

    if menu.0.is_null() {
        // Not every hosted menu is a real menu window; some still answer
        // WM_GETTEXT with their whole visible text.
        for line in win32::get_text(popup).split(['\r', '\n']) {
            push_menu_fragment(segments, line);
        }
        return;
    }

    let count = GetMenuItemCount(menu).clamp(0, MAX_MENU_ITEMS as i32);
    for index in 0..count {
        if segments.len() >= MAX_MENU_ITEMS {
            return;
        }
        let mut buffer = [0u16; MENU_ITEM_BUFFER];
        let length = GetMenuStringW(menu, index as u32, Some(&mut buffer), MF_BYPOSITION);
        if length <= 0 {
            continue;
        }
        let text = String::from_utf16_lossy(&buffer[..length as usize]);
        push_menu_fragment(segments, &text);
    }
}

// ---------------------------------------------------------------------------
// MSAA / IAccessible
// ---------------------------------------------------------------------------

unsafe fn msaa_menu_capture(point: POINT) -> Option<Capture> {
    let mut accessible: Option<IAccessible> = None;
    let mut child = VARIANT::new();
    AccessibleObjectFromPoint(point, &mut accessible, &mut child).ok()?;
    let mut accessible = accessible?;

    let mut hwnd = HWND::default();
    WindowFromAccessibleObject(&accessible, Some(&mut hwnd)).ok()?;
    let mut process_id = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut process_id));

    // Same outermost-container logic as in the UIA path.
    let mut container: Option<IAccessible> = None;
    for _ in 0..MAX_TREE_DEPTH {
        let role = msaa_role(&accessible, &VARIANT::from(0i32));
        if role == ROLE_SYSTEM_MENUPOPUP as i32 || role == ROLE_SYSTEM_MENUBAR as i32 {
            container = Some(accessible.clone());
        } else if container.is_some() {
            break;
        }

        let Ok(parent_dispatch) = accessible.accParent() else {
            break;
        };
        let Ok(parent) = parent_dispatch.cast::<IAccessible>() else {
            break;
        };

        let mut parent_hwnd = HWND::default();
        if WindowFromAccessibleObject(&parent, Some(&mut parent_hwnd)).is_err() {
            break;
        }
        let mut parent_process_id = 0u32;
        GetWindowThreadProcessId(parent_hwnd, Some(&mut parent_process_id));
        if parent_process_id != process_id {
            break;
        }
        accessible = parent;
    }

    let container = container?;
    let mut segments = Vec::new();
    msaa_menu_children(&container, &mut segments, 0);
    if segments.is_empty() {
        return None;
    }

    let rect = msaa_rect(&container);
    finish_with_title(
        collapse_menu_segments(segments),
        rect,
        hwnd,
        RetrievalMethod::Msaa,
        true,
        Some(String::new()),
    )
}

unsafe fn msaa_menu_children(accessible: &IAccessible, segments: &mut Vec<String>, depth: usize) {
    if depth > 8 || segments.len() >= MAX_MENU_ITEMS {
        return;
    }

    let count = accessible.accChildCount().unwrap_or(0);
    for id in 1..=count {
        if segments.len() >= MAX_MENU_ITEMS {
            return;
        }

        let child = VARIANT::from(id);
        let role = msaa_role(accessible, &child);
        if role == ROLE_SYSTEM_SEPARATOR as i32 || role == ROLE_SYSTEM_TITLEBAR as i32 {
            continue;
        }

        let name = accessible
            .get_accName(&child)
            .map(|value| value.to_string())
            .unwrap_or_default();
        push_fragment(segments, &name);

        // A menu item with an open submenu is exposed as a real child object;
        // simple items are only addressable through their parent's child id.
        if role == ROLE_SYSTEM_MENUITEM as i32 || role == ROLE_SYSTEM_MENUPOPUP as i32 {
            if let Ok(dispatch) = accessible.get_accChild(&child) {
                if let Ok(child_accessible) = dispatch.cast::<IAccessible>() {
                    msaa_menu_children(&child_accessible, segments, depth + 1);
                }
            }
        }
    }
}

unsafe fn msaa_role(accessible: &IAccessible, child: &VARIANT) -> i32 {
    accessible
        .get_accRole(child)
        .ok()
        .and_then(|role| i32::try_from(&role).ok())
        .unwrap_or(0)
}

unsafe fn msaa_rect(accessible: &IAccessible) -> RECT {
    let mut left = 0i32;
    let mut top = 0i32;
    let mut width = 0i32;
    let mut height = 0i32;
    if accessible
        .accLocation(&mut left, &mut top, &mut width, &mut height, &VARIANT::from(0i32))
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
        RECT::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_access_key_markers() {
        assert_eq!(strip_menu_markers("&Файл"), "Файл");
        assert_eq!(strip_menu_markers("Копировать\tCtrl+C"), "Копировать\tCtrl+C");
        assert_eq!(strip_menu_markers("a && b"), "a & b");
        assert_eq!(strip_menu_markers("&&&x"), "&x");
        assert_eq!(strip_menu_markers("plain"), "plain");
        assert_eq!(strip_menu_markers("&"), "");
    }
}
