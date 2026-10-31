//! Muse Responses mode: `MuseClient` drives `/responses` through the shared Open Responses
//! engine, streamed or not, attributed and priced as `muse`, and refuses settings the Meta
//! Responses API rejects before sending anything.
#![cfg(feature = "muse-responses")]

use adk_core::{Content, ErrorCategory, GenerateContentConfig, Llm, LlmRequest, LlmResponse, Part};
use adk_model::muse::{MuseApi, MuseClient, MuseConfig};
use adk_model::pricing::PricingCatalog;
use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn completed(output: Value) -> Value {
    json!({
        "type": "response.completed", "sequence_number": 20,
        "response": {
            "id": "resp_muse", "object": "response", "created_at": 0,
            "model": "muse-spark-1.3", "status": "completed", "output": output,
            "usage": {
                "input_tokens": 100, "input_tokens_details": {"cached_tokens": 40},
                "output_tokens": 20, "output_tokens_details": {"reasoning_tokens": 8},
                "total_tokens": 120
            }
        }
    })
}

fn delta(value: &str, sequence: u32) -> Value {
    json!({
        "type": "response.output_text.delta", "sequence_number": sequence, "item_id": "msg_1",
        "output_index": 1, "content_index": 0, "delta": value, "logprobs": []
    })
}

/// A Meta-shaped stream: keepalives, a reasoning summary, text deltas, one function call.
fn meta_stream() -> Vec<Value> {
    vec![
        json!({"type": "response.created", "sequence_number": 0,
            "response": {"id": "resp_muse", "object": "response", "created_at": 0,
                "model": "muse-spark-1.3", "status": "in_progress", "output": []}}),
        json!({"type": "keepalive", "sequence_number": 1}),
        json!({"type": "response.reasoning_summary_text.delta", "sequence_number": 2,
            "item_id": "rs_1", "output_index": 0, "summary_index": 0, "delta": "Plan"}),
        delta("Hel", 3),
        json!({"type": "keepalive", "sequence_number": 4}),
        delta("lo", 5),
        json!({"type": "response.output_item.added", "sequence_number": 6, "output_index": 2,
            "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                "name": "lookup", "arguments": "", "status": "in_progress"}}),
        json!({"type": "response.function_call_arguments.delta", "sequence_number": 7,
            "output_index": 2, "item_id": "fc_1", "delta": "{\"q\":\"barn\"}"}),
        completed(json!([
            {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "Plan"}]},
            {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "Hello", "annotations": []}]},
            {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "lookup",
                "arguments": "{\"q\":\"barn\"}", "status": "completed"}
        ])),
    ]
}

async fn server_streaming(events: &[Value]) -> MockServer {
    let server = MockServer::start().await;
    let body: String = events.iter().map(|event| format!("data: {event}\n\n")).collect();
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("authorization", "Bearer muse-key"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    server
}

fn client(server: &MockServer) -> MuseClient {
    MuseClient::new(
        MuseConfig::spark_1_3("muse-key").with_base_url(server.uri()).with_api(MuseApi::Responses),
    )
    .unwrap()
}

fn request(config: GenerateContentConfig) -> LlmRequest {
    let mut request =
        LlmRequest::new("muse-spark-1.3", vec![Content::new("user").with_text("find the barn")]);
    request.config = Some(config);
    request
}

fn muse_options(options: Value) -> GenerateContentConfig {
    let mut config = GenerateContentConfig::default();
    config.extensions.insert("muse".to_string(), options);
    config
}

async fn collect(
    client: &MuseClient,
    request: LlmRequest,
    stream: bool,
) -> adk_core::Result<Vec<LlmResponse>> {
    client.generate_content(request, stream).await?.try_collect().await
}

async fn sent_body(server: &MockServer) -> Value {
    let requests = server.received_requests().await.unwrap();
    serde_json::from_slice(&requests[0].body).unwrap()
}

#[tokio::test]
async fn a_streamed_turn_yields_partials_and_a_final_call_priced_as_muse() {
    let server = server_streaming(&meta_stream()).await;
    let client = client(&server);
    let options = json!({ "previous_response_id": "resp_prev", "store": true });
    let mut config = muse_options(options);
    config.max_output_tokens = Some(200);
    let responses = collect(&client, request(config), true).await.unwrap();

    let partial_text: String = responses
        .iter()
        .filter(|response| response.partial)
        .flat_map(|response| response.content.iter().flat_map(|content| &content.parts))
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(partial_text, "Hello");

    let last = responses.last().unwrap();
    assert!(last.turn_complete && !last.partial);
    assert!(last.content.as_ref().unwrap().parts.contains(&Part::FunctionCall {
        id: Some("call_1".to_string()),
        name: "lookup".to_string(),
        args: json!({ "q": "barn" }),
        thought_signature: None,
    }));
    assert_eq!(last.provider.as_deref(), Some("muse"));
    let usage = last.usage_metadata.as_ref().unwrap();
    let expected = PricingCatalog::standard().cost_usd(Some("muse"), "muse-spark-1.3", usage);
    assert!(expected.is_some());
    assert_eq!(usage.cost, expected);

    let body = sent_body(&server).await;
    assert_eq!(body["model"], "muse-spark-1.3");
    assert_eq!(body["stream"], true);
    assert_eq!(body["previous_response_id"], "resp_prev");
    assert_eq!(body["store"], true);
    // Spark reasoning counts toward the cap, so small caps are raised.
    assert_eq!(body["max_output_tokens"], 1024);
}

#[tokio::test]
async fn a_non_streamed_turn_returns_the_same_answer() {
    let server = MockServer::start().await;
    let response = completed(json!([
        {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": "Hello", "annotations": []}]}
    ]))["response"]
        .clone();
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(&server)
        .await;
    let responses =
        collect(&client(&server), request(GenerateContentConfig::default()), false).await.unwrap();
    let last = responses.last().unwrap();
    assert_eq!(last.content.as_ref().unwrap().parts, vec![Part::Text { text: "Hello".into() }]);
    assert_eq!(last.provider.as_deref(), Some("muse"));
    assert!(last.usage_metadata.as_ref().unwrap().cost.is_some());
    assert_eq!(sent_body(&server).await["stream"], false);
}

#[tokio::test]
async fn a_server_shutting_down_error_event_is_retryable_and_names_muse() {
    let server = server_streaming(&[
        delta("Hel", 1),
        json!({"type": "error", "sequence_number": 2,
            "error": {"code": "server_shutting_down", "message": "retry the request"}}),
    ])
    .await;
    let error = collect(&client(&server), request(GenerateContentConfig::default()), true)
        .await
        .unwrap_err();
    assert_eq!(error.category, ErrorCategory::Unavailable);
    assert!(error.is_retryable());
    assert_eq!(error.details.provider.as_deref(), Some("muse"));
}

#[tokio::test]
async fn a_failed_or_unfinished_stream_fails_as_muse() {
    let failed = server_streaming(&[json!({"type": "response.failed", "sequence_number": 1,
        "response": {"error": {"code": "server_error", "message": "boom"}}})])
    .await;
    let error = collect(&client(&failed), request(GenerateContentConfig::default()), true)
        .await
        .unwrap_err();
    assert_eq!(error.details.provider.as_deref(), Some("muse"));

    let unfinished = server_streaming(&[delta("Hel", 1)]).await;
    let error = collect(&client(&unfinished), request(GenerateContentConfig::default()), true)
        .await
        .unwrap_err();
    assert_eq!(error.category, ErrorCategory::Unavailable);
    assert_eq!(error.details.provider.as_deref(), Some("muse"));
}

#[tokio::test]
async fn settings_meta_rejects_fail_before_anything_is_sent() {
    let server = server_streaming(&meta_stream()).await;
    let client = client(&server);
    let top_logprobs = GenerateContentConfig { top_logprobs: Some(3), ..Default::default() };
    let stop =
        GenerateContentConfig { stop_sequences: vec!["END".to_string()], ..Default::default() };
    let cases = [
        (muse_options(json!({ "background": true })), true),
        (muse_options(json!({ "reasoning": { "effort": "none" } })), false),
        (
            muse_options(json!({
                "previous_response_id": "resp_prev",
                "include": ["reasoning.encrypted_content"]
            })),
            false,
        ),
        (top_logprobs, true),
        (stop, false),
    ];
    for (config, stream) in cases {
        let error = collect(&client, request(config.clone()), stream).await.unwrap_err();
        assert_eq!(error.code, "model.unsupported_config", "{config:?}");
        assert_eq!(error.details.provider.as_deref(), Some("muse"));
    }
    assert!(server.received_requests().await.unwrap().is_empty());

    // Background without streaming is a valid request.
    let background = muse_options(json!({ "background": true }));
    let _ = collect(&client, request(background), false).await;
    assert_eq!(sent_body(&server).await["background"], true);
}

#[tokio::test]
async fn an_openai_namespace_is_not_sent_to_meta() {
    let server = server_streaming(&meta_stream()).await;
    let mut config = GenerateContentConfig::default();
    config.extensions.insert(
        "openai".to_string(),
        json!({ "built_in_tools": [{ "type": "web_search" }], "service_tier": "priority" }),
    );
    collect(&client(&server), request(config), true).await.unwrap();
    let body = sent_body(&server).await;
    assert!(body.get("tools").is_none(), "{body}");
    assert!(body.get("service_tier").is_none(), "{body}");
}

#[test]
fn chat_completions_stays_the_default() {
    let client = MuseClient::new(MuseConfig::spark_1_3("muse-key")).unwrap();
    assert_eq!(client.api(), MuseApi::ChatCompletions);
}
