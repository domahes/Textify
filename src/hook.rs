//! A global low-level mouse hook that implements the "Shift + middle click"
//! style shortcut and the dedicated "Ctrl+Shift + middle click" whole-menu
//! mode. The hook lives on its own thread with a message loop, so a slow
//! target application can never stall it.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LBUTTON, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, MSG, MSLLHOOKSTRUCT,
    PM_NOREMOVE, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_QUIT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER,
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

thread_local! {
    // Low-level hooks are called on the thread that installed them.
    static SHARED: RefCell<Option<HookShared>> = const { RefCell::new(None) };
}

static HOOK_RUNNING: AtomicBool = AtomicBool::new(false);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

struct HookReservation;

impl HookReservation {
    fn acquire() -> Option<Self> {
        HOOK_RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for HookReservation {
    fn drop(&mut self) {
        HOOK_RUNNING.store(false, Ordering::SeqCst);
    }
}

struct HookControl {
    cancelled: AtomicBool,
    thread_id: Mutex<u32>,
}

impl HookControl {
    fn stop(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let thread_id = self.thread_id.lock().unwrap();
        if *thread_id != 0 {
            unsafe {
                let _ = PostThreadMessageW(*thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

struct ThreadIdGuard<'a>(&'a HookControl);

impl Drop for ThreadIdGuard<'_> {
    fn drop(&mut self) {
        *self.0.thread_id.lock().unwrap() = 0;
    }
}

/// Owns the hook thread. Dropping the value uninstalls the hook.
pub struct MouseHook {
    control: Arc<HookControl>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl MouseHook {
    pub fn install(
        main: HWND,
        notify: u32,
        hotkey: MouseHotkey,
        exclude: Vec<String>,
    ) -> Option<Self> {
        let reservation = HookReservation::acquire()?;
        let control = Arc::new(HookControl {
            cancelled: AtomicBool::new(false),
            thread_id: Mutex::new(0),
        });
        let thread_control = Arc::clone(&control);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let main_bits = main.0 as usize;
        let thread = thread::Builder::new()
            .name("textify-rs-mouse-hook".to_string())
            .spawn(move || {
                let thread_id_guard = ThreadIdGuard(&thread_control);
                hook_thread(
                    HWND(main_bits as _),
                    notify,
                    hotkey,
                    exclude,
                    ready_tx,
                    &thread_control,
                );
                drop(thread_id_guard);
                drop(reservation);
                let _ = done_tx.send(());
            })
            .ok()?;

        if !wait_for_startup(&ready_rx, &control, STOP_TIMEOUT) {
            return None;
        }

        Some(Self {
            control,
            done: done_rx,
            thread: Some(thread),
        })
    }

    pub fn shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.control.stop();
            if self.done.recv_timeout(STOP_TIMEOUT).is_ok() {
                let _ = thread.join();
            }
        }
    }
}

fn wait_for_startup(
    ready: &mpsc::Receiver<bool>,
    control: &HookControl,
    timeout: Duration,
) -> bool {
    if matches!(ready.recv_timeout(timeout), Ok(true)) {
        true
    } else {
        control.stop();
        false
    }
}

impl Drop for MouseHook {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn hook_thread(
    main: HWND,
    notify: u32,
    hotkey: MouseHotkey,
    exclude: Vec<String>,
    ready: mpsc::Sender<bool>,
    control: &HookControl,
) {
    unsafe {
        // Make sure the thread has a message queue before anyone posts to it.
        let mut message = MSG::default();
        let _ = PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE);

        *control.thread_id.lock().unwrap() = GetCurrentThreadId();

        let shared = HookShared {
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
        };
        SHARED.with(|slot| *slot.borrow_mut() = Some(shared));

        if control.cancelled.load(Ordering::SeqCst) {
            SHARED.with(|slot| *slot.borrow_mut() = None);
            return;
        }
        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) {
            Ok(hook) => {
                let _ = ready.send(true);

                while !control.cancelled.load(Ordering::SeqCst) {
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
                let _ = ready.send(false);
            }
        }

        SHARED.with(|slot| *slot.borrow_mut() = None);
    }
}

fn is_shift_down() -> bool {
    key_down(VK_SHIFT.0 as i32) || key_down(VK_LSHIFT.0 as i32) || key_down(VK_RSHIFT.0 as i32)
}

fn is_ctrl_down() -> bool {
    key_down(VK_CONTROL.0 as i32)
        || key_down(VK_LCONTROL.0 as i32)
        || key_down(VK_RCONTROL.0 as i32)
}

fn is_alt_down() -> bool {
    key_down(VK_MENU.0 as i32) || key_down(VK_LMENU.0 as i32) || key_down(VK_RMENU.0 as i32)
}

fn is_win_down() -> bool {
    unsafe { GetAsyncKeyState(VK_LWIN.0 as i32) < 0 || GetAsyncKeyState(VK_RWIN.0 as i32) < 0 }
}

fn modifiers_match(shared: &HookShared) -> bool {
    let ctrl = is_ctrl_down();
    let alt = is_alt_down();
    let shift = is_shift_down();
    let win = is_win_down();

    ctrl == shared.ctrl && alt == shared.alt && shift == shared.shift && win == shared.win
}

fn key_down(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) < 0 }
}

/// The whole-menu mode is Ctrl+Shift + middle button.
/// But if the user explicitly configured Ctrl+Shift+middle as the main shortcut,
/// do not hijack it for menu mode.
fn menu_mode_pressed(shared: &HookShared) -> bool {
    let is_main_shortcut = shared.button_vk == VK_MBUTTON.0 as u32
        && shared.ctrl
        && shared.shift
        && !shared.alt
        && !shared.win;
    if is_main_shortcut {
        return false;
    }
    is_ctrl_down() && is_shift_down() && !is_alt_down() && !is_win_down()
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    let handled = SHARED.with(|slot| {
        let Ok(shared) = slot.try_borrow() else {
            return false;
        };
        let Some(shared) = shared.as_ref() else {
            return false;
        };
        let message = wparam.0 as u32;
        let data = &*(lparam.0 as *const MSLLHOOKSTRUCT);

        match message {
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                // Special mode first: it is a fixed gesture that must not be
                // confused with the configurable shortcut.
                if message == WM_MBUTTONDOWN
                    && menu_mode_pressed(shared)
                    && !win32::is_excluded(data.pt, &shared.exclude)
                {
                    shared.pressed.store(false, Ordering::SeqCst);
                    shared.menu_pressed.store(true, Ordering::SeqCst);
                    return true;
                }

                let button_matches = shared.button_vk == button_for_message(message);
                if button_matches
                    && modifiers_match(shared)
                    && !win32::is_excluded(data.pt, &shared.exclude)
                {
                    shared.pressed.store(true, Ordering::SeqCst);
                    shared.menu_pressed.store(false, Ordering::SeqCst);
                    return true;
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
                    return true;
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
                    return true;
                }
            }
            _ => {}
        }
        false
    });

    if handled {
        return LRESULT(1);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_is_held_until_owner_exits() {
        let reservation = HookReservation::acquire().unwrap();
        assert!(HookReservation::acquire().is_none());
        drop(reservation);
        assert!(HookReservation::acquire().is_some());
    }

    #[test]
    fn stop_before_queue_creation_sets_cancellation() {
        let control = HookControl {
            cancelled: AtomicBool::new(false),
            thread_id: Mutex::new(0),
        };
        control.stop();
        assert!(control.cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn startup_timeout_cancels_without_waiting_for_thread() {
        let control = HookControl {
            cancelled: AtomicBool::new(false),
            thread_id: Mutex::new(0),
        };
        let (_ready_tx, ready_rx) = mpsc::channel();
        assert!(!wait_for_startup(
            &ready_rx,
            &control,
            Duration::from_millis(10)
        ));
        assert!(control.cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn point_round_trip_includes_negative_coordinates() {
        for (x, y) in [(0, 0), (-32768, 32767), (-120, -340)] {
            let point = unpack_point(pack_point(x, y));
            assert_eq!((point.x, point.y), (x, y));
        }
    }
}
