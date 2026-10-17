//! Content-free diagnostics for Responses deserialization failures.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::openai) enum Layer {
    StreamSetup,
    StreamEvent,
    ResponseBody,
}

#[derive(Serialize)]
struct Diagnostic {
    layer: Layer,
    category: &'static str,
    line: usize,
    column: usize,
    cause: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_type: Option<String>,
}

pub(in crate::openai) fn error(error: &serde_json::Error, body: &str, layer: Layer) -> AdkError {
    let category = match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    };
    // Serde data errors may include arbitrary provider text. Only static labels
    // and exact schema-field matches leave this boundary.
    let message = error.to_string();
    let (cause, field) = if error.is_data() {
        if message.starts_with("missing field `") {
            ("missing_field", schema_field(&message, "missing field `", error))
        } else if message.starts_with("duplicate field `") {
            ("duplicate_field", schema_field(&message, "duplicate field `", error))
        } else if message.starts_with("invalid type: ") {
            ("invalid_type", None)
        } else if message.starts_with("invalid value: ") {
            ("invalid_value", None)
        } else if message.starts_with("unknown variant `") {
            ("unknown_variant", None)
        } else {
            ("other", None)
        }
    } else {
        ("other", None)
    };
    let diagnostic = Diagnostic {
        layer,
        category,
        line: error.line(),
        column: error.column(),
        cause,
        field,
        event_type: matches!(layer, Layer::StreamEvent).then(|| event_type(body)).flatten(),
    };
    let mut mapped = AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Internal,
        "model.openai_responses.parse",
        "OpenAI Responses API parse error",
    )
    .with_provider("openai-responses");
    mapped.details.metadata.insert(
        "openai_responses_parse".into(),
        serde_json::to_value(diagnostic).expect("static parse diagnostic serializes"),
    );
    mapped
}

fn schema_field(message: &str, prefix: &str, error: &serde_json::Error) -> Option<&'static str> {
    let suffix = if error.line() == 0 {
        "`".to_owned()
    } else {
        format!("` at line {} column {}", error.line(), error.column())
    };
    let field = message.strip_prefix(prefix)?.strip_suffix(&suffix)?;
    // async-openai 0.41.3 Responses envelope, events and common nested output.
    const FIELDS: &[&str] = &[
        "type",
        "id",
        "object",
        "created_at",
        "model",
        "output",
        "status",
        "sequence_number",
        "response",
        "output_index",
        "item",
        "item_id",
        "content_index",
        "part",
        "delta",
        "text",
        "refusal",
        "arguments",
        "name",
        "summary_index",
        "partial_image_index",
        "annotation",
        "annotation_index",
        "input",
        "message",
        "code",
        "param",
        "role",
        "content",
        "annotations",
        "call_id",
        "summary",
        "encrypted_content",
        "input_tokens",
        "output_tokens",
        "total_tokens",
        "cached_tokens",
        "reasoning_tokens",
        "input_tokens_details",
        "output_tokens_details",
        "url",
        "title",
        "start_index",
        "end_index",
        "file_id",
        "filename",
        "index",
        "token",
        "logprob",
        "bytes",
        "top_logprobs",
        "server_label",
        "tools",
        "result",
        "action",
        "queries",
        "query",
        "sources",
    ];
    FIELDS.iter().copied().find(|candidate| *candidate == field)
}

fn event_type(body: &str) -> Option<String> {
    if body.len() > 64 * 1024 {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let name = value.get("type")?.as_str()?;
    // Preserve new protocol event identifiers without accepting arbitrary text.
    let technical = name.len() <= 96
        && name.strip_prefix("response.").is_some_and(|suffix| {
            suffix.split('.').all(|segment| {
                !segment.is_empty()
                    && segment.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        });
    Some(if name == "error" || technical { name } else { "unknown" }.to_owned())
}

#[cfg(test)]
#[path = "responses_parse/tests.rs"]
mod tests;
