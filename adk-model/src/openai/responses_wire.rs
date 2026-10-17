//! Responses wire projections validate consumed fields, not echoed request schemas.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use async_openai::types::responses::{
    IncompleteDetails, OutputItem, OutputStatus, Response as SdkResponse, ResponseStreamEvent,
    ResponseUsage, Status,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::parse;

#[derive(Clone, Deserialize)]
pub(crate) struct Response {
    pub id: String,
    pub status: Status,
    #[serde(default)]
    pub output: Vec<OutputItem>,
    #[serde(skip)]
    pub output_missing: bool,
    pub usage: Option<ResponseUsage>,
    pub incomplete_details: Option<IncompleteDetails>,
    pub service_tier: Option<Value>,
}

impl From<&SdkResponse> for Response {
    fn from(response: &SdkResponse) -> Self {
        Self {
            id: response.id.clone(),
            status: response.status.clone(),
            output: response.output.clone(),
            output_missing: false,
            usage: response.usage.clone(),
            incomplete_details: response.incomplete_details.clone(),
            service_tier: response.service_tier.and_then(|tier| serde_json::to_value(tier).ok()),
        }
    }
}

pub(in crate::openai) enum Event {
    Activity,
    Content(Box<ResponseStreamEvent>),
    Terminal(Response),
}

fn decode<T: DeserializeOwned>(
    value: Value,
    body: &Value,
    layer: parse::Layer,
) -> Result<T, AdkError> {
    serde_json::from_value(value).map_err(|error| parse::error(&error, &body.to_string(), layer))
}

#[derive(Deserialize)]
struct Kind {
    r#type: String,
}

#[derive(Deserialize)]
struct Identity {
    id: String,
}

#[derive(Deserialize)]
struct Lifecycle {
    response: Identity,
}

#[derive(Deserialize)]
struct Failure {
    code: Option<String>,
}

#[derive(Deserialize)]
struct Header {
    status: Status,
    error: Option<Failure>,
}

pub(in crate::openai) fn event(value: Value) -> Result<Event, AdkError> {
    let kind: Kind = decode(value.clone(), &value, parse::Layer::StreamEvent)?;
    match kind.r#type.as_str() {
        "response.created" | "response.in_progress" | "response.queued" => {
            let lifecycle: Lifecycle = decode(value.clone(), &value, parse::Layer::StreamEvent)?;
            if lifecycle.response.id.is_empty() {
                return Err(invalid("invalid_identity", "OpenAI Responses identity is empty"));
            }
            Ok(Event::Activity)
        }
        "response.completed" | "response.done" | "response.incomplete" => {
            let projected = response(
                value.get("response").cloned().unwrap_or(Value::Null),
                &value,
                parse::Layer::StreamEvent,
            )?;
            if kind.r#type == "response.incomplete" && projected.status != Status::Incomplete {
                return Err(invalid(
                    "invalid_terminal",
                    "OpenAI Responses terminal status conflicts",
                ));
            }
            Ok(Event::Terminal(projected))
        }
        "response.failed" => {
            let header: Header = decode(
                value.get("response").cloned().unwrap_or(Value::Null),
                &value,
                parse::Layer::StreamEvent,
            )?;
            Err(stream_error(header.error.as_ref().and_then(|error| error.code.as_deref())))
        }
        "response.cancelled" | "response.canceled" => {
            Err(invalid("cancelled", "OpenAI Responses generation was cancelled"))
        }
        "error" => {
            let failure: Failure = decode(value.clone(), &value, parse::Layer::StreamEvent)?;
            Err(stream_error(failure.code.as_deref()))
        }
        "response.output_item.added"
        | "response.output_item.done"
        | "response.output_text.delta"
        | "response.output_text.done"
        | "response.content_part.added"
        | "response.content_part.done"
        | "response.refusal.delta"
        | "response.refusal.done"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_summary_text.done"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done"
        | "response.reasoning_text.delta"
        | "response.reasoning_text.done"
        | "response.function_call_arguments.delta"
        | "response.function_call_arguments.done"
        | "response.custom_tool_call_input.delta"
        | "response.custom_tool_call_input.done" => {
            decode(value.clone(), &value, parse::Layer::StreamEvent)
                .map(|event| Event::Content(Box::new(event)))
        }
        // Preserve forward compatibility for unknown top-level event types.
        // A terminal response remains mandatory and its output stays strict.
        _ => Ok(Event::Activity),
    }
}

pub(in crate::openai) fn response(
    mut value: Value,
    body: &Value,
    layer: parse::Layer,
) -> Result<Response, AdkError> {
    let header: Header = decode(value.clone(), body, layer)?;
    if header.status == Status::Failed {
        return Err(stream_error(header.error.as_ref().and_then(|error| error.code.as_deref())));
    }
    // Detailed usage is optional on compatible providers; absent breakdowns do
    // not invalidate the authoritative input/output/total counts.
    if let Some(usage) = value.get_mut("usage").and_then(Value::as_object_mut) {
        for (key, details) in [
            ("input_tokens_details", json!({"cached_tokens":0})),
            ("output_tokens_details", json!({"reasoning_tokens":0})),
        ] {
            if usage.get(key).is_none_or(Value::is_null) {
                usage.insert(key.into(), details);
            }
        }
    }
    let output_missing = value.get("output").is_none();
    if output_missing && !matches!(layer, parse::Layer::StreamEvent) {
        return Err(parse::error(
            &<serde_json::Error as serde::de::Error>::missing_field("output"),
            &body.to_string(),
            layer,
        ));
    }
    let mut projected: Response = decode(value, body, layer)?;
    projected.output_missing = output_missing;
    validate(&projected)?;
    Ok(projected)
}

pub(in crate::openai) fn finalized_call(item: &OutputItem) -> Option<&str> {
    match item {
        OutputItem::FunctionCall(call)
            if !call.call_id.is_empty()
                && !call.name.is_empty()
                && call.status.as_ref().is_none_or(|status| *status == OutputStatus::Completed)
                && super::super::convert::decode_tool_call_arguments(Some(&Value::String(
                    call.arguments.clone(),
                )))
                .is_ok() =>
        {
            Some(&call.call_id)
        }
        OutputItem::CustomToolCall(call) if !call.call_id.is_empty() && !call.name.is_empty() => {
            Some(&call.call_id)
        }
        _ => None,
    }
}

pub(crate) fn validate(response: &Response) -> Result<(), AdkError> {
    if response.id.is_empty() {
        return Err(invalid("invalid_identity", "OpenAI Responses identity is empty"));
    }
    let mut calls = std::collections::HashSet::new();
    for item in &response.output {
        let (call_id, name, unfinished) = match item {
            OutputItem::FunctionCall(call) => (
                &call.call_id,
                &call.name,
                call.status.as_ref().is_some_and(|status| *status != OutputStatus::Completed),
            ),
            OutputItem::CustomToolCall(call) => (&call.call_id, &call.name, false),
            _ => continue,
        };
        if call_id.is_empty() || name.is_empty() || !calls.insert(call_id) {
            return Err(invalid("invalid_identity", "OpenAI Responses tool identity is ambiguous"));
        }
        if response.status != Status::Completed || unfinished {
            return Err(invalid("invalid_terminal", "OpenAI Responses tool call is unfinished"));
        }
    }
    match response.status {
        Status::Completed => Ok(()),
        Status::Incomplete
            if response.incomplete_details.as_ref().is_some_and(|details| {
                matches!(details.reason.as_str(), "max_output_tokens" | "content_filter")
            }) =>
        {
            Ok(())
        }
        Status::Incomplete => {
            Err(invalid("incomplete", "OpenAI Responses generation is incomplete"))
        }
        Status::Failed => Err(stream_error(None)),
        Status::Cancelled => Err(invalid("cancelled", "OpenAI Responses generation was cancelled")),
        Status::InProgress | Status::Queued => {
            Err(invalid("invalid_terminal", "OpenAI Responses generation has not completed"))
        }
    }
}

pub(in crate::openai) fn unfinished() -> AdkError {
    invalid("unfinished_stream", "OpenAI Responses stream ended before completion")
}

pub(in crate::openai) fn missing_output() -> AdkError {
    invalid("invalid_terminal", "OpenAI Responses terminal output is missing")
}

/// OpenAI's documented `response.error.code` values. Only these leave this boundary: a
/// compatible provider's code is free text and can echo request content.
pub(in crate::openai) const DOCUMENTED_ERROR_CODES: &[&str] = &[
    "server_error",
    "rate_limit_exceeded",
    "invalid_prompt",
    "vector_store_timeout",
    "invalid_image",
    "invalid_image_format",
    "invalid_base64_image",
    "invalid_image_url",
    "image_too_large",
    "image_too_small",
    "image_parse_error",
    "image_content_policy_violation",
    "invalid_image_mode",
    "image_file_too_large",
    "unsupported_image_media_type",
    "empty_image_file",
    "failed_to_download_image",
    "image_file_not_found",
];

fn stream_error(code: Option<&str>) -> AdkError {
    let code =
        code.and_then(|code| DOCUMENTED_ERROR_CODES.iter().copied().find(|known| *known == code));
    let category = match code {
        Some("rate_limit_exceeded") => ErrorCategory::RateLimited,
        Some("server_error") => ErrorCategory::Unavailable,
        Some("vector_store_timeout") => ErrorCategory::Timeout,
        Some(_) => ErrorCategory::InvalidInput,
        None => ErrorCategory::Internal,
    };
    let message = match code {
        Some(code) => format!("OpenAI Responses generation failed: {code}"),
        None => "OpenAI Responses generation failed".to_string(),
    };
    let error = AdkError::new(
        ErrorComponent::Model,
        category,
        "model.openai_responses.response_failed",
        message,
    )
    .with_provider("openai-responses");
    crate::error_metadata::with_code(error, code)
}

fn invalid(cause: &'static str, message: &'static str) -> AdkError {
    let code = match cause {
        "cancelled" => "model.openai_responses.cancelled",
        "incomplete" => "model.openai_responses.incomplete",
        "invalid_identity" => "model.openai_responses.invalid_identity",
        "invalid_terminal" => "model.openai_responses.invalid_terminal",
        "unfinished_stream" => "model.openai_responses.unfinished_stream",
        _ => "model.openai_responses.unsupported_event",
    };
    let category =
        if cause == "cancelled" { ErrorCategory::Cancelled } else { ErrorCategory::Internal };
    AdkError::new(ErrorComponent::Model, category, code, message).with_provider("openai-responses")
}
