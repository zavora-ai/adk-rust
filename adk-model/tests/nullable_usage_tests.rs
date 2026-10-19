//! Every provider's unary and streaming parser reads a usage report with `null` or missing
//! token counts without failing the turn: the tool call in the same response still reaches
//! the agent, the counts that are present are reported, and `provider_usage` is the usage
//! object exactly as the provider sent it.
#![cfg(any(
    feature = "openai",
    feature = "anthropic",
    feature = "gemini",
    feature = "deepseek",
    feature = "groq",
    feature = "ollama"
))]

use adk_core::{AdkError, Content, Llm, LlmRequest, LlmResponse, Part};
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// What a scripted endpoint answers: one JSON body, or SSE `data:` lines.
enum Body {
    Json(Value),
    Sse(String),
}

async fn serve(body: Body) -> MockServer {
    let server = MockServer::start().await;
    let template = match body {
        Body::Json(value) => ResponseTemplate::new(200).set_body_json(value),
        Body::Sse(text) => ResponseTemplate::new(200).set_body_raw(text, "text/event-stream"),
    };
    Mock::given(method("POST")).respond_with(template).expect(1).mount(&server).await;
    server
}

fn sse(events: &[Value]) -> String {
    events.iter().map(|event| format!("data: {event}\n\n")).collect()
}

fn request(model: &str) -> LlmRequest {
    let mut request = LlmRequest::new(model, vec![Content::new("user").with_text("Weather?")]);
    request.tools.insert(
        "lookup".to_string(),
        json!({
            "name": "lookup",
            "description": "Looks up the weather",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
        }),
    );
    request
}

/// Every response of the turn, partial ones included: the stream must succeed end to end.
async fn responses(model: &dyn Llm, model_id: &str, stream: bool) -> Vec<LlmResponse> {
    let items: Vec<Result<LlmResponse, AdkError>> =
        model.generate_content(request(model_id), stream).await.unwrap().collect().await;
    items
        .into_iter()
        .map(|item| item.unwrap_or_else(|error| panic!("turn failed (stream: {stream}): {error}")))
        .collect()
}

/// The turn's completed responses.
async fn generate(model: &dyn Llm, model_id: &str, stream: bool) -> Vec<LlmResponse> {
    responses(model, model_id, stream)
        .await
        .into_iter()
        .filter(|response| !response.partial)
        .collect()
}

/// Asserts the turn emitted exactly one `lookup` call and reported `expected` usage.
fn assert_turn(responses: &[LlmResponse], expected: (i32, i32, i32), provider_usage: &Value) {
    let calls: Vec<&Part> = responses
        .iter()
        .filter_map(|response| response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter(|part| matches!(part, Part::FunctionCall { name, .. } if name == "lookup"))
        .collect();
    assert_eq!(calls.len(), 1, "one tool call reaches the agent: {responses:?}");
    let usage = responses
        .iter()
        .rev()
        .find_map(|response| response.usage_metadata.as_ref())
        .unwrap_or_else(|| panic!("usage is reported: {responses:?}"));
    assert_eq!(
        (usage.prompt_token_count, usage.candidates_token_count, usage.total_token_count),
        expected
    );
    assert_eq!(usage.provider_usage.as_ref(), Some(provider_usage));
}

#[cfg(feature = "openai")]
mod openai_responses {
    use super::*;
    use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
    use adk_model::retry::RetryConfig;

    fn usage() -> Value {
        json!({
            "input_tokens": 21,
            "output_tokens": null,
            "total_tokens": null,
            "input_tokens_details": {"cached_tokens": null},
            "output_tokens_details": null
        })
    }

    fn response() -> Value {
        json!({
            "id": "resp_1",
            "object": "response",
            "status": "completed",
            "output": [{
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "lookup",
                "arguments": "{\"city\":\"Paris\"}",
                "status": "completed"
            }],
            "usage": usage()
        })
    }

    async fn turn(body: Body, stream: bool) -> Vec<LlmResponse> {
        let server = serve(body).await;
        let client = OpenAIResponsesClient::new(
            OpenAIResponsesConfig::new("test-key", "gpt-5.6-terra").with_base_url(server.uri()),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
        generate(&client, "gpt-5.6-terra", stream).await
    }

    #[tokio::test]
    async fn unary_null_counts() {
        let responses = turn(Body::Json(response()), false).await;
        assert_turn(&responses, (21, 0, 21), &usage());
        let usage = responses.last().unwrap().usage_metadata.as_ref().unwrap();
        assert_eq!((usage.cache_read_input_token_count, usage.thinking_token_count), (None, None));
    }

    #[tokio::test]
    async fn streaming_null_counts() {
        let events = [
            json!({"type": "response.created", "response": {"id": "resp_1", "status": "in_progress"}}),
            json!({"type": "response.completed", "response": response()}),
        ];
        assert_turn(&turn(Body::Sse(sse(&events)), true).await, (21, 0, 21), &usage());
    }
}

#[cfg(feature = "anthropic")]
mod anthropic {
    use super::*;
    use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
    use adk_model::retry::RetryConfig;

    async fn turn(body: Body, stream: bool) -> Vec<LlmResponse> {
        let server = serve(body).await;
        let client = AnthropicClient::new(
            AnthropicConfig::new("test-key", "claude-sonnet-4-6").with_base_url(server.uri()),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
        generate(&client, "claude-sonnet-4-6", stream).await
    }

    #[tokio::test]
    async fn unary_null_counts() {
        let usage = json!({
            "input_tokens": null,
            "output_tokens": 9,
            "cache_read_input_tokens": null,
            "server_tool_use": {"web_search_requests": null},
            "service_tier": "standard"
        });
        let body = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {"city": "Paris"}}],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": usage
        });
        assert_turn(&turn(Body::Json(body), false).await, (0, 9, 9), &usage);
    }

    #[tokio::test]
    async fn streaming_null_counts() {
        let events = [
            (
                "message_start",
                json!({"type": "message_start", "message": {
                "id": "msg_1", "type": "message", "role": "assistant", "content": [],
                "model": "claude-sonnet-4-6", "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 14, "output_tokens": null, "service_tier": "standard"}}}),
            ),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {}}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "{\"city\":\"Paris\"}"}}),
            ),
            ("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
            (
                "message_delta",
                json!({"type": "message_delta",
                "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                "usage": {"output_tokens": null, "server_tool_use": {"web_search_requests": null}}}),
            ),
            ("message_stop", json!({"type": "message_stop"})),
        ];
        let body = events
            .iter()
            .map(|(name, data)| format!("event: {name}\ndata: {data}\n\n"))
            .collect::<String>();
        // The start's usage with the delta's fields applied over it.
        let usage = json!({
            "input_tokens": 14,
            "output_tokens": null,
            "service_tier": "standard",
            "server_tool_use": {"web_search_requests": null}
        });
        assert_turn(&turn(Body::Sse(body), true).await, (14, 0, 14), &usage);
    }
}

#[cfg(feature = "gemini")]
mod gemini {
    use super::*;
    use adk_model::GeminiModel;
    use adk_model::retry::RetryConfig;

    fn usage() -> Value {
        json!({
            "promptTokenCount": 30,
            "candidatesTokenCount": null,
            "promptTokensDetails": [{"modality": "TEXT", "tokenCount": null}, {"modality": "IMAGE"}],
            "candidatesTokensDetails": [{"modality": "TEXT", "tokenCount": null}],
            "toolUsePromptTokenCount": 4,
            "trafficType": "ON_DEMAND"
        })
    }

    fn chunk() -> Value {
        json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"functionCall": {"name": "lookup", "args": {"city": "Paris"}}}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": usage()
        })
    }

    async fn turn(body: Body, stream: bool) -> Vec<LlmResponse> {
        let server = serve(body).await;
        let model = GeminiModel::new_with_base_url(
            "test-key",
            "gemini-3.7-flash",
            format!("{}/v1beta/", server.uri()),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
        generate(&model, "gemini-3.7-flash", stream).await
    }

    #[tokio::test]
    async fn unary_null_counts() {
        assert_turn(&turn(Body::Json(chunk()), false).await, (30, 0, 30), &usage());
    }

    #[tokio::test]
    async fn streaming_null_counts() {
        assert_turn(&turn(Body::Sse(sse(&[chunk()])), true).await, (30, 0, 30), &usage());
    }
}

#[cfg(feature = "gemini-interactions")]
mod gemini_interactions {
    use super::*;
    use adk_model::GeminiModel;
    use adk_model::retry::RetryConfig;

    fn usage() -> Value {
        json!({
            "total_input_tokens": 40,
            "total_output_tokens": null,
            "total_thought_tokens": null,
            "total_tokens": null,
            "input_tokens_by_modality": [{"modality": "text", "tokens": null}],
            "total_tool_use_tokens": null
        })
    }

    fn interaction(steps: Value) -> Value {
        json!({
            "id": "v1_usage",
            "model": "gemini-3.7-flash",
            "status": "requires_action",
            "steps": steps,
            "usage": usage()
        })
    }

    async fn turn(body: Body, stream: bool) -> Vec<LlmResponse> {
        let server = serve(body).await;
        let model = GeminiModel::new_with_base_url(
            "test-key",
            "gemini-3.7-flash",
            format!("{}/v1beta/", server.uri()),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled())
        .use_interactions_api(true)
        .unwrap();
        generate(&model, "gemini-3.7-flash", stream).await
    }

    #[tokio::test]
    async fn unary_null_counts() {
        let body = interaction(json!([{
            "type": "function_call", "id": "call_1", "name": "lookup", "arguments": {"city": "Paris"}
        }]));
        assert_turn(&turn(Body::Json(body), false).await, (40, 0, 40), &usage());
    }

    #[tokio::test]
    async fn streaming_null_counts() {
        let events = [
            json!({"event_type": "interaction.created", "interaction": {
                "id": "v1_usage", "status": "in_progress", "steps": []}}),
            json!({"event_type": "step.start", "index": 0, "step": {
                "type": "function_call", "id": "call_1", "name": "lookup", "arguments": {}}}),
            json!({"event_type": "step.delta", "index": 0, "delta": {
                "type": "function_call", "id": "call_1", "name": "lookup",
                "arguments_delta": "{\"city\":\"Paris\"}"}}),
            json!({"event_type": "step.stop", "index": 0}),
            json!({"event_type": "interaction.completed", "interaction": interaction(json!([]))}),
        ];
        assert_turn(&turn(Body::Sse(sse(&events)), true).await, (40, 0, 40), &usage());
    }
}

/// Chat Completions chunks for one `lookup` call; the finish chunk carries `usage` at `key`.
#[cfg(any(feature = "deepseek", feature = "groq"))]
fn chat_stream(usage: &Value, key: &str) -> String {
    let chunk = |delta: Value, finish: Value| {
        json!({
            "id": "chat-1",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]
        })
    };
    let call = chunk(
        json!({"role": "assistant", "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
            "function": {"name": "lookup", "arguments": "{\"city\":\"Paris\"}"}}]}),
        Value::Null,
    );
    let mut finish = chunk(json!({}), json!("tool_calls"));
    finish[key] = match key {
        "x_groq" => json!({"id": "req_1", "usage": usage}),
        _ => usage.clone(),
    };
    format!("{}data: [DONE]\n\n", sse(&[call, finish]))
}

#[cfg(any(feature = "deepseek", feature = "groq"))]
fn chat_unary(usage: &Value) -> Value {
    json!({
        "id": "chat-1",
        "object": "chat.completion",
        "created": 0,
        "model": "model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": {"name": "lookup", "arguments": "{\"city\":\"Paris\"}"}
            }]},
            "finish_reason": "tool_calls"
        }],
        "usage": usage
    })
}

#[cfg(feature = "deepseek")]
mod deepseek {
    use super::*;
    use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
    use adk_model::retry::RetryConfig;

    fn usage() -> Value {
        json!({
            "prompt_tokens": 17,
            "completion_tokens": null,
            "total_tokens": null,
            "prompt_cache_hit_tokens": null,
            "prompt_cache_miss_tokens": 17,
            "completion_tokens_details": {"reasoning_tokens": null}
        })
    }

    async fn turn(body: Body, stream: bool) -> Vec<LlmResponse> {
        let server = serve(body).await;
        let client = DeepSeekClient::new(
            DeepSeekConfig::new("test-key", "deepseek-chat").with_base_url(server.uri()),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
        generate(&client, "deepseek-chat", stream).await
    }

    #[tokio::test]
    async fn unary_null_counts() {
        assert_turn(&turn(Body::Json(chat_unary(&usage())), false).await, (17, 0, 17), &usage());
    }

    #[tokio::test]
    async fn streaming_null_counts() {
        let body = Body::Sse(chat_stream(&usage(), "usage"));
        assert_turn(&turn(body, true).await, (17, 0, 17), &usage());
    }
}

#[cfg(feature = "groq")]
mod groq {
    use super::*;
    use adk_model::groq::{GroqClient, GroqConfig};
    use adk_model::retry::RetryConfig;

    fn usage() -> Value {
        json!({
            "queue_time": 0.25,
            "prompt_tokens": 19,
            "completion_tokens": null,
            "total_tokens": null,
            "completion_tokens_details": {"reasoning_tokens": null}
        })
    }

    async fn turn(body: Body, stream: bool) -> Vec<LlmResponse> {
        let server = serve(body).await;
        let client = GroqClient::new(
            GroqConfig::new("test-key", "openai/gpt-oss-20b").with_base_url(server.uri()),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
        generate(&client, "openai/gpt-oss-20b", stream).await
    }

    #[tokio::test]
    async fn unary_null_counts() {
        assert_turn(&turn(Body::Json(chat_unary(&usage())), false).await, (19, 0, 19), &usage());
    }

    #[tokio::test]
    async fn streaming_null_counts() {
        let body = Body::Sse(chat_stream(&usage(), "usage"));
        assert_turn(&turn(body, true).await, (19, 0, 19), &usage());
    }

    /// Groq reports streaming usage in `x_groq.usage` on the finish chunk.
    #[tokio::test]
    async fn streaming_usage_from_x_groq() {
        let body = Body::Sse(chat_stream(&usage(), "x_groq"));
        assert_turn(&turn(body, true).await, (19, 0, 19), &usage());
    }
}

#[cfg(feature = "ollama")]
mod ollama {
    use super::*;
    use adk_model::ollama::{OllamaConfig, OllamaModel};

    /// A final line without `prompt_eval_count`, as Ollama sends when the prompt came from
    /// its cache.
    fn done_line() -> Value {
        json!({
            "model": "qwen3",
            "created_at": "2026-10-17T00:00:00Z",
            "message": {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "lookup", "arguments": {"city": "Paris"}}}
            ]},
            "done": true,
            "done_reason": "stop",
            "total_duration": 1200,
            "eval_count": 6,
            "eval_duration": 900
        })
    }

    fn metrics() -> Value {
        json!({"total_duration": 1200, "eval_count": 6, "eval_duration": 900})
    }

    /// A streamed tool call arrives partial, as part of the turn the `done` line ends, and
    /// `LlmAgent` folds it into that turn's content; so a streamed turn keeps every response.
    async fn turn(body: Body, stream: bool) -> Vec<LlmResponse> {
        let server = serve(body).await;
        let model = OllamaModel::new(OllamaConfig::with_host(server.uri(), "qwen3")).unwrap();
        if stream {
            responses(&model, "qwen3", true).await
        } else {
            generate(&model, "qwen3", false).await
        }
    }

    #[tokio::test]
    async fn unary_missing_prompt_eval_count() {
        assert_turn(&turn(Body::Json(done_line()), false).await, (0, 6, 6), &metrics());
    }

    /// A streamed turn: the tool call on its own line, then the final line with the metrics.
    #[tokio::test]
    async fn streaming_missing_prompt_eval_count() {
        let mut call = done_line();
        call["done"] = json!(false);
        for key in ["done_reason", "total_duration", "eval_count", "eval_duration"] {
            call.as_object_mut().unwrap().remove(key);
        }
        let mut done = done_line();
        done["message"] = json!({"role": "assistant", "content": ""});
        // Ollama streams newline-delimited JSON; the content type does not matter.
        let body = Body::Sse(format!("{call}\n{done}\n"));
        assert_turn(&turn(body, true).await, (0, 6, 6), &metrics());
    }
}
