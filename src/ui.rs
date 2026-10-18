//! UI contract shared by the macOS app host (menu-bar icon) and the floating pill.
//!
//! Flow: the daemon (tokio, background threads) emits [`UiEvent`]s through a [`UiSink`].
//! The macOS app host forwards them to the AppKit **main thread** and fans them out to every
//! registered [`UiComponent`] (the status item, the pill, ...). Components never block and never
//! touch the network; they only render.
//!
//! Emission rules (implemented in `daemon.rs`, `emit_ui_for`):
//! - `State(Idle)` once when the daemon starts serving.
//! - `State(Listening)` only once capture is verifiably live (first samples arrived).
//! - `State(Processing)` when recording stops and ASR/cleanup/paste begins.
//! - The terminal events `State(Done)`, `State(Cancelled)` and `Error(_)` end a cycle; the daemon
//!   follows each with `State(Idle)` after a short settle delay (~0.7 s / 0.4 s / 2.5 s) unless a
//!   newer event was emitted first. Components may therefore render terminal events as a brief
//!   flash and rely on the explicit `Idle` to reset.

/// Coarse dictation state, as the UI should present it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiState {
    /// Daemon running, nothing happening.
    Idle,
    /// Mic is live and recording (start sound has played).
    Listening,
    /// Recording stopped; ASR / cleanup / paste in progress.
    Processing,
    /// Text was pasted (or left on the clipboard). Brief, then back to Idle.
    Done,
    /// Recording discarded (Esc, too short).
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    State(UiState),
    /// Live input level while Listening, normalized 0.0..=1.0 (perceptual, already smoothed),
    /// emitted at ~30 Hz. Drives the pill's waveform bars.
    Level(f32),
    /// Something failed; show briefly (pill error flash / menu-bar tooltip).
    Error(String),
}

/// Thread-safe sender the daemon uses. Must be cheap and non-blocking.
pub trait UiSink: Send + Sync {
    fn send(&self, ev: UiEvent);
}

/// No-op sink (Linux for now, tests, headless runs).
pub struct NullSink;
impl UiSink for NullSink {
    fn send(&self, _ev: UiEvent) {}
}

/// A main-thread UI element. `handle` is always called on the AppKit main thread.
pub trait UiComponent {
    fn handle(&mut self, ev: &UiEvent);
}
