//! Textify RS - a small Windows utility that copies text from any dialog or
//! control, even when the application itself does not allow selecting it.
//!
//! The implementation deliberately mirrors the behavior of Textify
//! (<https://ramensoftware.com/textify>): point at the text, press the mouse
//! shortcut, and get a selectable view of what is under the cursor.

pub mod app;
pub mod capture;
mod capture_worker;

pub mod config;
pub mod hook;
pub mod icon_art;
#[cfg(test)]
mod live_capture;
pub mod menu;
pub mod result;
pub mod settings;
pub mod text;
pub mod tray;
pub mod win32;
