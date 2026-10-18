//! Test helpers: a tiny one-shot-per-connection HTTP responder.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve `responses` (status line + body) one per accepted connection, in order.
/// Returns the base URL (`http://127.0.0.1:port`).
pub async fn mock_http(responses: Vec<(u16, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for (status, body) in responses {
            let (mut s, _) = listener.accept().await.unwrap();
            // Read until headers end, then drain a bit of the body.
            let mut buf = vec![0u8; 65536];
            let mut got = 0;
            loop {
                let n = s.read(&mut buf[got..]).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                got += n;
                if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            // Give the client time to finish sending the body.
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes()).await;
            let _ = s.shutdown().await;
        }
    });
    format!("http://{addr}")
}
