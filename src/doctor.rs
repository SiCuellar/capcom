//! `capcom doctor`: checklist of config, key, endpoints and daemon.

use crate::asr::{http_client, AsrClient};
use crate::audio::wav;
use crate::cleanup::CleanupClient;
use crate::config::{self, Config};
use crate::ipc;
use std::time::Instant;

fn line(ok: bool, label: &str, detail: impl AsRef<str>) -> bool {
    println!("[{}] {label}: {}", if ok { " ok " } else { "FAIL" }, detail.as_ref());
    ok
}

/// Returns true if every required check passed.
pub async fn run(cfg: &Config) -> bool {
    let mut all = true;
    let cp = config::config_path();
    if cp.exists() {
        line(true, "config", cp.display().to_string());
    } else {
        println!("[warn] config: {} not found (run `capcom setup`)", cp.display());
    }

    let pf = config::expand_tilde(&cfg.cleanup.prompt_file);
    if pf.exists() {
        line(true, "cleanup prompt", pf.display().to_string());
    } else {
        println!("[info] cleanup prompt: {} missing, using the built-in prompt", pf.display());
    }

    all &= check_endpoints(cfg).await;

    let sock = ipc::socket_path();
    match ipc::send(&sock, &ipc::Request::Status).await {
        Ok(r) => line(true, "daemon", format!("running, state={}", r.state.map(|s| s.as_str()).unwrap_or("?"))),
        Err(_) => {
            println!("[info] daemon: not running ({})", sock.display());
            true
        }
    };
    #[cfg(target_os = "macos")]
    {
        all &= crate::platform::macos::doctor::run();
    }
    #[cfg(target_os = "linux")]
    {
        all &= crate::platform::linux::print_doctor();
    }
    all
}

/// Key + live round trip to both endpoints (0.5 s of silence to ASR, a short sentence to the
/// LLM). Shared by `doctor` and `setup`. Returns true if everything required passed.
pub async fn check_endpoints(cfg: &Config) -> bool {
    if let Some(p) = cfg.setup_problem() {
        return line(false, "endpoints", p);
    }
    let key = match cfg.api_key() {
        Ok(Some(k)) => {
            line(true, "api key", "present");
            k
        }
        Ok(None) => {
            println!("[info] api key: none set (fine for local servers; hosted APIs answer 401)");
            String::new()
        }
        Err(e) => return line(false, "api key", e.to_string()),
    };

    let mut all = true;
    let http = http_client();
    let silence = wav::encode_wav_16k_mono(&vec![0i16; 8000]).unwrap_or_default();
    let asr = AsrClient::new(http.clone(), &cfg.server.asr_url, &cfg.server.asr_model, &key);
    let t = Instant::now();
    all &= match asr.transcribe(silence).await {
        Ok(_) => line(true, "ASR endpoint", format!("{} ({} ms)", cfg.server.asr_url, t.elapsed().as_millis())),
        Err(e) => line(false, "ASR endpoint", format!("{}: {e}", cfg.server.asr_url)),
    };
    if !cfg.cleanup.enabled {
        println!("[info] LLM cleanup: disabled (raw transcripts are pasted)");
        return all;
    }
    let llm = CleanupClient::new(http, &cfg.server.llm_url, &cfg.server.llm_model, &key, "Repeat the user's text.".into());
    let t = Instant::now();
    all &= match llm.clean("hello there doctor").await {
        Ok(_) => line(true, "LLM endpoint", format!("{} ({} ms)", cfg.server.llm_url, t.elapsed().as_millis())),
        Err(e) => line(false, "LLM endpoint", format!("{}: {e}", cfg.server.llm_url)),
    };
    all
}
