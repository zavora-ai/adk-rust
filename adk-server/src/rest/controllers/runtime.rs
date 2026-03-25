use crate::ServerConfig;
use crate::auth_bridge::{RequestContextError, RequestContextExtractor};
use crate::rest::controllers::ui::{
    McpUiInitializeParams, McpUiMessageParams, McpUiUpdateModelContextParams,
    initialize_mcp_ui_bridge, mark_mcp_ui_initialized, message_mcp_ui_bridge,
    update_mcp_ui_bridge_model_context,
};
use crate::ui_protocol::{
    SUPPORTED_UI_PROTOCOLS, UI_PROTOCOL_CAPABILITIES, normalize_runtime_ui_protocol,
};
use adk_core::{RequestContext, SessionId, UserId};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures::{StreamExt, stream::Stream};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use tracing::{Instrument, info, warn};
use uuid::Uuid;

fn default_streaming_true() -> bool {
    true
}

const UI_PROTOCOL_HEADER: &str = "x-adk-ui-protocol";
const UI_TRANSPORT_HEADER: &str = "x-adk-ui-transport";

#[derive(Clone)]
pub struct RuntimeController {
    config: ServerConfig,
}

impl RuntimeController {
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }
}

/// Attachment structure for the legacy /run endpoint
#[derive(Serialize, Deserialize, Debug)]
pub struct Attachment {
    pub name: String,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub base64: String,
}

#[derive(Serialize, Deserialize)]
pub struct RunRequest {
    pub new_message: String,
    #[serde(default, alias = "uiProtocol")]
    pub ui_protocol: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default, alias = "ui_transport")]
    pub ui_transport: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

/// Request format for /run_sse (adk-go compatible)
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunSseRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    #[serde(default)]
    pub new_message: Option<NewMessage>,
    #[serde(default = "default_streaming_true")]
    pub streaming: bool,
    #[serde(default)]
    pub state_delta: Option<Value>,
    #[serde(default, alias = "ui_protocol")]
    pub ui_protocol: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default, alias = "ui_transport")]
    pub ui_transport: Option<String>,
    #[serde(default)]
    pub input: Option<AgUiRunInput>,
    #[serde(default)]
    pub ag_ui_input: Option<AgUiRunInput>,
    #[serde(default)]
    pub ag_ui_compatibility_event: Option<Value>,
    #[serde(default)]
    pub protocol_envelope: Option<Value>,
    #[serde(default)]
    pub mcp_apps_request: Option<McpAppsRuntimeEnvelope>,
    #[serde(default)]
    pub mcp_apps_initialize: Option<McpAppsRuntimeEnvelope>,
    #[serde(default)]
    pub mcp_apps_initialized: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgUiInputMessage {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub activity_type: Option<String>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub replace: Option<bool>,
    #[serde(default)]
    pub patch: Option<Vec<Value>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgUiRunInput {
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub state: Option<Value>,
    #[serde(default)]
    pub messages: Vec<AgUiInputMessage>,
    #[serde(default)]
    pub tools: Vec<Value>,
    #[serde(default)]
    pub context: Vec<Value>,
    #[serde(default)]
    pub forwarded_props: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct McpAppsRuntimeEnvelope {
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NewMessage {
    pub role: String,
    pub parts: Vec<MessagePart>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessagePart {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "inlineData")]
    pub inline_data: Option<InlineData>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InlineData {
    pub display_name: Option<String>,
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiProfile {
    AdkUi,
    A2ui,
    AgUi,
    McpApps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiTransportMode {
    LegacyWrapper,
    ProtocolNative,
}

impl UiProfile {
    fn as_str(self) -> &'static str {
        match self {
            Self::AdkUi => "adk_ui",
            Self::A2ui => "a2ui",
            Self::AgUi => "ag_ui",
            Self::McpApps => "mcp_apps",
        }
    }
}

type RuntimeError = (StatusCode, String);

/// Convert an `AdkError` into a `RuntimeError` using the structured error envelope.
///
/// Uses `AdkError::http_status_code()` for the HTTP status and
/// `AdkError::to_problem_json()` for the response body. The problem JSON
/// includes `retry_after_ms` when the error carries retry guidance.
fn adk_err_to_runtime(err: adk_core::AdkError) -> RuntimeError {
    let status =
        StatusCode::from_u16(err.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = err.to_problem_json().to_string();
    (status, body)
}

fn parse_ui_profile(raw: &str) -> Option<UiProfile> {
    match normalize_runtime_ui_protocol(raw)? {
        "adk_ui" => Some(UiProfile::AdkUi),
        "a2ui" => Some(UiProfile::A2ui),
        "ag_ui" => Some(UiProfile::AgUi),
        "mcp_apps" => Some(UiProfile::McpApps),
        _ => None,
    }
}

fn resolve_ui_profile(
    headers: &HeaderMap,
    body_ui_protocol: Option<&str>,
) -> Result<UiProfile, RuntimeError> {
    let header_value = headers.get(UI_PROTOCOL_HEADER).and_then(|v| v.to_str().ok());
    let candidate = header_value.or(body_ui_protocol);

    let Some(raw) = candidate else {
        return Ok(UiProfile::AdkUi);
    };

    parse_ui_profile(raw).ok_or_else(|| {
        let supported = SUPPORTED_UI_PROTOCOLS.join(", ");
        warn!(
            requested = %raw,
            header = %UI_PROTOCOL_HEADER,
            "unsupported ui protocol requested"
        );
        (
            StatusCode::BAD_REQUEST,
            format!("Unsupported ui protocol '{}'. Supported profiles: {}", raw, supported),
        )
    })
}

fn parse_ui_transport(raw: &str) -> Option<UiTransportMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "legacy" | "legacy_wrapper" => Some(UiTransportMode::LegacyWrapper),
        "native" | "protocol_native" => Some(UiTransportMode::ProtocolNative),
        _ => None,
    }
}

fn resolve_ui_transport(
    headers: &HeaderMap,
    body_ui_transport: Option<&str>,
) -> Result<UiTransportMode, RuntimeError> {
    let header_value = headers.get(UI_TRANSPORT_HEADER).and_then(|v| v.to_str().ok());
    let candidate = header_value.or(body_ui_transport);

    let Some(raw) = candidate else {
        return Ok(UiTransportMode::LegacyWrapper);
    };

    parse_ui_transport(raw).ok_or_else(|| {
        warn!(
            requested = %raw,
            header = %UI_TRANSPORT_HEADER,
            "unsupported ui transport requested"
        );
        (
            StatusCode::BAD_REQUEST,
            format!(
                "Unsupported ui transport '{}'. Supported values: legacy_wrapper, protocol_native",
                raw
            ),
        )
    })
}

fn validate_transport_support(
    profile: UiProfile,
    transport: UiTransportMode,
) -> Result<(), RuntimeError> {
    if transport == UiTransportMode::ProtocolNative && profile != UiProfile::AgUi {
        return Err((
            StatusCode::BAD_REQUEST,
            "protocol_native transport is currently available only for ag_ui; use the MCP Apps bridge endpoints for mcp_apps".to_string(),
        ));
    }
    Ok(())
}

fn protocol_from_envelope(envelope: &Value) -> Option<&str> {
    envelope.as_object().and_then(|object| object.get("protocol")).and_then(|value| value.as_str())
}

fn serialize_runtime_event(event: &adk_core::Event, profile: UiProfile) -> Option<String> {
    if profile == UiProfile::AdkUi {
        return serde_json::to_string(event).ok();
    }

    serde_json::to_string(&json!({
        "ui_protocol": profile.as_str(),
        "event": event
    }))
    .ok()
}

fn infer_sse_request_protocol(req: &RunSseRequest) -> Option<&str> {
    req.ui_protocol
        .as_deref()
        .or(req.protocol.as_deref())
        .or_else(|| req.protocol_envelope.as_ref().and_then(protocol_from_envelope))
        .or_else(|| req.ag_ui_input.as_ref().map(|_| "ag_ui"))
        .or_else(|| req.input.as_ref().map(|_| "ag_ui"))
        .or_else(|| req.mcp_apps_request.as_ref().map(|_| "mcp_apps"))
        .or_else(|| req.mcp_apps_initialize.as_ref().map(|_| "mcp_apps"))
}

fn infer_run_request_protocol(req: &RunRequest) -> Option<&str> {
    req.ui_protocol.as_deref().or(req.protocol.as_deref())
}

fn ag_ui_input_from_request(req: &RunSseRequest) -> Option<AgUiRunInput> {
    req.ag_ui_input.clone().or_else(|| req.input.clone()).or_else(|| {
        let envelope = req.protocol_envelope.as_ref()?;
        if protocol_from_envelope(envelope)? != "ag_ui" {
            return None;
        }
        envelope
            .as_object()
            .and_then(|object| object.get("input"))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    })
}

fn mcp_apps_request_from_request(req: &RunSseRequest) -> Option<McpAppsRuntimeEnvelope> {
    req.mcp_apps_request.clone().or_else(|| {
        if let Some(method) = req.method.clone() {
            return Some(McpAppsRuntimeEnvelope { method, params: req.params.clone() });
        }
        let envelope = req.protocol_envelope.as_ref()?;
        if protocol_from_envelope(envelope)? != "mcp_apps" {
            return None;
        }
        let object = envelope.as_object()?;
        let method = object.get("method")?.as_str()?.to_string();
        let params = object.get("params").cloned();
        Some(McpAppsRuntimeEnvelope { method, params })
    })
}

fn mcp_apps_initialize_from_request(req: &RunSseRequest) -> Option<McpAppsRuntimeEnvelope> {
    req.mcp_apps_initialize.clone()
}

fn extract_text_segments(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() { vec![] } else { vec![trimmed.to_string()] }
        }
        Value::Array(items) => items
            .iter()
            .flat_map(|item| {
                if let Some(text) = item
                    .as_object()
                    .and_then(|object| object.get("text"))
                    .and_then(|text| text.as_str())
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return vec![trimmed.to_string()];
                    }
                }
                vec![]
            })
            .collect(),
        Value::Object(object) => object
            .get("text")
            .and_then(|text| text.as_str())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .into_iter()
            .collect(),
        _ => vec![],
    }
}

fn new_message_from_ag_ui_input(input: &AgUiRunInput) -> Option<NewMessage> {
    let selected = input
        .messages
        .iter()
        .rev()
        .find(|message| message.role.as_deref().unwrap_or("user") == "user")
        .or_else(|| input.messages.last())?;

    let content = selected.content.as_ref()?;
    let parts: Vec<MessagePart> = extract_text_segments(content)
        .into_iter()
        .map(|text| MessagePart { text: Some(text), inline_data: None })
        .collect();
    if parts.is_empty() {
        return None;
    }

    Some(NewMessage { role: selected.role.clone().unwrap_or_else(|| "user".to_string()), parts })
}

fn activity_content_snapshot(value: Option<&Value>) -> Value {
    match value.cloned() {
        Some(Value::Object(object)) => Value::Object(object),
        Some(other) => json!({ "value": other }),
        None => json!({}),
    }
}

fn activity_message_id(message: &AgUiInputMessage) -> String {
    message.id.clone().unwrap_or_else(|| format!("activity-{}", Uuid::new_v4()))
}

fn activity_message_type(message: &AgUiInputMessage) -> String {
    message
        .activity_type
        .clone()
        .or_else(|| message.name.clone())
        .unwrap_or_else(|| "CUSTOM".to_string())
}

fn activity_events_from_ag_ui_input(input: &AgUiRunInput) -> Vec<Value> {
    input
        .messages
        .iter()
        .filter(|message| message.role.as_deref() == Some("activity"))
        .map(|message| {
            let timestamp = chrono::Utc::now().timestamp_millis().max(0) as u64;
            let message_id = activity_message_id(message);
            let activity_type = activity_message_type(message);
            if let Some(patch) = &message.patch {
                json!({
                    "type": "ACTIVITY_DELTA",
                    "messageId": message_id,
                    "activityType": activity_type,
                    "patch": patch,
                    "timestamp": timestamp,
                })
            } else {
                let mut event = json!({
                    "type": "ACTIVITY_SNAPSHOT",
                    "messageId": message_id,
                    "activityType": activity_type,
                    "content": activity_content_snapshot(message.content.as_ref()),
                    "timestamp": timestamp,
                });
                if let Some(replace) = message.replace {
                    if let Some(object) = event.as_object_mut() {
                        object.insert("replace".to_string(), Value::Bool(replace));
                    }
                }
                event
            }
        })
        .collect()
}

fn messages_snapshot_from_ag_ui_input(input: &AgUiRunInput) -> Option<Value> {
    if input.messages.is_empty() {
        return None;
    }

    let filtered: Vec<AgUiInputMessage> = input
        .messages
        .iter()
        .filter(|message| !(message.role.as_deref() == Some("activity") && message.patch.is_some()))
        .cloned()
        .collect();
    if filtered.is_empty() {
        return None;
    }

    serde_json::to_value(filtered).ok()
}

fn object_entries_to_state_delta(object: &Map<String, Value>) -> HashMap<String, Value> {
    object.iter().map(|(key, value)| (key.clone(), value.clone())).collect()
}

fn ag_ui_state_delta(input: &AgUiRunInput) -> HashMap<String, Value> {
    let mut delta = HashMap::new();

    if let Some(state) = input.state.clone() {
        match state {
            Value::Object(object) => {
                delta.extend(object_entries_to_state_delta(&object));
            }
            value => {
                delta.insert("temp:ag_ui_state".to_string(), value);
            }
        }
    }

    if !input.messages.is_empty() {
        if let Ok(value) = serde_json::to_value(&input.messages) {
            delta.insert("temp:ag_ui_messages".to_string(), value);
        }
    }
    if !input.tools.is_empty() {
        delta.insert("temp:ag_ui_tools".to_string(), Value::Array(input.tools.clone()));
    }
    if !input.context.is_empty() {
        delta.insert("temp:ag_ui_context".to_string(), Value::Array(input.context.clone()));
    }
    if let Some(forwarded_props) = input.forwarded_props.clone() {
        delta.insert("temp:ag_ui_forwarded_props".to_string(), forwarded_props);
    }

    delta
}

fn body_state_delta(value: Option<&Value>) -> Result<HashMap<String, Value>, RuntimeError> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        (StatusCode::BAD_REQUEST, "stateDelta must be a JSON object when provided".to_string())
    })?;
    Ok(object_entries_to_state_delta(object))
}

fn log_profile_deprecation(profile: UiProfile) {
    if profile != UiProfile::AdkUi {
        return;
    }
    let Some(spec) = UI_PROTOCOL_CAPABILITIES
        .iter()
        .find(|capability| capability.protocol == profile.as_str())
        .and_then(|capability| capability.deprecation)
    else {
        return;
    };

    warn!(
        protocol = %profile.as_str(),
        stage = %spec.stage,
        announced_on = %spec.announced_on,
        sunset_target_on = ?spec.sunset_target_on,
        replacements = ?spec.replacement_protocols,
        "legacy ui protocol profile selected"
    );
}

/// Build Content from message text and attachments
fn build_content_with_attachments(
    text: &str,
    attachments: &[Attachment],
) -> Result<adk_core::Content, RuntimeError> {
    let mut content = adk_core::Content::new("user");

    // Add the text part
    content.parts.push(adk_core::Part::Text { text: text.to_string() });

    // Add attachment parts
    for attachment in attachments {
        match BASE64_STANDARD.decode(&attachment.base64) {
            Ok(data) => {
                if data.len() > adk_core::MAX_INLINE_DATA_SIZE {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!(
                            "Attachment '{}' exceeds max inline size of {} bytes",
                            attachment.name,
                            adk_core::MAX_INLINE_DATA_SIZE
                        ),
                    ));
                }
                content.parts.push(adk_core::Part::InlineData {
                    mime_type: attachment.mime_type.clone(),
                    data,
                });
            }
            Err(e) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("Invalid base64 data for attachment '{}': {}", attachment.name, e),
                ));
            }
        }
    }

    Ok(content)
}

/// Build Content from message parts (for /run_sse endpoint)
fn build_content_from_parts(parts: &[MessagePart]) -> Result<adk_core::Content, RuntimeError> {
    let mut content = adk_core::Content::new("user");

    for part in parts {
        // Add text part if present
        if let Some(text) = &part.text {
            content.parts.push(adk_core::Part::Text { text: text.clone() });
        }

        // Add inline data part if present
        if let Some(inline_data) = &part.inline_data {
            match BASE64_STANDARD.decode(&inline_data.data) {
                Ok(data) => {
                    if data.len() > adk_core::MAX_INLINE_DATA_SIZE {
                        return Err((
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!(
                                "inline_data exceeds max inline size of {} bytes",
                                adk_core::MAX_INLINE_DATA_SIZE
                            ),
                        ));
                    }
                    content.parts.push(adk_core::Part::InlineData {
                        mime_type: inline_data.mime_type.clone(),
                        data,
                    });
                }
                Err(e) => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("Invalid base64 data in inline_data: {}", e),
                    ));
                }
            }
        }
    }

    Ok(content)
}

async fn apply_state_delta_to_session(
    session_service: &std::sync::Arc<dyn adk_session::SessionService>,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    state_delta: HashMap<String, Value>,
) -> Result<(), RuntimeError> {
    if state_delta.is_empty() {
        return Ok(());
    }

    let identity = adk_core::AdkIdentity::new(
        adk_core::AppName::try_from(app_name).map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid app_name for state delta application: {}", error),
            )
        })?,
        adk_core::UserId::try_from(user_id).map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid user_id for state delta application: {}", error),
            )
        })?,
        adk_core::SessionId::try_from(session_id).map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid session_id for state delta application: {}", error),
            )
        })?,
    );

    let mut event = adk_core::Event::new(format!("ui-input-{}", Uuid::new_v4()));
    event.author = "ui_protocol_bridge".to_string();
    event.actions.state_delta = state_delta;
    session_service
        .append_event_for_identity(adk_session::AppendEventRequest { identity, event })
        .await
        .map_err(adk_err_to_runtime)
}

fn merge_runtime_state_delta(
    body_delta: HashMap<String, Value>,
    ag_ui_delta: HashMap<String, Value>,
) -> HashMap<String, Value> {
    let mut merged = body_delta;
    merged.extend(ag_ui_delta);
    merged
}

fn json_pointer_escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn state_delta_to_json_patch(delta: &HashMap<String, Value>) -> Vec<Value> {
    delta
        .iter()
        .map(|(key, value)| {
            json!({
                "op": "add",
                "path": format!("/{}", json_pointer_escape(key)),
                "value": value
            })
        })
        .collect()
}

fn timestamp_millis(event: &adk_core::Event) -> u64 {
    event.timestamp.timestamp_millis().max(0) as u64
}

fn serialize_ag_ui_tool_call_delta(args: &Value, allow_raw_string_delta: bool) -> String {
    if allow_raw_string_delta {
        if let Value::String(delta) = args {
            return delta.clone();
        }
    }

    serde_json::to_string(args).unwrap_or_else(|_| args.to_string())
}

fn translate_ag_ui_event(event: &adk_core::Event, thread_id: &str, run_id: &str) -> Vec<Value> {
    let mut translated = Vec::new();
    let timestamp = timestamp_millis(event);
    let is_partial = event.llm_response.partial;

    if !event.actions.state_delta.is_empty() {
        translated.push(json!({
            "type": "STATE_DELTA",
            "delta": state_delta_to_json_patch(&event.actions.state_delta),
            "timestamp": timestamp,
        }));
    }

    if let Some(message) = event.llm_response.error_message.clone() {
        translated.push(json!({
            "type": "RUN_ERROR",
            "threadId": thread_id,
            "runId": run_id,
            "message": message,
            "code": event.llm_response.error_code,
            "timestamp": timestamp,
        }));
    }

    let Some(content) = &event.llm_response.content else {
        return translated;
    };

    for (index, part) in content.parts.iter().enumerate() {
        match part {
            adk_core::Part::Text { text } if !text.trim().is_empty() => {
                let message_id = format!("{}-text-{}", event.id, index);
                if is_partial {
                    translated.push(json!({
                        "type": "TEXT_MESSAGE_CHUNK",
                        "messageId": message_id,
                        "role": "assistant",
                        "delta": text,
                        "timestamp": timestamp,
                    }));
                } else {
                    translated.push(json!({
                        "type": "TEXT_MESSAGE_START",
                        "messageId": message_id,
                        "role": "assistant",
                        "timestamp": timestamp,
                    }));
                    translated.push(json!({
                        "type": "TEXT_MESSAGE_CONTENT",
                        "messageId": format!("{}-text-{}", event.id, index),
                        "delta": text,
                        "timestamp": timestamp,
                    }));
                    translated.push(json!({
                        "type": "TEXT_MESSAGE_END",
                        "messageId": format!("{}-text-{}", event.id, index),
                        "timestamp": timestamp,
                    }));
                }
            }
            adk_core::Part::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                let message_id = format!("{}-reasoning-{}", event.id, index);
                if is_partial {
                    translated.push(json!({
                        "type": "REASONING_MESSAGE_CHUNK",
                        "messageId": message_id,
                        "delta": thinking,
                        "timestamp": timestamp,
                    }));
                } else {
                    let reasoning_id = format!("{}-reasoning-phase-{}", event.id, index);
                    translated.push(json!({
                        "type": "REASONING_START",
                        "messageId": reasoning_id,
                        "timestamp": timestamp,
                    }));
                    translated.push(json!({
                        "type": "REASONING_MESSAGE_START",
                        "messageId": message_id,
                        "role": "assistant",
                        "timestamp": timestamp,
                    }));
                    translated.push(json!({
                        "type": "REASONING_MESSAGE_CONTENT",
                        "messageId": format!("{}-reasoning-{}", event.id, index),
                        "delta": thinking,
                        "timestamp": timestamp,
                    }));
                    translated.push(json!({
                        "type": "REASONING_MESSAGE_END",
                        "messageId": format!("{}-reasoning-{}", event.id, index),
                        "timestamp": timestamp,
                    }));
                    translated.push(json!({
                        "type": "REASONING_END",
                        "messageId": reasoning_id,
                        "timestamp": timestamp,
                    }));
                }
            }
            adk_core::Part::FunctionCall { name, args, id, .. } => {
                let tool_call_id =
                    id.clone().unwrap_or_else(|| format!("{}-tool-call-{}", event.id, index));
                let raw_chunk_supported = is_partial && matches!(args, Value::String(_));
                let args_delta = serialize_ag_ui_tool_call_delta(args, raw_chunk_supported);
                if raw_chunk_supported {
                    translated.push(json!({
                        "type": "TOOL_CALL_CHUNK",
                        "toolCallId": tool_call_id,
                        "toolCallName": name,
                        "delta": args_delta,
                        "timestamp": timestamp,
                    }));
                } else {
                    translated.push(json!({
                        "type": "TOOL_CALL_START",
                        "toolCallId": tool_call_id,
                        "toolCallName": name,
                        "timestamp": timestamp,
                    }));
                    translated.push(json!({
                        "type": "TOOL_CALL_ARGS",
                        "toolCallId": id.clone().unwrap_or_else(|| format!("{}-tool-call-{}", event.id, index)),
                        "delta": args_delta,
                        "timestamp": timestamp,
                    }));
                    translated.push(json!({
                        "type": "TOOL_CALL_END",
                        "toolCallId": id.clone().unwrap_or_else(|| format!("{}-tool-call-{}", event.id, index)),
                        "timestamp": timestamp,
                    }));
                }
            }
            adk_core::Part::FunctionResponse { function_response, id } => {
                let tool_call_id =
                    id.clone().unwrap_or_else(|| format!("{}-tool-result-{}", event.id, index));
                let response_content = serde_json::to_string(&function_response.response)
                    .unwrap_or_else(|_| function_response.response.to_string());
                translated.push(json!({
                    "type": "TOOL_CALL_RESULT",
                    "messageId": format!("msg-{}", tool_call_id),
                    "toolCallId": tool_call_id,
                    "toolCallName": function_response.name,
                    "content": response_content,
                    "role": "tool",
                    "timestamp": timestamp,
                }));
            }
            _ => {}
        }
    }

    translated
}

/// Extract [`RequestContext`] from the configured extractor, if present.
///
/// Constructs minimal HTTP request [`Parts`] from the provided headers so the
/// extractor can inspect `Authorization` and other headers. Returns `None`
/// when no extractor is configured (fall-through to existing behavior).
async fn extract_request_context(
    extractor: Option<&dyn RequestContextExtractor>,
    headers: &HeaderMap,
) -> Result<Option<RequestContext>, RuntimeError> {
    let Some(extractor) = extractor else {
        return Ok(None);
    };

    // Build minimal Parts from the headers
    let mut builder = axum::http::Request::builder();
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let (parts, _) = builder
        .body(())
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to build request parts: {e}"))
        })?
        .into_parts();

    match extractor.extract(&parts).await {
        Ok(ctx) => Ok(Some(ctx)),
        Err(RequestContextError::MissingAuth) => {
            Err((StatusCode::UNAUTHORIZED, "missing authorization".to_string()))
        }
        Err(RequestContextError::InvalidToken(msg)) => {
            Err((StatusCode::UNAUTHORIZED, format!("invalid token: {msg}")))
        }
        Err(RequestContextError::ExtractionFailed(msg)) => {
            Err((StatusCode::INTERNAL_SERVER_ERROR, format!("auth extraction failed: {msg}")))
        }
    }
}

fn bridge_params_with_identity(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    params: Option<Value>,
) -> Value {
    let mut object = params.and_then(|value| value.as_object().cloned()).unwrap_or_default();
    object.insert("appName".to_string(), Value::String(app_name.to_string()));
    object.insert("userId".to_string(), Value::String(user_id.to_string()));
    object.insert("sessionId".to_string(), Value::String(session_id.to_string()));
    Value::Object(object)
}

fn deserialize_bridge_params<T: for<'de> Deserialize<'de>>(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    params: Option<Value>,
) -> Result<T, RuntimeError> {
    serde_json::from_value(bridge_params_with_identity(app_name, user_id, session_id, params))
        .map_err(|error| {
            (StatusCode::BAD_REQUEST, format!("invalid protocol-native bridge payload: {}", error))
        })
}

fn maybe_mark_mcp_ui_initialized(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    initialized_notification: Option<&Value>,
) -> Result<(), RuntimeError> {
    let Some(value) = initialized_notification else {
        return Ok(());
    };
    let method = value
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if method == "ui/notifications/initialized" {
        mark_mcp_ui_initialized(app_name, user_id, session_id)?;
    }
    Ok(())
}

fn apply_mcp_apps_runtime_envelope(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    envelope: McpAppsRuntimeEnvelope,
) -> Result<(), RuntimeError> {
    match envelope.method.as_str() {
        "ui/initialize" => {
            let params = deserialize_bridge_params::<McpUiInitializeParams>(
                app_name,
                user_id,
                session_id,
                envelope.params,
            )?;
            initialize_mcp_ui_bridge(params)?;
            Ok(())
        }
        "ui/message" => {
            let params = deserialize_bridge_params::<McpUiMessageParams>(
                app_name,
                user_id,
                session_id,
                envelope.params,
            )?;
            message_mcp_ui_bridge(params)?;
            Ok(())
        }
        "ui/update-model-context" => {
            let params = deserialize_bridge_params::<McpUiUpdateModelContextParams>(
                app_name,
                user_id,
                session_id,
                envelope.params,
            )?;
            update_mcp_ui_bridge_model_context(params)?;
            Ok(())
        }
        "ui/notifications/initialized" => {
            mark_mcp_ui_initialized(app_name, user_id, session_id)?;
            Ok(())
        }
        method => Err((
            StatusCode::BAD_REQUEST,
            format!("unsupported MCP Apps runtime bridge method '{}'", method),
        )),
    }
}

fn direct_ag_ui_events(event: &adk_core::Event, thread_id: &str, run_id: &str) -> Vec<String> {
    translate_ag_ui_event(event, thread_id, run_id)
        .into_iter()
        .filter_map(|item| serde_json::to_string(&item).ok())
        .collect()
}

fn build_runtime_sse_stream<S>(
    mut event_stream: S,
    profile: UiProfile,
    transport: UiTransportMode,
    session_id: String,
    ag_ui_input: Option<AgUiRunInput>,
) -> std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
where
    S: Stream<Item = adk_core::Result<adk_core::Event>> + Send + 'static + Unpin,
{
    let selected_thread_id =
        ag_ui_input.as_ref().and_then(|input| input.thread_id.clone()).unwrap_or(session_id);
    let selected_run_input = ag_ui_input.clone();
    let selected_parent_run_id = ag_ui_input.as_ref().and_then(|input| input.parent_run_id.clone());
    let selected_initial_state = ag_ui_input.as_ref().and_then(|input| input.state.clone());
    let selected_messages_snapshot =
        ag_ui_input.as_ref().and_then(messages_snapshot_from_ag_ui_input);
    let selected_activity_events =
        ag_ui_input.as_ref().map(activity_events_from_ag_ui_input).unwrap_or_default();

    Box::pin(async_stream::stream! {
        let native_ag_ui = profile == UiProfile::AgUi && transport == UiTransportMode::ProtocolNative;
        let mut started = false;
        let mut active_run_id = ag_ui_input.as_ref().and_then(|input| input.run_id.clone());

        while let Some(item) = event_stream.next().await {
            match item {
                Ok(event) => {
                    if native_ag_ui {
                        let run_id = active_run_id
                            .get_or_insert_with(|| event.invocation_id.clone())
                            .clone();
                        if !started {
                            let mut started_event = json!({
                                "type": "RUN_STARTED",
                                "threadId": selected_thread_id,
                                "runId": run_id,
                            });
                            if let Some(parent_run_id) = selected_parent_run_id.clone() {
                                if let Some(object) = started_event.as_object_mut() {
                                    object.insert("parentRunId".to_string(), Value::String(parent_run_id));
                                }
                            }
                            if let Some(run_input) = selected_run_input.clone() {
                                if let Ok(value) = serde_json::to_value(run_input) {
                                    if let Some(object) = started_event.as_object_mut() {
                                        object.insert("input".to_string(), value);
                                    }
                                }
                            }
                            yield Ok(Event::default().data(started_event.to_string()));

                            if let Some(snapshot) = selected_initial_state.clone() {
                                yield Ok(Event::default().data(json!({
                                    "type": "STATE_SNAPSHOT",
                                    "snapshot": snapshot,
                                }).to_string()));
                            }
                            if let Some(messages) = selected_messages_snapshot.clone() {
                                yield Ok(Event::default().data(json!({
                                    "type": "MESSAGES_SNAPSHOT",
                                    "messages": messages,
                                }).to_string()));
                            }
                            for activity_event in selected_activity_events.clone() {
                                yield Ok(Event::default().data(activity_event.to_string()));
                            }
                            started = true;
                        }

                        for payload in direct_ag_ui_events(&event, &selected_thread_id, &run_id) {
                            yield Ok(Event::default().data(payload));
                        }
                    } else if let Some(payload) = serialize_runtime_event(&event, profile) {
                        yield Ok(Event::default().data(payload));
                    }
                }
                Err(error) => {
                    if native_ag_ui {
                        let run_id =
                            active_run_id.unwrap_or_else(|| format!("run-{}", Uuid::new_v4()));
                        if !started {
                            yield Ok(Event::default().data(json!({
                                "type": "RUN_STARTED",
                                "threadId": selected_thread_id,
                                "runId": run_id,
                            }).to_string()));
                        }
                        yield Ok(Event::default().data(json!({
                            "type": "RUN_ERROR",
                            "threadId": selected_thread_id,
                            "runId": run_id,
                            "message": error.to_string(),
                        }).to_string()));
                    }
                    return;
                }
            }
        }

        if native_ag_ui {
            let run_id = active_run_id.unwrap_or_else(|| format!("run-{}", Uuid::new_v4()));
            if !started {
                yield Ok(Event::default().data(json!({
                    "type": "RUN_STARTED",
                    "threadId": selected_thread_id,
                    "runId": run_id,
                }).to_string()));
            }
            yield Ok(Event::default().data(json!({
                "type": "RUN_FINISHED",
                "threadId": selected_thread_id,
                "runId": run_id,
            }).to_string()));
        }
    })
}

pub async fn run_sse(
    State(controller): State<RuntimeController>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, RuntimeError> {
    let ui_profile = resolve_ui_profile(&headers, infer_run_request_protocol(&req))?;
    let transport = resolve_ui_transport(&headers, req.ui_transport.as_deref())?;
    validate_transport_support(ui_profile, transport)?;
    let span = tracing::info_span!("run_sse", session_id = %session_id, app_name = %app_name, user_id = %user_id);

    async move {
        log_profile_deprecation(ui_profile);
        info!(
            ui_protocol = %ui_profile.as_str(),
            ui_transport = ?transport,
            "resolved ui protocol profile for runtime request"
        );

        // Extract request context from auth middleware bridge if configured.
        // This returns Err (401/500) when the extractor is present but auth
        // fails, ensuring authorization checks are never bypassed.
        let request_context = extract_request_context(
            controller.config.request_context_extractor.as_deref(),
            &headers,
        )
        .await?;

        // Explicit authenticated user override: when an auth extractor is
        // configured and succeeds, the authenticated user_id takes precedence
        // over the path parameter. This prevents callers from impersonating
        // other users via the URL while keeping the path param as a fallback
        // for unauthenticated deployments (no extractor configured).
        let effective_user_id = request_context.as_ref().map_or(user_id, |rc| rc.user_id.clone());

        // Validate session exists
        controller
            .config
            .session_service
            .get(adk_session::GetRequest {
                app_name: app_name.clone(),
                user_id: effective_user_id.clone(),
                session_id: session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .map_err(|_| (StatusCode::NOT_FOUND, "session not found".to_string()))?;

        // Load agent
        let agent = controller
            .config
            .agent_loader
            .load_agent(&app_name)
            .await
            .map_err(adk_err_to_runtime)?;

        // Create runner
        let runner = adk_runner::Runner::new(adk_runner::RunnerConfig {
            app_name: app_name.clone(),
            agent,
            session_service: controller.config.session_service.clone(),
            artifact_service: controller.config.artifact_service.clone(),
            memory_service: controller.config.memory_service.clone(),
            plugin_manager: None,
            run_config: None,
            compaction_config: controller.config.compaction_config.clone(),
            context_cache_config: controller.config.context_cache_config.clone(),
            cache_capable: controller.config.cache_capable.clone(),
            request_context,
            cancellation_token: None,
        })
        .map_err(adk_err_to_runtime)?;

        // Build content with attachments
        let content = build_content_with_attachments(&req.new_message, &req.attachments)?;

        // Log attachment info
        if !req.attachments.is_empty() {
            info!(attachment_count = req.attachments.len(), "processing request with attachments");
        }

        // Run agent
        let typed_user_id =
            UserId::new(effective_user_id).map_err(|err| adk_err_to_runtime(err.into()))?;
        let typed_session_id =
            SessionId::new(session_id.clone()).map_err(|err| adk_err_to_runtime(err.into()))?;
        let event_stream = runner
            .run(typed_user_id, typed_session_id, content)
            .await
            .map_err(adk_err_to_runtime)?;

        // Convert to SSE stream
        let sse_stream =
            build_runtime_sse_stream(event_stream, ui_profile, transport, session_id.clone(), None);

        Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
    }
    .instrument(span)
    .await
}

/// POST /run_sse - adk-go compatible endpoint
/// Accepts JSON body with appName, userId, sessionId, newMessage
pub async fn run_sse_compat(
    State(controller): State<RuntimeController>,
    headers: HeaderMap,
    Json(req): Json<RunSseRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, RuntimeError> {
    let ui_profile = resolve_ui_profile(&headers, infer_sse_request_protocol(&req))?;
    let transport = resolve_ui_transport(&headers, req.ui_transport.as_deref())?;
    validate_transport_support(ui_profile, transport)?;
    let app_name = req.app_name.clone();
    let user_id = req.user_id.clone();
    let session_id = req.session_id.clone();
    let ag_ui_input = ag_ui_input_from_request(&req);
    let mcp_apps_request = mcp_apps_request_from_request(&req);
    let mcp_apps_initialize = mcp_apps_initialize_from_request(&req);

    info!(
        app_name = %app_name,
        user_id = %user_id,
        session_id = %session_id,
        ui_protocol = %ui_profile.as_str(),
        ui_transport = ?transport,
        "POST /run_sse request received"
    );
    log_profile_deprecation(ui_profile);

    // Extract request context from auth middleware bridge if configured.
    // This returns Err (401/500) when the extractor is present but auth
    // fails, ensuring authorization checks are never bypassed.
    let request_context =
        extract_request_context(controller.config.request_context_extractor.as_deref(), &headers)
            .await?;

    // Explicit authenticated user override: when an auth extractor is
    // configured and succeeds, the authenticated user_id takes precedence
    // over the request body value. This prevents callers from impersonating
    // other users via the JSON payload while keeping the body param as a
    // fallback for unauthenticated deployments (no extractor configured).
    let effective_user_id = request_context.as_ref().map_or(user_id, |rc| rc.user_id.clone());

    let resolved_new_message = req
        .new_message
        .clone()
        .or_else(|| ag_ui_input.as_ref().and_then(new_message_from_ag_ui_input))
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "newMessage is required unless protocol-native ag_ui input supplies a user message"
                    .to_string(),
            )
        })?;

    // Build content from message parts (includes both text and inline_data)
    let content = build_content_from_parts(&resolved_new_message.parts)?;

    // Log part info
    let text_parts: Vec<_> =
        resolved_new_message.parts.iter().filter(|p| p.text.is_some()).collect();
    let data_parts: Vec<_> =
        resolved_new_message.parts.iter().filter(|p| p.inline_data.is_some()).collect();
    if !data_parts.is_empty() {
        info!(
            text_parts = text_parts.len(),
            inline_data_parts = data_parts.len(),
            "processing request with inline data"
        );
    }

    let merged_state_delta = merge_runtime_state_delta(
        body_state_delta(req.state_delta.as_ref())?,
        ag_ui_input.as_ref().map(ag_ui_state_delta).unwrap_or_default(),
    );

    // Validate session exists or create it
    let session_result = controller
        .config
        .session_service
        .get(adk_session::GetRequest {
            app_name: app_name.clone(),
            user_id: effective_user_id.clone(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await;

    // If session doesn't exist, create it
    if session_result.is_err() {
        controller
            .config
            .session_service
            .create(adk_session::CreateRequest {
                app_name: app_name.clone(),
                user_id: effective_user_id.clone(),
                session_id: Some(session_id.clone()),
                state: merged_state_delta.clone(),
            })
            .await
            .map_err(adk_err_to_runtime)?;
    } else {
        apply_state_delta_to_session(
            &controller.config.session_service,
            &app_name,
            &effective_user_id,
            &session_id,
            merged_state_delta.clone(),
        )
        .await?;
    }

    if ui_profile == UiProfile::McpApps {
        if let Some(initialize) = mcp_apps_initialize {
            apply_mcp_apps_runtime_envelope(
                &app_name,
                &effective_user_id,
                &session_id,
                initialize,
            )?;
        }
        if let Some(request) = mcp_apps_request {
            apply_mcp_apps_runtime_envelope(&app_name, &effective_user_id, &session_id, request)?;
        }
        maybe_mark_mcp_ui_initialized(
            &app_name,
            &effective_user_id,
            &session_id,
            req.mcp_apps_initialized.as_ref(),
        )?;
    }

    // Load agent
    let agent =
        controller.config.agent_loader.load_agent(&app_name).await.map_err(adk_err_to_runtime)?;

    // Create runner with streaming config from request
    let streaming_mode =
        if req.streaming { adk_core::StreamingMode::SSE } else { adk_core::StreamingMode::None };

    let runner = adk_runner::Runner::new(adk_runner::RunnerConfig {
        app_name,
        agent,
        session_service: controller.config.session_service.clone(),
        artifact_service: controller.config.artifact_service.clone(),
        memory_service: controller.config.memory_service.clone(),
        plugin_manager: None,
        run_config: Some(adk_core::RunConfig { streaming_mode, ..adk_core::RunConfig::default() }),
        compaction_config: controller.config.compaction_config.clone(),
        context_cache_config: controller.config.context_cache_config.clone(),
        cache_capable: controller.config.cache_capable.clone(),
        request_context,
        cancellation_token: None,
    })
    .map_err(adk_err_to_runtime)?;

    // Run agent with full content (text + inline data)
    let typed_user_id =
        UserId::new(effective_user_id).map_err(|err| adk_err_to_runtime(err.into()))?;
    let typed_session_id =
        SessionId::new(session_id.clone()).map_err(|err| adk_err_to_runtime(err.into()))?;
    let event_stream =
        runner.run(typed_user_id, typed_session_id, content).await.map_err(adk_err_to_runtime)?;

    // Convert to SSE stream
    let sse_stream = build_runtime_sse_stream(
        event_stream,
        ui_profile,
        transport,
        session_id.clone(),
        ag_ui_input,
    );

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}
