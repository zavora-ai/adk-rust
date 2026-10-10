//! `ManagedAgentsClient::define_outcome` sends the documented `user.define_outcome` body.

#![cfg(feature = "managed-agents")]

use adk_anthropic::managed_agents::{ManagedAgentsClient, OutcomeRubric};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Accepts one request, answers `200 {}`, and returns the request line and JSON body.
async fn capture_one_request() -> (String, tokio::task::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let (header_end, content_length) = loop {
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0, "connection closed before the request completed");
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let content_length = String::from_utf8_lossy(&request[..header_end])
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or_default();
            if request.len() >= header_end + 4 + content_length {
                break (header_end, content_length);
            }
        };

        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
            .await
            .unwrap();
        let request_line =
            String::from_utf8_lossy(&request[..header_end]).lines().next().unwrap().to_string();
        let body = &request[header_end + 4..header_end + 4 + content_length];
        (request_line, serde_json::from_slice(body).unwrap())
    });
    (format!("http://{address}"), server)
}

#[tokio::test]
async fn define_outcome_posts_description_and_rubric() {
    let (base_url, server) = capture_one_request().await;
    let client = ManagedAgentsClient::new("test-key").unwrap().with_base_url(base_url).unwrap();

    client
        .define_outcome(
            "sesn_01",
            "Build a DCF model for Costco in .xlsx",
            OutcomeRubric::text("- The workbook has a sheet named `DCF`"),
        )
        .await
        .unwrap();

    let (request_line, body) = server.await.unwrap();
    assert_eq!(request_line, "POST /v1/sessions/sesn_01/events?beta=true HTTP/1.1");
    assert_eq!(
        body,
        json!({
            "events": [{
                "type": "user.define_outcome",
                "description": "Build a DCF model for Costco in .xlsx",
                "rubric": { "type": "text", "content": "- The workbook has a sheet named `DCF`" },
            }]
        })
    );
}
