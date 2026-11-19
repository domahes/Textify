//! Settings window: mouse shortcut, optional keyboard shortcut and extraction
//! options. Closing the window hides it; the process keeps running in the tray.

use std::path::PathBuf;
use std::sync::Once;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetSysColor, GetSysColorBrush, SetBkMode, SetTextColor, COLOR_BTNFACE, COLOR_GRAYTEXT,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::BST_CHECKED;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON,
    CB_ADDSTRING, CB_SETCURSEL, CBS_DROPDOWNLIST, CreateWindowExW, DefWindowProcW,
    ES_READONLY, EnumChildWindows, GetDlgItem, GetWindowLongPtrW, LoadCursorW,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, BM_GETCHECK, BM_SETCHECK, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    GWLP_ID, GWLP_USERDATA, HICON, HMENU, IDC_ARROW, SW_HIDE, SW_SHOWNORMAL, SWP_NOACTIVATE,
    SWP_NOZORDER, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_GETDLGCODE, WM_KEYDOWN, WM_SETFONT,
    WM_SYSKEYDOWN, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_MINIMIZEBOX, WS_OVERLAPPED,
    WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::Win32::System::SystemServices::SS_LEFT;

use crate::config::{Config, KeyHotkey, MouseButton, RetrievalMethod};
use crate::win32;

const CLASS_NAME: &str = "TextifyRS_Settings";

const ID_HEADER: i32 = 2001;
const ID_SUB: i32 = 2002;
const ID_MOUSE_GROUP: i32 = 2003;
const ID_MOUSE_CTRL: i32 = 2004;
const ID_MOUSE_ALT: i32 = 2005;
const ID_MOUSE_SHIFT: i32 = 2006;
const ID_MOUSE_WIN: i32 = 2007;
const ID_MOUSE_LABEL: i32 = 2008;
const ID_MOUSE_BUTTON: i32 = 2009;
const ID_MOUSE_HINT: i32 = 2010;
const ID_KEY_GROUP: i32 = 2011;
const ID_KEY_EDIT: i32 = 2012;
const ID_KEY_CLEAR: i32 = 2013;
const ID_KEY_HINT: i32 = 2014;
const ID_METHOD_GROUP: i32 = 2015;
const ID_METHOD_LABEL: i32 = 2016;
const ID_METHOD: i32 = 2017;
const ID_UNICODE: i32 = 2018;
const ID_BEHAVIOR_GROUP: i32 = 2019;
const ID_AUTO_COPY: i32 = 2020;
const ID_RUN_AT_LOGIN: i32 = 2021;
const ID_SAVE: i32 = 2022;
const ID_OPEN_INI: i32 = 2023;
const ID_STATUS: i32 = 2025;
const ID_MENU_HINT: i32 = 2026;

struct SettingsState {
    hwnd: HWND,
    cfg: Config,
    ini_path: PathBuf,
    pending_key: Option<KeyHotkey>,
    font: windows::Win32::Graphics::Gdi::HFONT,
    font_bold: windows::Win32::Graphics::Gdi::HFONT,
    dpi: u32,
}

static REGISTERED: Once = Once::new();

pub fn show(owner: HWND, icon: HICON, config: &Config, ini_path: &std::path::Path) -> Option<HWND> {
    ensure_class(icon);

    let dpi = win32::dpi_for_window(owner);
    let mut client = RECT {
        left: 0,
        top: 0,
        right: win32::scale(500, dpi),
        bottom: win32::scale(696, dpi),
    };
    unsafe {
        let _ = AdjustWindowRectEx(
            &mut client,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            false,
            WINDOW_EX_STYLE(0),
        );
    }
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    let work = win32::work_area_near(win32::cursor_pos());
    let bounds = win32::center_rect_in(
        windows::Win32::Foundation::POINT {
            x: (work.left + work.right) / 2,
            y: (work.top + work.bottom) / 2,
        },
        width,
        height,
        &work,
    );

    let state = Box::new(SettingsState {
        hwnd: HWND::default(),
        cfg: config.clone(),
        ini_path: ini_path.to_path_buf(),
        pending_key: config.keyboard,
        font: win32::create_font("", 10.0, 400, dpi),
        font_bold: win32::create_font("", 16.0, win32::bold_weight(), dpi),
        dpi,
    });
    let state_raw = Box::into_raw(state);
    let class_name = win32::wide(CLASS_NAME);

    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            w!("Textify RS — настройки"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
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
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(state.font);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(state.font_bold);
            drop(state);
        }
        return None;
    };

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
    Some(hwnd)
}

pub fn reveal(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        win32::force_foreground(hwnd);
    }
}

fn ensure_class(icon: HICON) {
    REGISTERED.call_once(|| unsafe {
        let class_name = Box::leak(win32::wide(CLASS_NAME).into_boxed_slice());
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: win32::instance(),
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&class);
    });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_CREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        let state = &mut *(create.lpCreateParams as *mut SettingsState);
        state.hwnd = hwnd;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        build_controls(state);
        fill_controls(state);
        layout(state);
        return LRESULT(0);
    }

    let state = state_ptr(hwnd);
    if state.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }

    if message == WM_DESTROY {
        // Reclaim the box through the raw pointer, before any &mut is formed,
        // and clear the back-pointer to make a repeated WM_DESTROY harmless.
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let state = Box::from_raw(state);
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(state.font);
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(state.font_bold);
        drop(state);
        return LRESULT(0);
    }

    let state = &mut *state;
    match message {
        WM_COMMAND => {
            on_command(state, (wparam.0 & 0xFFFF) as i32);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let child = HWND(lparam.0 as _);
            let id = control_id(child);
            if matches!(id, ID_SUB | ID_MOUSE_HINT | ID_MENU_HINT | ID_KEY_HINT | ID_STATUS) {
                let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as _);
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, windows::Win32::Foundation::COLORREF(GetSysColor(COLOR_GRAYTEXT)));
                return LRESULT(GetSysColorBrush(COLOR_BTNFACE).0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_DPICHANGED => {
            state.dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
            let suggested = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(state.font);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(state.font_bold);
            state.font = win32::create_font("", 10.0, 400, state.dpi);
            state.font_bold = win32::create_font("", 16.0, win32::bold_weight(), state.dpi);
            apply_fonts(state);
            layout(state);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn build_controls(state: &mut SettingsState) {
    let hwnd = state.hwnd;
    control(hwnd, "STATIC", "Textify RS", style_static(), WINDOW_STYLE(0), ID_HEADER);
    control(hwnd, "STATIC", "Копирование текста из любых диалогов и окон", style_static(), WINDOW_STYLE(0), ID_SUB);

    control(hwnd, "BUTTON", "Мышиный шорткат", style_group(), WINDOW_STYLE(0), ID_MOUSE_GROUP);
    control(hwnd, "BUTTON", "Ctrl", style_check(), WINDOW_STYLE(0), ID_MOUSE_CTRL);
    control(hwnd, "BUTTON", "Alt", style_check(), WINDOW_STYLE(0), ID_MOUSE_ALT);
    control(hwnd, "BUTTON", "Shift", style_check(), WINDOW_STYLE(0), ID_MOUSE_SHIFT);
    control(hwnd, "BUTTON", "Win", style_check(), WINDOW_STYLE(0), ID_MOUSE_WIN);
    control(hwnd, "STATIC", "Кнопка:", style_static(), WINDOW_STYLE(0), ID_MOUSE_LABEL);
    control(hwnd, "COMBOBOX", "", style_combo(), WINDOW_STYLE(0), ID_MOUSE_BUTTON);
    control(
        hwnd,
        "STATIC",
        "Зажмите модификаторы и нажмите кнопку мыши на тексте, который нужно скопировать.",
        style_static(),
        WINDOW_STYLE(0),
        ID_MOUSE_HINT,
    );
    control(
        hwnd,
        "STATIC",
        "Ctrl+Shift+средняя кнопка мыши — копировать всё меню целиком (дополнительный жест).",
        style_static(),
        WINDOW_STYLE(0),
        ID_MENU_HINT,
    );

    control(hwnd, "BUTTON", "Клавиатурный шорткат", style_group(), WINDOW_STYLE(0), ID_KEY_GROUP);
    control(hwnd, "EDIT", "не задано", style_edit(), WINDOW_STYLE(0), ID_KEY_EDIT);
    control(hwnd, "BUTTON", "Очистить", style_button(), WINDOW_STYLE(0), ID_KEY_CLEAR);
    control(
        hwnd,
        "STATIC",
        "Щёлкните по полю и нажмите комбинацию, например Ctrl+Alt+X.",
        style_static(),
        WINDOW_STYLE(0),
        ID_KEY_HINT,
    );

    control(hwnd, "BUTTON", "Извлечение текста", style_group(), WINDOW_STYLE(0), ID_METHOD_GROUP);
    control(hwnd, "STATIC", "Способ:", style_static(), WINDOW_STYLE(0), ID_METHOD_LABEL);
    control(hwnd, "COMBOBOX", "", style_combo(), WINDOW_STYLE(0), ID_METHOD);
    control(
        hwnd,
        "BUTTON",
        "Заменять типографские пробелы обычными",
        style_check(),
        WINDOW_STYLE(0),
        ID_UNICODE,
    );

    control(hwnd, "BUTTON", "Поведение", style_group(), WINDOW_STYLE(0), ID_BEHAVIOR_GROUP);
    control(
        hwnd,
        "BUTTON",
        "Автоматически копировать текст в буфер обмена",
        style_check(),
        WINDOW_STYLE(0),
        ID_AUTO_COPY,
    );
    control(hwnd, "BUTTON", "Запускать при входе в Windows", style_check(), WINDOW_STYLE(0), ID_RUN_AT_LOGIN);

    control(hwnd, "BUTTON", "Сохранить", style_default_button(), WINDOW_STYLE(0), ID_SAVE);
    control(hwnd, "BUTTON", "Открыть INI", style_button(), WINDOW_STYLE(0), ID_OPEN_INI);
    control(
        hwnd,
        "STATIC",
        "Закрытие окна сворачивает программу в трей. Выход — через меню значка.",
        style_static(),
        WINDOW_STYLE(0),
        ID_STATUS,
    );

    // Missing controls must not stop the fonts from being applied to the rest
    // of the window.
    if let Ok(edit) = GetDlgItem(hwnd, ID_KEY_EDIT) {
        let _ = SetWindowSubclass(edit, Some(key_edit_proc), 2, hwnd.0 as usize);
    }
    apply_fonts(state);
}

fn style_static() -> WINDOW_STYLE {
    WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_LEFT.0)
}

fn style_check() -> WINDOW_STYLE {
    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32)
}

fn style_group() -> WINDOW_STYLE {
    WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_GROUPBOX as u32)
}

fn style_button() -> WINDOW_STYLE {
    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32)
}

fn style_default_button() -> WINDOW_STYLE {
    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32)
}

fn style_combo() -> WINDOW_STYLE {
    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32)
}

fn style_edit() -> WINDOW_STYLE {
    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WINDOW_STYLE(ES_READONLY as u32)
}

unsafe fn control(
    parent: HWND,
    class_name: &str,
    text: &str,
    style: WINDOW_STYLE,
    ex_style: WINDOW_STYLE,
    id: i32,
) {
    let class_name = HSTRING::from(class_name);
    let text = HSTRING::from(text);
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(ex_style.0),
        PCWSTR(class_name.as_ptr()),
        PCWSTR(text.as_ptr()),
        style,
        0,
        0,
        10,
        10,
        parent,
        HMENU(id as isize as _),
        win32::instance(),
        None,
    );
}

unsafe fn fill_controls(state: &SettingsState) {
    let hwnd = state.hwnd;
    set_check(hwnd, ID_MOUSE_CTRL, state.cfg.mouse.ctrl);
    set_check(hwnd, ID_MOUSE_ALT, state.cfg.mouse.alt);
    set_check(hwnd, ID_MOUSE_SHIFT, state.cfg.mouse.shift);
    set_check(hwnd, ID_MOUSE_WIN, state.cfg.mouse.win);
    set_check(hwnd, ID_UNICODE, state.cfg.unicode_spaces);
    set_check(hwnd, ID_AUTO_COPY, state.cfg.auto_copy);
    set_check(hwnd, ID_RUN_AT_LOGIN, state.cfg.run_at_login);

    add_item(hwnd, ID_MOUSE_BUTTON, "Левая");
    add_item(hwnd, ID_MOUSE_BUTTON, "Средняя");
    add_item(hwnd, ID_MOUSE_BUTTON, "Правая");
    let mouse_index = match state.cfg.mouse.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    if let Ok(combo) = GetDlgItem(hwnd, ID_MOUSE_BUTTON) {
        let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(mouse_index), LPARAM(0));
    }

    for method in [
        RetrievalMethod::Auto,
        RetrievalMethod::Uia,
        RetrievalMethod::Msaa,
        RetrievalMethod::WindowText,
    ] {
        add_item(hwnd, ID_METHOD, method.label_ru());
    }
    let method_index = match state.cfg.retrieval {
        RetrievalMethod::Auto => 0,
        RetrievalMethod::Uia => 1,
        RetrievalMethod::Msaa => 2,
        RetrievalMethod::WindowText => 3,
    };
    if let Ok(combo) = GetDlgItem(hwnd, ID_METHOD) {
        let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(method_index), LPARAM(0));
    }

    let label = state
        .pending_key
        .map(|hotkey| hotkey.label())
        .unwrap_or_else(|| "не задано".to_string());
    let label = HSTRING::from(label);
    if let Ok(edit) = GetDlgItem(hwnd, ID_KEY_EDIT) {
        let _ = SetWindowTextW(edit, &label);
    }
}

unsafe fn add_item(hwnd: HWND, id: i32, text: &str) {
    let Ok(control) = GetDlgItem(hwnd, id) else {
        return;
    };
    let wide = win32::wide(text);
    let _ = SendMessageW(control, CB_ADDSTRING, WPARAM(0), LPARAM(wide.as_ptr() as isize));
}

unsafe fn set_check(hwnd: HWND, id: i32, checked: bool) {
    let Ok(control) = GetDlgItem(hwnd, id) else {
        return;
    };
    let _ = SendMessageW(
        control,
        BM_SETCHECK,
        WPARAM(if checked { BST_CHECKED.0 as usize } else { 0 }),
        LPARAM(0),
    );
}

unsafe fn is_checked(hwnd: HWND, id: i32) -> bool {
    let Ok(control) = GetDlgItem(hwnd, id) else {
        return false;
    };
    SendMessageW(control, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 as u32 == BST_CHECKED.0
}

unsafe fn apply_fonts(state: &SettingsState) {
    let font = state.font.0 as isize;
    let _ = EnumChildWindows(state.hwnd, Some(set_child_font), LPARAM(font));
    if let Ok(header) = GetDlgItem(state.hwnd, ID_HEADER) {
        let _ = SendMessageW(header, WM_SETFONT, WPARAM(state.font_bold.0 as usize), LPARAM(1));
    }
}

unsafe extern "system" fn set_child_font(hwnd: HWND, lparam: LPARAM) -> windows::Win32::Foundation::BOOL {
    let _ = SendMessageW(hwnd, WM_SETFONT, WPARAM(lparam.0 as usize), LPARAM(1));
    windows::Win32::Foundation::BOOL(1)
}

unsafe fn layout(state: &SettingsState) {
    let dpi = state.dpi;
    let s = |value: i32| win32::scale(value, dpi);
    place(state.hwnd, ID_HEADER, s(20), s(10), s(440), s(32));
    place(state.hwnd, ID_SUB, s(20), s(42), s(460), s(32));

    place(state.hwnd, ID_MOUSE_GROUP, s(16), s(82), s(468), s(168));
    place(state.hwnd, ID_MOUSE_CTRL, s(32), s(108), s(72), s(28));
    place(state.hwnd, ID_MOUSE_ALT, s(112), s(108), s(64), s(28));
    place(state.hwnd, ID_MOUSE_SHIFT, s(184), s(108), s(72), s(28));
    place(state.hwnd, ID_MOUSE_WIN, s(264), s(108), s(64), s(28));
    place(state.hwnd, ID_MOUSE_LABEL, s(32), s(142), s(72), s(28));
    place(state.hwnd, ID_MOUSE_BUTTON, s(112), s(142), s(170), s(200));
    // Two lines are reserved: at 100 % scaling the hint wraps after "тексте".
    place(state.hwnd, ID_MOUSE_HINT, s(32), s(174), s(436), s(40));
    place(state.hwnd, ID_MENU_HINT, s(32), s(216), s(436), s(24));

    place(state.hwnd, ID_KEY_GROUP, s(16), s(260), s(468), s(112));
    place(state.hwnd, ID_KEY_EDIT, s(32), s(288), s(250), s(28));
    place(state.hwnd, ID_KEY_CLEAR, s(294), s(288), s(96), s(28));
    place(state.hwnd, ID_KEY_HINT, s(32), s(320), s(436), s(40));

    place(state.hwnd, ID_METHOD_GROUP, s(16), s(382), s(468), s(108));
    place(state.hwnd, ID_METHOD_LABEL, s(32), s(412), s(72), s(28));
    place(state.hwnd, ID_METHOD, s(112), s(412), s(300), s(200));
    place(state.hwnd, ID_UNICODE, s(32), s(448), s(436), s(28));

    place(state.hwnd, ID_BEHAVIOR_GROUP, s(16), s(500), s(468), s(92));
    place(state.hwnd, ID_AUTO_COPY, s(32), s(526), s(436), s(28));
    place(state.hwnd, ID_RUN_AT_LOGIN, s(32), s(556), s(436), s(28));

    place(state.hwnd, ID_OPEN_INI, s(16), s(604), s(150), s(32));
    place(state.hwnd, ID_SAVE, s(334), s(604), s(150), s(32));
    place(state.hwnd, ID_STATUS, s(20), s(646), s(460), s(40));
}

unsafe fn place(hwnd: HWND, id: i32, x: i32, y: i32, width: i32, height: i32) {
    let Ok(control) = GetDlgItem(hwnd, id) else {
        return;
    };
    let _ = SetWindowPos(control, None, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn on_command(state: &mut SettingsState, id: i32) {
    match id {
        ID_SAVE => save(state),
        ID_OPEN_INI => {
            if !state.ini_path.exists() {
                let _ = state.cfg.save(&state.ini_path);
            }
            win32::open_in_notepad(&state.ini_path);
        }
        ID_KEY_CLEAR => {
            state.pending_key = None;
            let text = HSTRING::from("не задано");
            if let Ok(edit) = GetDlgItem(state.hwnd, ID_KEY_EDIT) {
                let _ = SetWindowTextW(edit, &text);
            }
        }
        _ => {}
    }
}

unsafe fn save(state: &mut SettingsState) {
    let hwnd = state.hwnd;
    state.cfg.mouse.ctrl = is_checked(hwnd, ID_MOUSE_CTRL);
    state.cfg.mouse.alt = is_checked(hwnd, ID_MOUSE_ALT);
    state.cfg.mouse.shift = is_checked(hwnd, ID_MOUSE_SHIFT);
    state.cfg.mouse.win = is_checked(hwnd, ID_MOUSE_WIN);
    state.cfg.unicode_spaces = is_checked(hwnd, ID_UNICODE);
    state.cfg.auto_copy = is_checked(hwnd, ID_AUTO_COPY);
    state.cfg.run_at_login = is_checked(hwnd, ID_RUN_AT_LOGIN);

    let mouse = GetDlgItem(hwnd, ID_MOUSE_BUTTON)
        .map(|combo| {
            SendMessageW(
                combo,
                windows::Win32::UI::WindowsAndMessaging::CB_GETCURSEL,
                WPARAM(0),
                LPARAM(0),
            )
            .0 as i32
        })
        .unwrap_or(1);
    state.cfg.mouse.button = match mouse {
        0 => MouseButton::Left,
        2 => MouseButton::Right,
        _ => MouseButton::Middle,
    };
    let method = GetDlgItem(hwnd, ID_METHOD)
        .map(|combo| {
            SendMessageW(
                combo,
                windows::Win32::UI::WindowsAndMessaging::CB_GETCURSEL,
                WPARAM(0),
                LPARAM(0),
            )
            .0 as i32
        })
        .unwrap_or(0);
    state.cfg.retrieval = match method {
        1 => RetrievalMethod::Uia,
        2 => RetrievalMethod::Msaa,
        3 => RetrievalMethod::WindowText,
        _ => RetrievalMethod::Auto,
    };
    state.cfg.keyboard = state.pending_key;

    if !state.cfg.mouse.ctrl && !state.cfg.mouse.alt && !state.cfg.mouse.shift && !state.cfg.mouse.win {
        win32::show_error(
            hwnd,
            "Мышиный шорткат без модификатора перехватит обычный щелчок. Добавьте хотя бы Shift, Ctrl, Alt или Win.",
            "Textify RS",
        );
        return;
    }

    let status = match crate::app::apply_config(state.cfg.clone(), &state.ini_path) {
        Ok(()) => format!("Сохранено. Шорткат: {}", state.cfg.mouse_shortcut()),
        Err(error) => {
            win32::show_error(
                hwnd,
                &format!("Не удалось записать файл настроек:\n{error}"),
                "Textify RS",
            );
            "Настройки применены, но INI не сохранён — после перезапуска они будут потеряны."
                .to_string()
        }
    };
    let status = HSTRING::from(status);
    if let Ok(status_label) = GetDlgItem(hwnd, ID_STATUS) {
        let _ = SetWindowTextW(status_label, &status);
    }
}

unsafe extern "system" fn key_edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    let parent = HWND(data as _);
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let key = wparam.0 as u32;
            if matches!(key, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C) {
                return LRESULT(0);
            }
            let state = state_ptr(parent);
            if !state.is_null() {
                let state = &mut *state;
                if key == VK_ESCAPE.0 as u32 {
                    state.pending_key = None;
                    let text = HSTRING::from("не задано");
                    let _ = SetWindowTextW(hwnd, &text);
                    return LRESULT(0);
                }
                let hotkey = KeyHotkey {
                    vk: key,
                    ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
                    alt: GetKeyState(VK_MENU.0 as i32) < 0,
                    shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
                    win: GetKeyState(VK_LWIN.0 as i32) < 0 || GetKeyState(VK_RWIN.0 as i32) < 0,
                };
                let label = HSTRING::from(hotkey.label());
                state.pending_key = Some(hotkey);
                let _ = SetWindowTextW(hwnd, &label);
            }
            return LRESULT(0);
        }
        WM_GETDLGCODE => return LRESULT(0x0004),
        WM_CHAR => return LRESULT(0),
        _ => {}
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

fn control_id(hwnd: HWND) -> i32 {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_ID) as i32 }
}

/// Raw state pointer from GWLP_USERDATA, or null. Dereferenced only for the
/// duration of one message; never exposed as a long-lived `&'static mut`.
fn state_ptr(hwnd: HWND) -> *mut SettingsState {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SettingsState }
}


