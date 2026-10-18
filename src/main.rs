use anyhow::{bail, Context, Result};
use capcom::audio::wav;
use capcom::config::{self, Config};
use capcom::history::{Entry, History};
use capcom::ipc::{self, Request, Response};
use capcom::pipeline::{self, Clients};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "capcom", version, about = "Hold a key, talk, get clean text. Bring your own speech-to-text + LLM servers.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// First-run wizard: pick your servers (presets), store the key, test it.
    Setup(capcom::setup::SetupArgs),
    /// Run the long-lived daemon (IPC server + state machine).
    Daemon,
    /// Start recording.
    Start,
    /// Stop recording and process.
    Stop,
    /// Start if idle, stop if recording.
    Toggle,
    /// Cancel the current recording.
    Cancel,
    /// Show daemon state.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Paste the most recent history entry.
    PasteLast,
    /// Show recent history.
    History {
        #[arg(short, default_value_t = 10)]
        n: usize,
    },
    /// Offline end-to-end: WAV file -> ASR -> cleanup -> text.
    Transcribe {
        file: PathBuf,
        #[arg(long)]
        no_cleanup: bool,
        #[arg(long)]
        json: bool,
    },
    /// Debug: run a WAV through a running daemon's state machine.
    #[command(hide = true)]
    DebugTranscribe { file: PathBuf },
    /// List audio input devices.
    Devices,
    /// Record from the mic for N seconds (no daemon), print capture stats, run the pipeline.
    RecordTest {
        #[arg(long, default_value_t = 4.0)]
        seconds: f64,
        #[arg(long)]
        no_cleanup: bool,
    },
    /// Config management.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Check config, key, endpoints and daemon.
    Doctor,
    /// macOS: show the floating pill overlay driven by a script or the live microphone.
    PillDemo {
        /// Bars follow the real microphone (CpalSource + level metering).
        #[arg(long, conflicts_with = "synthetic")]
        mic: bool,
        /// Scripted sequence: listening -> processing -> done -> error flash (default).
        #[arg(long)]
        synthetic: bool,
        /// Listening duration in seconds (default: 4 synthetic, 8 mic).
        #[arg(long)]
        seconds: Option<f64>,
        /// Pill position: `caret` (anchor to the focused text field) or `bottom`.
        /// Default: `[ui] pill_position` from the config.
        #[arg(long)]
        position: Option<config::PillPosition>,
    },
    /// macOS debug: after a delay, print where the pill would anchor (frontmost app, strategy
    /// caret/field/window/screen, raw AX rects, final pill rect, lookup time).
    AnchorProbe {
        /// Seconds to wait before probing (switch to the target app meanwhile).
        #[arg(long, default_value_t = 3.0)]
        delay: f64,
        /// Number of lookups (the first may enable a Chromium/Electron AX tree).
        #[arg(long, default_value_t = 2)]
        count: u32,
        /// Probe as if `pill_position` were this.
        #[arg(long, default_value = "caret")]
        position: config::PillPosition,
        /// Probe this app (pid) instead of the focused one; works on background apps.
        #[arg(long)]
        pid: Option<i32>,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Write a blank config + cleanup prompt (existing files kept unless --force). Prefer `capcom setup`.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Print the config file path.
    Path,
    /// Print the effective config (key redacted).
    Show,
}

fn main() {
    // Finder used to pass `-psn_*`; ignore it so a double-clicked bundle sees "no arguments".
    let args: Vec<std::ffi::OsString> =
        std::env::args_os().filter(|a| !a.to_string_lossy().starts_with("-psn_")).collect();
    let bundled = launched_from_app_bundle();
    // Double-clicking Capcom.app (or a login item) runs with no arguments: act as `daemon`.
    let app_launch = bundled && args.len() == 1;
    let cli = if app_launch { Cli { cmd: Cmd::Daemon } } else { Cli::parse_from(args) };
    init_logging(matches!(cli.cmd, Cmd::Daemon), app_launch);

    #[cfg(target_os = "macos")]
    if matches!(cli.cmd, Cmd::Daemon) && std::env::var_os("CAPCOM_HEADLESS").is_none() {
        // AppKit owns the main thread (menu-bar icon); the daemon runs on a background thread.
        match Config::load() {
            Ok(cfg) => capcom::platform::macos::ui::run_daemon_app(cfg, app_launch),
            Err(e) => {
                if app_launch {
                    capcom::platform::macos::notify(&format!("Capcom cannot start: {e:#}"));
                }
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
    }

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    if let Err(e) = rt.block_on(run(cli)) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn launched_from_app_bundle() -> bool {
    #[cfg(target_os = "macos")]
    {
        capcom::platform::macos::ui::app::launched_from_app_bundle()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Logs go to stderr, except for a Finder/login-item launch (no terminal, no LaunchAgent
/// redirection): then append to `~/Library/Logs/Capcom.log`.
fn init_logging(daemon: bool, app_launch: bool) {
    let default_level = if daemon { "info" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_env("CAPCOM_LOG").unwrap_or_else(|_| default_level.into());
    let log_file = app_launch
        .then(|| dirs::home_dir().map(|h| h.join("Library/Logs")))
        .flatten()
        .and_then(|d| std::fs::create_dir_all(&d).ok().map(|_| d.join("Capcom.log")))
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    match log_file {
        Some(f) => tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(f)).init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init(),
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Setup(args) => {
            if !capcom::setup::run(args).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Daemon => capcom::daemon::run(Config::load()?).await,
        Cmd::Start => ipc_simple(Request::Start).await,
        Cmd::Stop => ipc_simple(Request::Stop).await,
        Cmd::Toggle => ipc_simple(Request::Toggle).await,
        Cmd::Cancel => ipc_simple(Request::Cancel).await,
        Cmd::PasteLast => ipc_simple(Request::PasteLast).await,
        Cmd::Status { json } => status(json).await,
        Cmd::History { n } => history(n),
        Cmd::Transcribe { file, no_cleanup, json } => transcribe(file, no_cleanup, json).await,
        Cmd::DebugTranscribe { file } => {
            let path = std::fs::canonicalize(&file)?.to_string_lossy().into_owned();
            let r = call(Request::DebugTranscribe { path }).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }
        Cmd::Devices => devices(),
        Cmd::RecordTest { seconds, no_cleanup } => record_test(seconds, no_cleanup).await,
        Cmd::Config { cmd } => match cmd {
            ConfigCmd::Init { force } => {
                let w = config::init(force)?;
                if w.is_empty() {
                    println!("nothing written (files exist; use --force to overwrite)");
                }
                for p in w {
                    println!("wrote {}", p.display());
                }
                Ok(())
            }
            ConfigCmd::Path => {
                println!("{}", config::config_path().display());
                Ok(())
            }
            ConfigCmd::Show => {
                print!("{}", Config::load()?.show_redacted()?);
                Ok(())
            }
        },
        Cmd::PillDemo { mic, synthetic: _, seconds, position } => pill_demo(mic, seconds, position),
        Cmd::AnchorProbe { delay, count, position, pid } => anchor_probe(delay, count, position, pid),
        Cmd::Doctor => {
            if !capcom::doctor::run(&Config::load()?).await {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

async fn call(req: Request) -> Result<Response> {
    let r = ipc::send(&ipc::socket_path(), &req).await?;
    if !r.ok {
        bail!("{}", r.error.as_deref().unwrap_or("daemon reported failure"));
    }
    Ok(r)
}

async fn ipc_simple(req: Request) -> Result<()> {
    let r = call(req).await?;
    println!("{}", r.state.map(|s| s.as_str()).unwrap_or("ok"));
    Ok(())
}

async fn status(json: bool) -> Result<()> {
    let r = ipc::send(&ipc::socket_path(), &Request::Status).await;
    match r {
        Ok(r) if json => println!("{}", serde_json::to_string(&r)?),
        Ok(r) => println!("{}", r.state.map(|s| s.as_str()).unwrap_or("unknown")),
        // For Waybar-style polling a stopped daemon is a state, not a failure.
        Err(_) if json => println!(r#"{{"ok":false,"state":"offline","error":"daemon not running"}}"#),
        Err(e) => return Err(e),
    }
    Ok(())
}

fn history(n: usize) -> Result<()> {
    let cfg = Config::load()?;
    for e in History::open_default(cfg.history.keep).read_last(n)? {
        println!("[{}ms asr {}ms cleanup {}] {}", e.ts_ms, e.asr_ms, e.cleanup_ms, e.final_text);
    }
    Ok(())
}

async fn transcribe(file: PathBuf, no_cleanup: bool, json: bool) -> Result<()> {
    let mut cfg = Config::load()?;
    if no_cleanup {
        cfg.cleanup.enabled = false;
    }
    if let Some(p) = cfg.setup_problem() {
        bail!("{p}");
    }
    let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
    let samples = wav::read_wav_16k_mono(&bytes)?;
    let audio_ms = capcom::audio::clip_ms(&samples);
    let wav_bytes = wav::encode_wav_16k_mono(&samples)?;
    let clients = Clients::from_config(&cfg)?;
    let o = pipeline::process(wav_bytes, &cfg, &clients).await;

    if cfg.history.enabled && !o.final_text.is_empty() {
        if let Err(e) = History::open_default(cfg.history.keep).append(&Entry::from_outcome(&o)) {
            eprintln!("warning: history append failed: {e}");
        }
    }
    if json {
        let mut v = serde_json::to_value(&o)?;
        v["audio_ms"] = audio_ms.into();
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("{}", o.final_text);
        eprintln!(
            "audio {audio_ms} ms | asr {} ms | cleanup {} ms ({})",
            o.timings.asr_ms,
            o.timings.cleanup_ms,
            if o.cleanup_used { "used" } else { "not used" }
        );
        if let Some(e) = &o.error {
            eprintln!("note: {e}");
        }
    }
    if o.final_text.is_empty() && o.error.is_some() {
        bail!("{}", o.error.unwrap_or_default());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn pill_demo(mic: bool, seconds: Option<f64>, position: Option<config::PillPosition>) -> Result<()> {
    use capcom::platform::macos::ui::pill::demo::{self, DemoMode};
    let cfg = Config::load().unwrap_or_default();
    let position = position.unwrap_or(cfg.ui.pill_position);
    demo::run(if mic { DemoMode::Mic } else { DemoMode::Synthetic }, seconds, &cfg.audio, position)
}

#[cfg(not(target_os = "macos"))]
fn pill_demo(_mic: bool, _seconds: Option<f64>, _position: Option<config::PillPosition>) -> Result<()> {
    // TODO(linux): gtk4-layer-shell pill (see research/linux-wayland.md)
    bail!("pill-demo is macOS-only for now")
}

#[cfg(target_os = "macos")]
fn anchor_probe(delay: f64, count: u32, position: config::PillPosition, pid: Option<i32>) -> Result<()> {
    capcom::platform::macos::ui::pill::demo::probe(delay, count, position, pid)
}

#[cfg(not(target_os = "macos"))]
fn anchor_probe(_delay: f64, _count: u32, _position: config::PillPosition, _pid: Option<i32>) -> Result<()> {
    bail!("anchor-probe is macOS-only")
}

fn devices() -> Result<()> {
    let devs = capcom::audio::capture::list_input_devices()?;
    if devs.is_empty() {
        println!("no input devices found");
    }
    for d in devs {
        println!("{} {}  ({} Hz, {} ch, {})", if d.is_default { "*" } else { " " }, d.name, d.rate, d.channels, d.format);
    }
    println!("(* = system default; set audio.device in config to a name substring)");
    Ok(())
}

async fn record_test(seconds: f64, no_cleanup: bool) -> Result<()> {
    use capcom::audio::capture::CpalSource;
    use capcom::audio::AudioSource;
    let mut cfg = Config::load()?;
    if no_cleanup {
        cfg.cleanup.enabled = false;
    }
    if let Some(p) = cfg.setup_problem() {
        bail!("{p}");
    }
    let mut src = CpalSource::new(&cfg.audio, None)?;
    src.start()?;
    eprintln!("recording {seconds} s ... speak now");
    tokio::time::sleep(std::time::Duration::from_secs_f64(seconds.max(0.1))).await;
    let samples = src.stop();
    let st = src.stats().clone();
    println!("device:            {}", st.device);
    println!("native format:     {} Hz, {} ch, {}", st.native_rate, st.native_channels, st.native_format);
    println!("mic mode:          {}", st.mic_mode);
    println!("stream open:       {} ms", st.open_ms);
    match st.first_sample_ms {
        Some(ms) => println!("first-sample:      {ms} ms after start()"),
        None => println!("first-sample:      never (no samples delivered)"),
    }
    println!("captured:          {} ms (16 kHz mono)", st.captured_ms);
    println!("peak level:        {:.4} ({:.1} dBFS)", st.peak, 20.0 * st.peak.max(1e-9).log10());
    let samples = samples?;
    if st.peak < 1e-4 {
        eprintln!("warning: capture is digital silence. On macOS grant the microphone permission to this terminal app (System Settings > Privacy & Security > Microphone).");
    }
    let wav_bytes = wav::encode_wav_16k_mono(&samples)?;
    let clients = Clients::from_config(&cfg)?;
    let o = pipeline::process(wav_bytes, &cfg, &clients).await;
    println!("raw:               {}", o.raw);
    println!("final:             {}", o.final_text);
    println!(
        "timings:           asr {} ms | cleanup {} ms ({})",
        o.timings.asr_ms,
        o.timings.cleanup_ms,
        if o.cleanup_used { "used" } else { "not used" }
    );
    if let Some(e) = &o.error {
        eprintln!("note: {e}");
    }
    Ok(())
}
