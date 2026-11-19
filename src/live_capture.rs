//! Creates real windows and checks that capture can read them: a static label
//! (ordinary mode) and an open popup menu (whole-menu mode).

use std::sync::mpsc;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EndMenu, GetSystemMetrics, GetWindowRect, IsWindowVisible, PeekMessageW, RegisterClassW,
    SendMessageTimeoutW, SetWindowPos, ShowWindow, TrackPopupMenuEx, HWND_TOPMOST, MF_STRING, MSG,
    PM_REMOVE, SM_CYMENU, SMTO_ABORTIFHUNG, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_SHOW,
    TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_TOPALIGN, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_DESTROY, WNDCLASSW, WS_CHILD, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::capture::capture_at;
use crate::config::RetrievalMethod;
use crate::win32;

/// Posted to the menu owner to leave the modal popup loop.
const WM_APP_END_MENU: u32 = WM_APP + 7;

#[test]
fn captures_static_label_from_a_real_window() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let class_name = Box::leak(win32::wide("TextifyRS_LiveCapture").into_boxed_slice());
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: win32::instance(),
            lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&class);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            windows::core::PCWSTR(class_name.as_ptr()),
            w!("Textify RS live"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            80,
            80,
            420,
            160,
            None,
            None,
            win32::instance(),
            None,
        )
        .expect("test window")
    };
    let label = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Привет, Textify RS!"),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0),
            24,
            36,
            360,
            32,
            hwnd,
            None,
            win32::instance(),
            None,
        )
        .expect("label")
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
    }
    pump();

    let point = POINT { x: 140, y: 150 };
    let capture = capture_at(point, RetrievalMethod::Auto)
        .or_else(|| capture_at(point, RetrievalMethod::WindowText));

    unsafe {
        let _ = DestroyWindow(label);
        let _ = DestroyWindow(hwnd);
    }

    let capture = capture.expect("expected captured text");
    assert!(
        capture.text.contains("Привет, Textify RS!"),
        "captured {:?}: {}",
        capture.method,
        capture.text
    );
}

#[test]
fn captures_text_from_richedit_control() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = windows::Win32::System::LibraryLoader::LoadLibraryW(w!("msftedit.dll"));
    }

    let class_name = Box::leak(win32::wide("TextifyRS_LiveRichEdit").into_boxed_slice());
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: win32::instance(),
            lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&class);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            windows::core::PCWSTR(class_name.as_ptr()),
            w!("Textify RS RichEdit test"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            80,
            80,
            420,
            200,
            None,
            None,
            win32::instance(),
            None,
        )
        .expect("test window")
    };
    let richedit = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("RICHEDIT50W"),
            w!("Первая строка\r\nВторая строка"),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(4),
            24,
            36,
            360,
            100,
            hwnd,
            None,
            win32::instance(),
            None,
        )
        .expect("richedit")
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
    }
    pump();

    let point = POINT { x: 140, y: 150 };
    let capture = capture_at(point, RetrievalMethod::Uia);

    unsafe {
        let _ = DestroyWindow(richedit);
        let _ = DestroyWindow(hwnd);
    }

    let capture = capture.expect("expected captured text from richedit");
    assert_eq!(capture.method, RetrievalMethod::Uia);
    assert_eq!(
        capture.text, "Первая строка\r\nВторая строка",
        "expected exact multiline capture without duplication or truncation, got: {:?}",
        capture.text
    );
    assert_eq!(capture.segments.len(), 1);
}

#[test]
#[ignore = "needs an interactive desktop: it activates a window and opens a real popup menu"]
fn captures_every_item_of_an_open_menu() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    const MENU_X: i32 = 240;
    const MENU_Y: i32 = 240;
    const ITEMS: [&str; 3] = ["Первая команда", "Вторая команда", "Третья команда"];

    let class_name = Box::leak(win32::wide("TextifyRS_LiveMenu").into_boxed_slice());
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: win32::instance(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&class);
    }

    let (owner_tx, owner_rx) = mpsc::channel::<usize>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let class_ptr = class_name.as_ptr() as usize;

    // The popup loop is modal and blocks this thread, exactly like a real
    // application's menu does.
    let thread = std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                PCWSTR(class_ptr as *const u16),
                w!("Textify RS menu"),
                WS_OVERLAPPEDWINDOW,
                60,
                60,
                320,
                180,
                None,
                None,
                win32::instance(),
                None,
            )
        }
        .expect("menu host window");
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            // A popup menu only sizes and tracks itself properly when its
            // owner is the active window.
            win32::force_foreground(hwnd);
        }

        let menu = unsafe { CreatePopupMenu() }.expect("menu");
        for (index, label) in ITEMS.iter().enumerate() {
            let label = win32::wide(label);
            unsafe {
                let _ = AppendMenuW(menu, MF_STRING, index + 1, PCWSTR(label.as_ptr()));
            }
        }

        let _ = owner_tx.send(hwnd.0 as usize);

        unsafe {
            let _ = TrackPopupMenuEx(
                menu,
                (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD | TPM_NONOTIFY).0,
                MENU_X,
                MENU_Y,
                hwnd,
                None,
            );
            let _ = DestroyWindow(hwnd);
        }
        let _ = done_tx.send(());
    });

    let owner = HWND(owner_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("menu host window") as *mut _);

    // A popup window exists before it is shown, sized and positioned; wait for
    // the visible one so the capture point lands on the first item.
    let mut popup = HWND::default();
    let mut rect = windows::Win32::Foundation::RECT::default();
    for _ in 0..250 {
        let candidate = win32::find_window("#32768");
        if !win32::hwnd_is_null(candidate) {
            let mut candidate_rect = windows::Win32::Foundation::RECT::default();
            let visible = unsafe { IsWindowVisible(candidate) }.as_bool();
            unsafe {
                let _ = GetWindowRect(candidate, &mut candidate_rect);
            }
            if visible && candidate_rect.right - candidate_rect.left > 20 {
                popup = candidate;
                rect = candidate_rect;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!win32::hwnd_is_null(popup), "popup menu did not appear");

    let item_height = unsafe { GetSystemMetrics(SM_CYMENU) }.max(16);
    let point = POINT {
        x: rect.left + (rect.right - rect.left).max(2) / 2,
        y: rect.top + item_height / 2,
    };
    let capture = crate::menu::capture_menu_at(point);

    // Leave the modal loop. A posted message is not dispatched while the menu
    // is modal, so the request is sent (and bounded) instead.
    for _ in 0..3 {
        unsafe {
            let _ = SendMessageTimeoutW(
                owner,
                WM_APP_END_MENU,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                500,
                None,
            );
        }
        if done_rx.recv_timeout(Duration::from_millis(700)).is_ok() {
            break;
        }
    }
    // Never block the suite on a stuck modal loop.
    if done_rx.recv_timeout(Duration::from_millis(300)).is_ok() {
        let _ = thread.join();
    }

    let capture = capture.expect("whole menu capture");
    assert!(capture.menu, "capture must be marked as a menu dump");
    for item in ITEMS {
        assert!(
            capture.text.contains(item),
            "captured {:?}: {}",
            capture.method,
            capture.text
        );
    }
}

fn pump() {
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(400) {
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_DESTROY => LRESULT(0),
        // Ends the modal popup menu from inside its owning thread.
        WM_APP_END_MENU => {
            let _ = EndMenu();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}
