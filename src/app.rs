//! Application entry point: hidden message window, tray icon, global shortcut
//! and the two visible windows (settings and the captured-text popup).

use std::cell::RefCell;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::w;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM, ERROR_ALREADY_EXISTS,
};
use windows::Win32::UI::WindowsAndMessaging::HICON;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, FindWindowW,
    GetMessageW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, TranslateMessage, IDC_ARROW, MSG, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY,
    WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

use crate::capture::{capture_at, Capture};
use crate::config::{set_run_at_login, Config, KeyHotkey};
use crate::hook::{self, MouseHook};
use crate::menu;
use crate::result;
use crate::settings;
use crate::text::join_segments;
use crate::tray::{self, TrayAction};
use crate::win32;

pub const WM_APP_TRAY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
pub const WM_APP_CAPTURE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;
pub const WM_APP_SHOW: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 3;

const CLASS_MAIN: &str = "TextifyRS_Main";
const HOTKEY_ID: i32 = 1;
const MUTEX_NAME: &str = "Local\\TextifyRS.SingleInstance";

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

struct AppState {
    hwnd: HWND,
    cfg: Config,
    ini_path: PathBuf,
    icon: HICON,
    hook: Option<MouseHook>,
    hotkey_registered: bool,
    tray_added: bool,
    result: HWND,
    settings: HWND,
}

thread_local! {
    static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

pub fn main_entry() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--capture") {
        debug_capture(&args);
        return;
    }

    let hidden = args.iter().any(|arg| arg == "-hidewnd" || arg == "--hidden");

    let mutex = unsafe { CreateMutexW(None, false, w!("Local\\TextifyRS.SingleInstance")) };
    let Ok(mutex) = mutex else {
        win32::show_error(HWND::default(), "Не удалось создать мьютекс единственного экземпляра.", "Textify RS");
        return;
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let class_name = win32::wide(CLASS_MAIN);
        let existing = unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), None).unwrap_or_default() };
        if !win32::hwnd_is_null(existing) {
            let _ = unsafe { PostMessageW(existing, WM_APP_SHOW, WPARAM(0), LPARAM(0)) };
        }
        let _ = unsafe { CloseHandle(mutex) };
        return;
    }

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let ini_path = config_path();
    let cfg = Config::load(&ini_path);
    let _ = set_run_at_login(cfg.run_at_login);

    let icon = win32::make_app_icon(32).unwrap_or_default();
    register_main_class(icon);

    let class_name = win32::wide(CLASS_MAIN);
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            w!("Textify RS"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            win32::instance(),
            None,
        )
    };
    let Ok(hwnd) = hwnd else {
        win32::show_error(HWND::default(), "Не удалось создать служебное окно.", "Textify RS");
        return;
    };

    TASKBAR_CREATED.store(
        unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        Ordering::SeqCst,
    );

    STATE.with(|cell| {
        *cell.borrow_mut() = Some(AppState {
            hwnd,
            cfg: cfg.clone(),
            ini_path: ini_path.clone(),
            icon,
            hook: None,
            hotkey_registered: false,
            tray_added: false,
            result: HWND::default(),
            settings: HWND::default(),
        });
    });

    let shortcut_errors = with_state(|state| {
        let errors = install_shortcuts(state);
        state.tray_added = tray::add(state.hwnd, state.icon, &tray_tip(&state.cfg), WM_APP_TRAY);
        errors
    })
    .unwrap_or_default();
    show_shortcut_errors(hwnd, shortcut_errors);

    if !hidden {
        show_settings();
    }

    unsafe {
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    with_state(|state| {
        if state.tray_added {
            tray::remove(state.hwnd);
        }
        if state.hotkey_registered {
            let _ = unsafe { UnregisterHotKey(state.hwnd, HOTKEY_ID) };
        }
        state.hook.take();
        if !state.icon.0.is_null() {
            let _ = unsafe { DestroyIcon(state.icon) };
        }
    });
    STATE.with(|cell| {
        cell.borrow_mut().take();
    });

    let _ = unsafe { CloseHandle(mutex) };
    unsafe { CoUninitialize() };
    let _ = MUTEX_NAME;
}

/// Applies and persists a new configuration. Returns the INI write error, if
/// any, so the caller can tell the user instead of claiming "saved".
pub fn apply_config(cfg: Config, ini_path: &Path) -> Result<(), String> {
    let save_result = cfg.save(ini_path).map_err(|error| error.to_string());
    let _ = set_run_at_login(cfg.run_at_login);
    let errors = with_state(|state| {
        state.cfg = cfg;
        let errors = install_shortcuts(state);
        if state.tray_added {
            tray::update(state.hwnd, state.icon, &tray_tip(&state.cfg), WM_APP_TRAY);
        }
        errors
    })
    .unwrap_or_default();
    let hwnd = with_state(|state| state.hwnd).unwrap_or_default();
    show_shortcut_errors(hwnd, errors);
    save_result
}

pub fn quit() {
    let hwnd = with_state(|state| state.hwnd).unwrap_or_default();
    if !win32::hwnd_is_null(hwnd) {
        let _ = unsafe { PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

fn show_settings() {
    let (owner, icon, cfg, path, existing) = match with_state(|state| {
        (
            state.hwnd,
            state.icon,
            state.cfg.clone(),
            state.ini_path.clone(),
            state.settings,
        )
    }) {
        Some(value) => value,
        None => return,
    };

    if !win32::hwnd_is_null(existing) {
        settings::reveal(existing);
        return;
    }

    if let Some(hwnd) = settings::show(owner, icon, &cfg, &path) {
        with_state(|state| state.settings = hwnd);
    }
}

fn capture_now(point: POINT, menu_mode: bool) {
    let (cfg, owner, icon, result_hwnd) = match with_state(|state| {
        (state.cfg.clone(), state.hwnd, state.icon, state.result)
    }) {
        Some(value) => value,
        None => return,
    };

    let target = unsafe {
        let window = windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(point);
        win32::get_root(window)
    };
    if !win32::hwnd_is_null(result_hwnd) && target == result_hwnd {
        let _ = unsafe { DestroyWindow(result_hwnd) };
        with_state(|state| state.result = HWND::default());
        return;
    }

    if !win32::hwnd_is_null(result_hwnd) {
        let _ = unsafe { DestroyWindow(result_hwnd) };
        with_state(|state| state.result = HWND::default());
    }

    // The whole-menu mode is a best effort: when the point is not over a menu,
    // fall back to the ordinary single-element capture.
    let capture = if menu_mode {
        menu::capture_menu_at(point).or_else(|| capture_at(point, cfg.retrieval))
    } else {
        capture_at(point, cfg.retrieval)
    };

    let (capture, is_placeholder) = match capture {
        Some(capture) => (capture, false),
        None => (placeholder(point, &cfg), true),
    };
    // Nothing useful to copy when all we could show was the "no text" hint.
    let auto_copy = cfg.auto_copy && !is_placeholder;
    if let Some(hwnd) = result::show(owner, icon, &capture, &cfg, auto_copy) {
        with_state(|state| state.result = hwnd);
    }
}

fn placeholder(point: POINT, cfg: &Config) -> Capture {
    let message = "Не удалось извлечь текст.\r\nНаведите курсор на текст в диалоге или смените способ извлечения в настройках.";
    let segments = vec![message.replace("\r\n", "\n")];
    let (text, _) = join_segments(&segments);
    Capture {
        text,
        segments,
        rect: windows::Win32::Foundation::RECT {
            left: point.x,
            top: point.y,
            right: point.x + 1,
            bottom: point.y + 1,
        },
        hwnd: HWND::default(),
        title: String::new(),
        method: cfg.retrieval,
        menu: false,
    }
}

/// Reinstalls the mouse hook and the keyboard hotkey. Errors are returned as
/// text rather than shown here: this runs while the app state is borrowed, and
/// a modal message box would pump messages and allow reentrant access to it.
fn install_shortcuts(state: &mut AppState) -> Vec<String> {
    let mut errors = Vec::new();

    state.hook = None;
    state.hook = MouseHook::install(
        state.hwnd,
        WM_APP_CAPTURE,
        state.cfg.mouse,
        state.cfg.exclude.clone(),
    );
    if state.hook.is_none() {
        errors.push(
            "Не удалось установить глобальный перехват мыши. Шорткат мыши работать не будет."
                .to_string(),
        );
    }

    if state.hotkey_registered {
        let _ = unsafe { UnregisterHotKey(state.hwnd, HOTKEY_ID) };
        state.hotkey_registered = false;
    }
    if let Some(hotkey) = state.cfg.keyboard {
        match unsafe { RegisterHotKey(state.hwnd, HOTKEY_ID, modifier_flags(&hotkey), hotkey.vk) } {
            Ok(()) => state.hotkey_registered = true,
            Err(_) => errors.push(format!(
                "Клавиатурный шорткат «{}» уже занят другой программой.",
                hotkey.label()
            )),
        }
    }

    errors
}

fn show_shortcut_errors(hwnd: HWND, errors: Vec<String>) {
    for message in errors {
        win32::show_error(hwnd, &message, "Textify RS");
    }
}

fn modifier_flags(hotkey: &KeyHotkey) -> HOT_KEY_MODIFIERS {
    let mut flags = MOD_NOREPEAT;
    if hotkey.ctrl {
        flags |= MOD_CONTROL;
    }
    if hotkey.alt {
        flags |= MOD_ALT;
    }
    if hotkey.shift {
        flags |= MOD_SHIFT;
    }
    if hotkey.win {
        flags |= MOD_WIN;
    }
    flags
}

fn tray_tip(config: &Config) -> String {
    format!("Textify RS — {}", config.mouse_shortcut())
}

fn on_tray(event: u32) {
    match event {
        WM_LBUTTONUP => show_settings(),
        WM_RBUTTONUP => {
            let config = match with_state(|state| state.cfg.clone()) {
                Some(config) => config,
                None => return,
            };
            let hwnd = with_state(|state| state.hwnd).unwrap_or_default();
            if let Some(action) = tray::show_menu(hwnd, &config) {
                handle_tray_action(action);
            }
        }
        _ => {}
    }
}

fn handle_tray_action(action: TrayAction) {
    match action {
        TrayAction::ShowSettings => show_settings(),
        TrayAction::CaptureNow => capture_now(win32::cursor_pos(), false),
        TrayAction::ToggleAutoCopy => {
            with_state(|state| {
                state.cfg.auto_copy = !state.cfg.auto_copy;
                let _ = state.cfg.save(&state.ini_path);
            });
        }
        TrayAction::CycleRetrieval => {
            with_state(|state| {
                state.cfg.retrieval = state.cfg.retrieval.next();
                let _ = state.cfg.save(&state.ini_path);
            });
        }
        TrayAction::OpenIni => {
            // ShellExecute can invoke COM/DDE, so keep it outside the state borrow.
            let path = with_state(|state| {
                if !state.ini_path.exists() {
                    let _ = state.cfg.save(&state.ini_path);
                }
                state.ini_path.clone()
            });
            if let Some(path) = path {
                win32::open_in_notepad(&path);
            }
        }
        TrayAction::About => {
            let shortcut = with_state(|state| state.cfg.mouse_shortcut()).unwrap_or_default();
            let hwnd = with_state(|state| state.hwnd).unwrap_or_default();
            win32::show_info(
                hwnd,
                &format!(
                    "Textify RS 0.1.0\n\n\
                     Копирует текст из диалогов и окон, где его нельзя выделить.\n\n\
                     Разработчик: Харченко Евгений Сергеевич\n\
                     @domahes\n\n\
                     Шорткат мыши: {shortcut}\n\
                     Ctrl+Shift+средняя кнопка — копировать всё меню целиком.\n\
                     Клавиатурный шорткат захватывает текст под курсором.\n\n\
                     Текст берётся через UI Automation, MSAA и, если они молчат, через WM_GETTEXT."
                ),
                "О программе",
            );
        }
        TrayAction::Exit => quit(),
    }
}

fn with_state<R>(function: impl FnOnce(&mut AppState) -> R) -> Option<R> {
    STATE.with(|cell| {
        // try_borrow_mut: a reentrant call (a modal dialog pumping messages)
        // must degrade to a no-op instead of panicking and aborting the process.
        let mut borrowed = cell.try_borrow_mut().ok()?;
        borrowed.as_mut().map(function)
    })
}

fn config_path() -> PathBuf {
    if let Ok(executable) = env::current_exe() {
        if let Some(directory) = executable.parent() {
            let candidate = directory.join("TextifyRS.ini");
            if candidate.exists() || directory_is_writable(directory) {
                return candidate;
            }
        }
    }
    let base = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    base.join("TextifyRS").join("settings.ini")
}

fn directory_is_writable(directory: &Path) -> bool {
    let probe = directory.join(".textify-rs-write-test");
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn register_main_class(icon: HICON) {
    unsafe {
        let class_name = Box::leak(win32::wide(CLASS_MAIN).into_boxed_slice());
        let class = WNDCLASSW {
            lpfnWndProc: Some(main_wnd_proc),
            hInstance: win32::instance(),
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&class);
    }
}

unsafe extern "system" fn main_wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_APP_TRAY => {
            on_tray(lparam.0 as u32);
            LRESULT(0)
        }
        WM_APP_CAPTURE => {
            capture_now(hook::unpack_point(lparam.0), wparam.0 == hook::CAPTURE_MENU);
            LRESULT(0)
        }
        WM_APP_SHOW => {
            show_settings();
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_ID {
                capture_now(win32::cursor_pos(), false);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let (settings, result) = with_state(|state| (state.settings, state.result)).unwrap_or_default();
            if !win32::hwnd_is_null(settings) {
                let _ = DestroyWindow(settings);
            }
            if !win32::hwnd_is_null(result) {
                let _ = DestroyWindow(result);
            }
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        other if other == TASKBAR_CREATED.load(Ordering::SeqCst) && other != 0 => {
            with_state(|state| {
                state.tray_added = tray::add(state.hwnd, state.icon, &tray_tip(&state.cfg), WM_APP_TRAY);
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn debug_capture(args: &[String]) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // Coordinates are the first two numeric arguments, so the flags may be
    // written in any order (`--capture --menu 120 340`).
    let mut coordinates = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .filter_map(|arg| arg.parse::<i32>().ok());
    let point = match (coordinates.next(), coordinates.next()) {
        (Some(x), Some(y)) => POINT { x, y },
        _ => win32::cursor_pos(),
    };
    let method = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--method="))
        .map(crate::config::RetrievalMethod::parse)
        .unwrap_or_default();

    // `--menu` exercises the whole-menu special mode from the console.
    let menu_mode = args.iter().any(|arg| arg == "--menu");
    let capture = if menu_mode {
        menu::capture_menu_at(point).or_else(|| capture_at(point, method))
    } else {
        capture_at(point, method)
    };

    match capture {
        Some(capture) => {
            println!("mode: {}", if capture.menu { "всё меню" } else { "элемент" });
            println!("method: {}", capture.method.label_ru());
            println!("title: {}", capture.title);
            println!("rect: {},{} {}x{}", capture.rect.left, capture.rect.top, win32::rect_width(&capture.rect), win32::rect_height(&capture.rect));
            println!("---");
            println!("{}", capture.text);
        }
        None => {
            println!("(no text)");
        }
    }
    unsafe { CoUninitialize() };
}


