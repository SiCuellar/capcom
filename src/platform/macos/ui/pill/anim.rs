//! Pure animation model for the pill (no AppKit): state machine + per-frame bar geometry.
//! [`Animator::tick`] produces a [`Frame`] that `pill.rs` copies onto CALayers.

use crate::ui::{UiEvent, UiState};

pub const BARS: usize = 14;
pub const BAR_W: f64 = 2.6;
pub const BAR_GAP: f64 = 3.2;
pub const BAR_MIN_H: f64 = 3.0;
pub const BAR_MAX_H: f64 = 22.0;

const APPEAR_S: f32 = 0.16;
const DONE_FADE_S: f32 = 0.24;
const CANCEL_FADE_S: f32 = 0.12;
const ERROR_HOLD_S: f32 = 0.9;
const ERROR_FADE_S: f32 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Hidden,
    Listening,
    Processing,
    /// Fading out; `rate` = opacity units per second.
    Leaving { rate: f32 },
    /// Red flash; `t` = seconds since the error.
    Error { t: f32 },
}

/// Everything the renderer needs for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Window should be on screen.
    pub visible: bool,
    /// Window alpha 0..=1.
    pub alpha: f32,
    /// Capsule scale (appear/leave "pop").
    pub scale: f32,
    /// 0 = normal near-black capsule, 1 = full error red.
    pub tint: f32,
    /// Bar heights in points.
    pub heights: [f64; BARS],
    /// Bar opacities 0..=1.
    pub opacity: [f32; BARS],
}

#[derive(Debug, Clone)]
pub struct Animator {
    pub phase: Phase,
    /// Latest level from `UiEvent::Level` (0..=1).
    target_level: f32,
    /// Displayed level (interpolated between ~30 Hz level events at 60 fps).
    level: f32,
    t: f32,
    alpha: f32,
    tint: f32,
    heights: [f64; BARS],
    seeds: [(f32, f32, f32); BARS],
}

impl Default for Animator {
    fn default() -> Self {
        Self::new()
    }
}

/// Cheap deterministic per-bar pseudo-random in 0..1.
fn hash01(i: usize, salt: u32) -> f32 {
    let mut x = (i as u32).wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

/// Center-weighted envelope: ~1.0 in the middle, ~0.3 at the edges.
pub fn envelope(i: usize) -> f32 {
    let c = (BARS as f32 - 1.0) / 2.0;
    let x = (i as f32 - c) / c; // -1..1
    0.28 + 0.72 * (-(x / 0.55).powi(2)).exp()
}

fn ease_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x).powi(3)
}

impl Animator {
    pub fn new() -> Self {
        let mut seeds = [(0.0, 0.0, 0.0); BARS];
        for (i, s) in seeds.iter_mut().enumerate() {
            // (angular speed rad/s, phase, per-bar gain jitter)
            *s = (7.0 + 9.0 * hash01(i, 1), std::f32::consts::TAU * hash01(i, 2), 0.8 + 0.35 * hash01(i, 3));
        }
        Self { phase: Phase::Hidden, target_level: 0.0, level: 0.0, t: 0.0, alpha: 0.0, tint: 0.0, heights: [BAR_MIN_H; BARS], seeds }
    }

    pub fn is_hidden(&self) -> bool {
        self.phase == Phase::Hidden
    }

    /// Feed a UI event. Returns true if the pill needs to be (or stay) animated.
    pub fn event(&mut self, ev: &UiEvent) -> bool {
        match ev {
            UiEvent::State(UiState::Listening) => {
                if matches!(self.phase, Phase::Hidden | Phase::Leaving { .. } | Phase::Error { .. }) {
                    if self.phase == Phase::Hidden {
                        self.heights = [BAR_MIN_H; BARS];
                    }
                    self.tint = 0.0;
                }
                self.phase = Phase::Listening;
                self.target_level = 0.0;
            }
            UiEvent::State(UiState::Processing) => {
                // Shown even without a preceding Listening (e.g. `capcom transcribe` via daemon).
                self.phase = Phase::Processing;
                self.target_level = 0.0;
            }
            UiEvent::State(UiState::Done) => self.leave(1.0 / DONE_FADE_S),
            UiEvent::State(UiState::Cancelled) => self.leave(1.0 / CANCEL_FADE_S),
            UiEvent::State(UiState::Idle) => self.leave(1.0 / DONE_FADE_S),
            UiEvent::Level(l) => {
                if self.phase == Phase::Listening {
                    self.target_level = if l.is_finite() { l.clamp(0.0, 1.0) } else { 0.0 };
                }
            }
            UiEvent::Error(_) => {
                self.phase = Phase::Error { t: 0.0 };
                self.target_level = 0.0;
            }
        }
        !self.is_hidden()
    }

    fn leave(&mut self, rate: f32) {
        if !self.is_hidden() {
            self.phase = Phase::Leaving { rate };
        }
    }

    /// Advance by `dt` seconds.
    pub fn tick(&mut self, dt: f32) -> Frame {
        let dt = dt.clamp(0.0, 0.1);
        self.t += dt;
        let t = self.t;
        // Level: interpolate toward the latest event (events are ~30 Hz, frames ~60 Hz).
        let k = 1.0 - (-dt * if self.target_level > self.level { 30.0 } else { 12.0 }).exp();
        self.level += (self.target_level - self.level) * k;

        let mut opacity = [0.95f32; BARS];
        let mut target = [BAR_MIN_H; BARS];
        let span = BAR_MAX_H - BAR_MIN_H;
        match self.phase {
            Phase::Hidden => {}
            Phase::Listening | Phase::Leaving { .. } => {
                // Perceptual boost so normal speech fills the bars.
                let lv = self.level.powf(0.7);
                for (i, tg) in target.iter_mut().enumerate() {
                    let (w, ph, g) = self.seeds[i];
                    let wobble = 0.62 + 0.38 * (t * w + ph).sin() * (t * w * 0.37 + ph * 1.7).cos();
                    // A slow travelling component so loud passages "roll" across the bars.
                    let roll = 0.85 + 0.15 * (t * 4.0 - i as f32 * 0.6).sin();
                    let a = (lv * envelope(i) * wobble * roll * g * 1.45).clamp(0.0, 1.0);
                    // Idle breathing: a hint of life in silence.
                    let idle = 0.04 * (0.5 + 0.5 * (t * 2.2 + i as f32 * 0.45).sin());
                    *tg = BAR_MIN_H + span * (a + idle * (1.0 - a)) as f64;
                }
            }
            Phase::Processing => {
                for (i, (tg, op)) in target.iter_mut().zip(opacity.iter_mut()).enumerate() {
                    let wave = 0.5 + 0.5 * (t * 6.0 - i as f32 * 0.55).sin();
                    let w2 = wave * wave;
                    *tg = BAR_MIN_H + 3.5 * w2 as f64;
                    *op = 0.35 + 0.6 * w2;
                }
            }
            Phase::Error { .. } => {
                opacity = [0.85; BARS];
            }
        }
        // Per-bar smoothing: quick rise, softer fall.
        for (h, tg) in self.heights.iter_mut().zip(target.iter()) {
            let rate = if tg > h { 28.0 } else { 14.0 };
            let k = 1.0 - (-dt * rate).exp() as f64;
            *h += (tg - *h) * k;
        }

        // Window alpha / scale / tint.
        let mut scale = 1.0;
        match self.phase {
            Phase::Hidden => self.alpha = 0.0,
            Phase::Listening | Phase::Processing => {
                self.alpha = (self.alpha + dt / APPEAR_S).min(1.0);
                scale = 0.82 + 0.18 * ease_out(self.alpha);
                self.tint = (self.tint - dt * 6.0).max(0.0);
            }
            Phase::Leaving { rate } => {
                self.alpha = (self.alpha - dt * rate).max(0.0);
                scale = 0.9 + 0.1 * self.alpha;
                if self.alpha <= 0.0 {
                    self.phase = Phase::Hidden;
                }
            }
            Phase::Error { t: et } => {
                let et = et + dt;
                self.phase = Phase::Error { t: et };
                // Pop in (if hidden), flash red twice, hold, fade.
                if et < ERROR_HOLD_S {
                    self.alpha = (self.alpha + dt / APPEAR_S).min(1.0);
                    scale = 0.82 + 0.18 * ease_out(self.alpha);
                    let flash = if et < 0.45 { 0.75 + 0.25 * (et * 14.0).cos().abs() } else { 1.0 };
                    self.tint = flash;
                } else {
                    self.alpha = (self.alpha - dt / ERROR_FADE_S).max(0.0);
                    scale = 0.9 + 0.1 * self.alpha;
                    if self.alpha <= 0.0 {
                        self.phase = Phase::Hidden;
                    }
                }
            }
        }
        if self.phase == Phase::Hidden {
            self.alpha = 0.0;
            self.tint = 0.0;
            self.level = 0.0;
            self.target_level = 0.0;
        }
        Frame { visible: !self.is_hidden(), alpha: self.alpha, scale, tint: self.tint, heights: self.heights, opacity }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(a: &mut Animator, secs: f32) -> Frame {
        let mut f = a.tick(0.0);
        for _ in 0..(secs * 60.0) as usize {
            f = a.tick(1.0 / 60.0);
        }
        f
    }

    #[test]
    fn starts_hidden_and_listening_shows() {
        let mut a = Animator::new();
        assert!(!run(&mut a, 0.2).visible);
        assert!(a.event(&UiEvent::State(UiState::Listening)));
        let f = run(&mut a, 0.3);
        assert!(f.visible);
        assert!((f.alpha - 1.0).abs() < 1e-6);
        assert!((f.scale - 1.0).abs() < 1e-6);
    }

    #[test]
    fn level_raises_center_bars_more_than_edges() {
        let mut a = Animator::new();
        a.event(&UiEvent::State(UiState::Listening));
        let quiet = run(&mut a, 0.5);
        assert!(quiet.heights.iter().all(|&h| h < BAR_MIN_H + 1.5), "{:?}", quiet.heights);
        let mut peak_c = 0f64;
        let mut peak_e = 0f64;
        for _ in 0..60 {
            a.event(&UiEvent::Level(0.8));
            let f = run(&mut a, 1.0 / 30.0);
            peak_c = peak_c.max(f.heights[BARS / 2]);
            peak_e = peak_e.max(f.heights[0]);
            assert!(f.heights.iter().all(|&h| (BAR_MIN_H - 1e-9..=BAR_MAX_H + 1e-9).contains(&h)));
        }
        assert!(peak_c > BAR_MIN_H + 0.6 * (BAR_MAX_H - BAR_MIN_H), "{peak_c}");
        assert!(peak_c > peak_e * 1.4, "center {peak_c} edge {peak_e}");
    }

    #[test]
    fn levels_ignored_unless_listening() {
        let mut a = Animator::new();
        a.event(&UiEvent::Level(1.0));
        assert!(a.is_hidden());
        a.event(&UiEvent::State(UiState::Listening));
        a.event(&UiEvent::State(UiState::Processing));
        a.event(&UiEvent::Level(1.0));
        let f = run(&mut a, 1.0);
        assert!(f.heights.iter().all(|&h| h <= BAR_MIN_H + 3.6), "processing keeps bars low");
    }

    #[test]
    fn done_and_cancel_fade_to_hidden() {
        for (st, max_s) in [(UiState::Done, 0.3), (UiState::Cancelled, 0.15)] {
            let mut a = Animator::new();
            a.event(&UiEvent::State(UiState::Listening));
            run(&mut a, 0.3);
            assert!(a.event(&UiEvent::State(st)) || a.is_hidden());
            let f = run(&mut a, max_s);
            assert!(!f.visible, "{st:?}");
            assert!(a.is_hidden());
        }
        // Done while hidden stays hidden.
        let mut a = Animator::new();
        assert!(!a.event(&UiEvent::State(UiState::Done)));
    }

    #[test]
    fn error_flashes_red_then_hides() {
        let mut a = Animator::new();
        assert!(a.event(&UiEvent::Error("boom".into())));
        let f = run(&mut a, 0.3);
        assert!(f.visible && f.tint > 0.7 && f.alpha > 0.9);
        let f = run(&mut a, 1.0);
        assert!(!f.visible);
        assert_eq!(f.tint, 0.0);
        // Listening after an error clears the tint.
        a.event(&UiEvent::Error("x".into()));
        run(&mut a, 0.2);
        a.event(&UiEvent::State(UiState::Listening));
        let f = run(&mut a, 0.3);
        assert!(f.tint < 0.01 && f.visible);
    }

    #[test]
    fn envelope_center_weighted_symmetric() {
        assert!(envelope(BARS / 2) > 0.95);
        assert!(envelope(0) < 0.35);
        for i in 0..BARS {
            assert!((envelope(i) - envelope(BARS - 1 - i)).abs() < 1e-6);
        }
    }
}
