//! WAV in, final text out: ASR then (optional) cleanup, with timings and graceful fallback.

use crate::asr::AsrClient;
use crate::cleanup::{self, CleanupClient};
use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Clone)]
pub struct Clients {
    pub asr: AsrClient,
    pub cleanup: CleanupClient,
}

impl Clients {
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let key = cfg.api_key()?.unwrap_or_default();
        let http = crate::asr::http_client();
        Ok(Self {
            asr: AsrClient::new(http.clone(), &cfg.server.asr_url, &cfg.server.asr_model, &key),
            cleanup: CleanupClient::new(http, &cfg.server.llm_url, &cfg.server.llm_model, &key, cfg.system_prompt()),
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Timings {
    pub asr_ms: u64,
    pub cleanup_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Outcome {
    pub raw: String,
    pub cleaned: Option<String>,
    /// What should be pasted: cleaned if available, else raw.
    pub final_text: String,
    pub timings: Timings,
    pub cleanup_used: bool,
    /// Non-fatal (cleanup failed -> raw used) or fatal (ASR failed -> empty text).
    pub error: Option<String>,
}

pub async fn process(wav: Vec<u8>, cfg: &Config, clients: &Clients) -> Outcome {
    let mut out = Outcome::default();

    let t = Instant::now();
    let raw = clients.asr.transcribe(wav).await;
    out.timings.asr_ms = t.elapsed().as_millis() as u64;
    match raw {
        Ok(r) => out.raw = r,
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    }
    out.final_text = out.raw.clone();

    if !cfg.cleanup.enabled || out.raw.is_empty() || cleanup::should_skip(&out.raw, cfg.cleanup.skip_under_words) {
        return out;
    }

    let t = Instant::now();
    let res = clients.cleanup.clean(&out.raw).await;
    out.timings.cleanup_ms = t.elapsed().as_millis() as u64;
    match res {
        Ok(c) if dropped_too_much(&out.raw, &c) => {
            // The LLM silently deleted content (seen live: a repeated paragraph was merged
            // away with finish_reason=stop). Never lose words: paste the raw text instead.
            tracing::warn!("cleanup dropped too many words, using raw text");
            out.error = Some("cleanup dropped too many words; used raw text".into());
            out.cleaned = Some(c);
        }
        Ok(c) => {
            out.final_text = c.clone();
            out.cleaned = Some(c);
            out.cleanup_used = true;
        }
        Err(e) => {
            tracing::warn!("cleanup failed, using raw text: {e}");
            out.error = Some(e.to_string());
        }
    }
    out
}

/// Below this many raw words, filler removal alone can legitimately cut ~half the text.
const GUARD_MIN_RAW_WORDS: usize = 20;
/// Cleaned text must keep at least this share of the raw word count.
const GUARD_MIN_KEEP_RATIO: f64 = 0.6;

fn dropped_too_much(raw: &str, cleaned: &str) -> bool {
    let raw_words = raw.split_whitespace().count();
    raw_words >= GUARD_MIN_RAW_WORDS
        && (cleaned.split_whitespace().count() as f64) < raw_words as f64 * GUARD_MIN_KEEP_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::http_client;
    use crate::testutil::mock_http;

    fn clients(asr_url: &str, llm_url: &str) -> Clients {
        let h = http_client();
        Clients {
            asr: AsrClient::new(h.clone(), asr_url, "m", "k"),
            cleanup: CleanupClient::new(h, llm_url, "m", "k", "sys".into()),
        }
    }

    #[tokio::test]
    async fn full_path_and_fallback() {
        let asr = mock_http(vec![(200, r#"{"text":"um so this is a longer sentence"}"#.into())]).await;
        let llm = mock_http(vec![(
            200,
            r#"{"choices":[{"finish_reason":"stop","message":{"content":"This is a longer sentence."}}]}"#.into(),
        )])
        .await;
        let o = process(vec![1], &Config::default(), &clients(&asr, &llm)).await;
        assert!(o.cleanup_used);
        assert_eq!(o.final_text, "This is a longer sentence.");

        // finish_reason=length -> raw text used
        let asr = mock_http(vec![(200, r#"{"text":"um so this is a longer sentence"}"#.into())]).await;
        let llm = mock_http(vec![(200, r#"{"choices":[{"finish_reason":"length","message":{"content":"Th"}}]}"#.into())]).await;
        let o = process(vec![1], &Config::default(), &clients(&asr, &llm)).await;
        assert!(!o.cleanup_used);
        assert_eq!(o.final_text, "um so this is a longer sentence");
        assert!(o.error.unwrap().contains("length"));
    }

    #[test]
    fn drop_guard() {
        let long = "word ".repeat(40);
        assert!(dropped_too_much(&long, &"word ".repeat(20)));
        assert!(!dropped_too_much(&long, &"word ".repeat(30)));
        // Short, filler-heavy dictation may legitimately shrink a lot.
        assert!(!dropped_too_much(
            "um so I was thinking we should uh meet at like 2pm actually no 3pm tomorrow",
            "I was thinking we should meet at 3 PM tomorrow."
        ));
    }

    #[tokio::test]
    async fn short_skips_cleanup() {
        let asr = mock_http(vec![(200, r#"{"text":"ok thanks"}"#.into())]).await;
        // LLM URL unreachable on purpose: must not be called.
        let o = process(vec![1], &Config::default(), &clients(&asr, "http://127.0.0.1:1")).await;
        assert!(!o.cleanup_used);
        assert!(o.error.is_none());
        assert_eq!(o.final_text, "ok thanks");
    }

    #[tokio::test]
    async fn asr_failure_is_reported() {
        let asr = mock_http(vec![(401, "{}".into())]).await;
        let o = process(vec![1], &Config::default(), &clients(&asr, "http://127.0.0.1:1")).await;
        assert!(o.final_text.is_empty());
        assert!(o.error.unwrap().contains("bad API key"));
    }
}
