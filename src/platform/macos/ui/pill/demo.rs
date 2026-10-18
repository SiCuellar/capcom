//! `capcom pill-demo`: standalone harness that runs its own NSApplication (accessory policy, no
//! Dock icon) on the main thread and drives a [`Pill`] either from a scripted sequence or from the
//! real microphone (CpalSource + level metering).

use super::Pill;
use crate::audio::capture::CpalSource;
use crate::audio::AudioSource;
use crate::config::{AudioConfig, PillPosition};
use crate::ui::{UiComponent, UiEvent, UiState};
use anyhow::{Context, Result};
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use std::cell::RefCell;
use std::io::Write;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoMode {
    /// Scripted: listening (fake speech) -> processing -> done -> error flash -> exit.
    Synthetic,
    /// Real microphone levels for `seconds`, then processing -> done -> exit.
    Mic,
}

/// Speech-like fake level: syllables (~4/s) grouped into phrases with short pauses.
pub fn fake_speech(t: f32) -> f32 {
    let phrase = (t % 1.9) / 1.9;
    if phrase > 0.82 {
        return 0.02; // pause between phrases
    }
    let syl = (t * std::f32::consts::TAU * 2.1).sin().max(0.0).powf(0.6);
    let accent = 0.55 + 0.35 * (t * 1.3).sin() + 0.1 * (t * 7.7).sin();
    (0.12 + 0.88 * syl * accent).clamp(0.0, 1.0)
}

struct Driver {
    pill: Pill,
    t0: Instant,
    mode: DemoMode,
    listen_s: f64,
    step: u8,
    mic: Option<CpalSource>,
    level: Arc<AtomicU32>,
    last_print: f64,
    /// `CAPCOM_PILL_SNAPSHOTS=<dir>`: (time, name) PNG snapshots still to take.
    snaps: Vec<(f64, &'static str)>,
    snap_dir: Option<std::path::PathBuf>,
}

impl Driver {
    fn send(&mut self, ev: UiEvent) {
        if !matches!(ev, UiEvent::Level(_)) {
            println!("[{:6.2}s] {ev:?}", self.t0.elapsed().as_secs_f64());
        }
        self.pill.handle(&ev);
    }

    /// Called at 30 Hz on the main thread. Returns false when finished.
    fn tick(&mut self) -> bool {
        let t = self.t0.elapsed().as_secs_f64();
        let l = self.listen_s;
        if let Some(dir) = &self.snap_dir {
            while let Some(&(at, name)) = self.snaps.first() {
                if t < at {
                    break;
                }
                self.snaps.remove(0);
                let p = dir.join(format!("pill-{name}.png"));
                match self.pill.snapshot_png(&p) {
                    Ok(()) => println!("[{t:6.2}s] snapshot {}", p.display()),
                    Err(e) => eprintln!("snapshot failed: {e:#}"),
                }
            }
        }
        if self.step == 0 {
            self.send(UiEvent::State(UiState::Listening));
            println!("[{t:6.2}s] placed: {}", self.pill.last_placement());
            self.step = 1;
        }
        if self.step == 1 {
            if t < l {
                let lv = match self.mode {
                    DemoMode::Synthetic => fake_speech(t as f32),
                    DemoMode::Mic => f32::from_bits(self.level.load(Ordering::Relaxed)),
                };
                self.pill.handle(&UiEvent::Level(lv));
                if t - self.last_print >= 0.1 {
                    self.last_print = t;
                    let n = (lv * 40.0).round() as usize;
                    println!("[{t:6.2}s] level {lv:.3} {}", "#".repeat(n));
                    let _ = std::io::stdout().flush();
                }
                return true;
            }
            if let Some(mut m) = self.mic.take() {
                m.set_level_sink(None);
                m.cancel();
            }
            self.send(UiEvent::State(UiState::Processing));
            self.step = 2;
        }
        if self.step == 2 && t >= l + 1.5 {
            self.send(UiEvent::State(UiState::Done));
            self.step = 3;
        }
        if self.step == 3 && t >= l + 2.5 {
            if self.mode == DemoMode::Mic {
                return false;
            }
            self.send(UiEvent::Error("demo: ASR endpoint unreachable".into()));
            self.step = 4;
        }
        if self.step == 4 && t >= l + 4.0 {
            return false;
        }
        true
    }
}

/// `capcom anchor-probe`: wait `delay_s` (switch to the target app meanwhile), then run the same
/// anchor lookup + placement the pill uses and print it. Repeats `count` times (the first lookup
/// may include switching on a Chromium/Electron AX tree; later ones show the warm cost).
pub fn probe(delay_s: f64, count: u32, position: PillPosition, pid: Option<i32>) -> Result<()> {
    let mtm = MainThreadMarker::new().context("anchor-probe must run on the main thread")?;
    let _app = NSApplication::sharedApplication(mtm);
    eprintln!("anchor-probe: focus the target text field now; probing in {delay_s:.1} s ...");
    // Spin the run loop (not sleep) so AppKit's frontmost-app tracking stays fresh.
    let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(delay_s.max(0.0));
    NSRunLoop::currentRunLoop().runUntilDate(&until);
    let (_, geoms) = super::screen_geoms(mtm);
    for (i, g) in geoms.iter().enumerate() {
        println!("screen {i}: frame={:?} visible={:?}", g.frame, g.visible);
    }
    for i in 0..count.max(1) {
        let (_, desc) = super::compute_placement(&geoms, position, pid);
        println!("probe {}: {desc}", i + 1);
    }
    Ok(())
}

/// Run the demo. Blocks in `NSApplication::run` and exits the process when done.
/// Must be called on the main thread.
pub fn run(mode: DemoMode, seconds: Option<f64>, audio: &AudioConfig, position: PillPosition) -> Result<()> {
    let mtm = MainThreadMarker::new().context("pill-demo must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let level = Arc::new(AtomicU32::new(0));
    let mic = if mode == DemoMode::Mic {
        let mut cfg = audio.clone();
        cfg.mic_mode = "on_demand".into();
        let mut src = CpalSource::new(&cfg, None)?;
        let lv = level.clone();
        src.set_level_sink(Some(Arc::new(move |l: f32| lv.store(l.to_bits(), Ordering::Relaxed))));
        src.start()?;
        println!("mic: {} ({} Hz, {} ch)", src.stats().device, src.stats().native_rate, src.stats().native_channels);
        Some(src)
    } else {
        None
    };
    let listen_s = seconds.unwrap_or(if mode == DemoMode::Mic { 8.0 } else { 4.0 }).max(0.5);

    let driver = RefCell::new(Driver {
        pill: Pill::new(mtm, position),
        t0: Instant::now(),
        mode,
        listen_s,
        step: 0,
        mic,
        level,
        last_print: -1.0,
        snaps: {
            let l = listen_s;
            let mut v = vec![(0.08, "appear"), (l * 0.3, "listen-1"), (l * 0.55, "listen-2"), (l * 0.8, "listen-3")];
            v.extend([(l + 0.5, "processing-1"), (l + 1.0, "processing-2"), (l + 1.62, "done-fade")]);
            if mode == DemoMode::Synthetic {
                v.extend([(l + 2.55, "error-1"), (l + 2.9, "error-2"), (l + 3.55, "error-fade")]);
            }
            v
        },
        snap_dir: std::env::var_os("CAPCOM_PILL_SNAPSHOTS").map(std::path::PathBuf::from),
    });
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
        let keep = driver.try_borrow_mut().map(|mut d| d.tick()).unwrap_or(true);
        if !keep {
            println!("pill-demo finished");
            std::process::exit(0);
        }
    });
    // SAFETY: scheduled on the main run loop; fires on the main thread only.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1.0 / 30.0, true, &block) };
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    app.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn fake_speech_in_range_with_pauses() {
        let v: Vec<f32> = (0..400).map(|i| super::fake_speech(i as f32 / 100.0)).collect();
        assert!(v.iter().all(|x| (0.0..=1.0).contains(x)));
        assert!(v.iter().any(|&x| x > 0.7));
        assert!(v.iter().filter(|&&x| x < 0.05).count() > 20);
    }
}
