//! Linux (Omarchy / Hyprland, Wayland) platform implementation.
//!
//! Zero extra crates: everything shells out to standard tools through `std::process`.
//!
//! - [`LinuxPaster`]: `wl-paste` snapshot, `wl-copy`, then a paste chord injected with
//!   `hyprctl dispatch` (Ctrl+Shift+V for terminals, Ctrl+V otherwise), clipboard restored
//!   ~500 ms later only if it still holds our text. Fallbacks: `wtype` typing, then clipboard.
//! - [`LinuxFeedback`]: `pw-play`/`paplay` sounds, `notify-send` errors, and the Waybar state
//!   file `$XDG_RUNTIME_DIR/capcom/state` (`idle` / `recording` / `processing`).
//! - No hotkey: Omarchy dictation-backend scripts and Hyprland binds call `capcom start|stop`.
//!
//! The approach (Hyprland `send_key_state` with the `sendshortcut` fallback, terminal detection by
//! window class/tags) mirrors Omarchy's own `default/hypr/bindings/clipboard.lua` and
//! `bin/omarchy-menu-keybindings` (MIT); clipboard save/restore ideas come from Voxtype's
//! `src/output/paste.rs` and Handy's `clipboard.rs` (both MIT). See `THIRD_PARTY.md`.

use super::{Feedback, FeedbackEvent, Paster};
use anyhow::{anyhow, bail, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Settle time after `wl-copy` (and a stand-in for "wait until modifiers are released",
/// which Wayland does not let us observe without evdev).
const SETTLE: Duration = Duration::from_millis(80);
/// Delay between synthetic key down and up (works around stuck/repeating keys, see Omarchy's
/// clipboard.lua / Hyprland discussion #14099).
const KEY_HOLD: Duration = Duration::from_millis(50);
/// Clipboard restore delay after the paste chord.
const RESTORE_AFTER: Duration = Duration::from_millis(500);
const CMD_TIMEOUT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------------------------
// Tool detection
// ---------------------------------------------------------------------------------------------

/// Which helper tools exist. Detected once at startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tools {
    /// `HYPRLAND_INSTANCE_SIGNATURE` is set.
    pub hyprland: bool,
    pub hyprctl: bool,
    pub wl_copy: bool,
    pub wl_paste: bool,
    pub wtype: bool,
    pub pw_play: bool,
    pub paplay: bool,
    pub notify_send: bool,
}

impl Tools {
    pub fn detect() -> Self {
        let hyprland = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some_and(|v| !v.is_empty());
        Self::from_lookup(hyprland, |n| find_in_path(n).is_some())
    }

    pub fn from_lookup(hyprland: bool, has: impl Fn(&str) -> bool) -> Self {
        Self {
            hyprland,
            hyprctl: has("hyprctl"),
            wl_copy: has("wl-copy"),
            wl_paste: has("wl-paste"),
            wtype: has("wtype"),
            pw_play: has("pw-play"),
            paplay: has("paplay"),
            notify_send: has("notify-send"),
        }
    }
}

pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Pure logic (unit tested)
// ---------------------------------------------------------------------------------------------

/// The focused window, from `hyprctl activewindow -j`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowInfo {
    pub class: String,
    pub tags: Vec<String>,
}

pub fn parse_active_window(json: &str) -> Option<WindowInfo> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let class = v.get("class").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let tags = v
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if class.is_empty() && v.get("address").is_none() {
        return None;
    }
    Some(WindowInfo { class, tags })
}

/// Terminal emulators that paste with Ctrl+Shift+V. Mirrors Omarchy's `apps/terminals.lua`
/// (which also treats `org.omarchy.*` and `TUI.*` app-ids as terminals).
pub fn is_terminal(w: &WindowInfo) -> bool {
    // Omarchy tags terminal windows ("terminal"; dynamic tags carry a trailing '*').
    if w.tags.iter().any(|t| t.trim_end_matches('*') == "terminal") {
        return true;
    }
    let c = w.class.to_lowercase();
    const EXACT: &[&str] = &[
        "alacritty",
        "kitty",
        "com.mitchellh.ghostty",
        "ghostty",
        "foot",
        "footclient",
        "org.codeberg.dnkl.foot",
        "wezterm",
        "org.wezfurlong.wezterm",
        "xterm",
        "urxvt",
        "st",
        "st-256color",
        "konsole",
        "org.kde.konsole",
        "gnome-terminal",
        "gnome-terminal-server",
        "org.gnome.terminal",
        "org.gnome.console",
        "xfce4-terminal",
        "terminator",
        "tilix",
        "com.raggesilver.blackbox",
        "contour",
        "rio",
    ];
    EXACT.contains(&c.as_str()) || c.starts_with("org.omarchy.") || c.starts_with("tui.")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    CtrlV,
    CtrlShiftV,
}

impl Chord {
    pub fn for_window(w: Option<&WindowInfo>) -> Self {
        match w {
            Some(w) if is_terminal(w) => Chord::CtrlShiftV,
            _ => Chord::CtrlV,
        }
    }
    fn mods(self) -> &'static str {
        match self {
            Chord::CtrlV => "CTRL",
            Chord::CtrlShiftV => "CTRL SHIFT",
        }
    }
    const KEY: &'static str = "V";
}

/// Lua form (Hyprland's Lua config API, which Omarchy uses). No `window` field: send to the
/// focused surface, as Omarchy's clipboard.lua does.
pub fn lua_send_key_state(chord: Chord, state: &str) -> String {
    format!("hl.dsp.send_key_state({{ mods = \"{}\", key = \"{}\", state = \"{state}\" }})", chord.mods(), Chord::KEY)
}

/// Legacy `hyprctl dispatch sendshortcut` argument (`mods,key,window`).
pub fn sendshortcut_arg(chord: Chord) -> String {
    format!("{},{},activewindow", chord.mods(), Chord::KEY)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// wl-copy + hyprctl chord.
    HyprlandChord,
    /// Type the text directly with wtype.
    Wtype,
    /// wl-copy only (caller notifies).
    ClipboardOnly,
}

pub fn choose_method(t: &Tools) -> Method {
    if t.hyprland && t.hyprctl && t.wl_copy {
        Method::HyprlandChord
    } else if t.wtype {
        Method::Wtype
    } else {
        Method::ClipboardOnly
    }
}

pub fn wtype_args(text: &str) -> Vec<String> {
    vec!["--".to_string(), text.to_string()]
}

/// Hyprland's reply to a successful dispatch is empty or `ok`.
pub fn dispatch_ok(status_ok: bool, output: &str) -> bool {
    status_ok && matches!(output.trim(), "" | "ok")
}

/// Clipboard snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    Empty,
    Text(String),
    /// Non-text content (image, files): cannot be restored, so we skip the restore.
    NonText,
}

/// From `wl-paste --list-types`: is the clipboard plain text we can restore losslessly enough?
pub fn types_are_text_only(list: &str) -> bool {
    let mut any_text = false;
    for t in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if t.starts_with("image/") || t == "text/uri-list" || t.starts_with("application/x-kde") {
            return false;
        }
        if t.starts_with("text/") || matches!(t, "UTF8_STRING" | "STRING" | "TEXT") {
            any_text = true;
        }
    }
    any_text
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundKind {
    Start,
    Stop,
    Error,
    Cancel,
}

impl SoundKind {
    fn file(self) -> &'static str {
        match self {
            SoundKind::Start => "start.wav",
            SoundKind::Stop => "stop.wav",
            SoundKind::Error => "error.wav",
            SoundKind::Cancel => "cancel.wav",
        }
    }
}

pub fn sound_for(ev: &FeedbackEvent) -> Option<SoundKind> {
    match ev {
        FeedbackEvent::RecordingStarted => Some(SoundKind::Start),
        FeedbackEvent::RecordingStopped => Some(SoundKind::Stop),
        FeedbackEvent::Error(_) => Some(SoundKind::Error),
        FeedbackEvent::Busy | FeedbackEvent::Discarded => Some(SoundKind::Cancel),
        FeedbackEvent::Done | FeedbackEvent::Notify(_) => None,
    }
}

/// Waybar state after an event; `None` = unchanged.
pub fn state_for(ev: &FeedbackEvent) -> Option<&'static str> {
    match ev {
        FeedbackEvent::RecordingStarted => Some("recording"),
        FeedbackEvent::RecordingStopped => Some("processing"),
        FeedbackEvent::Done | FeedbackEvent::Discarded | FeedbackEvent::Error(_) => Some("idle"),
        FeedbackEvent::Busy | FeedbackEvent::Notify(_) => None,
    }
}

/// `notify-send` arguments for an event, if it should notify.
pub fn notify_args(ev: &FeedbackEvent) -> Option<Vec<String>> {
    let (urgency, body) = match ev {
        FeedbackEvent::Error(m) => ("critical", m.as_str()),
        FeedbackEvent::Notify(m) => ("normal", m.as_str()),
        _ => return None,
    };
    Some(["-a", "Capcom", "-u", urgency, "-t", "6000", "Capcom", body].iter().map(|s| s.to_string()).collect())
}

// ---------------------------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------------------------

struct Captured {
    ok: bool,
    stdout: Vec<u8>,
    stderr: String,
}

/// Run a command to completion with a timeout, capturing output.
fn run_capture(cmd: &str, args: &[&str], timeout: Duration) -> Result<Captured> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("cannot run {cmd}: {e}"))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{cmd} timed out after {timeout:?}");
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    Ok(Captured {
        ok: status.success(),
        stdout: t_out.join().unwrap_or_default(),
        stderr: String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned(),
    })
}

fn wait_timeout(child: &mut std::process::Child, timeout: Duration, what: &str) -> Result<()> {
    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(s) if s.success() => return Ok(()),
            Some(s) => bail!("{what} exited with {s}"),
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{what} timed out");
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn wl_copy(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .args(["--type", "text/plain"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("cannot run wl-copy: {e}"))?;
    // Dropping the handle closes stdin so wl-copy can fork into the background.
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    wait_timeout(&mut child, CMD_TIMEOUT, "wl-copy")
}

fn wl_clear() {
    let _ = Command::new("wl-copy").arg("--clear").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn read_clipboard() -> Saved {
    let Ok(types) = run_capture("wl-paste", &["--list-types"], CMD_TIMEOUT) else { return Saved::NonText };
    if !types.ok {
        // wl-paste: "Nothing is copied" -> empty clipboard.
        return if types.stderr.to_lowercase().contains("nothing is copied") { Saved::Empty } else { Saved::NonText };
    }
    let list = String::from_utf8_lossy(&types.stdout);
    if list.trim().is_empty() {
        return Saved::Empty;
    }
    if !types_are_text_only(&list) {
        return Saved::NonText;
    }
    match run_capture("wl-paste", &["--no-newline"], CMD_TIMEOUT) {
        Ok(c) if c.ok => String::from_utf8(c.stdout).map(Saved::Text).unwrap_or(Saved::NonText),
        _ => Saved::NonText,
    }
}

fn active_window() -> Option<WindowInfo> {
    let c = run_capture("hyprctl", &["activewindow", "-j"], CMD_TIMEOUT).ok()?;
    if !c.ok {
        return None;
    }
    parse_active_window(&String::from_utf8_lossy(&c.stdout))
}

fn hypr_dispatch(args: &[&str]) -> Result<()> {
    let mut full = vec!["dispatch"];
    full.extend_from_slice(args);
    let c = run_capture("hyprctl", &full, CMD_TIMEOUT)?;
    let out = String::from_utf8_lossy(&c.stdout);
    if dispatch_ok(c.ok, &out) {
        Ok(())
    } else {
        bail!("hyprctl dispatch failed: {} {}", out.trim(), c.stderr.trim())
    }
}

/// Send the chord: Lua `send_key_state` down/up first, legacy `sendshortcut` as fallback.
fn send_chord(chord: Chord) -> Result<()> {
    if hypr_dispatch(&[&lua_send_key_state(chord, "down")]).is_ok() {
        std::thread::sleep(KEY_HOLD);
        return hypr_dispatch(&[&lua_send_key_state(chord, "up")]);
    }
    hypr_dispatch(&["sendshortcut", &sendshortcut_arg(chord)])
}

fn type_with_wtype(text: &str) -> Result<()> {
    let mut child = Command::new("wtype")
        .args(wtype_args(text))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("cannot run wtype: {e}"))?;
    wait_timeout(&mut child, Duration::from_secs(10), "wtype")
}

// ---------------------------------------------------------------------------------------------
// Paster
// ---------------------------------------------------------------------------------------------

struct Pending {
    gen: u64,
    text: String,
    saved: Saved,
}

pub struct LinuxPaster {
    tools: Tools,
    lock: Mutex<()>,
    gen: Arc<AtomicU64>,
    pending: Arc<Mutex<Option<Pending>>>,
}

impl LinuxPaster {
    pub fn new(tools: Tools) -> Self {
        Self { tools, lock: Mutex::new(()), gen: Arc::new(AtomicU64::new(0)), pending: Arc::new(Mutex::new(None)) }
    }

    fn paste_via_chord(&self, text: &str) -> Result<()> {
        // 1. Snapshot. If a previous restore has not run yet the clipboard holds that earlier
        //    dictation, so keep the earlier snapshot as the thing to restore.
        let mut saved = if self.tools.wl_paste { read_clipboard() } else { Saved::NonText };
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(prev) = self.pending.lock().unwrap().take() {
            if saved == Saved::Text(prev.text.clone()) {
                saved = prev.saved;
            }
        }
        // 2. Put our text on the clipboard.
        wl_copy(text)?;
        // 3. Let clients see the new selection and (hopefully) modifiers release.
        std::thread::sleep(SETTLE);
        // 4. Chord by window class.
        let chord = Chord::for_window(active_window().as_ref());
        if let Err(e) = send_chord(chord) {
            tracing::warn!("paste chord failed: {e}");
            if self.tools.wtype {
                type_with_wtype(text)?;
            } else {
                // Leave our text on the clipboard; the daemon notifies.
                return Err(e.context("could not send the paste shortcut; text left on the clipboard"));
            }
        }
        // 5. Restore later, only if the clipboard still holds our text.
        if !matches!(saved, Saved::NonText) {
            *self.pending.lock().unwrap() = Some(Pending { gen, text: text.to_string(), saved: saved.clone() });
            let (pending, gens, text) = (self.pending.clone(), self.gen.clone(), text.to_string());
            std::thread::spawn(move || {
                std::thread::sleep(RESTORE_AFTER);
                if gens.load(Ordering::SeqCst) != gen {
                    return; // a newer paste owns the clipboard now
                }
                if read_clipboard() == Saved::Text(text) {
                    match &saved {
                        Saved::Text(s) => {
                            let _ = wl_copy(s);
                        }
                        Saved::Empty => wl_clear(),
                        Saved::NonText => {}
                    }
                }
                let mut p = pending.lock().unwrap();
                if p.as_ref().is_some_and(|p| p.gen == gen) {
                    *p = None;
                }
            });
        }
        Ok(())
    }
}

impl Paster for LinuxPaster {
    fn paste(&self, text: &str) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        match choose_method(&self.tools) {
            Method::HyprlandChord => self.paste_via_chord(text),
            Method::Wtype => type_with_wtype(text),
            Method::ClipboardOnly => {
                if self.tools.wl_copy {
                    wl_copy(text)?;
                    bail!("no way to send keystrokes (install wtype or run under Hyprland); text left on the clipboard")
                }
                bail!("no paste tool available: install wl-clipboard and wtype")
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Feedback
// ---------------------------------------------------------------------------------------------

// Shared assets (also used by the macOS build); materialised into the runtime dir for pw-play.
const SND_START: &[u8] = include_bytes!("../../assets/sounds/start.wav");
const SND_STOP: &[u8] = include_bytes!("../../assets/sounds/stop.wav");
const SND_ERROR: &[u8] = include_bytes!("../../assets/sounds/error.wav");
const SND_CANCEL: &[u8] = include_bytes!("../../assets/sounds/cancel.wav");

fn sound_bytes(k: SoundKind) -> &'static [u8] {
    match k {
        SoundKind::Start => SND_START,
        SoundKind::Stop => SND_STOP,
        SoundKind::Error => SND_ERROR,
        SoundKind::Cancel => SND_CANCEL,
    }
}

/// `$XDG_RUNTIME_DIR/capcom`
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(r) if !r.is_empty() => PathBuf::from(r).join("capcom"),
        _ => PathBuf::from(format!("/tmp/capcom-{}", std::env::var("USER").unwrap_or_else(|_| "user".into()))),
    }
}

pub fn state_file() -> PathBuf {
    runtime_dir().join("state")
}

fn write_state(path: &Path, state: &str) {
    let tmp = path.with_extension("tmp");
    let r = (|| -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&tmp, format!("{state}\n"))?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = r {
        tracing::debug!("state file write failed: {e}");
    }
}

pub struct LinuxFeedback {
    tools: Tools,
    sounds_dir: PathBuf,
    state_path: PathBuf,
}

impl LinuxFeedback {
    pub fn new(tools: Tools) -> Self {
        let sounds_dir = runtime_dir().join("sounds");
        let _ = std::fs::create_dir_all(&sounds_dir);
        for k in [SoundKind::Start, SoundKind::Stop, SoundKind::Error, SoundKind::Cancel] {
            let p = sounds_dir.join(k.file());
            let want = sound_bytes(k);
            if std::fs::metadata(&p).map(|m| m.len() != want.len() as u64).unwrap_or(true) {
                let _ = std::fs::write(&p, want);
            }
        }
        let fb = Self { tools, sounds_dir, state_path: state_file() };
        write_state(&fb.state_path, "idle");
        fb
    }

    fn spawn_detached(cmd: &str, args: &[String]) {
        match Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(mut child) => {
                // Reap in the background so we never leave zombies.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => tracing::debug!("cannot run {cmd}: {e}"),
        }
    }

    fn play(&self, k: SoundKind) {
        let file = self.sounds_dir.join(k.file()).to_string_lossy().into_owned();
        if self.tools.pw_play {
            Self::spawn_detached("pw-play", &[file]);
        } else if self.tools.paplay {
            Self::spawn_detached("paplay", &[file]);
        }
    }
}

impl Feedback for LinuxFeedback {
    fn event(&self, ev: FeedbackEvent) {
        if let Some(s) = state_for(&ev) {
            write_state(&self.state_path, s);
        }
        if let Some(k) = sound_for(&ev) {
            self.play(k);
        }
        if let Some(args) = notify_args(&ev) {
            if self.tools.notify_send {
                Self::spawn_detached("notify-send", &args);
            } else {
                tracing::warn!("notification (no notify-send): {args:?}");
            }
        }
    }
}

/// Build the Linux platform pieces (no hotkey: compositor binds call the CLI).
pub fn new() -> super::Platform {
    let tools = Tools::detect();
    tracing::info!("linux platform: {tools:?}, paste method {:?}", choose_method(&tools));
    super::Platform {
        paster: Box::new(LinuxPaster::new(tools.clone())),
        feedback: Box::new(LinuxFeedback::new(tools)),
        hotkey: None,
    }
}

// ---------------------------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

pub struct Check {
    pub level: Level,
    pub label: &'static str,
    pub detail: String,
}

fn omarchy_backend_config() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_default();
    base.join("omarchy/defaults/dictation")
}

pub fn doctor_checks() -> Vec<Check> {
    let t = Tools::detect();
    let mut v = Vec::new();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok().filter(|s| !s.is_empty());
    v.push(match &wayland {
        Some(d) => Check { level: Level::Ok, label: "wayland", detail: d.clone() },
        None => Check { level: Level::Warn, label: "wayland", detail: "WAYLAND_DISPLAY unset (run doctor inside the session)".into() },
    });
    v.push(if t.hyprland {
        Check { level: Level::Ok, label: "hyprland", detail: "HYPRLAND_INSTANCE_SIGNATURE set".into() }
    } else {
        Check { level: Level::Warn, label: "hyprland", detail: "not detected: paste falls back to wtype typing".into() }
    });
    let tool = |ok: bool, name: &'static str, required: bool, hint: &str| Check {
        level: if ok { Level::Ok } else if required { Level::Fail } else { Level::Warn },
        label: name,
        detail: if ok { "found".into() } else { format!("missing ({hint})") },
    };
    v.push(tool(t.wl_copy, "wl-copy", true, "pacman -S wl-clipboard"));
    v.push(tool(t.wl_paste, "wl-paste", true, "pacman -S wl-clipboard"));
    v.push(tool(t.hyprctl || !t.hyprland, "hyprctl", true, "ships with hyprland"));
    v.push(tool(t.wtype, "wtype", false, "pacman -S wtype; used as paste fallback"));
    v.push(tool(t.pw_play || t.paplay, "pw-play/paplay", false, "pacman -S pipewire; no sounds"));
    v.push(tool(t.notify_send, "notify-send", false, "pacman -S libnotify; no error toasts"));
    let rd = runtime_dir();
    let writable = std::fs::create_dir_all(&rd).is_ok() && std::fs::write(rd.join(".probe"), b"").is_ok();
    v.push(Check {
        level: if writable { Level::Ok } else { Level::Fail },
        label: "runtime dir",
        detail: format!("{} (socket: {})", rd.display(), crate::ipc::socket_path().display()),
    });
    if find_in_path("omarchy-dictation").is_some() {
        let backend = find_in_path("omarchy-dictation-capcom").is_some();
        v.push(Check {
            level: if backend { Level::Ok } else { Level::Warn },
            label: "omarchy backend",
            detail: if backend {
                "omarchy-dictation-capcom installed".into()
            } else {
                "omarchy-dictation-capcom not in PATH (run packaging/linux/install.sh)".into()
            },
        });
        let sel = std::fs::read_to_string(omarchy_backend_config()).unwrap_or_default();
        let selected = sel.trim() == "capcom";
        v.push(Check {
            level: if selected { Level::Ok } else { Level::Warn },
            label: "omarchy default dictation",
            detail: if selected { "capcom".into() } else { format!("{:?} (run `omarchy-dictation-use capcom`)", sel.trim()) },
        });
    } else {
        v.push(Check {
            level: Level::Warn,
            label: "omarchy backend",
            detail: "Omarchy not detected; use packaging/linux/hyprland-binds.{lua,conf}".into(),
        });
    }
    v
}

/// Print the Linux section in `doctor` format. Returns false if a required check failed.
pub fn print_doctor() -> bool {
    let mut all = true;
    for c in doctor_checks() {
        let tag = match c.level {
            Level::Ok => " ok ",
            Level::Warn => "warn",
            Level::Fail => {
                all = false;
                "FAIL"
            }
        };
        println!("[{tag}] {}: {}", c.label, c.detail);
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(class: &str, tags: &[&str]) -> WindowInfo {
        WindowInfo { class: class.into(), tags: tags.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn terminals_use_ctrl_shift_v() {
        for c in [
            "kitty",
            "Alacritty",
            "com.mitchellh.ghostty",
            "foot",
            "org.wezfurlong.wezterm",
            "org.omarchy.terminal",
            "TUI.float",
            "org.codeberg.dnkl.foot",
        ] {
            assert_eq!(Chord::for_window(Some(&win(c, &[]))), Chord::CtrlShiftV, "{c}");
        }
    }

    #[test]
    fn terminal_tag_wins() {
        assert_eq!(Chord::for_window(Some(&win("something", &["terminal*"]))), Chord::CtrlShiftV);
        assert_eq!(Chord::for_window(Some(&win("something", &["terminal"]))), Chord::CtrlShiftV);
    }

    #[test]
    fn others_use_ctrl_v() {
        for c in ["firefox", "chromium", "slack", "obsidian", "org.omarchy_not", ""] {
            assert_eq!(Chord::for_window(Some(&win(c, &["tiled"]))), Chord::CtrlV, "{c}");
        }
        assert_eq!(Chord::for_window(None), Chord::CtrlV);
    }

    #[test]
    fn parses_activewindow_json() {
        let j = r#"{"address":"0x1","class":"kitty","title":"x","tags":["terminal*"]}"#;
        let w = parse_active_window(j).unwrap();
        assert_eq!(w.class, "kitty");
        assert!(is_terminal(&w));
        assert_eq!(parse_active_window("{}"), None);
        assert_eq!(parse_active_window("garbage"), None);
    }

    #[test]
    fn command_construction() {
        assert_eq!(
            lua_send_key_state(Chord::CtrlShiftV, "down"),
            r#"hl.dsp.send_key_state({ mods = "CTRL SHIFT", key = "V", state = "down" })"#
        );
        assert_eq!(sendshortcut_arg(Chord::CtrlV), "CTRL,V,activewindow");
        assert_eq!(wtype_args("-n hi"), vec!["--", "-n hi"]);
    }

    #[test]
    fn dispatch_reply() {
        assert!(dispatch_ok(true, "ok\n"));
        assert!(dispatch_ok(true, ""));
        assert!(!dispatch_ok(true, "Invalid dispatcher"));
        assert!(!dispatch_ok(false, ""));
    }

    #[test]
    fn method_selection() {
        let all = Tools::from_lookup(true, |_| true);
        assert_eq!(choose_method(&all), Method::HyprlandChord);
        let other_wm = Tools::from_lookup(false, |_| true);
        assert_eq!(choose_method(&other_wm), Method::Wtype);
        let bare = Tools::from_lookup(false, |n| n == "wl-copy");
        assert_eq!(choose_method(&bare), Method::ClipboardOnly);
        let no_wtype_hypr = Tools::from_lookup(true, |n| n != "wtype");
        assert_eq!(choose_method(&no_wtype_hypr), Method::HyprlandChord);
    }

    #[test]
    fn clipboard_type_classification() {
        assert!(types_are_text_only("text/plain;charset=utf-8\nTEXT\nUTF8_STRING\n"));
        assert!(types_are_text_only("text/html\ntext/plain\n"));
        assert!(!types_are_text_only("image/png\n"));
        assert!(!types_are_text_only("text/plain\nimage/png\n"));
        assert!(!types_are_text_only("text/uri-list\ntext/plain\n"));
        assert!(!types_are_text_only(""));
    }

    #[test]
    fn feedback_mapping() {
        assert_eq!(sound_for(&FeedbackEvent::RecordingStarted), Some(SoundKind::Start));
        assert_eq!(sound_for(&FeedbackEvent::Error("x".into())), Some(SoundKind::Error));
        assert_eq!(sound_for(&FeedbackEvent::Done), None);
        assert_eq!(state_for(&FeedbackEvent::RecordingStarted), Some("recording"));
        assert_eq!(state_for(&FeedbackEvent::RecordingStopped), Some("processing"));
        assert_eq!(state_for(&FeedbackEvent::Done), Some("idle"));
        assert_eq!(state_for(&FeedbackEvent::Busy), None);
        let a = notify_args(&FeedbackEvent::Error("boom".into())).unwrap();
        assert!(a.contains(&"critical".to_string()) && a.last().unwrap() == "boom");
        assert!(notify_args(&FeedbackEvent::Done).is_none());
    }

    #[test]
    fn state_file_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("capcom/state");
        write_state(&p, "recording");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "recording\n");
        write_state(&p, "idle");
        assert_eq!(std::fs::read_to_string(&p).unwrap().trim(), "idle");
    }
}
