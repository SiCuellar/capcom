//! Audio capture abstraction and WAV helpers.

pub mod capture;
pub mod level;
pub mod wav;

use anyhow::Result;

/// Microphone capture backend. **Implemented by the audio agent (M1/M2, cpal).**
///
/// Contract for implementers:
/// - `start` must open the device and begin buffering *immediately*, before any UI or sound
///   (architecture.md: "start the mic before any UI or sound"). In `always_on` mic mode the
///   stream is already open and `start` should prepend the 400 ms pre-roll ring buffer.
/// - Capture at the device's native rate, then convert with [`wav::to_16k_mono`] so that
///   `stop` returns **16 kHz mono i16** samples. Never trim silence (no VAD): never lose words.
/// - `stop` ends the clip and returns all samples; `cancel` discards them.
/// - Implementations are driven from the daemon; they may spawn their own threads
///   (cpal callbacks) but the trait methods themselves must not block for long.
/// - Optionally, `first_sample_received` lets the daemon play the start sound only once
///   audio is actually flowing.
pub trait AudioSource: Send {
    fn start(&mut self) -> Result<()>;
    /// Stop and return the clip as 16 kHz mono i16.
    fn stop(&mut self) -> Result<Vec<i16>>;
    /// Stop and discard.
    fn cancel(&mut self);
    /// True once the first samples of the current clip have arrived.
    fn first_sample_received(&self) -> bool {
        true
    }
    /// Identifies the current clip; capture events carry it so stale ones can be ignored.
    fn generation(&self) -> u64 {
        0
    }
    /// True if this source reports "capture is live" via [`capture::CaptureEvent::Live`]; the
    /// daemon then defers `FeedbackEvent::RecordingStarted` (the start sound) until that event.
    fn emits_live_event(&self) -> bool {
        false
    }
    /// Switch to the input device named by a config `audio.device` value (`"default"` or a
    /// name). Takes effect from the next clip; a clip in progress keeps its device.
    fn set_device(&mut self, _name: &str) -> Result<()> {
        anyhow::bail!("this audio source cannot switch devices")
    }
    /// Install (or clear) a callback receiving the live input level (0..=1, perceptual, smoothed)
    /// at ~30 Hz while a clip is recording. Called on the audio thread: must not block.
    /// Intended wiring: `Arc::new(move |l| ui.send(UiEvent::Level(l)))`. See [`level`].
    fn set_level_sink(&mut self, _sink: Option<level::LevelSink>) {}
}

/// Clip length in milliseconds for 16 kHz samples.
pub fn clip_ms(samples: &[i16]) -> u64 {
    samples.len() as u64 * 1000 / wav::TARGET_RATE as u64
}
