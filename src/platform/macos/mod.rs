//! macOS platform implementation: clipboard paste, afplay/osascript feedback, handy-keys hotkey.
//!
//! `ui`: the NSApplication host (menu-bar icon; the floating pill registers there too).

pub mod doctor;
mod feedback;
mod hotkey;
pub mod hotkey_logic;
mod paste;
pub mod secure;
pub mod ui;

use crate::config::HotkeyConfig;
use crate::platform::Platform;

pub use feedback::{notify, MacFeedback};
pub use hotkey::MacHotkey;
pub use paste::MacPaster;

pub fn new(hotkey: &HotkeyConfig) -> Platform {
    Platform {
        paster: Box::new(MacPaster::new()),
        feedback: Box::new(MacFeedback::new()),
        hotkey: Some(Box::new(MacHotkey::new(hotkey))),
    }
}
