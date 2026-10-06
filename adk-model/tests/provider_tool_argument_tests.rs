//! Streamed tool-call arguments that are not a JSON object fail the stream
//! instead of invoking the tool with substituted `{}` arguments.
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

async fn run(model: &dyn Llm) -> Vec<Result<LlmResponse, AdkError>> {
    let request = LlmRequest::new("model", vec![Content::new("user").with_text("Weather?")]);
    model.generate_content(request, true).await.unwrap().collect().await
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

/// Truncated arguments fail with the provider's `invalid_tool_arguments` code;
/// empty arguments are a zero-argument call with `{}`.
async fn assert_tool_argument_handling(model_for: impl Fn(&str) -> Box<dyn Llm>, code: &str) {
    let server = MockServer::start().await;
    mount_stream(&server, "{\"city\": \"Nai").await;
    let items = run(model_for(&server.uri()).as_ref()).await;
    let error = items.last().expect("stream yields items").as_ref().unwrap_err();
    assert_eq!(error.code, code);
    assert!(error.message.contains("'get_weather'"), "unexpected message: {}", error.message);

    let server = MockServer::start().await;
    mount_stream(&server, "").await;
    let items = run(model_for(&server.uri()).as_ref()).await;
    assert_eq!(
        function_calls(items),
        vec![Part::FunctionCall {
            name: "get_weather".to_string(),
            args: serde_json::json!({}),
            id: Some("call_1".to_string()),
            thought_signature: None,
        }]
    );
}

#[cfg(feature = "groq")]
#[tokio::test]
async fn groq_rejects_malformed_streamed_tool_arguments() {
    use adk_model::{GroqClient, GroqConfig};

    assert_tool_argument_handling(
        |uri| {
            Box::new(
                GroqClient::new(GroqConfig::new("key", "model").with_base_url(uri))
                    .unwrap()
                    .with_retry_config(RetryConfig::disabled()),
            )
        },
        "model.groq.invalid_tool_arguments",
    )
    .await;
}

#[cfg(feature = "deepseek")]
#[tokio::test]
async fn deepseek_rejects_malformed_streamed_tool_arguments() {
    use adk_model::{DeepSeekClient, DeepSeekConfig};

    assert_tool_argument_handling(
        |uri| {
            Box::new(
                DeepSeekClient::new(DeepSeekConfig::new("key", "model").with_base_url(uri))
                    .unwrap()
                    .with_retry_config(RetryConfig::disabled()),
            )
        },
        "model.deepseek.invalid_tool_arguments",
    )
    .await;
}

#[cfg(feature = "azure-ai")]
#[tokio::test]
async fn azure_ai_rejects_malformed_streamed_tool_arguments() {
    use adk_model::{AzureAIClient, AzureAIConfig};

    assert_tool_argument_handling(
        |uri| {
            Box::new(
                AzureAIClient::new(AzureAIConfig::new(uri, "key", "model"))
                    .unwrap()
                    .with_retry_config(RetryConfig::disabled()),
            )
        },
        "model.azure_ai.invalid_tool_arguments",
    )
    .await;
}
