//! Live input level metering for the UI (pill waveform bars).
//!
//! [`LevelMeter`] runs **inside the cpal data callback**: it accumulates the sum of squares of
//! the incoming interleaved f32 samples, and every [`WINDOW_MS`] (~33 ms, ~30 Hz) turns the window
//! RMS into dBFS, maps it to a perceptual `0.0..=1.0` value ([`FLOOR_DB`] -> 0, [`CEIL_DB`] -> 1)
//! and smooths it with a fast attack / slow release envelope. No allocation, no locking, O(n).
//!
//! Wiring (orchestrator): the capture layer calls the [`LevelSink`] installed with
//! [`crate::audio::AudioSource::set_level_sink`] once per window, **on the audio thread**, only
//! while a clip is being recorded. The sink must be cheap and non-blocking, e.g.
//! `Arc::new(move |l| ui_sink.send(UiEvent::Level(l)))`.

use std::sync::Arc;

/// Callback receiving smoothed levels (0..=1) at ~30 Hz from the audio thread.
pub type LevelSink = Arc<dyn Fn(f32) + Send + Sync>;

/// Metering window length.
pub const WINDOW_MS: u32 = 33;
/// dBFS mapped to 0.0 (room noise / silence).
pub const FLOOR_DB: f32 = -60.0;
/// dBFS mapped to 1.0 (loud, close speech).
pub const CEIL_DB: f32 = -10.0;
/// Fraction of the gap closed per window when the level rises (fast attack).
pub const ATTACK: f32 = 0.7;
/// Fraction of the gap closed per window when the level falls (slow release, ~150 ms).
pub const RELEASE: f32 = 0.2;

/// RMS (linear, full scale = 1.0) to dBFS, floored at -120 dB.
pub fn rms_to_dbfs(rms: f32) -> f32 {
    20.0 * rms.max(1e-6).log10()
}

/// Map dBFS onto the perceptual 0..=1 scale ([`FLOOR_DB`]..[`CEIL_DB`], clamped).
pub fn db_to_unit(db: f32) -> f32 {
    if !db.is_finite() {
        return 0.0;
    }
    ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

/// Windowed RMS meter with attack/release smoothing. See the module docs.
#[derive(Debug, Clone)]
pub struct LevelMeter {
    window: usize,
    acc: f64,
    n: usize,
    smoothed: f32,
}

impl LevelMeter {
    /// `rate` in Hz, `channels` interleaved; the window counts interleaved samples.
    pub fn new(rate: u32, channels: u16) -> Self {
        let window = (rate as usize * WINDOW_MS as usize / 1000).max(1) * channels.max(1) as usize;
        Self { window, acc: 0.0, n: 0, smoothed: 0.0 }
    }

    /// Feed samples. Returns the smoothed level if at least one window completed in this call
    /// (the latest one when several did).
    pub fn push(&mut self, data: &[f32]) -> Option<f32> {
        let mut out = None;
        for &s in data {
            self.acc += (s as f64) * (s as f64);
            self.n += 1;
            if self.n >= self.window {
                let rms = (self.acc / self.n as f64).sqrt() as f32;
                self.acc = 0.0;
                self.n = 0;
                out = Some(self.smooth(db_to_unit(rms_to_dbfs(rms))));
            }
        }
        out
    }

    /// Apply one window of attack/release smoothing toward `target`.
    pub fn smooth(&mut self, target: f32) -> f32 {
        let k = if target > self.smoothed { ATTACK } else { RELEASE };
        self.smoothed += (target - self.smoothed) * k;
        if self.smoothed < 1e-4 {
            self.smoothed = 0.0;
        }
        self.smoothed
    }

    /// Current smoothed level.
    pub fn level(&self) -> f32 {
        self.smoothed
    }

    /// Drop partial window and envelope (between clips).
    pub fn reset(&mut self) {
        self.acc = 0.0;
        self.n = 0;
        self.smoothed = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (i as f32 * 0.07).sin()).collect()
    }

    #[test]
    fn mapping_endpoints() {
        assert_eq!(db_to_unit(-90.0), 0.0);
        assert_eq!(db_to_unit(FLOOR_DB), 0.0);
        assert!((db_to_unit(-35.0) - 0.5).abs() < 1e-6);
        assert_eq!(db_to_unit(CEIL_DB), 1.0);
        assert_eq!(db_to_unit(0.0), 1.0);
        assert_eq!(db_to_unit(f32::NEG_INFINITY), 0.0);
        assert!((rms_to_dbfs(1.0)).abs() < 1e-6);
        assert!((rms_to_dbfs(0.1) + 20.0).abs() < 1e-4);
        assert!(rms_to_dbfs(0.0) <= -119.0);
    }

    #[test]
    fn window_size_and_emission_rate() {
        // 48 kHz stereo: 33 ms = 1584 frames = 3168 interleaved samples.
        let mut m = LevelMeter::new(48_000, 2);
        assert_eq!(m.window, 3168);
        assert!(m.push(&vec![0.0; 3167]).is_none());
        assert!(m.push(&[0.0]).is_some());
        // ~1 s of audio produces ~30 levels.
        let mut m = LevelMeter::new(16_000, 1);
        let count = (0..100).filter(|_| m.push(&vec![0.01; 160]).is_some()).count();
        assert!((29..=31).contains(&count), "{count}");
    }

    #[test]
    fn silence_is_zero_and_loud_saturates() {
        let mut m = LevelMeter::new(16_000, 1);
        assert_eq!(m.push(&vec![0.0; 16_000]), Some(0.0));
        let mut m = LevelMeter::new(16_000, 1);
        let mut last = 0.0;
        for _ in 0..20 {
            if let Some(l) = m.push(&sine(0.9, 528)) {
                last = l;
            }
        }
        assert!(last > 0.99, "{last}");
    }

    #[test]
    fn speech_level_maps_mid_scale() {
        // RMS of a 0.0447-amplitude sine ~ 0.0316 = -30 dBFS -> 0.6.
        let mut m = LevelMeter::new(16_000, 1);
        let mut last = 0.0;
        for _ in 0..30 {
            if let Some(l) = m.push(&sine(0.0447, 528)) {
                last = l;
            }
        }
        assert!((last - 0.6).abs() < 0.03, "{last}");
    }

    #[test]
    fn fast_attack_slow_release() {
        let mut m = LevelMeter::new(16_000, 1);
        let up = m.smooth(1.0);
        assert!((up - ATTACK).abs() < 1e-6, "one window reaches 70%");
        for _ in 0..5 {
            m.smooth(1.0);
        }
        let prev = m.level();
        assert!(prev > 0.99);
        let down = m.smooth(0.0);
        assert!((down - (1.0 - RELEASE) * prev).abs() < 1e-6);
        assert!(down > 0.75, "release is slow: {down}");
        // After ~0.5 s of silence the bars are essentially down.
        for _ in 0..15 {
            m.smooth(0.0);
        }
        assert!(m.level() < 0.05, "{}", m.level());
        m.reset();
        assert_eq!(m.level(), 0.0);
    }
}
