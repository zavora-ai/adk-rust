use adk_anthropic::{
    Anthropic, CompactionBlock, CompactionParam, ContentBlock, FallbackModel, HeaderMap,
    HeaderValue, KnownModel, MessageCreateParams, MessageParam, MessageRole, Model,
    ServerFallbackContentBlock, ServerFallbackRequest, ServerFallbackStreamEvent, StopReason,
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

fn request_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn request_body(request: &str) -> serde_json::Value {
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

// On-demand compaction (beta `compact-2026-09-04`), against the request and response shapes in
// Anthropic's "Compaction on demand" documentation.

/// The response sample of "Request a summary".
const COMPACTION_RESPONSE: &str = r#"{
  "id": "msg_013Zva2CMHLNnXjNJJKqJ2EF",
  "type": "message",
  "role": "assistant",
  "model": "claude-opus-5-5",
  "content": [
    {
      "type": "compaction",
      "content": "Summary of the conversation: the user is designing the data model for a recipe app.",
      "signature": "EuYBCkQY"
    }
  ],
  "stop_reason": "compaction",
  "stop_sequence": null,
  "usage": {
    "input_tokens": 0,
    "output_tokens": 0,
    "iterations": [{ "type": "compaction", "input_tokens": 144, "output_tokens": 276 }]
  }
}"#;

const REPLY: &str = r#"{
  "id": "msg_reply",
  "type": "message",
  "role": "assistant",
  "model": "claude-opus-5-5",
  "content": [{"type": "text", "text": "Use name and unit."}],
  "stop_reason": "end_turn",
  "stop_sequence": null,
  "usage": {"input_tokens": 120, "output_tokens": 8}
}"#;

fn conversation() -> Vec<MessageParam> {
    vec![
        MessageParam::new_with_string(
            "I am building a recipe app. Help me name the main entities.".to_string(),
            MessageRole::User,
        ),
        MessageParam::new_with_string(
            "Start with Recipe, Ingredient, and Step.".to_string(),
            MessageRole::Assistant,
        ),
        MessageParam::new_with_string(
            "Good. Now suggest field names for Recipe.".to_string(),
            MessageRole::User,
        ),
    ]
}

#[tokio::test]
async fn a_compaction_request_sends_the_parameter_and_beta_and_reads_the_signed_block() {
    let (base_url, server) = serve_once("application/json", COMPACTION_RESPONSE).await;
    let client =
        Anthropic::new(Some("test-key".to_string())).unwrap().with_base_url(base_url).unwrap();
    let mut params = MessageCreateParams::new(
        4096,
        conversation(),
        Model::Custom("claude-opus-5-5".to_string()),
    );
    params.compaction = Some(CompactionParam::summarize());

    let message = client.send(params).await.unwrap();

    let request = server.await.unwrap();
    assert!(
        request_header(&request, "anthropic-beta")
            .is_some_and(|betas| betas.split(',').any(|beta| beta.trim() == "compact-2026-09-04")),
        "the compaction request must carry the beta header: {request}"
    );
    assert_eq!(request_body(&request)["compaction"], serde_json::json!({"type": "summarize"}));
    assert_eq!(message.stop_reason, Some(StopReason::Compaction));
    assert_eq!(
        message.content,
        vec![ContentBlock::Compaction(CompactionBlock::new(
            "Summary of the conversation: the user is designing the data model for a recipe app.",
            "EuYBCkQY",
        ))]
    );
    let raw_usage = message.usage.raw.as_deref().unwrap();
    assert_eq!(raw_usage["iterations"][0]["output_tokens"], 276);
}

#[tokio::test]
async fn a_request_that_carries_the_block_sends_it_first_and_unchanged_with_the_beta() {
    let (base_url, server) = serve_once("application/json", REPLY).await;
    let client =
        Anthropic::new(Some("test-key".to_string())).unwrap().with_base_url(base_url).unwrap();
    let block = ContentBlock::Compaction(CompactionBlock::new(
        "Summary of the conversation: the user is designing the data model for a recipe app.",
        "EuYBCkQY",
    ));
    let messages = vec![
        MessageParam::new_with_blocks(vec![block], MessageRole::Assistant),
        MessageParam::new_with_string(
            "Now do the same for Ingredient.".to_string(),
            MessageRole::User,
        ),
    ];
    let params =
        MessageCreateParams::new(2048, messages, Model::Custom("claude-opus-5-5".to_string()));

    client.send(params).await.unwrap();

    let request = server.await.unwrap();
    assert!(
        request_header(&request, "anthropic-beta")
            .is_some_and(|betas| betas.split(',').any(|beta| beta.trim() == "compact-2026-09-04")),
        "a request that carries the block must carry the beta header: {request}"
    );
    let sent = request_body(&request);
    assert_eq!(
        sent["messages"][0],
        serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "compaction",
                "content": "Summary of the conversation: the user is designing the data model for a recipe app.",
                "signature": "EuYBCkQY"
            }]
        })
    );
    assert!(sent.get("compaction").is_none(), "a later request asks for no new summary");
}
