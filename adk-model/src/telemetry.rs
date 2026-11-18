//! OpenTelemetry GenAI inference spans for every provider client.
//!
//! Each client opens [`inference_span`] once per `generate_content` call and passes its stream
//! through [`with_priced_usage_tracking`](crate::usage_tracking::with_priced_usage_tracking),
//! so every provider reports the same span: named `{gen_ai.operation.name} {gen_ai.request.model}`,
//! kind `client`, with the request parameters, the response model and finish reasons, the
//! token usage, and `error.type` on failure. The conversation itself
//! (`gen_ai.system_instructions`, `gen_ai.input.messages`, `gen_ai.output.messages`,
//! `gen_ai.tool.definitions`) is recorded only under content capture
//! ([`adk_telemetry::config::capture_content`]): as span attributes, on the
//! `gen_ai.client.inference.operation.details` event, or both.
//!
//! The message attributes are JSON in the conventions' message schema, since the
//! OpenTelemetry Rust span API takes no map-valued attribute:
//!
//! | ADK-Rust | GenAI part |
//! |---|---|
//! | `Part::Text` | `{"type": "text", "content": ...}` |
//! | `Part::Thinking` | `{"type": "reasoning", "content": ...}` |
//! | `Part::FunctionCall` | `{"type": "tool_call", "id", "name", "arguments"}` |
//! | `Part::FunctionResponse` | `{"type": "tool_call_response", "id", "response"}` |
//! | `Part::InlineData` | `{"type": "blob", "mime_type", "modality", "content"}` (base64) |
//! | `Part::FileData`, `Part::EmbeddedResource` | `{"type": "uri", "mime_type", "modality", "uri"}` |
//! | `Part::ServerToolCall`, `Part::ServerToolResponse` | `{"type": "server_tool_call" \| "server_tool_call_response", ...}` with the provider's payload |

use adk_core::{Content, FinishReason, LlmRequest, Part, ToolDeclarations};
use base64::Engine as _;
use serde_json::{Map, Value, json};
use tracing::Span;

/// The `gen_ai.response.finish_reasons` value of `reason`: `stop`, `length`,
/// `content_filter` or `error`. A stop that ends on tool calls is `tool_call`
/// ([`response_finish_reason`]).
///
/// # Example
///
/// ```
/// use adk_core::FinishReason;
/// use adk_model::telemetry::finish_reason;
///
/// assert_eq!(finish_reason(FinishReason::Stop), "stop");
/// assert_eq!(finish_reason(FinishReason::MaxTokens), "length");
/// assert_eq!(finish_reason(FinishReason::Safety), "content_filter");
/// ```
pub fn finish_reason(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::MaxTokens => "length",
        FinishReason::Safety | FinishReason::Recitation | FinishReason::Refusal => "content_filter",
        // `Other` also covers reasons a newer release names; none of them is a stop.
        FinishReason::MalformedToolCall | FinishReason::Other | _ => "error",
    }
}

/// The `gen_ai.response.finish_reasons` value of a response that ended with `reason` and
/// `content`: `tool_call` when it stopped on tool calls, otherwise [`finish_reason`].
///
/// # Example
///
/// ```
/// use adk_core::{Content, FinishReason, Part};
/// use adk_model::telemetry::response_finish_reason;
///
/// let call = Content {
///     role: "model".into(),
///     parts: vec![Part::FunctionCall {
///         name: "get_weather".into(),
///         args: serde_json::json!({}),
///         id: None,
///         thought_signature: None,
///     }],
/// };
/// assert_eq!(response_finish_reason(FinishReason::Stop, Some(&call)), "tool_call");
/// assert_eq!(response_finish_reason(FinishReason::Stop, None), "stop");
/// ```
pub fn response_finish_reason(reason: FinishReason, content: Option<&Content>) -> &'static str {
    let calls = content.is_some_and(|content| {
        content.parts.iter().any(|part| matches!(part, Part::FunctionCall { .. }))
    });
    match reason {
        FinishReason::Stop if calls => "tool_call",
        reason => finish_reason(reason),
    }
}

/// Opens the inference span of one `generate_content` call by `provider` (the client's
/// provider id, such as `anthropic` or `gemini`) for `model`, recording `request`'s
/// parameters and, under content capture, its conversation.
///
/// # Example
///
/// ```
/// use adk_core::{Content, LlmRequest};
/// use adk_model::telemetry::inference_span;
///
/// let request = LlmRequest::new("claude-sonnet-5-5", vec![Content::new("user").with_text("hi")]);
/// let span = inference_span("anthropic", "claude-sonnet-5-5", true, &request);
/// let _entered = span.enter();
/// ```
pub fn inference_span(provider: &str, model: &str, stream: bool, request: &LlmRequest) -> Span {
    let span = adk_telemetry::llm_generate_span(provider, model, stream);
    if span.is_disabled() {
        return span;
    }
    if let Some(config) = &request.config {
        if let Some(value) = config.temperature {
            span.record("gen_ai.request.temperature", f64::from(value));
        }
        if let Some(value) = config.top_p {
            span.record("gen_ai.request.top_p", f64::from(value));
        }
        if let Some(value) = config.top_k {
            span.record("gen_ai.request.top_k", i64::from(value));
        }
        if let Some(value) = config.max_output_tokens {
            span.record("gen_ai.request.max_tokens", i64::from(value));
        }
        if let Some(value) = config.frequency_penalty {
            span.record("gen_ai.request.frequency_penalty", f64::from(value));
        }
        if let Some(value) = config.presence_penalty {
            span.record("gen_ai.request.presence_penalty", f64::from(value));
        }
        if let Some(value) = config.seed {
            span.record("gen_ai.request.seed", value);
        }
        if !config.stop_sequences.is_empty() {
            let stops: Vec<&str> = config.stop_sequences.iter().map(String::as_str).collect();
            adk_telemetry::record_str_array(&span, "gen_ai.request.stop_sequences", &stops);
        }
        if config.response_schema.is_some() {
            span.record("gen_ai.output.type", "json");
        }
    }
    let capture = adk_telemetry::config::capture_content();
    if capture.on_spans() || capture.on_events() {
        let (system, messages) = input_messages(&request.contents);
        let details = adk_telemetry::events::InferenceDetails {
            system_instructions: system.map(|system| system.to_string()),
            input_messages: Some(messages.to_string()),
            output_messages: None,
            tool_definitions: (!request.tools.is_empty())
                .then(|| tool_definitions(&request.tools).to_string()),
        };
        if capture.on_spans() {
            for (key, value) in [
                ("gen_ai.system_instructions", &details.system_instructions),
                ("gen_ai.input.messages", &details.input_messages),
                ("gen_ai.tool.definitions", &details.tool_definitions),
            ] {
                if let Some(value) = value {
                    span.record(key, value.as_str());
                }
            }
        }
        adk_telemetry::events::ContentEventEmitter::emit_inference_details(
            &adk_telemetry::config::SemconvConfig { capture_content: capture },
            &span,
            &details,
        );
    }
    span
}

/// `contents` as the conventions' system instructions (the text of the `system` contents,
/// which take only text parts) and input messages (the rest, in order).
///
/// # Example
///
/// ```
/// use adk_core::Content;
/// use adk_model::telemetry::input_messages;
/// use serde_json::json;
///
/// let (system, messages) = input_messages(&[
///     Content::new("system").with_text("Be brief."),
///     Content::new("user").with_text("Hi"),
/// ]);
/// assert_eq!(system, Some(json!([{ "type": "text", "content": "Be brief." }])));
/// assert_eq!(messages, json!([{ "role": "user", "parts": [{ "type": "text", "content": "Hi" }] }]));
/// ```
pub fn input_messages(contents: &[Content]) -> (Option<Value>, Value) {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for content in contents {
        if content.role == "system" {
            system.extend(content.parts.iter().filter_map(|part| match part {
                Part::Text { text } => Some(json!({ "type": "text", "content": text })),
                _ => None,
            }));
        } else {
            messages.push(json!({ "role": role(content), "parts": parts(content) }));
        }
    }
    ((!system.is_empty()).then_some(Value::Array(system)), Value::Array(messages))
}

/// `content`, the model's answer, as the conventions' output messages. The finish reason is
/// the span's `gen_ai.response.finish_reasons`, the per-message field being deprecated.
///
/// # Example
///
/// ```
/// use adk_core::Content;
/// use adk_model::telemetry::output_messages;
/// use serde_json::json;
///
/// assert_eq!(
///     output_messages(&Content::new("model").with_text("Hello")),
///     json!([{ "role": "assistant", "parts": [{ "type": "text", "content": "Hello" }] }])
/// );
/// ```
pub fn output_messages(content: &Content) -> Value {
    json!([{ "role": "assistant", "parts": parts(content) }])
}

/// `tools` as the conventions' tool definitions: `{type: "function", name, description,
/// parameters}` for each declaration.
///
/// # Example
///
/// ```
/// use adk_core::ToolDeclarations;
/// use adk_model::telemetry::tool_definitions;
/// use serde_json::json;
///
/// let mut tools = ToolDeclarations::new();
/// tools.insert("get_weather".to_string(), json!({ "name": "get_weather", "description": "Weather by city" }));
/// assert_eq!(
///     tool_definitions(&tools),
///     json!([{ "type": "function", "name": "get_weather", "description": "Weather by city" }])
/// );
/// ```
pub fn tool_definitions(tools: &ToolDeclarations) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|(name, declaration)| {
                let mut definition = Map::new();
                definition.insert("type".into(), json!("function"));
                definition.insert("name".into(), json!(name));
                for key in ["description", "parameters"] {
                    if let Some(value) = declaration.get(key) {
                        definition.insert(key.into(), value.clone());
                    }
                }
                Value::Object(definition)
            })
            .collect(),
    )
}

fn role(content: &Content) -> &'static str {
    let responses = !content.parts.is_empty()
        && content.parts.iter().all(|part| matches!(part, Part::FunctionResponse { .. }));
    match content.role.as_str() {
        _ if responses => "tool",
        "model" | "assistant" => "assistant",
        "function" | "tool" => "tool",
        _ => "user",
    }
}

fn parts(content: &Content) -> Value {
    Value::Array(content.parts.iter().map(part).collect())
}

fn modality(mime_type: &str) -> &str {
    match mime_type.split('/').next().unwrap_or_default() {
        "image" => "image",
        "audio" => "audio",
        "video" => "video",
        _ => "document",
    }
}

fn part(part: &Part) -> Value {
    match part {
        Part::Text { text } => json!({ "type": "text", "content": text }),
        Part::Thinking { thinking, .. } => json!({ "type": "reasoning", "content": thinking }),
        Part::FunctionCall { name, args, id, .. } => {
            json!({ "type": "tool_call", "id": id, "name": name, "arguments": args })
        }
        Part::FunctionResponse { function_response, id, .. } => json!({
            "type": "tool_call_response",
            "id": id,
            "response": function_response.response,
        }),
        Part::InlineData { mime_type, data, .. } => json!({
            "type": "blob",
            "mime_type": mime_type,
            "modality": modality(mime_type),
            "content": base64::engine::general_purpose::STANDARD.encode(data),
        }),
        Part::FileData { mime_type, file_uri, .. } => json!({
            "type": "uri",
            "mime_type": mime_type,
            "modality": modality(mime_type),
            "uri": file_uri,
        }),
        Part::EmbeddedResource { resource } => {
            let mime_type = resource.mime_type().unwrap_or("application/octet-stream");
            json!({
                "type": "uri",
                "mime_type": mime_type,
                "modality": modality(mime_type),
                "uri": resource.uri(),
            })
        }
        Part::ServerToolCall { server_tool_call } => {
            json!({ "type": "server_tool_call", "server_tool_call": server_tool_call })
        }
        Part::ServerToolResponse { server_tool_response } => json!({
            "type": "server_tool_call_response",
            "server_tool_call_response": server_tool_response,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use adk_core::{GenerateContentConfig, LlmResponse};
    use futures::StreamExt;
    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// Every field value recorded on any span, as text.
    #[derive(Clone, Default)]
    struct Fields(Arc<Mutex<HashMap<String, String>>>);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.lock().unwrap().insert(field.name().into(), format!("{value:?}"));
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.lock().unwrap().insert(field.name().into(), value.into());
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Fields {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _: &tracing::span::Id,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            attrs.record(&mut self.clone());
        }
        fn on_record(
            &self,
            _: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            values.record(&mut self.clone());
        }
    }

    fn request() -> LlmRequest {
        let mut request = LlmRequest::new(
            "claude-sonnet-5-5",
            vec![
                Content::new("system").with_text("Answer in one line."),
                Content::new("user").with_text("Weather in Nairobi?"),
            ],
        )
        .with_config(GenerateContentConfig {
            temperature: Some(0.5),
            top_k: Some(40),
            max_output_tokens: Some(256),
            ..Default::default()
        });
        request.tools.insert(
            "get_weather".to_string(),
            json!({ "name": "get_weather", "description": "Weather by city", "parameters": { "type": "object" } }),
        );
        request
    }

    #[tokio::test]
    async fn the_inference_span_records_parameters_and_captured_content() {
        adk_telemetry::config::set_capture_content(adk_telemetry::config::CaptureContent::SpanOnly);
        let fields = Fields::default();
        let _guard =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(fields.clone()));
        let span = inference_span("anthropic", "claude-sonnet-5-5", false, &request());
        let answer = LlmResponse {
            content: Some(Content::new("model").with_text("Sunny, 24 C.")),
            finish_reason: Some(FinishReason::Stop),
            model: Some("claude-sonnet-5-5-20261101".into()),
            ..Default::default()
        };
        let stream: adk_core::LlmResponseStream = Box::pin(futures::stream::iter([Ok(answer)]));
        let _ = crate::usage_tracking::with_priced_usage_tracking(
            stream,
            span,
            "anthropic",
            "claude-sonnet-5-5",
        )
        .collect::<Vec<_>>()
        .await;

        let fields = fields.0.lock().unwrap().clone();
        let field = |key: &str| fields.get(key).cloned().unwrap_or_default();
        assert_eq!(field("otel.name"), "chat claude-sonnet-5-5");
        assert_eq!(field("gen_ai.request.temperature"), "0.5");
        assert_eq!(field("gen_ai.request.top_k"), "40");
        assert_eq!(field("gen_ai.request.max_tokens"), "256");
        assert_eq!(field("gen_ai.response.model"), "claude-sonnet-5-5-20261101");
        assert_eq!(
            serde_json::from_str::<Value>(&field("gen_ai.system_instructions")).unwrap(),
            json!([{ "type": "text", "content": "Answer in one line." }])
        );
        assert_eq!(
            serde_json::from_str::<Value>(&field("gen_ai.input.messages")).unwrap(),
            json!([{ "role": "user", "parts": [{ "type": "text", "content": "Weather in Nairobi?" }] }])
        );
        assert_eq!(
            serde_json::from_str::<Value>(&field("gen_ai.output.messages")).unwrap(),
            json!([{ "role": "assistant", "parts": [{ "type": "text", "content": "Sunny, 24 C." }] }])
        );
        assert_eq!(
            serde_json::from_str::<Value>(&field("gen_ai.tool.definitions")).unwrap(),
            json!([{
                "type": "function",
                "name": "get_weather",
                "description": "Weather by city",
                "parameters": { "type": "object" }
            }])
        );
    }

    #[tokio::test]
    async fn without_capture_no_content_is_recorded_and_a_failure_sets_error_type() {
        adk_telemetry::config::set_capture_content(
            adk_telemetry::config::CaptureContent::NoContent,
        );
        let fields = Fields::default();
        let _guard =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(fields.clone()));
        let span = inference_span("openai", "gpt-6.1-sol", true, &request());
        let stream: adk_core::LlmResponseStream = Box::pin(futures::stream::iter([Err(
            adk_core::AdkError::model("upstream closed the stream"),
        )]));
        let _ = crate::usage_tracking::with_priced_usage_tracking(
            stream,
            span,
            "openai",
            "gpt-6.1-sol",
        )
        .collect::<Vec<_>>()
        .await;
        let fields = fields.0.lock().unwrap().clone();
        assert!(
            !fields.keys().any(|key| key.contains("messages")
                || key.contains("definitions")
                || key == "gen_ai.system_instructions"),
            "{fields:?}"
        );
        assert_eq!(fields.get("gen_ai.request.stream").map(String::as_str), Some("true"));
        assert!(fields.get("error.type").is_some_and(|code| !code.is_empty()), "{fields:?}");
    }

    #[test]
    fn messages_map_every_part_kind() {
        let call = Content {
            role: "model".into(),
            parts: vec![
                Part::Thinking { thinking: "check the tool".into(), signature: None },
                Part::FunctionCall {
                    name: "get_weather".into(),
                    args: json!({ "city": "Nairobi" }),
                    id: Some("toolu_1".into()),
                    thought_signature: None,
                },
            ],
        };
        let result = Content {
            role: "function".into(),
            parts: vec![Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new(
                    "get_weather",
                    json!({ "c": 24 }),
                ),
                id: Some("toolu_1".into()),
                annotations: None,
            }],
        };
        let (_, messages) = input_messages(&[call, result]);
        assert_eq!(
            messages,
            json!([
                { "role": "assistant", "parts": [
                    { "type": "reasoning", "content": "check the tool" },
                    { "type": "tool_call", "id": "toolu_1", "name": "get_weather", "arguments": { "city": "Nairobi" } }
                ] },
                { "role": "tool", "parts": [
                    { "type": "tool_call_response", "id": "toolu_1", "response": { "c": 24 } }
                ] }
            ])
        );
        assert_eq!(finish_reason(FinishReason::Refusal), "content_filter");
    }
}
