//! The long-running daemon: owns the state machine, history, platform hooks and the IPC server.

use crate::audio::capture::{CaptureEvent, CpalSource};
use crate::audio::{self, wav, AudioSource};
use crate::config::Config;
use crate::history::{Entry, History};
use crate::ipc::{self, Request, Response};
use crate::pipeline::{self, Clients, Outcome};
use crate::platform::{FeedbackEvent, Platform};
use crate::state::{step, Action, Event, State};
use crate::ui::{NullSink, UiEvent, UiSink, UiState};
use anyhow::Result;
use serde_json::json;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Inner {
    state: State,
    audio: Option<Box<dyn AudioSource>>,
    last: Option<Outcome>,
}

pub struct Daemon {
    cfg: Config,
    clients: Clients,
    platform: Arc<Platform>,
    history: History,
    inner: Mutex<Inner>,
    ui: Arc<dyn UiSink>,
    /// Bumped on every UI emission; a delayed "settle back to Idle" only fires if nothing newer
    /// was emitted in between.
    ui_gen: Arc<AtomicU64>,
}

/// How long a terminal UI state stays visible before the daemon emits `State(Idle)`.
const SETTLE_DONE: Duration = Duration::from_millis(700);
const SETTLE_CANCELLED: Duration = Duration::from_millis(400);
const SETTLE_ERROR: Duration = Duration::from_millis(2500);

impl Daemon {
    pub fn new(cfg: Config, clients: Clients, platform: Platform, audio: Option<Box<dyn AudioSource>>) -> Self {
        let history = History::open_default(cfg.history.keep);
        Self {
            cfg,
            clients,
            platform: Arc::new(platform),
            history,
            inner: Mutex::new(Inner { state: State::Idle, audio, last: None }),
            ui: Arc::new(NullSink),
            ui_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Route UI events (menu-bar icon, pill) to `sink` instead of dropping them.
    pub fn with_ui(mut self, sink: Arc<dyn UiSink>) -> Self {
        self.ui = sink;
        self
    }

    fn state(&self) -> State {
        self.inner.lock().unwrap().state
    }

    /// Apply an event under the lock, returning the action.
    fn apply(&self, ev: Event) -> (State, Action) {
        let mut g = self.inner.lock().unwrap();
        let (s, a) = step(g.state, ev, self.cfg.audio.min_clip_ms);
        g.state = s;
        (s, a)
    }

    /// Sound/notification feedback. Every feedback hook sits exactly at a user-visible
    /// transition, so this is also where the UI is told (see `ui_for_feedback`).
    fn fb(&self, ev: FeedbackEvent) {
        self.emit_ui_for(&ev);
        self.platform.feedback.event(ev);
    }

    /// Map a feedback event to the UI contract and emit it. Terminal events (Done, Cancelled,
    /// Error) are followed by `State(Idle)` after a short settle delay, unless something newer
    /// was emitted meanwhile.
    fn emit_ui_for(&self, ev: &FeedbackEvent) {
        let Some((ui, settle)) = ui_for_feedback(ev) else { return };
        let gen = self.ui_gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.ui.send(ui);
        let Some(delay) = settle else { return };
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        let (sink, gens) = (self.ui.clone(), self.ui_gen.clone());
        rt.spawn(async move {
            tokio::time::sleep(delay).await;
            if gens.load(Ordering::SeqCst) == gen {
                sink.send(UiEvent::State(UiState::Idle));
            }
        });
    }

    pub async fn handle(self: &Arc<Self>, req: Request) -> Response {
        match req {
            Request::Start => self.cmd_start(),
            Request::Stop => self.cmd_stop(),
            Request::Toggle => match self.state() {
                State::Recording => self.cmd_stop(),
                _ => self.cmd_start(),
            },
            Request::Cancel => self.cmd_cancel(),
            Request::Status => {
                let g = self.inner.lock().unwrap();
                let last = g.last.as_ref().map(|o| json!({
                    "final_text": o.final_text, "asr_ms": o.timings.asr_ms,
                    "cleanup_ms": o.timings.cleanup_ms, "cleanup_used": o.cleanup_used, "error": o.error,
                }));
                Response::ok_data(g.state, json!({ "has_audio_source": g.audio.is_some(), "last": last }))
            }
            Request::PasteLast => self.cmd_paste_last().await,
            Request::History { n } => match self.history.read_last(n) {
                Ok(v) => Response::ok_data(self.state(), json!(v)),
                Err(e) => Response::err(e.to_string()),
            },
            Request::DebugTranscribe { path } => self.cmd_debug_transcribe(path).await,
        }
    }

    fn cmd_start(self: &Arc<Self>) -> Response {
        if let Some(p) = self.cfg.setup_problem() {
            if self.state() == State::Idle {
                self.fb(FeedbackEvent::Error(p.clone()));
                return Response::err(p);
            }
        }
        {
            let g = self.inner.lock().unwrap();
            if g.audio.is_none() && g.state == State::Idle {
                return Response::err("no audio source on this platform yet");
            }
        }
        let (s, a) = self.apply(Event::Start);
        match a {
            Action::BeginCapture => {
                let res = self.inner.lock().unwrap().audio.as_mut().map(|a| (a.start(), a.emits_live_event()));
                let live_deferred = match res {
                    Some((Err(e), _)) => {
                        self.apply(Event::Cancel);
                        self.fb(FeedbackEvent::Error(format!("cannot start recording: {e}")));
                        return Response::err(format!("cannot start recording: {e}"));
                    }
                    Some((Ok(()), deferred)) => deferred,
                    None => false,
                };
                // FEEDBACK HOOK ("capture is live"): the mic is already buffering at this point
                // (start() returned), but the start sound must wait for the first samples.
                // When the audio source emits `CaptureEvent::Live` (CpalSource does), the
                // RecordingStarted feedback is fired from `on_capture_event`, i.e. only after
                // capture is verifiably live. Sources without events fire it immediately.
                if live_deferred {
                    self.spawn_live_watchdog();
                } else {
                    self.fb(FeedbackEvent::RecordingStarted);
                }
                Response::ok(s)
            }
            Action::Busy => {
                self.fb(FeedbackEvent::Busy);
                Response::err("busy")
            }
            _ => Response::ok(s),
        }
    }

    /// If no samples arrive within 2 s (permission denied, stalled device), abort loudly.
    fn spawn_live_watchdog(self: &Arc<Self>) {
        let me = self.clone();
        let gen = self.audio_generation();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let stalled = {
                let g = me.inner.lock().unwrap();
                g.state == State::Recording
                    && g.audio.as_ref().is_some_and(|a| a.generation() == gen && !a.first_sample_received())
            };
            if stalled {
                me.cmd_cancel();
                me.fb(FeedbackEvent::Error("no audio from the microphone (permission denied or device stalled)".into()));
            }
        });
    }

    fn audio_generation(&self) -> u64 {
        self.inner.lock().unwrap().audio.as_ref().map(|a| a.generation()).unwrap_or(0)
    }

    /// Consume capture events (live / max length / stream error) from the audio source.
    pub fn spawn_capture_events(self: &Arc<Self>, mut rx: tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>) {
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                me.on_capture_event(ev);
            }
        });
    }

    fn on_capture_event(self: &Arc<Self>, ev: CaptureEvent) {
        let current = self.state() == State::Recording;
        match ev {
            CaptureEvent::Live { gen, after } if current && gen == self.audio_generation() => {
                tracing::debug!("capture live after {} ms", after.as_millis());
                self.fb(FeedbackEvent::RecordingStarted);
            }
            CaptureEvent::MaxSecondsReached { gen } if current && gen == self.audio_generation() => {
                // Never silently drop: process what we have and tell the user why it ended.
                self.fb(FeedbackEvent::Notify(format!(
                    "Recording stopped at the {} s limit; transcribing what was captured",
                    self.cfg.audio.max_seconds
                )));
                let _ = self.cmd_stop();
            }
            CaptureEvent::StreamError { gen, message } if current && gen == self.audio_generation() => {
                self.fb(FeedbackEvent::Error(format!("microphone error, using audio captured so far: {message}")));
                let _ = self.cmd_stop();
            }
            _ => {}
        }
    }

    fn cmd_stop(self: &Arc<Self>) -> Response {
        if self.state() != State::Recording {
            return Response::err("not recording");
        }
        let samples = {
            let mut g = self.inner.lock().unwrap();
            match g.audio.as_mut().map(|a| a.stop()) {
                Some(Ok(s)) => s,
                Some(Err(e)) => {
                    g.state = State::Idle;
                    drop(g);
                    self.fb(FeedbackEvent::Error(format!("capture failed: {e}")));
                    return Response::err(format!("capture failed: {e}"));
                }
                None => vec![],
            }
        };
        let (s, a) = self.apply(Event::Stop { clip_ms: audio::clip_ms(&samples) });
        match a {
            Action::RunPipeline => {
                self.fb(FeedbackEvent::RecordingStopped);
                match wav::encode_wav_16k_mono(&samples) {
                    Ok(bytes) => {
                        let me = self.clone();
                        tokio::spawn(async move {
                            me.run_pipeline(bytes).await;
                        });
                        Response::ok(s)
                    }
                    Err(e) => {
                        self.apply(Event::AsrFailed);
                        Response::err(format!("wav encode failed: {e}"))
                    }
                }
            }
            Action::DiscardTooShort => {
                self.fb(FeedbackEvent::Discarded);
                Response::ok(s)
            }
            _ => Response::ok(s),
        }
    }

    fn cmd_cancel(self: &Arc<Self>) -> Response {
        let (s, a) = self.apply(Event::Cancel);
        if a == Action::DiscardCancelled {
            if let Some(au) = self.inner.lock().unwrap().audio.as_mut() {
                au.cancel();
            }
            self.fb(FeedbackEvent::Discarded);
        }
        Response::ok(s)
    }

    async fn cmd_paste_last(self: &Arc<Self>) -> Response {
        match self.history.last() {
            Ok(Some(e)) if !e.final_text.is_empty() => match self.paste(e.final_text).await {
                Ok(()) => Response::ok(self.state()),
                Err(e) => Response::err(format!("paste failed: {e}")),
            },
            Ok(_) => Response::err("history is empty"),
            Err(e) => Response::err(e.to_string()),
        }
    }

    async fn cmd_debug_transcribe(self: &Arc<Self>, path: String) -> Response {
        if self.state() != State::Idle {
            return Response::err("busy");
        }
        let bytes = match std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| wav::read_wav_16k_mono(&b)) {
            Ok(s) => match wav::encode_wav_16k_mono(&s) {
                Ok(b) => b,
                Err(e) => return Response::err(e.to_string()),
            },
            Err(e) => return Response::err(format!("{path}: {e}")),
        };
        // Stand in for a recording: Idle -> Recording -> Transcribing via the real transitions.
        self.apply(Event::Start);
        self.apply(Event::Stop { clip_ms: u64::MAX });
        self.ui_gen.fetch_add(1, Ordering::SeqCst);
        self.ui.send(UiEvent::State(UiState::Processing));
        let o = self.run_pipeline(bytes).await;
        Response::ok_data(self.state(), serde_json::to_value(&o).unwrap_or_default())
    }

    async fn paste(&self, text: String) -> Result<()> {
        let p = self.platform.clone();
        tokio::task::spawn_blocking(move || p.paster.paste(&text)).await?
    }

    /// Transcribing -> (Cleaning) -> Pasting -> Idle. History is written *before* pasting.
    async fn run_pipeline(self: &Arc<Self>, wav: Vec<u8>) -> Outcome {
        let o = pipeline::process(wav, &self.cfg, &self.clients).await;
        let asr_failed = o.raw.is_empty() && o.error.is_some();
        let skipped = !o.cleanup_used && o.error.is_none();

        let (_, action) = if asr_failed {
            self.apply(Event::AsrFailed)
        } else if skipped {
            self.apply(Event::AsrDoneSkipCleanup)
        } else {
            self.apply(Event::AsrDone);
            self.apply(if o.cleanup_used { Event::CleanupDone } else { Event::CleanupFailed })
        };

        if self.cfg.history.enabled && !o.final_text.is_empty() {
            if let Err(e) = self.history.append(&Entry::from_outcome(&o)) {
                tracing::warn!("history append failed: {e}");
            }
        }
        self.inner.lock().unwrap().last = Some(o.clone());

        match action {
            Action::NotifyError => {
                self.fb(FeedbackEvent::Error(o.error.clone().unwrap_or_default()));
            }
            Action::Paste | Action::PasteRawAndNotify => {
                if o.final_text.is_empty() {
                    self.fb(FeedbackEvent::Discarded);
                } else if let Err(e) = self.paste(o.final_text.clone()).await {
                    self.fb(FeedbackEvent::Error(format!("paste failed: {e}")));
                } else if action == Action::PasteRawAndNotify {
                    self.fb(FeedbackEvent::Error(format!("cleanup failed, pasted raw text: {}", o.error.clone().unwrap_or_default())));
                } else {
                    self.fb(FeedbackEvent::Done);
                }
                self.apply(Event::PasteDone);
            }
            _ => {}
        }
        o
    }
}

/// Pure mapping from a feedback hook to (UI event, settle-to-Idle delay).
fn ui_for_feedback(ev: &FeedbackEvent) -> Option<(UiEvent, Option<Duration>)> {
    Some(match ev {
        // Fired only once capture is verifiably live (see the FEEDBACK HOOK in `cmd_start`).
        FeedbackEvent::RecordingStarted => (UiEvent::State(UiState::Listening), None),
        FeedbackEvent::RecordingStopped => (UiEvent::State(UiState::Processing), None),
        FeedbackEvent::Discarded => (UiEvent::State(UiState::Cancelled), Some(SETTLE_CANCELLED)),
        FeedbackEvent::Done => (UiEvent::State(UiState::Done), Some(SETTLE_DONE)),
        FeedbackEvent::Error(m) => (UiEvent::Error(m.clone()), Some(SETTLE_ERROR)),
        // "busy" leaves the current cycle untouched; notifications are not state.
        FeedbackEvent::Busy | FeedbackEvent::Notify(_) => return None,
    })
}

/// Run the daemon until SIGINT/SIGTERM (headless: UI events are dropped).
pub async fn run(cfg: Config) -> Result<()> {
    run_with(cfg, Arc::new(NullSink), std::future::pending()).await
}

/// Run the daemon until SIGINT/SIGTERM or until `shutdown` resolves (e.g. "Quit" in the menu),
/// emitting UI events on `ui`. The socket is removed on every exit path.
pub async fn run_with(cfg: Config, ui: Arc<dyn UiSink>, shutdown: impl Future<Output = ()>) -> Result<()> {
    let clients = Clients::from_config(&cfg)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let audio: Option<Box<dyn AudioSource>> = match CpalSource::new(&cfg.audio, Some(tx)) {
        Ok(mut src) => {
            // Live mic level -> pill waveform (called ~30 Hz on the audio thread, only while recording).
            let level_ui = ui.clone();
            src.set_level_sink(Some(Arc::new(move |l| level_ui.send(UiEvent::Level(l)))));
            Some(Box::new(src))
        }
        Err(e) => {
            tracing::error!("audio capture unavailable: {e:#}");
            None
        }
    };
    let mut platform = Platform::detect_with(&cfg.hotkey);
    let hotkey = platform.hotkey.take();
    let daemon = Arc::new(Daemon::new(cfg, clients, platform, audio).with_ui(ui.clone()));
    daemon.spawn_capture_events(rx);
    daemon.spawn_hotkey(hotkey);
    let path = ipc::socket_path();
    tracing::info!("capcom daemon listening on {}", path.display());
    let d = daemon.clone();
    let serve = ipc::serve(path.clone(), move |req| {
        let d = d.clone();
        async move { d.handle(req).await }
    });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    ui.send(UiEvent::State(UiState::Idle));
    let res = tokio::select! {
        r = serve => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = term.recv() => Ok(()),
        _ = shutdown => {
            tracing::info!("shutdown requested");
            Ok(())
        }
    };
    let _ = std::fs::remove_file(&path);
    res
}

impl Daemon {
    /// Run the platform hotkey listener (if any) on its own thread and apply its events.
    /// A listener failure (e.g. missing macOS permissions) is logged; the daemon keeps running.
    fn spawn_hotkey(self: &Arc<Self>, hotkey: Option<Box<dyn crate::platform::Hotkey>>) {
        use crate::platform::HotkeyEvent;
        let Some(mut hk) = hotkey else { return };
        if std::env::var_os("CAPCOM_NO_HOTKEY").is_some() {
            // Dev/test: lets a second daemon run without grabbing the real hotkey.
            tracing::info!("CAPCOM_NO_HOTKEY set: hotkey listener disabled");
            return;
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let spawned = std::thread::Builder::new().name("capcom-hotkey".into()).spawn(move || {
            if let Err(e) = hk.run(tx) {
                tracing::error!("hotkey disabled: {e:#}");
            }
        });
        if let Err(e) = spawned {
            tracing::error!("cannot start hotkey thread: {e}");
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let resp = match ev {
                    HotkeyEvent::Start => me.cmd_start(),
                    HotkeyEvent::Stop => me.cmd_stop(),
                    HotkeyEvent::Toggle => match me.state() {
                        State::Recording => me.cmd_stop(),
                        _ => me.cmd_start(),
                    },
                    HotkeyEvent::Cancel => me.cmd_cancel(),
                };
                if !resp.ok {
                    tracing::debug!("hotkey {ev:?}: {:?}", resp.error);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_maps_to_ui_contract() {
        let st = |ev| ui_for_feedback(&ev).map(|(u, s)| (u, s.is_some()));
        assert_eq!(st(FeedbackEvent::RecordingStarted), Some((UiEvent::State(UiState::Listening), false)));
        assert_eq!(st(FeedbackEvent::RecordingStopped), Some((UiEvent::State(UiState::Processing), false)));
        assert_eq!(st(FeedbackEvent::Done), Some((UiEvent::State(UiState::Done), true)));
        assert_eq!(st(FeedbackEvent::Discarded), Some((UiEvent::State(UiState::Cancelled), true)));
        assert_eq!(st(FeedbackEvent::Error("x".into())), Some((UiEvent::Error("x".into()), true)));
        assert_eq!(st(FeedbackEvent::Busy), None);
        assert_eq!(st(FeedbackEvent::Notify("n".into())), None);
    }

    struct Rec(Mutex<Vec<UiEvent>>);
    impl UiSink for Rec {
        fn send(&self, ev: UiEvent) {
            self.0.lock().unwrap().push(ev);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_states_settle_to_idle_unless_superseded() {
        let rec = Arc::new(Rec(Mutex::new(vec![])));
        let platform = Platform { paster: Box::new(crate::platform::LogPaster), feedback: Box::new(crate::platform::LogFeedback), hotkey: None };
        let cfg = Config::default();
        let http = crate::asr::http_client();
        let clients = Clients {
            asr: crate::asr::AsrClient::new(http.clone(), "http://127.0.0.1:9/v1/audio/transcriptions", "m", "k"),
            cleanup: crate::cleanup::CleanupClient::new(http, "http://127.0.0.1:9/v1/chat/completions", "m", "k", String::new()),
        };
        let d = Daemon::new(cfg, clients, platform, None).with_ui(rec.clone());
        d.fb(FeedbackEvent::Done);
        tokio::time::sleep(SETTLE_DONE + Duration::from_millis(50)).await;
        assert_eq!(*rec.0.lock().unwrap(), vec![UiEvent::State(UiState::Done), UiEvent::State(UiState::Idle)]);

        rec.0.lock().unwrap().clear();
        d.fb(FeedbackEvent::Error("boom".into()));
        d.fb(FeedbackEvent::RecordingStarted); // a new dictation starts before the error settles
        tokio::time::sleep(SETTLE_ERROR * 2).await;
        assert_eq!(*rec.0.lock().unwrap(), vec![UiEvent::Error("boom".into()), UiEvent::State(UiState::Listening)]);
    }
}
