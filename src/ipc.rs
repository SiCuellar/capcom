//! Unix-socket JSON-lines IPC between the CLI and the daemon.
//! One request line -> one response line per connection.

use crate::state::State;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Start,
    Stop,
    Toggle,
    Cancel,
    Status,
    PasteLast,
    History {
        #[serde(default = "default_n")]
        n: usize,
    },
    /// Debug: run a WAV file through the daemon's state machine and pipeline
    /// (stands in for a recording until an `AudioSource` exists).
    DebugTranscribe { path: String },
}

fn default_n() -> usize {
    10
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub state: Option<State>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<serde_json::Value>,
}

impl Response {
    pub fn ok(state: State) -> Self {
        Self { ok: true, state: Some(state), ..Default::default() }
    }
    pub fn ok_data(state: State, data: serde_json::Value) -> Self {
        Self { ok: true, state: Some(state), data: Some(data), error: None }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self { ok: false, error: Some(msg.into()), ..Default::default() }
    }
}

/// Linux: `$XDG_RUNTIME_DIR/capcom.sock`; macOS: `~/Library/Application Support/Capcom/capcom.sock`.
/// `CAPCOM_SOCKET` overrides it (dev/test: run several daemons side by side).
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("CAPCOM_SOCKET") {
        PathBuf::from(p)
    } else if cfg!(target_os = "macos") {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("Capcom").join("capcom.sock")
    } else if let Some(r) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(r).join("capcom.sock")
    } else {
        PathBuf::from(format!("/tmp/capcom-{}.sock", std::env::var("USER").unwrap_or_else(|_| "user".into())))
    }
}

/// Send one request to the daemon and wait for the reply.
pub async fn send(path: &std::path::Path, req: &Request) -> Result<Response> {
    let fut = async {
        let mut s = UnixStream::connect(path).await.with_context(|| {
            format!("cannot connect to daemon at {} (is `capcom daemon` running?)", path.display())
        })?;
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        s.write_all(line.as_bytes()).await?;
        let mut r = BufReader::new(s);
        let mut resp = String::new();
        if r.read_line(&mut resp).await? == 0 {
            bail!("daemon closed the connection without replying");
        }
        Ok::<_, anyhow::Error>(serde_json::from_str::<Response>(&resp)?)
    };
    // DebugTranscribe may take a while; everything else is instant.
    tokio::time::timeout(Duration::from_secs(60), fut).await.map_err(|_| anyhow!("daemon timed out"))?
}

/// Bind the socket (0600) and serve forever, calling `handler` for each request.
pub async fn serve<F, Fut>(path: PathBuf, handler: F) -> Result<()>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            bail!("another daemon is already listening on {}", path.display());
        }
        std::fs::remove_file(&path)?; // stale
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    let handler = Arc::new(handler);
    loop {
        let (stream, _) = listener.accept().await?;
        let h = handler.clone();
        tokio::spawn(async move {
            let (rd, mut wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let resp = match serde_json::from_str::<Request>(&line) {
                    Ok(req) => h(req).await,
                    Err(e) => Response::err(format!("bad request: {e}")),
                };
                let mut out = serde_json::to_string(&resp).unwrap_or_else(|_| r#"{"ok":false}"#.into());
                out.push('\n');
                if wr.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serde() {
        assert_eq!(serde_json::to_string(&Request::Start).unwrap(), r#"{"cmd":"start"}"#);
        assert_eq!(serde_json::from_str::<Request>(r#"{"cmd":"paste_last"}"#).unwrap(), Request::PasteLast);
        assert_eq!(serde_json::from_str::<Request>(r#"{"cmd":"history"}"#).unwrap(), Request::History { n: 10 });
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"history","n":3}"#).unwrap(),
            Request::History { n: 3 }
        );
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"nope"}"#).is_err());
    }

    #[test]
    fn response_serde() {
        let r = Response::ok(State::Idle);
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"ok":true,"state":"idle"}"#);
        let e = Response::err("boom");
        let back: Response = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[tokio::test]
    async fn server_client_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.sock");
        let p2 = p.clone();
        tokio::spawn(async move {
            serve(p2, |req| async move {
                match req {
                    Request::Status => Response::ok(State::Recording),
                    _ => Response::err("nope"),
                }
            })
            .await
            .unwrap();
        });
        for _ in 0..50 {
            if p.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let r = send(&p, &Request::Status).await.unwrap();
        assert_eq!(r.state, Some(State::Recording));
        let r = send(&p, &Request::Start).await.unwrap();
        assert!(!r.ok);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
