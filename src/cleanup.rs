//! LLM cleanup client (OpenAI chat-completions) plus the pure helpers around it.

use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    #[error("cleanup HTTP error: {0}")]
    Http(String),
    #[error("cleanup server returned {0}: {1}")]
    Status(u16, String),
    #[error("cleanup response unparseable: {0}")]
    Parse(String),
    #[error("cleanup incomplete (finish_reason={0})")]
    NotStopped(String),
    #[error("cleanup returned empty text")]
    Empty,
}

#[derive(Clone)]
pub struct CleanupClient {
    http: Client,
    url: String,
    model: String,
    key: String,
    system_prompt: String,
}

impl CleanupClient {
    pub fn new(http: Client, url: &str, model: &str, key: &str, system_prompt: String) -> Self {
        Self { http, url: url.into(), model: model.into(), key: key.into(), system_prompt }
    }

    /// Clean up `text`. Any error means the caller should use the raw text.
    pub async fn clean(&self, text: &str) -> Result<String, CleanupError> {
        let body = request_body(&self.model, &self.system_prompt, text);
        let mut req = self.http.post(&self.url);
        if !self.key.is_empty() {
            req = req.bearer_auth(&self.key);
        }
        let resp = req
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| CleanupError::Http(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| CleanupError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(CleanupError::Status(status.as_u16(), text.chars().take(300).collect()));
        }
        parse_response(&text)
    }
}

/// Rough token estimate (~4 chars per token).
pub fn approx_tokens(text: &str) -> usize {
    text.chars().count() / 4 + 1
}

/// `max(256, 3 x approx input tokens)`.
pub fn max_tokens_for(text: &str) -> usize {
    (approx_tokens(text) * 3).max(256)
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Skip cleanup when the transcript has fewer than `skip_under_words` words.
pub fn should_skip(text: &str, skip_under_words: usize) -> bool {
    word_count(text) < skip_under_words
}

pub fn request_body(model: &str, system: &str, text: &str) -> Value {
    json!({
        "model": model,
        "temperature": 0,
        "max_tokens": max_tokens_for(text),
        "stream": false,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": text},
        ],
    })
}

/// Remove `<think>…</think>` blocks. Also handles a template-prefilled opener
/// (only `</think>` present: keep what follows) and an unterminated block (drop the rest).
pub fn strip_think(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    // Closer without opener before it: everything up to the last closer is reasoning.
    if let Some(c) = rest.rfind("</think>") {
        let before = &rest[..c];
        if !before.contains("<think>") {
            rest = &rest[c + "</think>".len()..];
        }
    }
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => rest = &rest[i + j + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Parse a chat-completions response body; enforce finish_reason == "stop".
pub fn parse_response(body: &str) -> Result<String, CleanupError> {
    let v: Value = serde_json::from_str(body).map_err(|e| CleanupError::Parse(e.to_string()))?;
    let choice = v.pointer("/choices/0").ok_or_else(|| CleanupError::Parse("no choices".into()))?;
    let finish = choice.get("finish_reason").and_then(Value::as_str).unwrap_or("none");
    if finish != "stop" {
        return Err(CleanupError::NotStopped(finish.to_string()));
    }
    let content = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| CleanupError::Parse("no message content".into()))?;
    let cleaned = strip_think(content);
    if cleaned.is_empty() {
        return Err(CleanupError::Empty);
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_stripping() {
        assert_eq!(strip_think("<think>hmm</think>Hello."), "Hello.");
        assert_eq!(strip_think("a <think>x\ny</think> b"), "a  b");
        assert_eq!(strip_think("reasoning</think>\n\nAnswer"), "Answer");
        assert_eq!(strip_think("Answer<think>never closed"), "Answer");
        assert_eq!(strip_think("plain"), "plain");
    }

    #[test]
    fn finish_reason() {
        let ok = r#"{"choices":[{"finish_reason":"stop","message":{"content":" Hi. "}}]}"#;
        assert_eq!(parse_response(ok).unwrap(), "Hi.");
        let len = r#"{"choices":[{"finish_reason":"length","message":{"content":"Hi"}}]}"#;
        assert!(matches!(parse_response(len), Err(CleanupError::NotStopped(r)) if r == "length"));
        let none = r#"{"choices":[{"message":{"content":"Hi"}}]}"#;
        assert!(matches!(parse_response(none), Err(CleanupError::NotStopped(_))));
        let empty = r#"{"choices":[{"finish_reason":"stop","message":{"content":"<think>x</think>"}}]}"#;
        assert!(matches!(parse_response(empty), Err(CleanupError::Empty)));
        assert!(matches!(parse_response("{}"), Err(CleanupError::Parse(_))));
    }

    #[test]
    fn skip_short() {
        assert!(should_skip("one two three", 4));
        assert!(!should_skip("one two three four", 4));
        assert!(should_skip("", 4));
        assert!(!should_skip("a", 0));
    }

    #[test]
    fn token_budget() {
        assert_eq!(max_tokens_for("hi"), 256);
        let long = "x".repeat(4000);
        assert_eq!(max_tokens_for(&long), 3 * (1000 + 1));
    }

    #[test]
    fn body_shape() {
        let b = request_body("m", "sys", "hello");
        assert_eq!(b["temperature"], 0);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "hello");
    }
}
