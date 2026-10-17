//! A scripted model upstream on 127.0.0.1.
//!
//! Each reply is a list of network writes, sent as separate HTTP chunks with a short pause
//! between them, and every response ends with a well-formed zero-length chunk.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One HTTP response: its content type and the body as separate network writes.
pub struct Reply {
    pub content_type: &'static str,
    pub writes: Vec<Vec<u8>>,
}

impl Reply {
    /// Server-sent events, one `data:` line per value, then `data: [DONE]`.
    pub fn sse(events: &[serde_json::Value]) -> Self {
        let mut writes: Vec<Vec<u8>> =
            events.iter().map(|event| format!("data: {event}\n\n").into_bytes()).collect();
        writes.push(b"data: [DONE]\n\n".to_vec());
        Self { content_type: "text/event-stream", writes }
    }

    /// Ollama's newline-delimited JSON stream.
    pub fn ndjson(lines: &[serde_json::Value]) -> Self {
        Self {
            content_type: "application/x-ndjson",
            writes: lines.iter().map(|line| format!("{line}\n").into_bytes()).collect(),
        }
    }

    /// One JSON body.
    pub fn json(body: &serde_json::Value) -> Self {
        Self { content_type: "application/json", writes: vec![body.to_string().into_bytes()] }
    }
}

/// A request the upstream received: its request line target and JSON body.
pub type Request = (String, serde_json::Value);

/// A running scripted upstream that answers request `n` with `replies[n]`.
pub struct Upstream {
    pub base: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Upstream {
    pub async fn start(replies: Vec<Reply>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        let recorded = requests.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let recorded = recorded.clone();
                let replies = replies.clone();
                // A client stops reading once it has the turn, so a failed write is expected.
                tokio::spawn(async move {
                    let _ = serve(socket, recorded, replies).await;
                });
            }
        });
        Ok(Self { base, requests })
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests lock").clone()
    }
}

async fn serve(
    mut socket: TcpStream,
    recorded: Arc<Mutex<Vec<Request>>>,
    replies: Arc<Mutex<std::vec::IntoIter<Reply>>>,
) -> anyhow::Result<()> {
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
    let head = String::from_utf8_lossy(&received[..header_end]).to_string();
    let target = head.split_whitespace().nth(1).unwrap_or_default().to_string();
    let content_length = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while received.len() < header_end + content_length {
        let read = socket.read(&mut buffer).await?;
        anyhow::ensure!(read > 0, "client closed before sending the request body");
        received.extend_from_slice(&buffer[..read]);
    }
    let body = serde_json::from_slice(&received[header_end..header_end + content_length])
        .unwrap_or(serde_json::Value::Null);
    recorded.lock().expect("requests lock").push((target, body));
    let reply = replies.lock().expect("replies lock").next();
    let Some(reply) = reply else {
        socket
            .write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await?;
        return Ok(());
    };

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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    socket.write_all(b"0\r\n\r\n").await?;
    socket.flush().await?;
    socket.shutdown().await?;
    Ok(())
}
