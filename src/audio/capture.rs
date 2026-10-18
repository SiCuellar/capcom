//! cpal microphone capture: [`CpalSource`] implements [`AudioSource`].
//!
//! Design:
//! - The cpal data callback only converts to f32 and copies into a [`Recorder`] behind a
//!   short `Mutex` (no allocation after warm-up, no resampling, no I/O).
//! - Downmix + resample to 16 kHz mono i16 happens in `stop()`, off the callback thread.
//! - `on_demand`: the stream is opened in `start()` and dropped in `stop()`/`cancel()`.
//! - `always_on`: the stream stays open; while idle the callback feeds a ring buffer of
//!   `preroll_ms`, which `start()` prepends to the clip.
//! - No VAD and no trimming: the clip contains every sample that was delivered.
//! - Events (first samples live, max length reached, stream error) are sent over an optional
//!   unbounded channel so the daemon can react without polling.

use super::level::{LevelMeter, LevelSink};
use super::{wav, AudioSource};
use crate::config::AudioConfig;
use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicMode {
    OnDemand,
    AlwaysOn,
}

impl MicMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on_demand" | "ondemand" => Ok(Self::OnDemand),
            "always_on" | "alwayson" => Ok(Self::AlwaysOn),
            other => bail!("unknown audio.mic_mode {other:?} (expected on_demand or always_on)"),
        }
    }
}

/// Events emitted by the capture layer. `gen` identifies the clip (see
/// [`AudioSource::generation`]) so stale events can be ignored.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureEvent {
    /// First samples of the clip have arrived: the mic is live. **Play the start sound now.**
    /// `after` is the time from `start()` being called to the first samples.
    Live { gen: u64, after: Duration },
    /// `audio.max_seconds` reached. The clip is capped; the daemon should stop and process it.
    MaxSecondsReached { gen: u64 },
    /// The stream failed (e.g. device unplugged). Samples captured so far are kept.
    StreamError { gen: u64, message: String },
}

/// Fixed-capacity ring buffer of f32 samples (keeps the most recent `cap`).
#[derive(Debug)]
pub struct PreRoll {
    buf: Vec<f32>,
    cap: usize,
    head: usize, // next write position
    len: usize,
}

impl PreRoll {
    pub fn new(cap: usize) -> Self {
        Self { buf: vec![0.0; cap], cap, head: 0, len: 0 }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
    pub fn push(&mut self, data: &[f32]) {
        if self.cap == 0 || data.is_empty() {
            return;
        }
        let data = if data.len() > self.cap { &data[data.len() - self.cap..] } else { data };
        let first = (self.cap - self.head).min(data.len());
        self.buf[self.head..self.head + first].copy_from_slice(&data[..first]);
        let rest = data.len() - first;
        if rest > 0 {
            self.buf[..rest].copy_from_slice(&data[first..]);
        }
        self.head = (self.head + data.len()) % self.cap;
        self.len = (self.len + data.len()).min(self.cap);
    }
    /// Append the contents, oldest first, to `out`.
    pub fn drain_into(&mut self, out: &mut Vec<f32>) {
        if self.len > 0 {
            let start = (self.head + self.cap - self.len) % self.cap;
            let first = (self.cap - start).min(self.len);
            out.extend_from_slice(&self.buf[start..start + first]);
            out.extend_from_slice(&self.buf[..self.len - first]);
        }
        self.clear();
    }
}

/// What a callback push produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PushOutcome {
    pub first_samples: bool,
    pub capped_now: bool,
}

/// Callback-side state: idle pre-roll ring + the clip being recorded.
#[derive(Debug)]
pub struct Recorder {
    ring: PreRoll,
    clip: Vec<f32>,
    recording: bool,
    got_samples: bool,
    capped: bool,
    max_samples: usize,
}

impl Recorder {
    /// `preroll_samples` / `max_samples` are interleaved sample counts (frames * channels).
    pub fn new(preroll_samples: usize, max_samples: usize) -> Self {
        Self { ring: PreRoll::new(preroll_samples), clip: Vec::new(), recording: false, got_samples: false, capped: false, max_samples }
    }
    /// Begin a clip; the pre-roll (if any) becomes its first samples.
    pub fn begin(&mut self, reserve: usize) {
        self.clip = Vec::with_capacity(reserve);
        self.ring.drain_into(&mut self.clip);
        self.recording = true;
        self.got_samples = !self.clip.is_empty();
        self.capped = false;
    }
    pub fn push(&mut self, data: &[f32]) -> PushOutcome {
        let mut out = PushOutcome::default();
        if !self.recording {
            self.ring.push(data);
            return out;
        }
        if !data.is_empty() && !self.got_samples {
            self.got_samples = true;
            out.first_samples = true;
        }
        if self.capped {
            return out;
        }
        let room = self.max_samples.saturating_sub(self.clip.len());
        if data.len() >= room {
            self.clip.extend_from_slice(&data[..room]);
            self.capped = true;
            out.capped_now = true;
        } else {
            self.clip.extend_from_slice(data);
        }
        out
    }
    pub fn finish(&mut self) -> Vec<f32> {
        self.recording = false;
        self.ring.clear();
        std::mem::take(&mut self.clip)
    }
    pub fn got_samples(&self) -> bool {
        self.got_samples
    }
    pub fn is_recording(&self) -> bool {
        self.recording
    }
}

struct Shared {
    rec: Mutex<Recorder>,
    dead: AtomicBool,
    gen: std::sync::atomic::AtomicU64,
    t0: Mutex<Instant>,
    first_after: Mutex<Option<Duration>>,
}

impl Shared {
    fn rec(&self) -> MutexGuard<'_, Recorder> {
        self.rec.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Measurements of the most recent clip.
#[derive(Debug, Clone, Default)]
pub struct CaptureStats {
    pub device: String,
    pub native_rate: u32,
    pub native_channels: u16,
    pub native_format: String,
    pub mic_mode: String,
    /// `start()` call until `stream.play()` returned (device open cost). 0 when already open.
    pub open_ms: u64,
    /// `start()` call until the first samples arrived (`None` = never). In `always_on` mode
    /// with a warm stream this is 0 because the pre-roll supplies the first samples.
    pub first_sample_ms: Option<u64>,
    pub captured_ms: u64,
    pub peak: f32,
    pub capped: bool,
}

struct Open {
    stream: cpal::Stream,
    device: String,
    rate: u32,
    channels: u16,
    format: String,
}

pub struct CpalSource {
    cfg: AudioConfig,
    mode: MicMode,
    events: Option<UnboundedSender<CaptureEvent>>,
    shared: Option<Arc<Shared>>,
    open: Option<Open>,
    open_ms: u64,
    stats: CaptureStats,
    /// Level callback; shared with the stream callback so it survives stream reopen.
    level_sink: Arc<Mutex<Option<LevelSink>>>,
}

fn find_device(host: &cpal::Host, want: &str) -> Result<cpal::Device> {
    let want = want.trim();
    if want.is_empty() || want.eq_ignore_ascii_case("default") {
        return host.default_input_device().ok_or_else(|| anyhow!("no default input device"));
    }
    let needle = want.to_lowercase();
    let mut names = vec![];
    for d in host.input_devices().context("listing input devices")? {
        let name = device_name(&d);
        if name.to_lowercase().contains(&needle) {
            return Ok(d);
        }
        names.push(name);
    }
    bail!("no input device matching {want:?}; available: {}", names.join(", "))
}

fn device_name(d: &cpal::Device) -> String {
    d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| "<unknown>".into())
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    pub rate: u32,
    pub channels: u16,
    pub format: String,
}

/// Enumerate input devices with their default configs.
pub fn list_input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default = host.default_input_device().map(|d| device_name(&d));
    let mut out = vec![];
    for d in host.input_devices().context("listing input devices")? {
        let name = device_name(&d);
        let (rate, channels, format) = match d.default_input_config() {
            Ok(c) => (c.sample_rate(), c.channels(), c.sample_format().to_string()),
            Err(_) => (0, 0, "?".into()),
        };
        out.push(DeviceInfo { is_default: default.as_deref() == Some(name.as_str()), name, rate, channels, format });
    }
    Ok(out)
}

impl CpalSource {
    /// `always_on` mode opens the stream immediately (failure is logged and retried on `start`).
    pub fn new(cfg: &AudioConfig, events: Option<UnboundedSender<CaptureEvent>>) -> Result<Self> {
        let mode = MicMode::parse(&cfg.mic_mode)?;
        let mut s = Self { cfg: cfg.clone(), mode, events, shared: None, open: None, open_ms: 0, stats: CaptureStats::default(), level_sink: Arc::new(Mutex::new(None)) };
        if mode == MicMode::AlwaysOn {
            if let Err(e) = s.ensure_open() {
                tracing::warn!("always_on: cannot open microphone yet: {e:#}");
            }
        }
        Ok(s)
    }

    pub fn stats(&self) -> &CaptureStats {
        &self.stats
    }

    fn ensure_open(&mut self) -> Result<()> {
        if let (Some(_), Some(sh)) = (&self.open, &self.shared) {
            if !sh.dead.load(Ordering::Relaxed) {
                return Ok(());
            }
            tracing::warn!("microphone stream died; reopening");
        }
        self.open = None;
        let host = cpal::default_host();
        let device = find_device(&host, &self.cfg.device)?;
        let name = device_name(&device);
        let conf = device.default_input_config().with_context(|| format!("no input config for {name}"))?;
        let (rate, channels, format) = (conf.sample_rate(), conf.channels(), conf.sample_format());
        let ch = channels as usize;
        let preroll = if self.mode == MicMode::AlwaysOn { (rate as u64 * self.cfg.preroll_ms / 1000) as usize * ch } else { 0 };
        let max = (rate as u64 * self.cfg.max_seconds) as usize * ch;
        let shared = Arc::new(Shared {
            rec: Mutex::new(Recorder::new(preroll, max)),
            dead: AtomicBool::new(false),
            gen: std::sync::atomic::AtomicU64::new(self.shared.as_ref().map(|s| s.gen.load(Ordering::Relaxed)).unwrap_or(0)),
            t0: Mutex::new(Instant::now()),
            first_after: Mutex::new(None),
        });
        let sc = conf.config();
        let meter = LevelMeter::new(rate, channels);
        let ls = &self.level_sink;
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, sc, &shared, &self.events, meter, ls),
            SampleFormat::I16 => build::<i16>(&device, sc, &shared, &self.events, meter, ls),
            SampleFormat::I32 => build::<i32>(&device, sc, &shared, &self.events, meter, ls),
            SampleFormat::U16 => build::<u16>(&device, sc, &shared, &self.events, meter, ls),
            SampleFormat::I8 => build::<i8>(&device, sc, &shared, &self.events, meter, ls),
            other => bail!("unsupported sample format {other} on {name}"),
        }
        .map_err(|e| anyhow!("opening {name}: {e}"))?;
        if self.mode == MicMode::AlwaysOn {
            stream.play().map_err(|e| anyhow!("starting {name}: {e}"))?;
        }
        self.shared = Some(shared);
        self.open = Some(Open { stream, device: name, rate, channels, format: format.to_string() });
        Ok(())
    }

    fn send(&self, ev: CaptureEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(ev);
        }
    }

    fn end_clip(&mut self) -> Vec<f32> {
        let Some(sh) = self.shared.clone() else { return vec![] };
        if self.mode == MicMode::OnDemand {
            self.open = None; // dropping the stream closes the device
        }
        let mut rec = sh.rec();
        rec.finish()
    }
}

/// Wrap the typed callback. Allocation-free after the first buffer; does only a short lock.
/// While recording, also meters the input level (see [`super::level`]) and hands it to the
/// level sink (`try_lock`, never blocks the audio thread).
fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: &Arc<Shared>,
    events: &Option<UnboundedSender<CaptureEvent>>,
    mut meter: LevelMeter,
    level_sink: &Arc<Mutex<Option<LevelSink>>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let sh = shared.clone();
    let tx = events.clone();
    let mut scratch: Vec<f32> = Vec::with_capacity(8192);
    let sh_err = shared.clone();
    let tx_err = events.clone();
    let level_sink = level_sink.clone();
    let mut was_recording = false;
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            scratch.clear();
            scratch.extend(data.iter().map(|&s| f32::from_sample(s)));
            let (out, recording) = {
                let mut rec = sh.rec();
                let out = rec.push(&scratch);
                (out, rec.is_recording())
            };
            if recording {
                if !was_recording {
                    meter.reset();
                }
                if let Some(level) = meter.push(&scratch) {
                    if let Ok(g) = level_sink.try_lock() {
                        if let Some(f) = g.as_ref() {
                            f(level);
                        }
                    }
                }
            }
            was_recording = recording;
            if out.first_samples || out.capped_now {
                let gen = sh.gen.load(Ordering::Relaxed);
                if out.first_samples {
                    let after = sh.t0.lock().map(|t| t.elapsed()).unwrap_or_default();
                    *sh.first_after.lock().unwrap_or_else(|e| e.into_inner()) = Some(after);
                    if let Some(tx) = &tx {
                        let _ = tx.send(CaptureEvent::Live { gen, after });
                    }
                }
                if out.capped_now {
                    if let Some(tx) = &tx {
                        let _ = tx.send(CaptureEvent::MaxSecondsReached { gen });
                    }
                }
            }
        },
        move |err| {
            sh_err.dead.store(true, Ordering::Relaxed);
            tracing::error!("audio stream error: {err}");
            if let Some(tx) = &tx_err {
                let gen = sh_err.gen.load(Ordering::Relaxed);
                let _ = tx.send(CaptureEvent::StreamError { gen, message: err.to_string() });
            }
        },
        None,
    )
}

impl AudioSource for CpalSource {
    fn start(&mut self) -> Result<()> {
        let t0 = Instant::now();
        let was_open = self.open.is_some() && self.shared.as_ref().is_some_and(|s| !s.dead.load(Ordering::Relaxed));
        self.ensure_open()?;
        let (sh, open) = (self.shared.clone().unwrap(), self.open.as_ref().unwrap());
        let (rate, ch) = (open.rate as usize, open.channels as usize);
        let gen = sh.gen.fetch_add(1, Ordering::Relaxed) + 1;
        *sh.t0.lock().unwrap_or_else(|e| e.into_inner()) = t0;
        *sh.first_after.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // Begin the clip *before* play() so not a single early sample is missed.
        let pre_filled = {
            let mut rec = sh.rec();
            rec.begin(rate * ch * 20);
            rec.got_samples()
        };
        if self.mode == MicMode::OnDemand {
            if let Err(e) = open.stream.play() {
                let _ = sh.rec().finish();
                self.open = None;
                let msg = e.to_string();
                if matches!(e.kind(), cpal::ErrorKind::PermissionDenied) {
                    bail!("microphone access denied ({msg}); grant it in System Settings > Privacy & Security > Microphone");
                }
                bail!("starting microphone stream: {msg}");
            }
        }
        self.open_ms = t0.elapsed().as_millis() as u64;
        self.stats = CaptureStats {
            device: open.device.clone(),
            native_rate: open.rate,
            native_channels: open.channels,
            native_format: open.format.clone(),
            mic_mode: if self.mode == MicMode::AlwaysOn { "always_on" } else { "on_demand" }.into(),
            open_ms: if was_open { 0 } else { self.open_ms },
            ..Default::default()
        };
        if pre_filled {
            // Warm stream with pre-roll: audio is already flowing, report live right away.
            *sh.first_after.lock().unwrap_or_else(|e| e.into_inner()) = Some(Duration::ZERO);
            self.send(CaptureEvent::Live { gen, after: Duration::ZERO });
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<Vec<i16>> {
        let Some(sh) = self.shared.clone() else { bail!("not recording") };
        let first = *sh.first_after.lock().unwrap_or_else(|e| e.into_inner());
        let capped = sh.rec().capped;
        let raw = self.end_clip();
        let peak = raw.iter().fold(0f32, |m, s| m.max(s.abs()));
        let (rate, ch) = (self.stats.native_rate, self.stats.native_channels);
        let out = wav::to_16k_mono(&raw, rate, ch);
        self.stats.first_sample_ms = first.map(|d| d.as_millis() as u64);
        self.stats.peak = peak;
        self.stats.captured_ms = super::clip_ms(&out);
        self.stats.capped = capped;
        if raw.is_empty() {
            bail!("no audio was captured from {:?} (microphone permission denied or device stalled?)", self.stats.device);
        }
        Ok(out)
    }

    fn cancel(&mut self) {
        let _ = self.end_clip();
    }

    fn first_sample_received(&self) -> bool {
        self.shared.as_ref().is_some_and(|s| s.rec().got_samples())
    }

    fn generation(&self) -> u64 {
        self.shared.as_ref().map(|s| s.gen.load(Ordering::Relaxed)).unwrap_or(0)
    }

    fn emits_live_event(&self) -> bool {
        self.events.is_some()
    }

    fn set_level_sink(&mut self, sink: Option<LevelSink>) {
        *self.level_sink.lock().unwrap_or_else(|e| e.into_inner()) = sink;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(a: usize, b: usize) -> Vec<f32> {
        (a..b).map(|i| i as f32).collect()
    }

    #[test]
    fn preroll_partial_then_wrap() {
        let mut r = PreRoll::new(5);
        r.push(&ramp(0, 3));
        assert_eq!(r.len(), 3);
        r.push(&ramp(3, 7)); // wraps; keeps last 5 = 2..7
        let mut out = vec![];
        r.drain_into(&mut out);
        assert_eq!(out, ramp(2, 7));
        assert!(r.is_empty());
    }

    #[test]
    fn preroll_oversized_push_and_many_small() {
        let mut r = PreRoll::new(4);
        r.push(&ramp(0, 100));
        let mut out = vec![];
        r.drain_into(&mut out);
        assert_eq!(out, ramp(96, 100));
        for i in 0..23 {
            r.push(&[i as f32]);
        }
        r.drain_into(&mut out);
        assert_eq!(&out[4..], &ramp(19, 23)[..]);
        let mut z = PreRoll::new(0);
        z.push(&[1.0]);
        assert!(z.is_empty());
    }

    #[test]
    fn recorder_prepends_preroll_in_order() {
        let mut rec = Recorder::new(4, 1000);
        rec.push(&ramp(0, 6)); // idle: ring keeps 2..6
        rec.begin(0);
        assert!(rec.got_samples());
        let o = rec.push(&ramp(6, 8));
        assert!(!o.first_samples, "pre-roll already counts as live");
        assert_eq!(rec.finish(), ramp(2, 8));
        // idle again afterwards; ring restarts empty
        rec.push(&ramp(10, 12));
        rec.begin(0);
        assert_eq!(rec.finish(), ramp(10, 12));
    }

    #[test]
    fn recorder_first_samples_without_preroll() {
        let mut rec = Recorder::new(0, 1000);
        rec.begin(0);
        assert!(!rec.got_samples());
        assert!(rec.push(&[0.1]).first_samples);
        assert!(!rec.push(&[0.2]).first_samples);
    }

    #[test]
    fn max_seconds_cap() {
        // 2 "seconds" of 4 samples each = 8-sample cap.
        let mut rec = Recorder::new(0, 8);
        rec.begin(0);
        let o = rec.push(&ramp(0, 6));
        assert!(!o.capped_now);
        let o = rec.push(&ramp(6, 12));
        assert!(o.capped_now);
        let o = rec.push(&ramp(12, 16));
        assert!(!o.capped_now, "cap fires once");
        assert_eq!(rec.finish(), ramp(0, 8));
    }

    #[test]
    fn mic_mode_parse() {
        assert_eq!(MicMode::parse("on_demand").unwrap(), MicMode::OnDemand);
        assert_eq!(MicMode::parse("Always_On").unwrap(), MicMode::AlwaysOn);
        assert!(MicMode::parse("sometimes").is_err());
    }
}
