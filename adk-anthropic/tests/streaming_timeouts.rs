//! Streaming requests are not bounded by the client's total request timeout.

use std::time::Duration;

use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams, MessageStreamEvent};
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MESSAGE_START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_slow\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4-6\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n";
const PING: &str = "event: ping\ndata: {\"type\":\"ping\"}\n\n";
const MESSAGE_STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

/// Serves one SSE response that sends `pings` ping events `interval` apart.
async fn serve_slow_stream(pings: usize, interval: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let header_text = String::from_utf8_lossy(&request[..header_end]);
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or_default();
            if read == 0 || request.len() >= header_end + 4 + content_length {
                break;
            }
        }

        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        socket.write_all(MESSAGE_START.as_bytes()).await.unwrap();
        for _ in 0..pings {
            tokio::time::sleep(interval).await;
            if socket.write_all(PING.as_bytes()).await.is_err() {
                return;
            }
        }
        let _ = socket.write_all(MESSAGE_STOP.as_bytes()).await;
    });
    format!("http://{address}")
}

fn params() -> MessageCreateParams {
    MessageCreateParams::simple_streaming("hello", KnownModel::ClaudeSonnet46)
}

#[tokio::test]
async fn stream_outlives_the_total_request_timeout() {
    // The body takes ~1.2 s; the client's total request timeout is 400 ms.
    let base_url = serve_slow_stream(6, Duration::from_millis(200)).await;
    let client = Anthropic::new(Some("test-key".to_string()))
        .unwrap()
        .with_base_url(base_url)
        .unwrap()
        .with_timeout(Duration::from_millis(400))
        .unwrap();

    let events: Vec<_> = client.stream(&params()).await.unwrap().collect().await;
    let events = events.into_iter().collect::<Result<Vec<_>, _>>().unwrap();

    assert!(matches!(events.first(), Some(MessageStreamEvent::MessageStart(_))));
    assert!(matches!(events.last(), Some(MessageStreamEvent::MessageStop(_))));
    assert_eq!(events.len(), 8);
}

#[tokio::test]
async fn stream_timeout_bounds_the_whole_stream_when_set() {
    let base_url = serve_slow_stream(10, Duration::from_millis(200)).await;
    let client = Anthropic::new(Some("test-key".to_string()))
        .unwrap()
        .with_base_url(base_url)
        .unwrap()
        .with_stream_timeout(Some(Duration::from_millis(500)));

    let events: Vec<_> = client.stream(&params()).await.unwrap().collect().await;

    let (last, delivered) = events.split_last().expect("the stream yields events");
    assert!(last.is_err(), "the stream should end with the timeout error: {last:?}");
    assert!(
        delivered.iter().all(|event| matches!(
            event,
            Ok(MessageStreamEvent::MessageStart(_) | MessageStreamEvent::Ping)
        )),
        "only events sent before the deadline are delivered: {delivered:?}"
    );
}
