//! Tool-call arguments that are not a JSON object reach the agent as
//! [`adk_core::invalid_tool_arguments`], so the call is answered with an error and the
//! model can retry. The tool never runs with substituted `{}` arguments, and the turn
//! does not fail.
#![cfg(any(feature = "groq", feature = "deepseek", feature = "azure-ai"))]

use adk_core::{AdkError, Content, Llm, LlmRequest, LlmResponse, Part};
use adk_model::retry::RetryConfig;
use futures::StreamExt;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tool_call_stream(arguments: &str) -> String {
    let call = serde_json::json!({
        "id": "chunk-1",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000,
        "model": "model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": arguments}
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = serde_json::json!({
        "id": "chunk-1",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000,
        "model": "model",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    });
    format!("data: {call}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

async fn mount_stream(server: &MockServer, arguments: &str) {
    Mock::given(method("POST"))
        .and(path_regex("chat/completions$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_call_stream(arguments)),
        )
        .mount(server)
        .await;
}

async fn mount_unary(server: &MockServer, arguments: &str) {
    Mock::given(method("POST"))
        .and(path_regex("chat/completions$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "chat-1",
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": "model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(server)
        .await;
}

async fn run(model: &dyn Llm, stream: bool) -> Vec<Result<LlmResponse, AdkError>> {
    let request = LlmRequest::new("model", vec![Content::new("user").with_text("Weather?")]);
    model.generate_content(request, stream).await.unwrap().collect().await
}

/// Function calls from the completed (non-partial) responses.
fn function_calls(items: Vec<Result<LlmResponse, AdkError>>) -> Vec<Part> {
    items
        .into_iter()
        .map(|item| item.expect("stream succeeds"))
        .filter(|response| !response.partial)
        .filter_map(|response| response.content)
        .flat_map(|content| content.parts)
        .filter(|part| matches!(part, Part::FunctionCall { .. }))
        .collect()
}

fn weather_call(args: serde_json::Value) -> Part {
    Part::FunctionCall {
        name: "get_weather".to_string(),
        args,
        id: Some("call_1".to_string()),
        thought_signature: None,
    }
}

/// Truncated arguments become a marked call, streamed and unary; empty arguments are a
/// zero-argument call with `{}`.
async fn assert_tool_argument_handling(model_for: impl Fn(&str) -> Box<dyn Llm>) {
    for stream in [true, false] {
        let server = MockServer::start().await;
        if stream {
            mount_stream(&server, "{\"city\": \"Nai").await;
        } else {
            mount_unary(&server, "{\"city\": \"Nai").await;
        }
        let calls = function_calls(run(model_for(&server.uri()).as_ref(), stream).await);
        let [Part::FunctionCall { args, .. }] = calls.as_slice() else {
            panic!("expected one call, got {calls:?} (stream: {stream})");
        };
        let reason = adk_core::invalid_tool_arguments_reason(args)
            .unwrap_or_else(|| panic!("arguments are not marked: {args} (stream: {stream})"));
        assert!(reason.contains("not valid JSON"), "{reason}");
        assert_eq!(args[adk_core::INVALID_TOOL_ARGUMENTS_KEY]["raw"], "{\"city\": \"Nai");
    }

    let server = MockServer::start().await;
    mount_stream(&server, "").await;
    let items = run(model_for(&server.uri()).as_ref(), true).await;
    assert_eq!(function_calls(items), vec![weather_call(serde_json::json!({}))]);
}

#[cfg(feature = "groq")]
#[tokio::test]
async fn groq_marks_malformed_tool_arguments() {
    use adk_model::{GroqClient, GroqConfig};

    assert_tool_argument_handling(|uri| {
        Box::new(
            GroqClient::new(GroqConfig::new("key", "model").with_base_url(uri))
                .unwrap()
                .with_retry_config(RetryConfig::disabled()),
        )
    })
    .await;
}

#[cfg(feature = "deepseek")]
#[tokio::test]
async fn deepseek_marks_malformed_tool_arguments() {
    use adk_model::{DeepSeekClient, DeepSeekConfig};

    assert_tool_argument_handling(|uri| {
        Box::new(
            DeepSeekClient::new(DeepSeekConfig::new("key", "model").with_base_url(uri))
                .unwrap()
                .with_retry_config(RetryConfig::disabled()),
        )
    })
    .await;
}

#[cfg(feature = "azure-ai")]
#[tokio::test]
async fn azure_ai_marks_malformed_tool_arguments() {
    use adk_model::{AzureAIClient, AzureAIConfig};

    assert_tool_argument_handling(|uri| {
        Box::new(
            AzureAIClient::new(AzureAIConfig::new(uri, "key", "model"))
                .unwrap()
                .with_retry_config(RetryConfig::disabled()),
        )
    })
    .await;
}
