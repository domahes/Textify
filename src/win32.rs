//! Thin, mostly unsafe helpers around the Win32 API used by the application.

use std::ffi::c_void;
use std::mem::size_of;
use std::path::Path;

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, CreateFontW, DeleteObject, GetMonitorInfoW, HFONT,
    MonitorFromPoint, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, FW_BOLD, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_GETTEXT_TIMEOUT_MS: u32 = 150;

pub const COLOR_TEXT_MUTED: u32 = 0x00808080;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn hwnd_is_null(hwnd: HWND) -> bool {
    hwnd.0.is_null()
}

pub fn hwnd_from_isize(value: isize) -> HWND {
    HWND(value as *mut c_void)
}

pub fn hwnd_to_isize(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

pub fn instance() -> HINSTANCE {
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        HINSTANCE(module.0)
    }
}

/// Retrieves text from a window or control, including controls that belong to
/// another process (WM_GETTEXT is used instead of GetWindowTextW).
pub fn get_text(hwnd: HWND) -> String {
    if hwnd_is_null(hwnd) {
        return String::new();
    }

    unsafe {
        let length = SendMessageTimeoutW(
            hwnd,
            WM_GETTEXTLENGTH,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            WM_GETTEXT_TIMEOUT_MS,
            None,
        )
        .0 as i32;

        if length > 0 {
            let mut buffer = vec![0u16; length as usize + 1];
            let copied = SendMessageTimeoutW(
                hwnd,
                WM_GETTEXT,
                WPARAM(buffer.len()),
                LPARAM(buffer.as_mut_ptr() as isize),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                WM_GETTEXT_TIMEOUT_MS,
                None,
            )
            .0 as i32;
            if copied > 0 {
                return String::from_utf16_lossy(&buffer[..copied as usize]);
            }
        }

        // WM_GETTEXT is not implemented by every window; fall back to the
        // window caption API.
        let mut buffer = vec![0u16; 1024];
        let copied = GetWindowTextW(hwnd, &mut buffer);
        if copied > 0 {
            String::from_utf16_lossy(&buffer[..copied as usize])
        } else {
            String::new()
        }
    }
}

pub fn get_class(hwnd: HWND) -> String {
    if hwnd_is_null(hwnd) {
        return String::new();
    }
    let mut buffer = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if length > 0 {
        String::from_utf16_lossy(&buffer[..length as usize])
    } else {
        String::new()
    }
}

pub fn get_root(hwnd: HWND) -> HWND {
    unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        if hwnd_is_null(root) {
            hwnd
        } else {
            root
        }
    }
}

pub fn cursor_pos() -> POINT {
    unsafe {
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        point
    }
}

pub fn work_area_near(point: POINT) -> RECT {
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN),
                bottom: GetSystemMetrics(SM_CYSCREEN),
            }
        }
    }
}

pub fn work_area_for_window(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    if !hwnd_is_null(hwnd) {
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
    }
    work_area_near(POINT {
        x: rect.left,
        y: rect.top,
    })
}

pub fn get_window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    if !hwnd_is_null(hwnd) {
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
    }
    rect
}

pub fn rect_width(rect: &RECT) -> i32 {
    rect.right - rect.left
}

pub fn rect_height(rect: &RECT) -> i32 {
    rect.bottom - rect.top
}

pub fn rect_is_empty(rect: &RECT) -> bool {
    rect_width(rect) <= 0 || rect_height(rect) <= 0
}

pub fn center_rect_in(point: POINT, width: i32, height: i32, work: &RECT) -> RECT {
    let mut left = point.x - width / 2;
    let mut top = point.y - height / 2;

    if left + width > work.right {
        left = work.right - width;
    }
    if top + height > work.bottom {
        top = work.bottom - height;
    }
    if left < work.left {
        left = work.left;
    }
    if top < work.top {
        top = work.top;
    }

    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

pub fn dpi_for_window(hwnd: HWND) -> u32 {
    unsafe {
        let dpi = if hwnd_is_null(hwnd) {
            0
        } else {
            GetDpiForWindow(hwnd)
        };
        if dpi == 0 {
            GetDpiForSystem()
        } else {
            dpi
        }
    }
}

pub fn scale(value: i32, dpi: u32) -> i32 {
    ((value as f32) * (dpi as f32) / 96.0).round() as i32
}

pub fn create_font(face: &str, points: f32, weight: i32, dpi: u32) -> HFONT {
    let face = if face.trim().is_empty() {
        "Segoe UI".to_string()
    } else {
        face.to_string()
    };
    let face_wide = wide(&face);
    let height = -((points * dpi as f32 / 72.0).round() as i32);

    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            PCWSTR(face_wide.as_ptr()),
        )
    }
}

pub fn bold_weight() -> i32 {
    FW_BOLD.0 as i32
}

pub fn copy_to_clipboard(owner: HWND, text: &str) -> bool {
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(owner).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        if !opened {
            return false;
        }

        let _ = EmptyClipboard();

        let mut data: Vec<u16> = text.encode_utf16().collect();
        data.push(0);

        let handle = match GlobalAlloc(GMEM_MOVEABLE, data.len() * 2) {
            Ok(handle) => handle,
            Err(_) => {
                let _ = CloseClipboard();
                return false;
            }
        };

        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            let _ = GlobalFree(handle);
            let _ = CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), pointer as *mut u16, data.len());
        let _ = GlobalUnlock(handle);

        let ok = SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(handle.0)).is_ok();
        if !ok {
            let _ = GlobalFree(handle);
        }
        let _ = CloseClipboard();
        ok
    }
}

pub fn open_path(path: &Path) {
    let target = HSTRING::from(path.to_string_lossy().to_string());
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(target.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

pub fn open_in_notepad(path: &Path) {
    let parameters = HSTRING::from(format!("\"{}\"", path.display()));
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("notepad.exe"),
            PCWSTR(parameters.as_ptr()),
            None,
            SW_SHOWNORMAL,
        );
    }
}

pub fn show_error(owner: HWND, text: &str, caption: &str) {
    let text = HSTRING::from(text);
    let caption = HSTRING::from(caption);
    unsafe {
        MessageBoxW(owner, &text, &caption, MB_OK | MB_ICONERROR);
    }
}

pub fn show_info(owner: HWND, text: &str, caption: &str) {
    let text = HSTRING::from(text);
    let caption = HSTRING::from(caption);
    unsafe {
        MessageBoxW(owner, &text, &caption, MB_OK | MB_ICONINFORMATION);
    }
}

pub fn force_foreground(hwnd: HWND) {
    unsafe {
        let foreground = GetForegroundWindow();
        let foreground_thread = GetWindowThreadProcessId(foreground, None);
        let current_thread = GetCurrentThreadId();
        let attached = foreground_thread != 0
            && foreground_thread != current_thread
            && AttachThreadInput(current_thread, foreground_thread, true).as_bool();

        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);

        if attached {
            let _ = AttachThreadInput(current_thread, foreground_thread, false);
        }
    }
}

pub fn process_name_for_point(point: POINT) -> Option<String> {
    unsafe {
        let hwnd = WindowFromPoint(point);
        if hwnd_is_null(hwnd) {
            return None;
        }

        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        if process_id == 0 {
            return None;
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(process);

        if !ok || size == 0 {
            return None;
        }

        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        path.rsplit('\\')
            .next()
            .map(|name| name.to_ascii_lowercase())
    }
}

pub fn is_excluded(point: POINT, exclude: &[String]) -> bool {
    if exclude.is_empty() {
        return false;
    }
    let Some(process) = process_name_for_point(point) else {
        return false;
    };
    exclude
        .iter()
        .any(|entry| entry.eq_ignore_ascii_case(&process))
}

pub fn find_window(class_name: &str) -> HWND {
    let class_name = wide(class_name);
    unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), None).unwrap_or_default() }
}

pub fn make_app_icon(size: i32) -> Option<HICON> {
    unsafe {
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = size;
        info.bmiHeader.biHeight = -size;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0 as u32;

        let mut bits: *mut c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(color);
            return None;
        }

        let pixels = crate::icon_art::rasterize(size as usize);
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());

        let mask = CreateBitmap(size, size, 1, 1, None);
        let icon_info = ICONINFO {
            fIcon: TRUE,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };

        let icon = CreateIconIndirect(&icon_info).ok();

        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);

        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(wide("ab"), vec![0x61, 0x62, 0]);
    }

    #[test]
    fn scales_by_dpi() {
        assert_eq!(scale(100, 96), 100);
        assert_eq!(scale(100, 144), 150);
    }
}
