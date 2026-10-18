//! Platform abstraction. Everything OS-specific lives behind these traits so the daemon,
//! pipeline and state machine stay portable. See `docs/architecture.md` (Pipeline details table).

// TODO(linux): gtk4-layer-shell pill (see research/linux-wayland.md); macOS pill lives in
// `macos::ui::pill`, its animation model (`pill::anim`) is platform-neutral and can be reused.
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

use anyhow::Result;

/// Events the daemon wants the user to perceive (sound and/or notification).
#[derive(Debug, Clone, PartialEq)]
pub enum FeedbackEvent {
    /// Mic is live (play only after first samples arrive).
    RecordingStarted,
    RecordingStopped,
    /// `start` while busy.
    Busy,
    /// Short clip or cancel: nothing happened.
    Discarded,
    Done,
    /// Error with a user-readable message (notification + error sound).
    Error(String),
    /// Informational notification (e.g. "text left on clipboard").
    Notify(String),
}

/// Insert text into the focused application. **Implemented per platform:**
/// macOS (Handy-style pasteboard snapshot, CGEvent Cmd+V, restore if changeCount unchanged,
/// Secure Input detection) and Linux (wl-copy + `hyprctl dispatch sendshortcut`, wtype fallback).
/// Must leave the text on the clipboard if pasting is impossible, and return `Err` so the
/// daemon can notify. May block briefly (the daemon calls it via `spawn_blocking`).
pub trait Paster: Send + Sync {
    fn paste(&self, text: &str) -> Result<()>;
}

/// Sounds, notifications and (macOS) menu-bar state.
pub trait Feedback: Send + Sync {
    fn event(&self, ev: FeedbackEvent);
}

/// Events from a global hotkey listener (macOS; Linux uses compositor binds -> CLI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Start,
    Stop,
    Toggle,
    Cancel,
}

/// Optional global hotkey source. `run` blocks (own thread) and forwards events on `tx`.
pub trait Hotkey: Send + Sync {
    fn run(&mut self, tx: tokio::sync::mpsc::UnboundedSender<HotkeyEvent>) -> Result<()>;
}

/// Everything the daemon needs from the OS.
pub struct Platform {
    pub paster: Box<dyn Paster>,
    pub feedback: Box<dyn Feedback>,
    pub hotkey: Option<Box<dyn Hotkey>>,
}

impl Platform {
    /// Pick the platform implementation. Currently always the log-only fallback.
    /// TODO(platform agents): dispatch to `macos::new()` / `linux::new()` here.
    pub fn detect() -> Self {
        #[cfg(target_os = "linux")]
        {
            linux::new()
        }
        #[cfg(not(target_os = "linux"))]
        Self { paster: Box::new(LogPaster), feedback: Box::new(LogFeedback), hotkey: None }
    }
}

impl Platform {
    /// Like `detect`, but lets platforms that own a hotkey listener (macOS) read `[hotkey]` config.
    pub fn detect_with(hotkey: &crate::config::HotkeyConfig) -> Self {
        #[cfg(target_os = "macos")]
        {
            macos::new(hotkey)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = hotkey;
            Self::detect()
        }
    }
}

/// Fallback: logs instead of pasting.
pub struct LogPaster;
impl Paster for LogPaster {
    fn paste(&self, text: &str) -> Result<()> {
        tracing::info!("[paste stub] {} chars: {text}", text.chars().count());
        Ok(())
    }
}

/// Fallback: logs instead of playing sounds.
pub struct LogFeedback;
impl Feedback for LogFeedback {
    fn event(&self, ev: FeedbackEvent) {
        tracing::info!("[feedback stub] {ev:?}");
    }
}
