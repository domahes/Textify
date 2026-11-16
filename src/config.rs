//! Persistent settings (stored next to the executable, Textify style) and
//! a few small helpers for hotkey presentation.

use std::fmt;
use std::fs;
use std::path::Path;

use windows::core::w;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyNameTextW, MapVirtualKeyW, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, MAPVK_VK_TO_VSC,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    pub fn vk(self) -> u32 {
        match self {
            MouseButton::Left => VK_LBUTTON.0 as u32,
            MouseButton::Middle => VK_MBUTTON.0 as u32,
            MouseButton::Right => VK_RBUTTON.0 as u32,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MouseButton::Left => "left",
            MouseButton::Middle => "middle",
            MouseButton::Right => "right",
        }
    }

    pub fn label_ru(self) -> &'static str {
        match self {
            MouseButton::Left => "левая кнопка",
            MouseButton::Middle => "средняя кнопка",
            MouseButton::Right => "правая кнопка",
        }
    }

    pub fn short_ru(self) -> &'static str {
        match self {
            MouseButton::Left => "ЛКМ",
            MouseButton::Middle => "СКМ",
            MouseButton::Right => "ПКМ",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "left" | "l" | "lbutton" => Some(MouseButton::Left),
            "middle" | "m" | "mbutton" => Some(MouseButton::Middle),
            "right" | "r" | "rbutton" => Some(MouseButton::Right),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseHotkey {
    pub button: MouseButton,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Default for MouseHotkey {
    fn default() -> Self {
        Self {
            button: MouseButton::Middle,
            ctrl: false,
            alt: false,
            shift: true,
            win: false,
        }
    }
}

impl MouseHotkey {
    pub fn modifiers(&self) -> String {
        modifiers_text(self.ctrl, self.alt, self.shift, self.win)
    }

    pub fn label(&self) -> String {
        let modifiers = self.modifiers();
        if modifiers.is_empty() {
            self.button.label_ru().to_string()
        } else {
            format!("{} + {}", modifiers, self.button.label_ru())
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyHotkey {
    pub vk: u32,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl KeyHotkey {
    pub fn label(&self) -> String {
        let modifiers = modifiers_text(self.ctrl, self.alt, self.shift, self.win);
        let key = vk_label(self.vk);
        if modifiers.is_empty() {
            key
        } else {
            format!("{modifiers} + {key}")
        }
    }
}

fn modifiers_text(ctrl: bool, alt: bool, shift: bool, win: bool) -> String {
    let mut parts = Vec::new();
    if ctrl {
        parts.push("Ctrl");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    if win {
        parts.push("Win");
    }
    parts.join(" + ")
}

/// Human-readable name of a virtual-key code.
pub fn vk_label(vk: u32) -> String {
    if vk == 0 {
        return "не задано".to_string();
    }

    if (0x41..=0x5A).contains(&vk) {
        return char::from_u32(vk).unwrap().to_string();
    }
    if (0x30..=0x39).contains(&vk) {
        return char::from_u32(vk).unwrap().to_string();
    }
    if (0x70..=0x87).contains(&vk) {
        return format!("F{}", vk - 0x6F);
    }

    match vk {
        0x08 => "Backspace".to_string(),
        0x09 => "Tab".to_string(),
        0x0D => "Enter".to_string(),
        0x13 => "Pause".to_string(),
        0x14 => "Caps Lock".to_string(),
        0x1B => "Esc".to_string(),
        0x20 => "Space".to_string(),
        0x21 => "Page Up".to_string(),
        0x22 => "Page Down".to_string(),
        0x23 => "End".to_string(),
        0x24 => "Home".to_string(),
        0x25 => "←".to_string(),
        0x26 => "↑".to_string(),
        0x27 => "→".to_string(),
        0x28 => "↓".to_string(),
        0x2C => "Print Screen".to_string(),
        0x2D => "Insert".to_string(),
        0x2E => "Delete".to_string(),
        0x5B => "Win".to_string(),
        0x5C => "Win".to_string(),
        0x5D => "Menu".to_string(),
        0x90 => "Num Lock".to_string(),
        0x91 => "Scroll Lock".to_string(),
        0xBA => ";".to_string(),
        0xBB => "=".to_string(),
        0xBC => ",".to_string(),
        0xBD => "-".to_string(),
        0xBE => ".".to_string(),
        0xBF => "/".to_string(),
        0xC0 => "`".to_string(),
        0xDB => "[".to_string(),
        0xDC => "\\".to_string(),
        0xDD => "]".to_string(),
        0xDE => "'".to_string(),
        _ => {
            unsafe {
                let scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
                let mut buffer = [0u16; 64];
                let length = GetKeyNameTextW((scan << 16) as i32, &mut buffer);
                if length > 0 {
                    String::from_utf16_lossy(&buffer[..length as usize])
                } else {
                    format!("0x{vk:02X}")
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RetrievalMethod {
    #[default]
    Auto,
    Uia,
    Msaa,
    WindowText,
}

impl RetrievalMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            RetrievalMethod::Auto => "auto",
            RetrievalMethod::Uia => "uia",
            RetrievalMethod::Msaa => "msaa",
            RetrievalMethod::WindowText => "window",
        }
    }

    pub fn label_ru(self) -> &'static str {
        match self {
            RetrievalMethod::Auto => "Авто (UIA + MSAA)",
            RetrievalMethod::Uia => "UI Automation",
            RetrievalMethod::Msaa => "MSAA (IAccessible)",
            RetrievalMethod::WindowText => "Текст окна (WM_GETTEXT)",
        }
    }

    /// Compact label for places where the full description does not fit.
    pub fn short_ru(self) -> &'static str {
        match self {
            RetrievalMethod::Auto => "авто",
            RetrievalMethod::Uia => "UIA",
            RetrievalMethod::Msaa => "MSAA",
            RetrievalMethod::WindowText => "окно",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "uia" => RetrievalMethod::Uia,
            "msaa" => RetrievalMethod::Msaa,
            "window" | "wm_gettext" | "gettext" => RetrievalMethod::WindowText,
            _ => RetrievalMethod::Auto,
        }
    }

    pub fn next(self) -> Self {
        match self {
            RetrievalMethod::Auto => RetrievalMethod::Uia,
            RetrievalMethod::Uia => RetrievalMethod::Msaa,
            RetrievalMethod::Msaa => RetrievalMethod::WindowText,
            RetrievalMethod::WindowText => RetrievalMethod::Auto,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub mouse: MouseHotkey,
    pub keyboard: Option<KeyHotkey>,
    pub retrieval: RetrievalMethod,
    pub auto_copy: bool,
    pub unicode_spaces: bool,
    pub font_face: String,
    pub font_size: u32,
    pub run_at_login: bool,
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mouse: MouseHotkey::default(),
            keyboard: None,
            retrieval: RetrievalMethod::Auto,
            auto_copy: true,
            unicode_spaces: false,
            font_face: String::new(),
            font_size: 0,
            run_at_login: false,
            exclude: Vec::new(),
        }
    }
}

impl Config {
    pub fn mouse_shortcut(&self) -> String {
        self.mouse.label()
    }

    pub fn keyboard_shortcut(&self) -> String {
        self.keyboard
            .map(|hotkey| hotkey.label())
            .unwrap_or_else(|| "не задано".to_string())
    }

    pub fn load(path: &Path) -> Self {
        let mut config = Config::default();

        let Ok(contents) = fs::read_to_string(path) else {
            return config;
        };

        let mut section = String::new();
        let mut keyboard_enabled = true;
        let mut keyboard_vk = 0u32;
        let mut keyboard_ctrl = false;
        let mut keyboard_alt = false;
        let mut keyboard_shift = false;
        let mut keyboard_win = false;

        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = name.trim().to_ascii_lowercase();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            match (section.as_str(), key.as_str()) {
                ("hotkey", "mouse_button") => {
                    if let Some(button) = MouseButton::parse(value) {
                        config.mouse.button = button;
                    }
                }
                ("hotkey", "mouse_ctrl") => config.mouse.ctrl = parse_bool(value),
                ("hotkey", "mouse_alt") => config.mouse.alt = parse_bool(value),
                ("hotkey", "mouse_shift") => config.mouse.shift = parse_bool(value),
                ("hotkey", "mouse_win") => config.mouse.win = parse_bool(value),
                ("hotkey", "keyboard_enabled") => keyboard_enabled = parse_bool(value),
                ("hotkey", "keyboard_vk") => {
                    keyboard_vk = value.parse::<u32>().unwrap_or(0);
                }
                ("hotkey", "keyboard_ctrl") => keyboard_ctrl = parse_bool(value),
                ("hotkey", "keyboard_alt") => keyboard_alt = parse_bool(value),
                ("hotkey", "keyboard_shift") => keyboard_shift = parse_bool(value),
                ("hotkey", "keyboard_win") => keyboard_win = parse_bool(value),
                ("general", "retrieval") => config.retrieval = RetrievalMethod::parse(value),
                ("general", "auto_copy") => config.auto_copy = parse_bool(value),
                ("general", "unicode_spaces") => config.unicode_spaces = parse_bool(value),
                ("general", "font_face") => config.font_face = value.to_string(),
                ("general", "font_size") => config.font_size = value.parse().unwrap_or(0),
                ("startup", "run_at_login") => config.run_at_login = parse_bool(value),
                ("exclude", _) => {
                    if !value.is_empty() {
                        config.exclude.push(value.to_string());
                    }
                }
                _ => {}
            }
        }

        if keyboard_enabled && keyboard_vk != 0 {
            config.keyboard = Some(KeyHotkey {
                vk: keyboard_vk,
                ctrl: keyboard_ctrl,
                alt: keyboard_alt,
                shift: keyboard_shift,
                win: keyboard_win,
            });
        }

        config
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let keyboard = self.keyboard.unwrap_or(KeyHotkey {
            vk: 0,
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
        });

        let mut text = String::new();
        text.push_str("[hotkey]\n");
        text.push_str(&format!("mouse_button={}\n", self.mouse.button.as_str()));
        text.push_str(&format!("mouse_ctrl={}\n", bool_int(self.mouse.ctrl)));
        text.push_str(&format!("mouse_alt={}\n", bool_int(self.mouse.alt)));
        text.push_str(&format!("mouse_shift={}\n", bool_int(self.mouse.shift)));
        text.push_str(&format!("mouse_win={}\n", bool_int(self.mouse.win)));
        text.push_str(&format!(
            "keyboard_enabled={}\n",
            bool_int(self.keyboard.is_some())
        ));
        text.push_str(&format!("keyboard_vk={}\n", keyboard.vk));
        text.push_str(&format!("keyboard_ctrl={}\n", bool_int(keyboard.ctrl)));
        text.push_str(&format!("keyboard_alt={}\n", bool_int(keyboard.alt)));
        text.push_str(&format!("keyboard_shift={}\n", bool_int(keyboard.shift)));
        text.push_str(&format!("keyboard_win={}\n", bool_int(keyboard.win)));
        text.push_str("\n[general]\n");
        text.push_str(&format!("retrieval={}\n", self.retrieval.as_str()));
        text.push_str(&format!("auto_copy={}\n", bool_int(self.auto_copy)));
        text.push_str(&format!("unicode_spaces={}\n", bool_int(self.unicode_spaces)));
        text.push_str(&format!("font_face={}\n", self.font_face));
        text.push_str(&format!("font_size={}\n", self.font_size));
        text.push_str("\n[startup]\n");
        text.push_str(&format!("run_at_login={}\n", bool_int(self.run_at_login)));

        if !self.exclude.is_empty() {
            text.push_str("\n[exclude]\n");
            for (index, program) in self.exclude.iter().enumerate() {
                text.push_str(&format!("{}={}\n", index + 1, program));
            }
        }

        fs::write(path, text)
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn bool_int(value: bool) -> u8 {
    if value {
        1
    } else {
        0
    }
}

/// Adds or removes the application from the current user's Run key.
pub fn set_run_at_login(enabled: bool) -> bool {
    unsafe {
        let mut key = HKEY::default();
        let create_result = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            w!(""),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        );
        if create_result.is_err() {
            return false;
        }

        let result = if enabled {
            let exe = std::env::current_exe().unwrap_or_default();
            let command = format!("\"{}\" -hidewnd", exe.display());
            let mut data: Vec<u16> = command.encode_utf16().collect();
            data.push(0);
            let bytes =
                std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
            RegSetValueExW(key, w!("TextifyRS"), 0, REG_SZ, Some(bytes)).is_ok()
        } else {
            let _ = RegDeleteValueW(key, w!("TextifyRS"));
            true
        };

        let _ = RegCloseKey(key);
        result
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mouse={}, keyboard={}, retrieval={}, auto_copy={}",
            self.mouse.label(),
            self.keyboard_shortcut(),
            self.retrieval.label_ru(),
            self.auto_copy
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            mouse: MouseHotkey {
                button: MouseButton::Right,
                ctrl: true,
                alt: false,
                shift: false,
                win: false,
            },
            keyboard: Some(KeyHotkey {
                vk: 0x58,
                ctrl: true,
                alt: true,
                shift: false,
                win: false,
            }),
            retrieval: RetrievalMethod::Msaa,
            auto_copy: false,
            unicode_spaces: true,
            font_face: "Segoe UI".to_string(),
            font_size: 14,
            run_at_login: true,
            exclude: vec!["game.exe".to_string(), "other.exe".to_string()],
        }
    }

    #[test]
    fn round_trips_through_ini() {
        let path = std::env::temp_dir().join("textify_rs_config_test.ini");
        let config = sample();
        config.save(&path).unwrap();

        let loaded = Config::load(&path);
        assert_eq!(loaded, config);
        assert_eq!(loaded.keyboard.unwrap().label(), "Ctrl + Alt + X");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let loaded = Config::load(Path::new("Z:\\this\\path\\does\\not\\exist.ini"));
        assert_eq!(loaded, Config::default());
    }

    #[test]
    fn formats_shortcuts() {
        assert_eq!(
            MouseHotkey::default().label(),
            "Shift + средняя кнопка"
        );
        assert_eq!(vk_label(0x70), "F1");
        assert_eq!(vk_label(0x41), "A");
    }

    #[test]
    fn keyboard_enabled_zero_disables() {
        let path = std::env::temp_dir().join("textify_rs_config_disabled.ini");
        std::fs::write(
            &path,
            "[hotkey]\nkeyboard_vk=88\nkeyboard_ctrl=1\nkeyboard_enabled=0\n",
        )
        .unwrap();
        let loaded = Config::load(&path);
        assert!(loaded.keyboard.is_none());
        let _ = std::fs::remove_file(path);
    }
}
