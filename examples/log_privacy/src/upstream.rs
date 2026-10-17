//! A scripted model upstream on 127.0.0.1.
//!
//! The script sees each request's body and index, and returns the status, content
//! type, and body to answer with. Real provider clients point at it through their base URL.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One HTTP response.
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Reply {
    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        Self { status, content_type: "application/json", body: body.to_string() }
    }

    pub fn sse(events: &[serde_json::Value]) -> Self {
        let body = events.iter().map(|event| format!("data: {event}\n\n")).collect();
        Self { status: 200, content_type: "text/event-stream", body }
    }

    /// An SSE body whose events are already written, such as `event:` lines or `[DONE]`.
    pub fn raw_sse(body: String) -> Self {
        Self { status: 200, content_type: "text/event-stream", body }
    }
}

/// One request the upstream received.
pub struct Request<'a> {
    pub body: &'a str,
    /// Zero-based count of requests before this one.
    pub index: usize,
}

/// Builds the reply for one request.
pub type Script = Arc<dyn Fn(&Request<'_>) -> Reply + Send + Sync>;

/// A running scripted upstream.
pub struct Upstream {
    pub base: String,
}

impl Upstream {
    pub async fn start(script: Script) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let index = requests.fetch_add(1, Ordering::SeqCst);
                let script = script.clone();
                tokio::spawn(async move {
                    let _ = serve(socket, script, index).await;
                });
            }
        });
        Ok(Self { base })
    }
}

async fn serve(mut socket: TcpStream, script: Script, index: usize) -> anyhow::Result<()> {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 8192];
    let header_end = loop {
        let read = socket.read(&mut buffer).await?;
        anyhow::ensure!(read > 0, "client closed before sending a request");
        received.extend_from_slice(&buffer[..read]);
        if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&received[..header_end]).to_ascii_lowercase();
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while received.len() < header_end + content_length {
        let read = socket.read(&mut buffer).await?;
        anyhow::ensure!(read > 0, "client closed before sending the request body");
        received.extend_from_slice(&buffer[..read]);
    }
    let body = String::from_utf8_lossy(&received[header_end..header_end + content_length]);
    let reply = script(&Request { body: &body, index });

    let reason = match reply.status {
        200 => "OK",
        400 => "Bad Request",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let header = format!(
        "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\ncontent-length: {}\r\nrequest-id: req_scripted\r\nconnection: close\r\n\r\n",
        reply.status,
        reply.content_type,
        reply.body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(reply.body.as_bytes()).await?;
    socket.flush().await?;
    socket.shutdown().await?;
    Ok(())
}
