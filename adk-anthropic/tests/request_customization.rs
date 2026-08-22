use adk_anthropic::{
    Anthropic, FallbackModel, HeaderMap, HeaderValue, KnownModel, MessageCreateParams, Model,
    ServerFallbackContentBlock, ServerFallbackRequest, ServerFallbackStreamEvent,
};
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_once(
    content_type: &'static str,
    response_body: &'static str,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
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
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }

        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    (format!("http://{address}"), server)
}

const STANDARD_RESPONSE: &str = r#"{
  "id":"msg_test",
  "content":[{"type":"text","text":"ok"}],
  "model":"claude-sonnet-4-6",
  "role":"assistant",
  "stop_reason":"end_turn",
  "stop_sequence":null,
  "type":"message",
  "usage":{"input_tokens":1,"output_tokens":1}
}"#;

#[test]
fn client_header_builders_validate_and_replace_defaults() {
    let client = Anthropic::new(Some("test-key".to_string()))
        .unwrap()
        .with_api_version("2026-08-01")
        .unwrap()
        .with_auth_token("access-token")
        .unwrap();
    let headers = client.default_headers_for_request();
    assert_eq!(headers.get("anthropic-version").unwrap(), "2026-08-01");
    assert_eq!(headers.get("authorization").unwrap(), "Bearer access-token");
    assert!(!headers.contains_key("x-api-key"));
    assert!(Anthropic::new_with_auth_token("").is_err());
    assert!(Anthropic::new(Some("test-key".to_string())).unwrap().with_api_version("").is_err());
}

#[tokio::test]
async fn replacement_headers_support_bearer_only_requests() {
    let (base_url, server) = serve_once("application/json", STANDARD_RESPONSE).await;
    let client =
        Anthropic::new_with_auth_token("default-token").unwrap().with_base_url(base_url).unwrap();
    let defaults = client.default_headers_for_request();
    assert!(client.api_key().is_empty());
    assert_eq!(defaults.get("authorization").unwrap(), "Bearer default-token");
    assert!(!defaults.contains_key("x-api-key"));

    let mut headers = HeaderMap::new();
    headers.insert("authorization", HeaderValue::from_static("Bearer request-token"));
    headers.insert("anthropic-version", HeaderValue::from_static("2026-08-01"));
    headers.insert("anthropic-beta", HeaderValue::from_static("custom-beta"));
    client
        .send_with_headers(
            MessageCreateParams::simple("hello", KnownModel::ClaudeSonnet46),
            headers,
        )
        .await
        .unwrap();

    let request = server.await.unwrap().to_ascii_lowercase();
    assert!(request.contains("authorization: bearer request-token\r\n"));
    assert!(request.contains("anthropic-version: 2026-08-01\r\n"));
    assert!(request.contains("anthropic-beta: custom-beta\r\n"));
    assert!(!request.contains("x-api-key:"));
}

#[tokio::test]
async fn caller_betas_compose_in_headers_without_entering_the_body() {
    let (base_url, server) = serve_once("application/json", STANDARD_RESPONSE).await;
    let client =
        Anthropic::new(Some("test-key".to_string())).unwrap().with_base_url(base_url).unwrap();
    client
        .send_with_betas(
            MessageCreateParams::simple("hello", KnownModel::ClaudeSonnet46),
            &["fine-grained-tool-streaming-2025-05-14", "custom-beta"],
        )
        .await
        .unwrap();

    let request = server.await.unwrap();
    let (headers, body) = request.split_once("\r\n\r\n").unwrap();
    assert!(headers.contains("anthropic-beta: fine-grained-tool-streaming-2025-05-14,custom-beta"));
    assert!(!body.contains("fine-grained-tool-streaming-2025-05-14"));
    assert!(!body.contains("custom-beta"));
}

#[tokio::test]
async fn fallback_request_and_response_preserve_beta_wire_shape() {
    const RESPONSE: &str = r#"{
      "id":"msg_fallback",
      "content":[
        {"type":"fallback","from":{"model":"claude-fable-5"},"to":{"model":"claude-opus-4-8"}},
        {"type":"text","text":"safe response"}
      ],
      "model":"claude-opus-4-8",
      "role":"assistant",
      "stop_reason":"end_turn",
      "stop_sequence":null,
      "type":"message",
      "usage":{"input_tokens":2,"output_tokens":3,"iterations":[{"type":"fallback_message"}]}
    }"#;
    let (base_url, server) = serve_once("application/json", RESPONSE).await;
    let client =
        Anthropic::new(Some("test-key".to_string())).unwrap().with_base_url(base_url).unwrap();
    let request = ServerFallbackRequest::explicit(
        MessageCreateParams::simple("hello", Model::Custom("claude-fable-5".to_string())),
        vec![FallbackModel::new("claude-opus-4-8").with_max_tokens(512)],
    )
    .unwrap();

    let message = client.send_with_server_fallbacks(request).await.unwrap();
    assert!(message.served_by_fallback());
    assert!(matches!(message.content[0], ServerFallbackContentBlock::Fallback(_)));

    let request = server.await.unwrap();
    let lower = request.to_ascii_lowercase();
    assert!(lower.contains("anthropic-beta: server-side-fallback-2026-07-01\r\n"));
    assert!(request.contains(r#""fallbacks":[{"model":"claude-opus-4-8","max_tokens":512}]"#));
    assert!(request.contains(r#""stream":false"#));
}

#[tokio::test]
async fn fallback_stream_decodes_handoff_markers() {
    const RESPONSE: &str = concat!(
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"fallback\",\"from\":{\"model\":\"claude-fable-5\"},\"to\":{\"model\":\"claude-opus-4-8\"}}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"iterations\":[{\"type\":\"fallback_message\",\"model\":\"claude-opus-4-8\"}]}}\n\n",
        "event: ping\n",
        "data: {}\n\n"
    );
    let (base_url, server) = serve_once("text/event-stream", RESPONSE).await;
    let client =
        Anthropic::new(Some("test-key".to_string())).unwrap().with_base_url(base_url).unwrap();
    let request = ServerFallbackRequest::default_routing(MessageCreateParams::simple(
        "hello",
        Model::Custom("claude-fable-5".to_string()),
    ))
    .unwrap();

    let mut stream = Box::pin(client.stream_with_server_fallbacks(&request).await.unwrap());
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ServerFallbackStreamEvent::ContentBlockStart(_)
    ));
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ServerFallbackStreamEvent::MessageDelta(_)
    ));
    assert!(matches!(stream.next().await.unwrap().unwrap(), ServerFallbackStreamEvent::Ping));
    drop(stream);

    let request = server.await.unwrap();
    assert!(request.contains(r#""fallbacks":"default""#));
    assert!(request.contains(r#""stream":true"#));
    assert!(request.to_ascii_lowercase().contains("accept: text/event-stream\r\n"));
}
