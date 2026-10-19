//! Configuration: TOML schema (see `docs/architecture.md`), defaults, loading, key handling.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What to tell a user whose install is not configured.
pub const SETUP_HINT: &str = "run `capcom setup` (or menu bar > Set up Capcom…)";

/// Default cleanup system prompt, embedded at build time.
pub const DEFAULT_CLEANUP_PROMPT: &str = include_str!("../assets/cleanup-prompt.txt");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub cleanup: CleanupConfig,
    pub audio: AudioConfig,
    pub hotkey: HotkeyConfig,
    pub history: HistoryConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ServerConfig {
    pub asr_url: String,
    pub asr_model: String,
    pub llm_url: String,
    pub llm_model: String,
    pub api_key_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CleanupConfig {
    pub enabled: bool,
    pub prompt_file: String,
    pub skip_under_words: usize,
    pub vocabulary: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudioConfig {
    /// `on_demand` or `always_on` (pre-roll).
    pub mic_mode: String,
    /// `default`, or an input device name (`capcom devices`): an exact name (any case) wins, else
    /// the first name containing it.
    pub device: String,
    pub min_clip_ms: u64,
    /// Pre-roll kept while the mic is open in `always_on` mode.
    pub preroll_ms: u64,
    /// Hard cap on one recording; hitting it auto-stops, processes and notifies.
    pub max_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HotkeyConfig {
    pub push_to_talk: String,
    pub cancel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub keep: usize,
}

/// Where the floating pill appears when dictation starts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PillPosition {
    /// Next to the text insertion point (Accessibility API), falling back to the focused field,
    /// the focused window, then `bottom`.
    #[default]
    Caret,
    /// Bottom-center of the screen under the mouse.
    Bottom,
}

impl std::str::FromStr for PillPosition {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "caret" => Ok(Self::Caret),
            "bottom" => Ok(Self::Bottom),
            _ => Err(anyhow::anyhow!("pill position must be \"caret\" or \"bottom\", got {s:?}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct UiConfig {
    /// `caret` (default) or `bottom`.
    pub pill_position: PillPosition,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            // Deliberately empty: an unconfigured install asks for `capcom setup` instead of
            // silently probing some default host.
            asr_url: String::new(),
            asr_model: String::new(),
            llm_url: String::new(),
            llm_model: String::new(),
            api_key_file: "~/.config/capcom/api_key".into(),
        }
    }
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt_file: "~/.config/capcom/cleanup-prompt.txt".into(),
            skip_under_words: 4,
            vocabulary: vec![],
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { mic_mode: "on_demand".into(), device: "default".into(), min_clip_ms: 500, preroll_ms: 400, max_seconds: 600 }
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self { push_to_talk: "ctrlright".into(), cancel: "escape".into() }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self { enabled: true, keep: 500 }
    }
}

/// Expand a leading `~` / `~/` to the home directory.
pub fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(p)
}

/// `~/.config/capcom` (same on macOS and Linux, per architecture.md).
pub fn config_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".config").join("capcom")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

impl Config {
    pub fn from_toml(s: &str) -> Result<Self> {
        toml::from_str(s).context("parsing config TOML")
    }

    /// Load from the default path; defaults if the file does not exist.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml(&s).with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// API key from `CAPCOM_API_KEY`, else the `api_key_file`. `None` (no key, no Bearer header)
    /// when neither is set: many local servers need no key.
    pub fn api_key(&self) -> Result<Option<String>> {
        if let Ok(k) = std::env::var("CAPCOM_API_KEY") {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Ok(Some(k));
            }
        }
        if self.server.api_key_file.trim().is_empty() {
            return Ok(None);
        }
        let p = expand_tilde(&self.server.api_key_file);
        match std::fs::read_to_string(&p) {
            Ok(k) => Ok(Some(k.trim().to_string()).filter(|k| !k.is_empty())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read api_key_file {}", p.display())),
        }
    }

    /// `Some(reason)` when the endpoints are not configured yet (fresh install).
    pub fn setup_problem(&self) -> Option<String> {
        let s = &self.server;
        let mut missing = vec![];
        if s.asr_url.trim().is_empty() {
            missing.push("server.asr_url");
        }
        if s.asr_model.trim().is_empty() {
            missing.push("server.asr_model");
        }
        if self.cleanup.enabled && s.llm_url.trim().is_empty() {
            missing.push("server.llm_url");
        }
        if self.cleanup.enabled && s.llm_model.trim().is_empty() {
            missing.push("server.llm_model");
        }
        (!missing.is_empty()).then(|| format!("Capcom is not set up yet ({} missing): {SETUP_HINT}", missing.join(", ")))
    }

    /// System prompt: prompt_file if readable, else embedded default; vocabulary appended.
    pub fn system_prompt(&self) -> String {
        let base = std::fs::read_to_string(expand_tilde(&self.cleanup.prompt_file))
            .unwrap_or_else(|_| DEFAULT_CLEANUP_PROMPT.to_string());
        build_system_prompt(&base, &self.cleanup.vocabulary)
    }

    /// TOML with secrets redacted (the key itself is never in the config, but be explicit).
    pub fn show_redacted(&self) -> Result<String> {
        let mut out = self.to_toml()?;
        let key_state = match self.api_key() {
            Ok(Some(k)) => format!("# api key: present ({} chars, redacted)\n", k.chars().count()),
            Ok(None) => "# api key: none (no Authorization header is sent)\n".to_string(),
            Err(e) => format!("# api key: UNREADABLE ({e})\n"),
        };
        if let Some(p) = self.setup_problem() {
            out.push_str(&format!("# {p}\n"));
        }
        out.push_str(&key_state);
        Ok(out)
    }
}

pub fn build_system_prompt(base: &str, vocabulary: &[String]) -> String {
    let mut p = base.trim_end().to_string();
    if !vocabulary.is_empty() {
        p.push_str("\n\nAdditional vocabulary (spell exactly like this): ");
        p.push_str(&vocabulary.join(", "));
        p.push('.');
    }
    p
}

/// Write `contents` to `path` with mode 0600 (creating parent dirs).
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty() && !p.exists()) {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Return `text` with `[audio] device` set to `name`, touching nothing else (comments, key
/// order and `vocabulary` survive). Edits the existing `device` line, else adds one under
/// `[audio]`, else appends an `[audio]` table. The result is re-parsed and compared with the
/// original, so an unusual layout (dotted keys, inline tables) is an error, never a corrupt file.
pub fn set_audio_device_text(text: &str, name: &str) -> Result<String> {
    let name = name.trim();
    let value = toml::Value::String(name.to_string()).to_string();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let mut in_audio = false;
    let mut in_multiline = false;
    let mut header_at = None;
    let mut device_at = None;
    for (i, line) in lines.iter().enumerate() {
        let was_multiline = in_multiline;
        // An odd number of triple quotes opens or closes a multi-line string.
        if (line.matches("\"\"\"").count() + line.matches("'''").count()) % 2 == 1 {
            in_multiline = !in_multiline;
        }
        if was_multiline {
            continue;
        }
        let t = line.trim_start();
        if t.starts_with('[') {
            let table = t.trim_start_matches('[').split(']').next().unwrap_or("").trim();
            in_audio = t.starts_with("[audio]") || (!t.starts_with("[[") && table == "audio");
            if in_audio && header_at.is_none() {
                header_at = Some(i);
            }
        } else if in_audio && device_at.is_none() {
            if let Some(rest) = t.strip_prefix("device") {
                if rest.trim_start().starts_with('=') {
                    device_at = Some(i);
                }
            }
        }
    }
    if let Some(i) = device_at {
        let line = &lines[i];
        let indent = &line[..line.len() - line.trim_start().len()];
        let after_eq = line.split_once('=').map(|x| x.1).unwrap_or("").trim_start();
        // Keep a trailing `# comment` when the old value is a plain one-line string.
        let tail = match after_eq.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let mut esc = false;
                let end = after_eq[1..].char_indices().find(|&(_, c)| {
                    let hit = c == q && !(esc && q == '"');
                    esc = c == '\\' && !esc;
                    hit
                });
                end.map(|(j, _)| after_eq[j + 2..].to_string()).unwrap_or_default()
            }
            _ => String::new(),
        };
        lines[i] = format!("{indent}device = {value}{tail}");
    } else if let Some(i) = header_at {
        lines.insert(i + 1, format!("device = {value}"));
    } else {
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("[audio]".into());
        lines.push(format!("device = {value}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');

    let mut expected = Config::from_toml(text)?;
    expected.audio.device = name.to_string();
    let got = Config::from_toml(&out).context("edited config no longer parses")?;
    anyhow::ensure!(got == expected, "cannot edit [audio] device in place; set it by hand in the config file");
    Ok(out)
}

/// Set `[audio] device` in the config file at `path` (created with defaults' layout if absent),
/// keeping the file private (0600).
pub fn save_audio_device(path: &Path, name: &str) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default().to_toml()?,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    write_private(path, &set_audio_device_text(&text, name)?)
}

/// `capcom config init`: write default config + cleanup prompt. Returns paths written.
pub fn init(force: bool) -> Result<Vec<PathBuf>> {
    let mut written = vec![];
    let cfg_path = config_path();
    let cfg = Config::default();
    if force || !cfg_path.exists() {
        write_private(&cfg_path, &cfg.to_toml()?)?;
        written.push(cfg_path);
    }
    let prompt_path = expand_tilde(&cfg.cleanup.prompt_file);
    if force || !prompt_path.exists() {
        write_private(&prompt_path, DEFAULT_CLEANUP_PROMPT)?;
        written.push(prompt_path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unconfigured() {
        let c = Config::default();
        assert!(c.server.asr_url.is_empty() && c.server.llm_url.is_empty());
        assert!(c.setup_problem().unwrap().contains("capcom setup"));
        assert!(c.cleanup.vocabulary.is_empty());
        assert_eq!(c.cleanup.skip_under_words, 4);
        assert_eq!(c.audio.min_clip_ms, 500);
        assert_eq!(c.history.keep, 500);
        assert_eq!(c.ui.pill_position, PillPosition::Caret);
    }

    #[test]
    fn pill_position_parses_and_defaults() {
        // Config files written before `[ui]` existed keep working.
        assert_eq!(Config::from_toml("[history]\nkeep = 3\n").unwrap().ui.pill_position, PillPosition::Caret);
        let c = Config::from_toml("[ui]\npill_position = \"bottom\"\n").unwrap();
        assert_eq!(c.ui.pill_position, PillPosition::Bottom);
        assert!(Config::from_toml("[ui]\npill_position = \"middle\"\n").is_err());
        assert!(c.to_toml().unwrap().contains("pill_position = \"bottom\""));
        assert_eq!("Caret".parse::<PillPosition>().unwrap(), PillPosition::Caret);
        assert!("x".parse::<PillPosition>().is_err());
    }

    #[test]
    fn empty_toml_is_defaults() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
    }

    #[test]
    fn partial_override() {
        let c = Config::from_toml("[cleanup]\nenabled = false\nvocabulary = [\"X\"]\n[history]\nkeep = 3\n").unwrap();
        assert!(!c.cleanup.enabled);
        assert_eq!(c.cleanup.vocabulary, vec!["X"]);
        assert_eq!(c.history.keep, 3);
        assert_eq!(c.cleanup.skip_under_words, 4);
    }

    #[test]
    fn roundtrip() {
        let c = Config::default();
        assert_eq!(Config::from_toml(&c.to_toml().unwrap()).unwrap(), c);
    }

    #[test]
    fn setup_problem_respects_cleanup_toggle() {
        let mut c = Config::default();
        c.server.asr_url = "http://localhost:8000/v1/audio/transcriptions".into();
        c.server.asr_model = "m".into();
        assert!(c.setup_problem().unwrap().contains("llm_url"));
        c.cleanup.enabled = false;
        assert_eq!(c.setup_problem(), None);
    }

    #[test]
    fn missing_key_file_is_no_key() {
        let mut c = Config::default();
        c.server.api_key_file = "/nonexistent/capcom/api_key".into();
        if std::env::var_os("CAPCOM_API_KEY").is_none() {
            assert_eq!(c.api_key().unwrap(), None);
        }
    }

    #[test]
    fn set_device_edits_the_line_and_keeps_the_rest() {
        let src = "# my config\n[cleanup]\nvocabulary = [\n  \"Acme\",\n  \"[audio]\",\n]\n\n[audio]\nmic_mode = \"always_on\"\ndevice = \"default\"  # which mic\nmin_clip_ms = 700\n\n[history]\nkeep = 3\n";
        let out = set_audio_device_text(src, "Shure MV7").unwrap();
        assert_eq!(out, src.replace("device = \"default\"", "device = \"Shure MV7\""));
        let c = Config::from_toml(&out).unwrap();
        assert_eq!((c.audio.device.as_str(), c.audio.min_clip_ms, c.history.keep), ("Shure MV7", 700, 3));
        assert_eq!(c.cleanup.vocabulary, vec!["Acme", "[audio]"]);
        assert_eq!(set_audio_device_text(&out, "default").unwrap(), src);
    }

    #[test]
    fn set_device_adds_missing_key_or_table() {
        let out = set_audio_device_text("[audio]\nmic_mode = \"on_demand\"\n[history]\nkeep = 2\n", "AirPods").unwrap();
        assert_eq!(out, "[audio]\ndevice = \"AirPods\"\nmic_mode = \"on_demand\"\n[history]\nkeep = 2\n");
        let out = set_audio_device_text("[history]\nkeep = 2\n", "AirPods").unwrap();
        assert_eq!(out, "[history]\nkeep = 2\n\n[audio]\ndevice = \"AirPods\"\n");
        assert_eq!(Config::from_toml(&set_audio_device_text("", "x").unwrap()).unwrap().audio.device, "x");
    }

    #[test]
    fn set_device_quotes_awkward_names_and_rejects_odd_layouts() {
        let name = "Mic \"A\" \\ B";
        let out = set_audio_device_text("[audio]\ndevice = 'old' # c\n", name).unwrap();
        assert_eq!(Config::from_toml(&out).unwrap().audio.device, name);
        assert!(out.ends_with(" # c\n"));
        assert!(set_audio_device_text("audio = { device = \"a\" }\n", "b").is_err());
    }

    #[test]
    fn save_device_keeps_file_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        std::fs::write(&p, "[history]\nkeep = 9\n").unwrap();
        save_audio_device(&p, "Studio Mic").unwrap();
        let c = Config::load_from(&p).unwrap();
        assert_eq!((c.audio.device.as_str(), c.history.keep), ("Studio Mic", 9));
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn tilde() {
        let h = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~/a/b"), h.join("a/b"));
        assert_eq!(expand_tilde("/x/y"), PathBuf::from("/x/y"));
    }

    #[test]
    fn vocabulary_appended() {
        let p = build_system_prompt("Base.\n", &["A".into(), "B".into()]);
        assert!(p.starts_with("Base."));
        assert!(p.ends_with("A, B."));
        assert_eq!(build_system_prompt("Base.\n", &[]), "Base.");
    }

    #[test]
    fn default_prompt_embedded() {
        assert!(DEFAULT_CLEANUP_PROMPT.contains("dictation cleanup engine"));
    }
}
