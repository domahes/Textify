//! Transient popup that shows the captured text.
//!
//! The window is borderless and closes when it loses focus. Pressing F2
//! "pins" it: a caption and a resizable frame appear, and it stays open.

use std::sync::Once;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetSysColor,
    GetSysColorBrush, InvalidateRect, ReleaseDC, ScreenToClient, SelectObject, SetBkMode,
    SetTextColor, COLOR_GRAYTEXT, COLOR_MENUBAR, COLOR_WINDOW, DT_CALCRECT, DT_END_ELLIPSIS,
    DT_EXPANDTABS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, HBRUSH,
    HFONT, TRANSPARENT,
};
use windows::Win32::UI::Controls::{EM_GETSEL, EM_SETMARGINS, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_A, VK_C, VK_CONTROL, VK_ESCAPE, VK_F2, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOVSCROLL, ES_MULTILINE,
    ES_NOHIDESEL, ES_READONLY, GetClientRect, GetParent, GetWindowLongPtrW, GetWindowRect,
    EC_LEFTMARGIN, EC_RIGHTMARGIN, GetWindowTextLengthW, GetWindowTextW, KillTimer,
    LoadCursorW, PostMessageW, RegisterClassW, SendMessageW, SetTimer, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, CREATESTRUCTW, CS_DROPSHADOW, GWLP_USERDATA,
    GWL_EXSTYLE, GWL_STYLE, HICON, HMENU, HTCLIENT, HTCAPTION, IDC_ARROW, SW_SHOWNOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOZORDER, WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_ACTIVATE, WM_APP, WM_CHAR, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_GETDLGCODE,
    WM_KEYDOWN, WM_NCHITTEST, WM_PAINT, WM_SETFONT, WM_SIZE, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW,
    WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WS_SYSMENU,
    WS_THICKFRAME, WS_VISIBLE, WS_VSCROLL,
};

use crate::capture::Capture;
use crate::config::Config;
use crate::text::{join_segments, utf16_len};
use crate::win32;

const CLASS_NAME: &str = "TextifyRS_Result";
const TIMER_ARM: usize = 1;
const WM_APP_PIN: u32 = WM_APP + 20;

struct ResultState {
    hwnd: HWND,
    edit: HWND,
    text: String,
    offsets: Vec<usize>,
    method_label: String,
    title: String,
    pinned: bool,
    armed: bool,
    auto_copy: bool,
    font: HFONT,
    font_small: HFONT,
    white: HBRUSH,
    dpi: u32,
    footer: i32,
}

static REGISTERED: Once = Once::new();

pub fn show(owner: HWND, icon: HICON, capture: &Capture, config: &Config, auto_copy: bool) -> Option<HWND> {
    ensure_class(icon);

    let dpi = win32::dpi_for_window(owner);
    let points = if config.font_size == 0 {
        10.5
    } else {
        config.font_size as f32
    };
    let font = win32::create_font(&config.font_face, points, 400, dpi);
    let font_small = win32::create_font(&config.font_face, 8.5, 400, dpi);

    let segments = if config.unicode_spaces {
        capture
            .segments
            .iter()
            .map(|segment| crate::text::unicode_spaces_to_ascii(segment))
            .collect::<Vec<_>>()
    } else {
        capture.segments.clone()
    };
    let (text, offsets) = join_segments(&segments);

    let footer = win32::scale(30, dpi);
    let padding = win32::scale(10, dpi);
    let work = win32::work_area_near(win32::cursor_pos());
    let max_width = (win32::rect_width(&work) - win32::scale(60, dpi)).clamp(win32::scale(260, dpi), win32::scale(640, dpi));

    let (text_width, text_height) = measure_text(&text, font, max_width - padding * 2);
    let width = (text_width + padding * 2 + win32::scale(8, dpi)).clamp(win32::scale(280, dpi), max_width);
    let height = (text_height + padding * 2 + footer).clamp(
        win32::scale(120, dpi),
        (win32::rect_height(&work) - win32::scale(48, dpi)).max(win32::scale(160, dpi)),
    );

    let cursor = win32::cursor_pos();
    let bounds = win32::center_rect_in(cursor, width, height, &work);

    let white = unsafe { CreateSolidBrush(COLORREF(0x00FFFFFF)) };

    // The whole-menu mode produced a different kind of capture; say so instead
    // of pretending a single element was extracted.
    let method_label = if capture.menu {
        format!("Всё меню · {}", capture.method.short_ru())
    } else {
        capture.method.label_ru().to_string()
    };

    let state = Box::new(ResultState {
        hwnd: HWND::default(),
        edit: HWND::default(),
        text,
        offsets,
        method_label,
        title: capture.title.clone(),
        pinned: false,
        armed: false,
        auto_copy,
        font,
        font_small,
        white,
        dpi,
        footer,
    });
    let state_raw = Box::into_raw(state);

    let class_name = win32::wide(CLASS_NAME);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            PCWSTR(class_name.as_ptr()),
            w!(""),
            WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
            bounds.left,
            bounds.top,
            width,
            height,
            owner,
            None,
            win32::instance(),
            Some(state_raw as *const _),
        )
    };

    let Ok(hwnd) = hwnd else {
        // Window creation failed before WM_CREATE took ownership of the state.
        unsafe {
            let state = Box::from_raw(state_raw);
            let _ = DeleteObject(state.font);
            let _ = DeleteObject(state.font_small);
            let _ = DeleteObject(state.white);
            drop(state);
        }
        return None;
    };

    unsafe {
        let preference: u32 = 2;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const u32 as *const _,
            4,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        win32::force_foreground(hwnd);
        let state = state_ptr(hwnd);
        if !state.is_null() {
            let state = &*state;
            let _ = SetFocus(state.edit);
            let _ = SendMessageW(state.edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
            if state.auto_copy {
                let _ = win32::copy_to_clipboard(hwnd, &state.text);
            }
        }
        let _ = SetTimer(hwnd, TIMER_ARM, 150, None);
    }

    Some(hwnd)
}

fn ensure_class(icon: HICON) {
    REGISTERED.call_once(|| unsafe {
        let class_name = win32::wide(CLASS_NAME);
        // Leak the class name: RegisterClassW stores the pointer.
        let class_name = Box::leak(class_name.into_boxed_slice());
        let class = WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: win32::instance(),
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&class);
    });
}

fn measure_text(text: &str, font: HFONT, max_width: i32) -> (i32, i32) {
    unsafe {
        let hdc = GetDC(None);
        let previous = SelectObject(hdc, font);
        let mut units: Vec<u16> = text.encode_utf16().collect();
        if units.is_empty() {
            units.push(b' ' as u16);
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: max_width.max(1),
            bottom: 0,
        };
        DrawTextW(
            hdc,
            &mut units,
            &mut rect,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX | DT_EXPANDTABS,
        );
        SelectObject(hdc, previous);
        let _ = ReleaseDC(None, hdc);
        ((rect.right - rect.left).max(1), (rect.bottom - rect.top).max(1))
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_CREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        let state = &mut *(create.lpCreateParams as *mut ResultState);
        state.hwnd = hwnd;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        create_edit(state);
        return LRESULT(0);
    }

    let state = state_ptr(hwnd);
    if state.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }

    if message == WM_DESTROY {
        // Handled before any &mut is formed: the box is reclaimed through the
        // raw pointer, and the back-pointer is cleared first so a repeated
        // WM_DESTROY cannot double free it.
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let state = Box::from_raw(state);
        let _ = KillTimer(hwnd, TIMER_ARM);
        let _ = DeleteObject(state.font);
        let _ = DeleteObject(state.font_small);
        let _ = DeleteObject(state.white);
        drop(state);
        return LRESULT(0);
    }

    let state = &mut *state;
    match message {
        WM_SIZE => {
            layout(state);
            LRESULT(0)
        }
        WM_PAINT => {
            paint(state);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER if wparam.0 == TIMER_ARM => {
            let _ = KillTimer(hwnd, TIMER_ARM);
            state.armed = true;
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let inactive = (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE;
            if inactive && state.armed && !state.pinned {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_APP_PIN => {
            pin(state);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            handle_key(state, wparam.0 as u32);
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let result = DefWindowProcW(hwnd, message, wparam, lparam);
            if result.0 == HTCLIENT as isize && hit_footer(state, lparam.0) {
                LRESULT(HTCAPTION as isize)
            } else {
                result
            }
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn create_edit(state: &mut ResultState) {
    let hwnd = state.hwnd;
    state.edit = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("EDIT"),
        w!(""),
        WS_CHILD
            | WS_VISIBLE
            | WS_VSCROLL
            | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_NOHIDESEL | ES_AUTOVSCROLL) as u32),
        0,
        0,
        10,
        10,
        hwnd,
        HMENU(1isize as _),
        win32::instance(),
        None,
    )
    .unwrap_or_default();

    let _ = SendMessageW(state.edit, WM_SETFONT, WPARAM(state.font.0 as usize), LPARAM(1));
    let text = HSTRING::from(&state.text);
    let _ = SetWindowTextW(state.edit, &text);
    let margin = win32::scale(6, state.dpi) as usize;
    let _ = SendMessageW(
        state.edit,
        EM_SETMARGINS,
        WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize),
        LPARAM((margin | (margin << 16)) as isize),
    );
    let _ = SetWindowSubclass(state.edit, Some(edit_proc), 1, hwnd.0 as usize);
    layout(state);
}

unsafe fn layout(state: &ResultState) {
    let mut client = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut client);
    let pad = win32::scale(6, state.dpi);
    let gap = win32::scale(4, state.dpi);
    let _ = SetWindowPos(
        state.edit,
        None,
        pad,
        pad,
        (client.right - pad * 2).max(1),
        (client.bottom - state.footer - pad - gap).max(1),
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

unsafe fn paint(state: &ResultState) {
    let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
    let hdc = BeginPaint(state.hwnd, &mut paint);
    let mut client = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut client);

    let footer = RECT {
        left: 0,
        top: client.bottom - state.footer,
        right: client.right,
        bottom: client.bottom,
    };
    FillRect(hdc, &footer, GetSysColorBrush(COLOR_MENUBAR));

    let previous = SelectObject(hdc, state.font_small);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, COLORREF(GetSysColor(COLOR_GRAYTEXT)));

    let hint = if state.pinned {
        "Ctrl+C — копировать  ·  Esc — закрыть"
    } else {
        "Ctrl+C — копировать  ·  Tab — фрагмент  ·  F2 — закрепить"
    };
    let mut hint_units: Vec<u16> = hint.encode_utf16().collect();
    let inset = win32::scale(10, state.dpi);
    let gap = win32::scale(12, state.dpi);
    let mut measured = RECT::default();
    DrawTextW(
        hdc,
        &mut hint_units,
        &mut measured,
        DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
    );
    let hint_width = measured.right - measured.left;
    let available = footer.right - footer.left - inset * 2 - gap;
    // Keep the hint readable, but never let it crowd the source label out.
    let hint_span = hint_width.min(available * 2 / 3).max(0);
    let mut hint_rect = RECT {
        left: footer.left + inset,
        top: footer.top,
        right: footer.left + inset + hint_span,
        bottom: footer.bottom,
    };
    DrawTextW(
        hdc,
        &mut hint_units,
        &mut hint_rect,
        DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX | DT_END_ELLIPSIS,
    );

    let source = if state.title.is_empty() {
        state.method_label.clone()
    } else {
        format!("{}  ·  {}", state.method_label, state.title)
    };
    let mut source_units: Vec<u16> = source.encode_utf16().collect();
    let mut source_rect = RECT {
        left: hint_rect.right + gap,
        top: footer.top,
        right: footer.right - inset,
        bottom: footer.bottom,
    };
    DrawTextW(
        hdc,
        &mut source_units,
        &mut source_rect,
        DT_SINGLELINE | DT_VCENTER | DT_RIGHT | DT_NOPREFIX | DT_END_ELLIPSIS,
    );

    SelectObject(hdc, previous);
    let _ = EndPaint(state.hwnd, &paint);
}

fn hit_footer(state: &ResultState, lparam: isize) -> bool {
    unsafe {
        let packed = lparam as u32;
        let mut point = POINT {
            x: (packed & 0xFFFF) as u16 as i16 as i32,
            y: ((packed >> 16) & 0xFFFF) as u16 as i16 as i32,
        };
        let _ = ScreenToClient(state.hwnd, &mut point);
        let mut client = RECT::default();
        let _ = GetClientRect(state.hwnd, &mut client);
        point.y >= client.bottom - state.footer
    }
}

unsafe fn pin(state: &mut ResultState) {
    if state.pinned {
        return;
    }
    state.pinned = true;

    let style = GetWindowLongPtrW(state.hwnd, GWL_STYLE) as u32;
    let new_style = (style & !WS_POPUP.0) | WS_CAPTION.0 | WS_SYSMENU.0 | WS_THICKFRAME.0;
    SetWindowLongPtrW(state.hwnd, GWL_STYLE, new_style as isize);

    let mut client = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut client);
    let ex_style = WINDOW_EX_STYLE(GetWindowLongPtrW(state.hwnd, GWL_EXSTYLE) as u32);
    let mut outer = client;
    let _ = AdjustWindowRectEx(&mut outer, WINDOW_STYLE(new_style), false, ex_style);

    let mut position = RECT::default();
    let _ = GetWindowRect(state.hwnd, &mut position);
    let _ = SetWindowPos(
        state.hwnd,
        None,
        position.left,
        position.top,
        outer.right - outer.left,
        outer.bottom - outer.top,
        SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
    );
    let _ = SetWindowTextW(state.hwnd, w!("Textify RS"));
    let _ = InvalidateRect(state.hwnd, None, true);
}

unsafe fn handle_key(state: &mut ResultState, key: u32) {
    match key {
        k if k == VK_ESCAPE.0 as u32 => {
            let _ = DestroyWindow(state.hwnd);
        }
        k if k == VK_F2.0 as u32 => pin(state),
        _ => {}
    }
}

unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let Ok(parent) = GetParent(hwnd) else {
        return DefSubclassProc(hwnd, message, wparam, lparam);
    };
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let key = wparam.0 as u32;
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
            if key == VK_ESCAPE.0 as u32 {
                let _ = PostMessageW(parent, WM_CLOSE, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            if key == VK_F2.0 as u32 {
                let _ = PostMessageW(parent, WM_APP_PIN, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            if key == VK_TAB.0 as u32 {
                let state = state_ptr(parent);
                if !state.is_null() {
                    cycle_segment(&*state, shift);
                }
                return LRESULT(0);
            }
            if ctrl && key == VK_A.0 as u32 {
                let _ = SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
                return LRESULT(0);
            }
            if ctrl && key == VK_C.0 as u32 {
                copy_selection(hwnd, parent);
                return LRESULT(0);
            }
        }
        WM_CHAR => {
            if (wparam.0 as u32) < 0x20 {
                return LRESULT(0);
            }
        }
        WM_GETDLGCODE => return LRESULT(0x0004),
        _ => {}
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe fn cycle_segment(state: &ResultState, backwards: bool) {
    if state.offsets.is_empty() {
        return;
    }
    let total = utf16_len(&state.text) as u32;
    let (start, end) = selection(state.edit);
    let index = if start == 0 && end >= total {
        if backwards {
            state.offsets.len() - 1
        } else {
            0
        }
    } else {
        let current = state
            .offsets
            .iter()
            .rposition(|&offset| (offset as u32) <= start)
            .unwrap_or(0);
        if backwards {
            current.saturating_sub(1)
        } else {
            (current + 1) % state.offsets.len()
        }
    };

    let from = state.offsets[index] as u32;
    let to = state
        .offsets
        .get(index + 1)
        .map(|offset| offset.saturating_sub(4) as u32)
        .unwrap_or(total);
    let _ = SendMessageW(state.edit, EM_SETSEL, WPARAM(from as usize), LPARAM(to as isize));
}

unsafe fn copy_selection(edit: HWND, owner: HWND) {
    let (start, end) = selection(edit);
    let length = GetWindowTextLengthW(edit);
    if length <= 0 {
        return;
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let copied = GetWindowTextW(edit, &mut buffer);
    if copied <= 0 {
        return;
    }
    let units = &buffer[..copied as usize];
    let slice = if end > start && (end as usize) <= units.len() {
        &units[start as usize..end as usize]
    } else {
        units
    };
    let text = String::from_utf16_lossy(slice);
    let _ = win32::copy_to_clipboard(owner, &text);
}

unsafe fn selection(edit: HWND) -> (u32, u32) {
    let mut start = 0u32;
    let mut end = 0u32;
    let _ = SendMessageW(
        edit,
        EM_GETSEL,
        WPARAM(&mut start as *mut u32 as usize),
        LPARAM(&mut end as *mut u32 as isize),
    );
    (start, end)
}

/// Returns the raw state pointer stored in GWLP_USERDATA, or null. Callers
/// dereference it only for the duration of one message and never turn it into
/// a long-lived `&'static mut`.
fn state_ptr(hwnd: HWND) -> *mut ResultState {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ResultState }
}

#[allow(dead_code)]
fn _silence_color(color: COLORREF) -> COLORREF {
    color
}
