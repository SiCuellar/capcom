//! `capcom setup`: a short first-run wizard (or flag-driven, for scripts and AI agents) that
//! writes `~/.config/capcom/config.toml` (0600), stores the API key (0600) and runs a live check.

use crate::config::{self, Config, DEFAULT_CLEANUP_PROMPT};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, IsTerminal, Write};

/// One known backend: where its OpenAI-compatible endpoint usually lives and what to send.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    /// Empty = the user must name the model (e.g. whatever is loaded in LM Studio).
    pub model: &'static str,
    pub needs_key: bool,
}

const fn p(id: &'static str, label: &'static str, url: &'static str, model: &'static str, needs_key: bool) -> Preset {
    Preset { id, label, url, model, needs_key }
}

/// Speech-to-text backends (`POST /v1/audio/transcriptions`).
pub const ASR_PRESETS: &[Preset] = &[
    p("vllm", "vLLM (Whisper / Voxtral)", "http://localhost:8000/v1/audio/transcriptions", "openai/whisper-large-v3-turbo", false),
    p("speaches", "speaches (faster-whisper)", "http://localhost:8000/v1/audio/transcriptions", "deepdml/faster-whisper-large-v3-turbo-ct2", false),
    p("whisper-cpp", "whisper.cpp server", "http://localhost:8080/v1/audio/transcriptions", "whisper-1", false),
    p("parakeet", "Parakeet TDT 0.6B v3 (FastAPI)", "http://localhost:5092/v1/audio/transcriptions", "istupakov/parakeet-tdt-0.6b-v3-onnx", false),
    p("groq", "Groq (cloud)", "https://api.groq.com/openai/v1/audio/transcriptions", "whisper-large-v3-turbo", true),
    p("openai", "OpenAI (cloud)", "https://api.openai.com/v1/audio/transcriptions", "gpt-4o-mini-transcribe", true),
];

/// Cleanup LLMs (`POST /v1/chat/completions`).
pub const LLM_PRESETS: &[Preset] = &[
    p("ollama", "Ollama", "http://localhost:11434/v1/chat/completions", "llama3.2:3b", false),
    p("vllm", "vLLM (second server)", "http://localhost:8001/v1/chat/completions", "Qwen/Qwen3-4B-Instruct-2507", false),
    p("lmstudio", "LM Studio", "http://localhost:1234/v1/chat/completions", "", false),
    p("llama-cpp", "llama.cpp llama-server", "http://localhost:8080/v1/chat/completions", "local", false),
    p("groq", "Groq (cloud)", "https://api.groq.com/openai/v1/chat/completions", "llama-3.1-8b-instant", true),
    p("openai", "OpenAI (cloud)", "https://api.openai.com/v1/chat/completions", "gpt-4.1-mini", true),
];

pub fn find(presets: &[Preset], id: &str) -> Option<Preset> {
    presets.iter().copied().find(|p| p.id.eq_ignore_ascii_case(id.trim()))
}

#[derive(Debug, Default, Clone, clap::Args)]
pub struct SetupArgs {
    /// Speech-to-text preset: vllm, speaches, whisper-cpp, parakeet, groq, openai.
    #[arg(long, value_name = "PRESET")]
    pub asr_preset: Option<String>,
    /// Cleanup LLM preset: ollama, vllm, lmstudio, llama-cpp, groq, openai.
    #[arg(long, value_name = "PRESET")]
    pub llm_preset: Option<String>,
    /// Transcription endpoint, e.g. http://localhost:8000/v1/audio/transcriptions.
    #[arg(long)]
    pub asr_url: Option<String>,
    #[arg(long)]
    pub asr_model: Option<String>,
    /// Chat endpoint, e.g. http://localhost:11434/v1/chat/completions.
    #[arg(long)]
    pub llm_url: Option<String>,
    #[arg(long)]
    pub llm_model: Option<String>,
    /// API key sent as Bearer to both endpoints ("-" reads it from stdin; "" removes it).
    #[arg(long)]
    pub api_key: Option<String>,
    /// Paste the raw transcript; no LLM cleanup.
    #[arg(long)]
    pub no_cleanup: bool,
    /// Never prompt: take presets/flags (and the existing config) as-is.
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Do not run the live endpoint check.
    #[arg(long)]
    pub skip_test: bool,
    /// Print the presets and exit.
    #[arg(long)]
    pub list_presets: bool,
}

/// The resolved answers: the new server/cleanup config plus what to do with the key.
#[derive(Debug, PartialEq)]
pub struct Answers {
    pub asr_url: String,
    pub asr_model: String,
    pub cleanup: bool,
    pub llm_url: String,
    pub llm_model: String,
    /// `None` = keep whatever key file exists; `Some("")` = remove; `Some(k)` = store.
    pub key: Option<String>,
}

/// Minimal line-based prompter (generic so tests can script it).
pub struct Prompter<R, W> {
    input: R,
    out: W,
}

impl<R: BufRead, W: Write> Prompter<R, W> {
    pub fn new(input: R, out: W) -> Self {
        Self { input, out }
    }

    fn say(&mut self, s: &str) {
        let _ = writeln!(self.out, "{s}");
    }

    /// Ask with a default shown in brackets; Enter keeps the default.
    fn ask(&mut self, q: &str, default: &str) -> Result<String> {
        if default.is_empty() {
            write!(self.out, "{q}: ")?;
        } else {
            write!(self.out, "{q} [{default}]: ")?;
        }
        self.out.flush()?;
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            bail!("input closed (use `capcom setup --yes` with flags for non-interactive setup)");
        }
        let a = line.trim();
        Ok(if a.is_empty() { default.to_string() } else { a.to_string() })
    }

    fn ask_required(&mut self, q: &str, default: &str) -> Result<String> {
        loop {
            let a = self.ask(q, default)?;
            if !a.is_empty() {
                return Ok(a);
            }
            self.say("  (required)");
        }
    }

    /// Numbered menu; returns the chosen index into `items` (accepts a number or a preset id).
    fn menu(&mut self, title: &str, items: &[(String, String)], default: usize) -> Result<usize> {
        self.say(title);
        for (i, (_, label)) in items.iter().enumerate() {
            self.say(&format!("  {}) {label}", i + 1));
        }
        loop {
            let a = self.ask("  pick", &(default + 1).to_string())?;
            if let Ok(n) = a.parse::<usize>() {
                if (1..=items.len()).contains(&n) {
                    return Ok(n - 1);
                }
            }
            if let Some(i) = items.iter().position(|(id, _)| id.eq_ignore_ascii_case(&a)) {
                return Ok(i);
            }
            self.say("  (type a number from the list)");
        }
    }
}

fn menu_items(presets: &[Preset], extra: &[(&str, &str)]) -> Vec<(String, String)> {
    presets
        .iter()
        .map(|p| (p.id.to_string(), format!("{:<34} {}", p.label, host_of(p.url))))
        .chain(extra.iter().map(|(id, l)| (id.to_string(), l.to_string())))
        .collect()
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

fn preset_arg(presets: &[Preset], id: Option<&str>, kind: &str) -> Result<Option<Preset>> {
    match id {
        None => Ok(None),
        Some(id) => find(presets, id).map(Some).with_context(|| {
            let ids: Vec<_> = presets.iter().map(|p| p.id).collect();
            format!("unknown {kind} preset `{id}` (choose from: {})", ids.join(", "))
        }),
    }
}

/// Non-interactive resolution: flags > preset > existing config.
pub fn resolve_flags(a: &SetupArgs, base: &Config) -> Result<Answers> {
    let asr_p = preset_arg(ASR_PRESETS, a.asr_preset.as_deref(), "ASR")?;
    let llm_p = preset_arg(LLM_PRESETS, a.llm_preset.as_deref(), "LLM")?;
    let pick = |flag: &Option<String>, preset: Option<&str>, current: &str| -> String {
        flag.clone().or_else(|| preset.map(String::from)).unwrap_or_else(|| current.to_string()).trim().to_string()
    };
    let s = &base.server;
    let asr_url = pick(&a.asr_url, asr_p.map(|p| p.url), &s.asr_url);
    let asr_model = pick(&a.asr_model, asr_p.map(|p| p.model), &s.asr_model);
    if asr_url.is_empty() || asr_model.is_empty() {
        bail!("speech-to-text is not configured: pass --asr-preset or --asr-url and --asr-model");
    }
    let llm_given = llm_p.is_some() || a.llm_url.is_some() || a.llm_model.is_some();
    let cleanup = !a.no_cleanup && (llm_given || base.cleanup.enabled);
    let llm_url = pick(&a.llm_url, llm_p.map(|p| p.url), &s.llm_url);
    let llm_model = pick(&a.llm_model, llm_p.map(|p| p.model).filter(|m| !m.is_empty()), &s.llm_model);
    if cleanup && (llm_url.is_empty() || llm_model.is_empty()) {
        bail!("cleanup LLM is not configured: pass --llm-preset / --llm-url + --llm-model, or --no-cleanup");
    }
    Ok(Answers { asr_url, asr_model, cleanup, llm_url, llm_model, key: a.api_key.clone() })
}

/// Interactive wizard; flags and the existing config become the defaults.
pub fn resolve_interactive<R: BufRead, W: Write>(a: &SetupArgs, base: &Config, io: &mut Prompter<R, W>) -> Result<Answers> {
    let s = &base.server;
    io.say("🛰  Capcom setup. You talk, Capcom relays. Enter keeps the [default].\n");

    // 1. Speech-to-text.
    let asr_p = preset_arg(ASR_PRESETS, a.asr_preset.as_deref(), "ASR")?;
    let items = menu_items(ASR_PRESETS, &[("custom", "Custom / keep current URL")]);
    let default = match asr_p {
        Some(p) => ASR_PRESETS.iter().position(|x| x.id == p.id).unwrap_or(0),
        None if !s.asr_url.is_empty() || a.asr_url.is_some() => ASR_PRESETS.len(),
        None => 0,
    };
    let i = io.menu("1/3  Speech-to-text server (OpenAI /v1/audio/transcriptions):", &items, default)?;
    let chosen = ASR_PRESETS.get(i);
    let (du, dm) = match chosen {
        Some(p) => (p.url.to_string(), p.model.to_string()),
        None => (s.asr_url.clone(), s.asr_model.clone()),
    };
    let asr_url = io.ask_required("  ASR URL", a.asr_url.as_deref().unwrap_or(&du))?;
    let asr_model = io.ask_required("  ASR model", a.asr_model.as_deref().unwrap_or(&dm))?;
    let mut needs_key = chosen.is_some_and(|p| p.needs_key);

    // 2. Cleanup LLM.
    let llm_p = preset_arg(LLM_PRESETS, a.llm_preset.as_deref(), "LLM")?;
    let items = menu_items(LLM_PRESETS, &[("custom", "Custom / keep current URL"), ("none", "No cleanup (paste the raw transcript)")]);
    let default = if a.no_cleanup {
        LLM_PRESETS.len() + 1
    } else {
        match llm_p {
            Some(p) => LLM_PRESETS.iter().position(|x| x.id == p.id).unwrap_or(0),
            None if !s.llm_url.is_empty() || a.llm_url.is_some() => LLM_PRESETS.len(),
            None => 0,
        }
    };
    let i = io.menu("\n2/3  Cleanup LLM (OpenAI /v1/chat/completions) — removes ums, fixes punctuation:", &items, default)?;
    let (cleanup, llm_url, llm_model) = if i == LLM_PRESETS.len() + 1 {
        (false, s.llm_url.clone(), s.llm_model.clone())
    } else {
        let chosen = LLM_PRESETS.get(i);
        let (du, dm) = match chosen {
            Some(p) => (p.url.to_string(), if p.model.is_empty() { s.llm_model.clone() } else { p.model.to_string() }),
            None => (s.llm_url.clone(), s.llm_model.clone()),
        };
        needs_key |= chosen.is_some_and(|p| p.needs_key);
        let u = io.ask_required("  LLM URL", a.llm_url.as_deref().unwrap_or(&du))?;
        let m = io.ask_required("  LLM model", a.llm_model.as_deref().unwrap_or(&dm))?;
        (true, u, m)
    };

    // 3. Key (one key for both endpoints).
    let key = match &a.api_key {
        Some(k) => Some(k.clone()),
        None => {
            let have = base.api_key().ok().flatten().is_some();
            let hint = match (have, needs_key) {
                (true, _) => "Enter keeps the current key, `-` removes it",
                (false, true) => "this backend needs one",
                (false, false) => "Enter for none: most local servers need no key",
            };
            io.say(&format!("\n3/3  API key ({hint}). Typing is hidden."));
            let k = with_echo_off(|| io.ask("  key", ""))?;
            io.say("");
            match k.as_str() {
                "" => None,
                "-" => Some(String::new()),
                k => Some(k.to_string()),
            }
        }
    };
    Ok(Answers { asr_url, asr_model, cleanup, llm_url, llm_model, key })
}

/// Hide typed input while `f` runs (best effort; no-op when stdin is not a terminal).
fn with_echo_off<T>(f: impl FnOnce() -> T) -> T {
    let tty = std::io::stdin().is_terminal();
    let stty = |arg: &str| {
        if tty {
            let _ = std::process::Command::new("stty").arg(arg).stdin(std::process::Stdio::inherit()).status();
        }
    };
    stty("-echo");
    let r = f();
    stty("echo");
    r
}

/// Apply answers to `base`, write config (+ key, + prompt if missing). Returns the new config.
pub fn apply(ans: &Answers, base: &Config) -> Result<Config> {
    let mut cfg = base.clone();
    cfg.server.asr_url = ans.asr_url.clone();
    cfg.server.asr_model = ans.asr_model.clone();
    cfg.server.llm_url = ans.llm_url.clone();
    cfg.server.llm_model = ans.llm_model.clone();
    cfg.cleanup.enabled = ans.cleanup;
    if cfg.server.api_key_file.trim().is_empty() {
        cfg.server.api_key_file = "~/.config/capcom/api_key".into();
    }
    let key_path = config::expand_tilde(&cfg.server.api_key_file);
    match ans.key.as_deref().map(str::trim) {
        Some("") => match std::fs::remove_file(&key_path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).context("removing the key file"),
            _ => {}
        },
        Some(k) => config::write_private(&key_path, &format!("{k}\n"))?,
        None => {}
    }
    config::write_private(&config::config_path(), &cfg.to_toml()?)?;
    let prompt = config::expand_tilde(&cfg.cleanup.prompt_file);
    if !prompt.exists() {
        config::write_private(&prompt, DEFAULT_CLEANUP_PROMPT)?;
    }
    Ok(cfg)
}

pub fn print_presets() {
    println!("Speech-to-text presets (--asr-preset):");
    for p in ASR_PRESETS {
        println!("  {:<12} {:<52} model {}", p.id, p.url, p.model);
    }
    println!("\nCleanup LLM presets (--llm-preset):");
    for p in LLM_PRESETS {
        let m = if p.model.is_empty() { "<pass --llm-model>" } else { p.model };
        println!("  {:<12} {:<52} model {}", p.id, p.url, m);
    }
}

/// `capcom setup`. Returns Ok(false) when the config was written but the live check failed.
pub async fn run(mut a: SetupArgs) -> Result<bool> {
    if a.list_presets {
        print_presets();
        return Ok(true);
    }
    if a.api_key.as_deref() == Some("-") {
        let mut k = String::new();
        std::io::stdin().lock().read_line(&mut k)?;
        a.api_key = Some(k.trim().to_string());
    }
    let base = Config::load().unwrap_or_else(|e| {
        eprintln!("warning: existing config unreadable ({e:#}); starting fresh");
        Config::default()
    });
    let interactive = !a.yes && std::io::stdin().is_terminal();
    let ans = if interactive {
        let stdin = std::io::stdin();
        resolve_interactive(&a, &base, &mut Prompter::new(stdin.lock(), std::io::stdout()))?
    } else {
        resolve_flags(&a, &base)?
    };
    apply(&ans, &base)?;
    println!("✓ wrote {} (0600)", config::config_path().display());
    if let Some(k) = &ans.key {
        let kp = config::expand_tilde(&base.server.api_key_file);
        if k.trim().is_empty() {
            println!("✓ removed the API key file");
        } else {
            println!("✓ stored the API key in {} (0600)", kp.display());
        }
    }

    let cfg = Config::load()?;
    let ok = if a.skip_test {
        true
    } else {
        println!("\nT-minus 0: live check …");
        crate::doctor::check_endpoints(&cfg).await
    };
    if a.skip_test {
        println!("\nLive check skipped; run `capcom doctor` when your servers are up.");
    } else if ok {
        println!("\n🚀 All systems go.");
    } else {
        println!("\nHouston, we have a problem: config saved, but a check failed. Fix it and run `capcom doctor`.");
    }
    if cfg!(target_os = "macos") {
        println!("Next: open Capcom.app (or Quit + reopen it to load the new config), grant Microphone,");
        println!("      Accessibility and Input Monitoring, then hold right ⌃ and talk.");
    } else {
        println!("Next: `systemctl --user restart capcom` and use your dictation keybind (see README).");
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> SetupArgs {
        SetupArgs::default()
    }

    #[test]
    fn presets_are_well_formed() {
        for p in ASR_PRESETS {
            assert!(p.url.ends_with("/v1/audio/transcriptions"), "{}", p.id);
            assert!(!p.model.is_empty(), "{}", p.id);
        }
        for p in LLM_PRESETS {
            assert!(p.url.ends_with("/v1/chat/completions"), "{}", p.id);
        }
        assert_eq!(find(ASR_PRESETS, "WHISPER-CPP").unwrap().id, "whisper-cpp");
    }

    #[test]
    fn flags_with_presets() {
        let a = SetupArgs { asr_preset: Some("vllm".into()), llm_preset: Some("ollama".into()), ..args() };
        let ans = resolve_flags(&a, &Config::default()).unwrap();
        assert_eq!(ans.asr_url, "http://localhost:8000/v1/audio/transcriptions");
        assert_eq!(ans.llm_model, "llama3.2:3b");
        assert!(ans.cleanup);
        assert_eq!(ans.key, None);
    }

    #[test]
    fn flags_override_presets_and_no_cleanup() {
        let a = SetupArgs {
            asr_preset: Some("speaches".into()),
            asr_url: Some("http://gpu:9000/v1/audio/transcriptions".into()),
            no_cleanup: true,
            api_key: Some("k".into()),
            ..args()
        };
        let ans = resolve_flags(&a, &Config::default()).unwrap();
        assert_eq!(ans.asr_url, "http://gpu:9000/v1/audio/transcriptions");
        assert_eq!(ans.asr_model, "deepdml/faster-whisper-large-v3-turbo-ct2");
        assert!(!ans.cleanup);
        assert_eq!(ans.key.as_deref(), Some("k"));
    }

    #[test]
    fn flags_errors_are_helpful() {
        let e = resolve_flags(&args(), &Config::default()).unwrap_err().to_string();
        assert!(e.contains("--asr-preset"));
        let a = SetupArgs { asr_preset: Some("vllm".into()), ..args() };
        assert!(resolve_flags(&a, &Config::default()).unwrap_err().to_string().contains("--no-cleanup"));
        let a = SetupArgs { asr_preset: Some("vllm".into()), llm_preset: Some("lmstudio".into()), ..args() };
        assert!(resolve_flags(&a, &Config::default()).is_err(), "LM Studio needs --llm-model");
        let a = SetupArgs { asr_preset: Some("nope".into()), ..args() };
        assert!(resolve_flags(&a, &Config::default()).unwrap_err().to_string().contains("parakeet"));
    }

    #[test]
    fn existing_config_is_kept() {
        let mut base = Config::default();
        base.server.asr_url = "http://a/v1/audio/transcriptions".into();
        base.server.asr_model = "m1".into();
        base.server.llm_url = "http://b/v1/chat/completions".into();
        base.server.llm_model = "m2".into();
        let ans = resolve_flags(&args(), &base).unwrap();
        assert_eq!((ans.asr_model.as_str(), ans.llm_model.as_str(), ans.cleanup), ("m1", "m2", true));
    }

    #[test]
    fn interactive_script() {
        // ASR: pick 3 (whisper.cpp), keep URL, keep model; LLM: by id "ollama", custom model; key: none.
        let input = b"3\n\n\nollama\n\ngemma3:4b\n\n" as &[u8];
        let mut out = vec![];
        let ans = resolve_interactive(&args(), &Config::default(), &mut Prompter::new(input, &mut out)).unwrap();
        assert_eq!(ans.asr_url, "http://localhost:8080/v1/audio/transcriptions");
        assert_eq!(ans.llm_url, "http://localhost:11434/v1/chat/completions");
        assert_eq!(ans.llm_model, "gemma3:4b");
        assert!(ans.cleanup);
        assert_eq!(ans.key, None);
        assert!(String::from_utf8(out).unwrap().contains("Speech-to-text"));
    }

    #[test]
    fn interactive_no_cleanup_and_retry() {
        let none = (LLM_PRESETS.len() + 2).to_string();
        let script = format!("99\n1\n\n\n{none}\nsecret\n");
        let mut out = vec![];
        let ans = resolve_interactive(&args(), &Config::default(), &mut Prompter::new(script.as_bytes(), &mut out)).unwrap();
        assert_eq!(ans.asr_model, "openai/whisper-large-v3-turbo");
        assert!(!ans.cleanup);
        assert_eq!(ans.key.as_deref(), Some("secret"));
    }

    #[test]
    fn interactive_eof_errors() {
        let mut out = vec![];
        assert!(resolve_interactive(&args(), &Config::default(), &mut Prompter::new(b"" as &[u8], &mut out)).is_err());
    }
}
