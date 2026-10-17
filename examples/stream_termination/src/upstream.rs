//! A scripted model upstream on 127.0.0.1.
//!
//! Each reply is a list of separate network writes, sent as separate HTTP chunks with a
//! pause between them, so a script controls exactly where the client sees a boundary —
//! inside a UTF-8 character, between two SSE lines, or before the terminal event. Every
//! response ends with a well-formed zero-length chunk: the stream ends cleanly, as it does
//! when a proxy or load balancer closes the response after the model stopped.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One HTTP response: its content type and the body as separate network writes.
pub struct Reply {
    pub content_type: &'static str,
    pub writes: Vec<Vec<u8>>,
}

impl Reply {
    pub fn sse(writes: Vec<Vec<u8>>) -> Self {
        Self { content_type: "text/event-stream", writes }
    }

    pub fn ndjson(writes: Vec<Vec<u8>>) -> Self {
        Self { content_type: "application/x-ndjson", writes }
    }

    pub fn json(body: &serde_json::Value) -> Self {
        Self { content_type: "application/json", writes: vec![body.to_string().into_bytes()] }
    }
}

/// Builds the reply for one request from its body.
pub type Script = Arc<dyn Fn(&str) -> Reply + Send + Sync>;

/// A running scripted upstream.
pub struct Upstream {
    pub base: String,
    requests: Arc<AtomicUsize>,
}

impl Upstream {
    pub async fn start(script: Script) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let script = script.clone();
                // A client stops reading once it has the turn, so a failed write is expected.
                tokio::spawn(async move {
                    let _ = serve(socket, script).await;
                });
            }
        });
        Ok(Self { base, requests })
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

async fn serve(mut socket: TcpStream, script: Script) -> anyhow::Result<()> {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 4096];
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
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while received.len() < header_end + content_length {
        let read = socket.read(&mut buffer).await?;
        anyhow::ensure!(read > 0, "client closed before sending the request body");
        received.extend_from_slice(&buffer[..read]);
    }
    let body = String::from_utf8_lossy(&received[header_end..header_end + content_length]);
    let reply = script(&body);

    let header = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {}\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
        reply.content_type
    );
    socket.write_all(header.as_bytes()).await?;
    for write in reply.writes {
        let mut frame = format!("{:x}\r\n", write.len()).into_bytes();
        frame.extend_from_slice(&write);
        frame.extend_from_slice(b"\r\n");
        socket.write_all(&frame).await?;
        socket.flush().await?;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    socket.write_all(b"0\r\n\r\n").await?;
    socket.flush().await?;
    socket.shutdown().await?;
    Ok(())
}

/// Splits `bytes` in the middle of the first occurrence of `needle`, so the two halves of
/// that character sequence travel in different network writes.
pub fn split_inside(bytes: Vec<u8>, needle: &str) -> Vec<Vec<u8>> {
    let needle = needle.as_bytes();
    let Some(start) = bytes.windows(needle.len()).position(|window| window == needle) else {
        return vec![bytes];
    };
    let split = start + needle.len() / 2;
    vec![bytes[..split].to_vec(), bytes[split..].to_vec()]
}
