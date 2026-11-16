//! Notification-area icon and its context menu.

use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, HMENU, MF_CHECKED, MF_SEPARATOR,
    MF_STRING, PostMessageW, SetForegroundWindow, TrackPopupMenu, MENU_ITEM_FLAGS, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_NULL,
};

use crate::config::Config;
use crate::win32;

pub const TRAY_UID: u32 = 1;

pub enum TrayAction {
    ShowSettings,
    CaptureNow,
    ToggleAutoCopy,
    CycleRetrieval,
    OpenIni,
    About,
    Exit,
}

pub fn add(hwnd: HWND, icon: HICON, tip: &str, callback: u32) -> bool {
    let data = make_data(hwnd, icon, tip, callback);
    unsafe { Shell_NotifyIconW(NIM_ADD, &data).as_bool() }
}

pub fn remove(hwnd: HWND) {
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_UID;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

pub fn update(hwnd: HWND, icon: HICON, tip: &str, callback: u32) {
    let data = make_data(hwnd, icon, tip, callback);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

fn make_data(hwnd: HWND, icon: HICON, tip: &str, callback: u32) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_UID;
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = callback;
    data.hIcon = icon;

    let units: Vec<u16> = tip.encode_utf16().take(127).collect();
    data.szTip[..units.len()].copy_from_slice(&units);
    data
}

pub fn show_menu(hwnd: HWND, config: &Config) -> Option<TrayAction> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;

        append(menu, MF_STRING, 100, "Настройки…");
        append(
            menu,
            MF_STRING,
            101,
            &format!("Захватить текст под курсором\t{}", config.mouse_shortcut()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let auto_flags = if config.auto_copy {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        append(menu, auto_flags, 102, "Автокопировать текст");
        append(
            menu,
            MF_STRING,
            103,
            &format!("Способ: {}", config.retrieval.label_ru()),
        );
        append(menu, MF_STRING, 104, "Открыть файл настроек");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append(menu, MF_STRING, 105, "О программе");
        append(menu, MF_STRING, 106, "Выход");

        let _ = SetForegroundWindow(hwnd);
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let command = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            point.x,
            point.y,
            0,
            hwnd,
            None,
        )
        .0 as u32;

        let _ = DestroyMenu(menu);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));

        match command {
            100 => Some(TrayAction::ShowSettings),
            101 => Some(TrayAction::CaptureNow),
            102 => Some(TrayAction::ToggleAutoCopy),
            103 => Some(TrayAction::CycleRetrieval),
            104 => Some(TrayAction::OpenIni),
            105 => Some(TrayAction::About),
            106 => Some(TrayAction::Exit),
            _ => None,
        }
    }
}

fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    let wide = win32::wide(text);
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr()));
    }
}
