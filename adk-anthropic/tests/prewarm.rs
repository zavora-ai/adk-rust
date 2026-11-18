//! `Anthropic::prewarm` opens the connection the next stream uses: after it, a streamed
//! message opens no connection of its own.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const STREAM: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4-6\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

/// What the fixture server saw: connections accepted and requests answered.
#[derive(Default)]
struct Seen {
    connections: AtomicUsize,
    requests: AtomicUsize,
}

/// A keep-alive HTTP/1.1 server: `GET /v1/models` answers `models_status`, a `POST` answers a
/// short message stream.
async fn serve(models_status: u16) -> (String, Arc<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Seen::default());
    let counts = seen.clone();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            counts.connections.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(answer(socket, models_status, counts.clone()));
        }
    });
    (format!("http://{address}"), seen)
}

async fn answer(mut socket: TcpStream, models_status: u16, seen: Arc<Seen>) {
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let Some(end) = pending.windows(4).position(|window| window == b"\r\n\r\n") else {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => pending.extend_from_slice(&buffer[..read]),
            }
            continue;
        };
        let head = String::from_utf8_lossy(&pending[..end]).to_string();
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0_usize);
        while pending.len() < end + 4 + length {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => pending.extend_from_slice(&buffer[..read]),
            }
        }
        pending.drain(..end + 4 + length);
        seen.requests.fetch_add(1, Ordering::SeqCst);
        let (status, kind, body) = if head.starts_with("GET /v1/models") {
            let body = r#"{"type":"error","error":{"type":"not_found_error","message":"no list"}}"#;
            (models_status, "application/json", body)
        } else {
            (200, "text/event-stream", STREAM)
        };
        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        if socket.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn a_stream_after_prewarm_opens_no_connection() {
    let (base_url, seen) = serve(404).await;
    let client = Anthropic::new_with_base_url("fixture-key", base_url).unwrap();

    // An endpoint without a models list still has its connections opened.
    client.prewarm().await.expect("prewarm");
    assert_eq!(seen.connections.load(Ordering::SeqCst), 2, "one per pooled client");
    assert_eq!(seen.requests.load(Ordering::SeqCst), 2);

    let params = MessageCreateParams::simple_streaming("hello", KnownModel::ClaudeSonnet46);
    let events: Vec<_> = client.stream(&params).await.unwrap().collect().await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(seen.requests.load(Ordering::SeqCst), 3);
    assert_eq!(seen.connections.load(Ordering::SeqCst), 2, "the stream reused a warm connection");
}

#[tokio::test]
async fn a_cold_stream_opens_its_own_connection() {
    let (base_url, seen) = serve(404).await;
    let client = Anthropic::new_with_base_url("fixture-key", base_url).unwrap();
    let params = MessageCreateParams::simple_streaming("hello", KnownModel::ClaudeSonnet46);
    let events: Vec<_> = client.stream(&params).await.unwrap().collect().await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(seen.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refused_key_fails_prewarm() {
    let (base_url, _seen) = serve(401).await;
    let client = Anthropic::new_with_base_url("fixture-key", base_url).unwrap();
    assert!(client.prewarm().await.is_err());
}
