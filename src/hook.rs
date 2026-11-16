//! A global low-level mouse hook that implements the "Shift + middle click"
//! style shortcut and the dedicated "Ctrl+Shift + middle click" whole-menu
//! mode. The hook lives on its own thread with a message loop, so a slow
//! target application can never stall it.

use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RWIN,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER,
};

use crate::config::MouseHotkey;
use crate::win32;

/// `WM_APP_CAPTURE` payload flag: the capture is the whole-menu special mode.
pub const CAPTURE_MENU: usize = 1;

struct HookShared {
    main: HWND,
    notify: u32,
    button_vk: u32,
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
    exclude: Vec<String>,
    pressed: AtomicBool,
    menu_pressed: AtomicBool,
}

static SHARED: AtomicPtr<HookShared> = AtomicPtr::new(std::ptr::null_mut());

/// Owns the hook thread. Dropping the value uninstalls the hook.
pub struct MouseHook {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl MouseHook {
    pub fn install(
        main: HWND,
        notify: u32,
        hotkey: MouseHotkey,
        exclude: Vec<String>,
    ) -> Option<Self> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let main_bits = main.0 as usize;
        let thread = thread::Builder::new()
            .name("textify-rs-mouse-hook".to_string())
            .spawn(move || {
                hook_thread(HWND(main_bits as _), notify, hotkey, exclude, ready_tx)
            })
            .ok()?;

        let thread_id = match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Some(thread_id)) => thread_id,
            _ => {
                let _ = thread.join();
                return None;
            }
        };

        Some(Self {
            thread_id,
            thread: Some(thread),
        })
    }
}

impl Drop for MouseHook {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn hook_thread(
    main: HWND,
    notify: u32,
    hotkey: MouseHotkey,
    exclude: Vec<String>,
    ready: mpsc::Sender<Option<u32>>,
) {
    unsafe {
        // Make sure the thread has a message queue before anyone posts to it.
        let mut message = MSG::default();
        let _ = PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE);

        let thread_id = GetCurrentThreadId();

        let shared = Box::new(HookShared {
            main,
            notify,
            button_vk: hotkey.button.vk(),
            ctrl: hotkey.ctrl,
            alt: hotkey.alt,
            shift: hotkey.shift,
            win: hotkey.win,
            exclude,
            pressed: AtomicBool::new(false),
            menu_pressed: AtomicBool::new(false),
        });
        let shared_ptr = Box::into_raw(shared);
        SHARED.store(shared_ptr, Ordering::SeqCst);

        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) {
            Ok(hook) => {
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
                let _ = ready.send(Some(thread_id));

                loop {
                    let result = GetMessageW(&mut message, None, 0, 0);
                    if result.0 <= 0 {
                        let _ = result;
                        break;
                    }
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }

                let _ = UnhookWindowsHookEx(hook);
            }
            Err(_) => {
                let _ = ready.send(None);
            }
        }

        SHARED.store(std::ptr::null_mut(), Ordering::SeqCst);
        drop(Box::from_raw(shared_ptr));
    }
}

fn modifiers_match(shared: &HookShared) -> bool {
    let ctrl = key_down(VK_CONTROL.0 as i32);
    let alt = key_down(VK_MENU.0 as i32);
    let shift = key_down(VK_SHIFT.0 as i32);
    let win = unsafe {
        GetAsyncKeyState(VK_LWIN.0 as i32) < 0 || GetAsyncKeyState(VK_RWIN.0 as i32) < 0
    };

    ctrl == shared.ctrl && alt == shared.alt && shift == shared.shift && win == shared.win
}

fn key_down(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) < 0 }
}

/// The whole-menu mode is reserved: Ctrl+Shift + middle button.
fn menu_mode_pressed() -> bool {
    key_down(VK_CONTROL.0 as i32) && key_down(VK_SHIFT.0 as i32)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    let shared_ptr = SHARED.load(Ordering::SeqCst);
    if !shared_ptr.is_null() {
        let shared = &*shared_ptr;
        let message = wparam.0 as u32;
        let data = &*(lparam.0 as *const MSLLHOOKSTRUCT);

        match message {
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                // Special mode first: it is a fixed gesture that must not be
                // confused with the configurable shortcut.
                if message == WM_MBUTTONDOWN
                    && menu_mode_pressed()
                    && !win32::is_excluded(data.pt, &shared.exclude)
                {
                    shared.pressed.store(false, Ordering::SeqCst);
                    shared.menu_pressed.store(true, Ordering::SeqCst);
                    return LRESULT(1);
                }

                let button_matches = shared.button_vk == button_for_message(message);
                if button_matches
                    && modifiers_match(shared)
                    && !win32::is_excluded(data.pt, &shared.exclude)
                {
                    shared.pressed.store(true, Ordering::SeqCst);
                    shared.menu_pressed.store(false, Ordering::SeqCst);
                    return LRESULT(1);
                }
                shared.pressed.store(false, Ordering::SeqCst);
                shared.menu_pressed.store(false, Ordering::SeqCst);
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                if message == WM_MBUTTONUP && shared.menu_pressed.swap(false, Ordering::SeqCst) {
                    let cursor = win32::cursor_pos();
                    let _ = PostMessageW(
                        shared.main,
                        shared.notify,
                        WPARAM(CAPTURE_MENU),
                        LPARAM(pack_point(cursor.x, cursor.y)),
                    );
                    return LRESULT(1);
                }

                let button_matches = shared.button_vk == button_for_message(message);
                if button_matches && shared.pressed.swap(false, Ordering::SeqCst) {
                    let cursor = win32::cursor_pos();
                    let _ = PostMessageW(
                        shared.main,
                        shared.notify,
                        WPARAM(0),
                        LPARAM(pack_point(cursor.x, cursor.y)),
                    );
                    return LRESULT(1);
                }
            }
            _ => {}
        }
    }

    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

fn button_for_message(message: u32) -> u32 {
    match message {
        WM_LBUTTONDOWN | WM_LBUTTONUP => VK_LBUTTON.0 as u32,
        WM_RBUTTONDOWN | WM_RBUTTONUP => VK_RBUTTON.0 as u32,
        WM_MBUTTONDOWN | WM_MBUTTONUP => VK_MBUTTON.0 as u32,
        _ => 0,
    }
}

fn pack_point(x: i32, y: i32) -> isize {
    let packed = ((x as u32 & 0xFFFF) | ((y as u32 & 0xFFFF) << 16)) as usize;
    packed as isize
}

pub fn unpack_point(lparam: isize) -> windows::Win32::Foundation::POINT {
    let packed = lparam as u32;
    let x = (packed & 0xFFFF) as u16 as i16 as i32;
    let y = ((packed >> 16) & 0xFFFF) as u16 as i16 as i32;
    windows::Win32::Foundation::POINT { x, y }
}
