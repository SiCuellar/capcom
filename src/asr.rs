//! ASR client: multipart POST of a WAV to an OpenAI-compatible transcription endpoint.

use reqwest::{multipart, Client, StatusCode};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error("bad API key (401 from ASR server)")]
    Unauthorized,
    #[error("ASR server rejected the request (400): {0}")]
    BadRequest(String),
    #[error("ASR server error {0}: {1}")]
    Status(u16, String),
    #[error("cannot reach ASR server: {0}")]
    Connect(String),
    #[error("ASR request failed: {0}")]
    Http(String),
    #[error("unparseable ASR response: {0}")]
    Parse(String),
}

#[derive(Clone)]
pub struct AsrClient {
    http: Client,
    url: String,
    model: String,
    key: String,
}

#[derive(Deserialize)]
struct AsrResponse {
    text: String,
}

/// Build the shared HTTP client (keep-alive pool; no system proxy lookups).
pub fn http_client() -> Client {
    Client::builder()
        .pool_idle_timeout(Duration::from_secs(300))
        .tcp_keepalive(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .expect("reqwest client")
}

impl AsrClient {
    pub fn new(http: Client, url: &str, model: &str, key: &str) -> Self {
        Self { http, url: url.into(), model: model.into(), key: key.into() }
    }

    /// Transcribe in-memory WAV bytes. 30 s timeout; one retry on connect error.
    pub async fn transcribe(&self, wav: Vec<u8>) -> Result<String, AsrError> {
        match self.try_once(&wav).await {
            Err(AsrError::Connect(e)) => {
                tracing::warn!("ASR connect error ({e}); retrying once");
                tokio::time::sleep(Duration::from_millis(150)).await;
                self.try_once(&wav).await
            }
            r => r,
        }
    }

    async fn try_once(&self, wav: &[u8]) -> Result<String, AsrError> {
        let part = multipart::Part::bytes(wav.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| AsrError::Http(e.to_string()))?;
        let form = multipart::Form::new()
            .part("file", part)
            .text("model", self.model.clone())
            .text("response_format", "json");
        let mut req = self.http.post(&self.url);
        if !self.key.is_empty() {
            req = req.bearer_auth(&self.key);
        }
        let resp = req
            .multipart(form)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| if e.is_connect() { AsrError::Connect(e.to_string()) } else { AsrError::Http(e.to_string()) })?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| AsrError::Http(e.to_string()))?;
        parse_response(status, &body)
    }
}

pub(crate) fn parse_response(status: StatusCode, body: &str) -> Result<String, AsrError> {
    match status {
        s if s.is_success() => serde_json::from_str::<AsrResponse>(body)
            .map(|r| r.text.trim().to_string())
            .map_err(|e| AsrError::Parse(format!("{e}: {}", truncate(body, 200)))),
        StatusCode::UNAUTHORIZED => Err(AsrError::Unauthorized),
        StatusCode::BAD_REQUEST => Err(AsrError::BadRequest(server_message(body))),
        s => Err(AsrError::Status(s.as_u16(), server_message(body))),
    }
}

/// Pull a human message out of `{"detail":..}` / `{"error":{"message":..}}` bodies.
fn server_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        for m in [
            v.pointer("/detail"),
            v.pointer("/error/message"),
            v.pointer("/message"),
            v.pointer("/error"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(s) = m.as_str() {
                return s.to_string();
            }
            if !m.is_null() {
                return m.to_string();
            }
        }
    }
    truncate(body, 300)
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::mock_http;

    #[test]
    fn parses_ok() {
        assert_eq!(parse_response(StatusCode::OK, r#"{"text":" hello "}"#).unwrap(), "hello");
    }

    #[test]
    fn maps_errors() {
        assert!(matches!(parse_response(StatusCode::UNAUTHORIZED, ""), Err(AsrError::Unauthorized)));
        let e = parse_response(StatusCode::BAD_REQUEST, r#"{"detail":"unknown model x"}"#).unwrap_err();
        assert!(e.to_string().contains("unknown model x"));
        assert!(matches!(parse_response(StatusCode::OK, "nope"), Err(AsrError::Parse(_))));
    }

    #[tokio::test]
    async fn live_against_mock() {
        let base = mock_http(vec![(200, r#"{"text":"hi there"}"#.into())]).await;
        let c = AsrClient::new(http_client(), &base, "m", "k");
        assert_eq!(c.transcribe(vec![0; 100]).await.unwrap(), "hi there");
    }

    #[tokio::test]
    async fn mock_401() {
        let base = mock_http(vec![(401, "{}".into())]).await;
        let c = AsrClient::new(http_client(), &base, "m", "k");
        assert!(matches!(c.transcribe(vec![0; 10]).await, Err(AsrError::Unauthorized)));
    }
}
