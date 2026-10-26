use super::ag_ui_decisions::{
    AgUiCaller, AgUiDecisions, HeldCall, RESUME_IN_PROGRESS, ResumeOutcome, ResumesInProgress,
    commit_resume, decision_event, plan_resume,
};
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
    http::{HeaderMap, StatusCode, request::Parts},
    response::sse::{Event, KeepAlive, Sse},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures::{StreamExt, stream::Stream};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
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
    approval_service: Option<Arc<dyn adk_core::ApprovalService>>,
    resumes: ResumesInProgress,
}

impl RuntimeController {
    pub fn new(config: ServerConfig) -> Self {
        Self { config, approval_service: None, resumes: ResumesInProgress::default() }
    }

    /// Routes the calls that protocol-native AG-UI runs hold for approval to `service`,
    /// and answers AG-UI resumes through it (spec 3.3 §10 S2).
    ///
    /// A held call ends the run with an AG-UI interrupt whose `id` is the call's inbox
    /// entry, and a later run's `resume` entry becomes [`ApprovalService::decide`] on that
    /// entry. A call the service routes to no inbox entry — no router, or a service without an
    /// [`approval_store`](adk_core::ApprovalService::approval_store) — ends the run with an
    /// interrupt carrying the call ID, `metadata.answerable: false` and no `responseSchema`.
    /// `ServerBuilder::with_approval_service` (feature `approvals`) sets it.
    ///
    /// [`ApprovalService::decide`]: adk_core::ApprovalService::decide
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_core::{ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, StoreApprovalService};
    ///
    /// let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
    ///     .with_router(ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval());
    /// let controller = RuntimeController::new(config).with_approval_service(Arc::new(service));
    /// ```
    #[must_use]
    pub fn with_approval_service(mut self, service: Arc<dyn adk_core::ApprovalService>) -> Self {
        self.approval_service = Some(service);
        self
    }

    /// Whether AG-UI resumes can be decided: the controller has an approval service.
    pub(crate) fn answers_ag_ui_resume(&self) -> bool {
        self.approval_service.is_some()
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
    /// Answers to the interrupts the previous run on the thread ended with (spec 3.3 §10
    /// S2). A run that carries them starts no new user turn unless `newMessage` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<AgUiResume>,
}

/// The `resume` of an AG-UI run input (spec 3.3 §10 S2): the AG-UI 1.1 list of
/// [`AgUiResumeEntry`], or the single entry of the earlier interrupt draft.
///
/// # Example
///
/// ```rust
/// use adk_server::rest::controllers::runtime::{AgUiResume, AgUiResumeStatus};
/// use serde_json::json;
///
/// let resume: AgUiResume = serde_json::from_value(json!([
///     { "interruptId": "apr_1", "status": "resolved", "payload": { "approved": true } }
/// ]))
/// .unwrap();
/// assert_eq!(resume.entries()[0].status, AgUiResumeStatus::Resolved);
/// ```
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum AgUiResume {
    /// One entry per interrupt, as AG-UI 1.1 sends them.
    Entries(Vec<AgUiResumeEntry>),
    /// One entry, as the earlier draft sends it.
    Entry(AgUiResumeEntry),
}

impl AgUiResume {
    /// The entries, in order.
    pub fn entries(&self) -> &[AgUiResumeEntry] {
        match self {
            Self::Entries(entries) => entries,
            Self::Entry(entry) => std::slice::from_ref(entry),
        }
    }
}

/// One answer to an AG-UI interrupt (spec 3.3 §10 S2; AG-UI `ResumeEntry`).
///
/// A `resolved` answer's `payload` is `{"approved": true}` (allow once),
/// `{"approved": true, "editedArgs": {...}}` (edit), or
/// `{"approved": false, "reason": "..."}` (deny). `metadata.version`, when the client echoes
/// the version its interrupt carried, is the inbox entry version the decision is made at.
///
/// # Example
///
/// ```rust
/// use adk_server::rest::controllers::runtime::{AgUiResumeEntry, AgUiResumeStatus};
/// use serde_json::json;
///
/// let entry: AgUiResumeEntry = serde_json::from_value(json!({
///     "interruptId": "apr_1",
///     "payload": { "approved": false, "reason": "wrong client" }
/// }))
/// .unwrap();
/// assert_eq!(entry.status, AgUiResumeStatus::Resolved);
/// ```
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgUiResumeEntry {
    /// The interrupt answered, as the interrupt's `id` named it.
    pub interrupt_id: String,
    /// Whether the person answered or abandoned the interrupt; `resolved` when absent.
    #[serde(default)]
    pub status: AgUiResumeStatus,
    /// The answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    /// Envelope information, not part of the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// How an AG-UI client ended an interrupt (spec 3.3 §10 S2).
///
/// # Example
///
/// ```rust
/// use adk_server::rest::controllers::runtime::AgUiResumeStatus;
///
/// assert_eq!(serde_json::to_value(AgUiResumeStatus::Cancelled).unwrap(), "cancelled");
/// ```
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AgUiResumeStatus {
    /// The person answered; the payload is the answer.
    #[default]
    Resolved,
    /// The person abandoned the interrupt; the decision keeps waiting in the inbox.
    Cancelled,
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

fn serialize_runtime_error(error: &adk_core::AdkError, profile: UiProfile) -> Option<String> {
    let mut event = adk_core::Event::new(String::new());
    event.llm_response.error_code = Some(error.code.to_string());
    event.llm_response.error_message = Some(error.to_string());
    serialize_runtime_event(&event, profile)
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
                if let Some(replace) = message.replace
                    && let Some(object) = event.as_object_mut()
                {
                    object.insert("replace".to_string(), Value::Bool(replace));
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

    if !input.messages.is_empty()
        && let Ok(value) = serde_json::to_value(&input.messages)
    {
        delta.insert("temp:ag_ui_messages".to_string(), value);
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
                    uri: None,
                    annotations: None,
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
                        uri: None,
                        annotations: None,
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

/// Creates the session when it does not exist and applies `state_delta` to it when it does.
///
/// Only a `session.not_found` read creates the session, with `state_delta` as its initial
/// state. Any other read failure is returned, so a transient backend error never replaces a
/// stored session. A create that loses a race to a concurrent request finds the session that
/// request created and applies `state_delta` to it.
async fn ensure_session(
    session_service: &std::sync::Arc<dyn adk_session::SessionService>,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    state_delta: HashMap<String, Value>,
) -> Result<(), RuntimeError> {
    let existing = session_service
        .get(adk_session::GetRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await;
    match existing {
        Ok(_) => {
            apply_state_delta_to_session(
                session_service,
                app_name,
                user_id,
                session_id,
                state_delta,
            )
            .await
        }
        Err(error) if error.is_not_found() => {
            let created = session_service
                .create(adk_session::CreateRequest {
                    app_name: app_name.to_string(),
                    user_id: user_id.to_string(),
                    session_id: Some(session_id.to_string()),
                    state: state_delta.clone(),
                })
                .await;
            match created {
                Ok(_) => Ok(()),
                Err(error) if error.is_conflict() => {
                    apply_state_delta_to_session(
                        session_service,
                        app_name,
                        user_id,
                        session_id,
                        state_delta,
                    )
                    .await
                }
                Err(error) => Err(adk_err_to_runtime(error)),
            }
        }
        Err(error) => Err(adk_err_to_runtime(error)),
    }
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
    if allow_raw_string_delta && let Value::String(delta) = args {
        return delta.clone();
    }

    serde_json::to_string(args).unwrap_or_else(|_| args.to_string())
}

fn translate_ag_ui_event(
    event: &adk_core::Event,
    thread_id: &str,
    run_id: &str,
    open: &AgUiMessages,
) -> Vec<Value> {
    let mut translated = Vec::new();
    let timestamp = timestamp_millis(event);
    let is_partial = event.llm_response.partial;
    // Every action is named, so a new kind is a compile error here rather than dropped.
    let adk_core::EventActions {
        state_delta,
        // AG-UI 1.0 has no event for these; they stay in the session.
        artifact_delta: _,
        skip_summarization: _,
        transfer_to_agent: _,
        escalate: _,
        compaction: _,
        route: _,
        input_screened: _,
        workflow_positions: _,
        // A held call ends the run with an interrupt (spec 3.3 §10 S2), built when the
        // run finishes; a decision is reported by the result of the call it decided.
        tool_confirmation,
        tool_confirmation_decision: _,
    } = &event.actions;

    if !state_delta.is_empty() {
        translated.push(json!({
            "type": "STATE_DELTA",
            "delta": state_delta_to_json_patch(state_delta),
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

    // The text of the event that holds a call is the agent's placeholder; the interrupt
    // describes the decision instead, drawn from the inbox entry (spec 3.3 §7.1).
    if tool_confirmation.is_some() {
        return translated;
    }
    let Some(content) = &event.llm_response.content else {
        return translated;
    };

    let mut text_seen = false;
    let mut thinking_seen = false;
    for (index, part) in content.parts.iter().enumerate() {
        match part {
            // Every text part of the event is one message, sent at the first.
            adk_core::Part::Text { .. } => {
                if text_seen {
                    continue;
                }
                text_seen = true;
                let text: String = content
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        adk_core::Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if text.is_empty() && (is_partial || open.text.is_none()) {
                    continue;
                }
                let message_id =
                    open.text.clone().unwrap_or_else(|| format!("{}-text-{}", event.id, index));
                if is_partial {
                    translated.push(json!({
                        "type": "TEXT_MESSAGE_CHUNK",
                        "messageId": message_id,
                        "role": "assistant",
                        "delta": text,
                        "timestamp": timestamp,
                    }));
                } else {
                    if open.text.is_none() {
                        translated.push(json!({
                        "type": "TEXT_MESSAGE_START",
                        "messageId": message_id,
                        "role": "assistant",
                        "timestamp": timestamp,
                        }));
                    }
                    if !text.is_empty() {
                        translated.push(json!({
                        "type": "TEXT_MESSAGE_CONTENT",
                        "messageId": message_id,
                        "delta": text,
                        "timestamp": timestamp,
                        }));
                    }
                    translated.push(json!({
                        "type": "TEXT_MESSAGE_END",
                        "messageId": message_id,
                        "timestamp": timestamp,
                    }));
                }
            }
            adk_core::Part::Thinking { .. } => {
                if thinking_seen {
                    continue;
                }
                thinking_seen = true;
                let thinking: String = content
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        adk_core::Part::Thinking { thinking, .. } => Some(thinking.as_str()),
                        _ => None,
                    })
                    .collect();
                if thinking.is_empty() && (is_partial || open.reasoning.is_none()) {
                    continue;
                }
                let message_id = open
                    .reasoning
                    .clone()
                    .unwrap_or_else(|| format!("{}-reasoning-{}", event.id, index));
                if is_partial {
                    translated.push(json!({
                        "type": "REASONING_MESSAGE_CHUNK",
                        "messageId": message_id,
                        "delta": thinking,
                        "timestamp": timestamp,
                    }));
                } else {
                    let reasoning_id = format!("{}-reasoning-phase-{}", event.id, index);
                    if open.reasoning.is_none() {
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
                    }
                    if !thinking.is_empty() {
                        translated.push(json!({
                        "type": "REASONING_MESSAGE_CONTENT",
                        "messageId": message_id,
                        "delta": thinking,
                        "timestamp": timestamp,
                        }));
                    }
                    translated.push(json!({
                        "type": "REASONING_MESSAGE_END",
                        "messageId": message_id,
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
            adk_core::Part::FunctionResponse { function_response, id, .. } => {
                // A held call has no result yet: it arrives against the same tool call ID
                // in the run that resumes it.
                if adk_core::is_confirmation_pending(&function_response.response) {
                    continue;
                }
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
            // AG-UI 1.0 has no event for inline or file data, provider-executed tools, or
            // embedded resources.
            adk_core::Part::InlineData { .. }
            | adk_core::Part::FileData { .. }
            | adk_core::Part::ServerToolCall { .. }
            | adk_core::Part::ServerToolResponse { .. }
            | adk_core::Part::EmbeddedResource { .. } => {}
        }
    }

    translated
}

/// Minimal request [`Parts`] carrying only `headers`, for a handler called without the
/// request it serves. Such parts have no extensions, so an extractor that reads the peer
/// address (`ConnectInfo`) cannot see it.
fn parts_from_headers(headers: &HeaderMap) -> Result<Parts, RuntimeError> {
    let mut builder = axum::http::Request::builder();
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    Ok(builder
        .body(())
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to build request parts: {e}"))
        })?
        .into_parts()
        .0)
}

/// Extract [`RequestContext`] from the configured extractor, if present.
///
/// The extractor sees the request's own parts — its headers and its extensions, such as the
/// peer address a server records with `ConnectInfo` — as the auth middleware does on every
/// other route. Returns `None` when no extractor is configured (fall-through to existing
/// behavior).
async fn extract_request_context(
    extractor: Option<&dyn RequestContextExtractor>,
    parts: &Parts,
) -> Result<Option<RequestContext>, RuntimeError> {
    let Some(extractor) = extractor else {
        return Ok(None);
    };

    match extractor.extract(parts).await {
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

#[derive(Default)]
struct AgUiMessages {
    text: Option<String>,
    reasoning: Option<String>,
}

#[derive(Default)]
struct AgUiStream {
    deltas: adk_core::EventTextDeltas,
    messages: HashMap<(String, String), AgUiMessages>,
    /// Calls the run held for approval, one per fingerprint, in the order held.
    held: Vec<HeldCall>,
}

impl AgUiStream {
    fn translate(&mut self, event: &adk_core::Event, thread_id: &str, run_id: &str) -> Vec<Value> {
        if let Some(request) = &event.actions.tool_confirmation {
            let fingerprint = request.fingerprint();
            self.held.retain(|held| held.request.fingerprint() != fingerprint);
            self.held.push(HeldCall { request: request.clone(), agent: event.author.clone() });
        }
        let key = (event.invocation_id.clone(), event.id.clone());
        let mut open = self.messages.remove(&key).unwrap_or_default();
        let mut delta = self.deltas.push(event);
        if !event.llm_response.partial {
            let parts = delta.content().map(|content| content.parts.as_slice()).unwrap_or(&[]);
            let mut endings = Vec::new();
            if open.text.is_some()
                && !parts.iter().any(|part| matches!(part, adk_core::Part::Text { .. }))
            {
                endings.push(adk_core::Part::Text { text: String::new() });
            }
            if open.reasoning.is_some()
                && !parts.iter().any(|part| matches!(part, adk_core::Part::Thinking { .. }))
            {
                endings.push(adk_core::Part::Thinking { thinking: String::new(), signature: None });
            }
            if !endings.is_empty() {
                delta
                    .to_mut()
                    .llm_response
                    .content
                    .get_or_insert_with(|| adk_core::Content::new("model"))
                    .parts
                    .extend(endings);
            }
        }
        let translated = translate_ag_ui_event(&delta, thread_id, run_id, &open);
        if event.llm_response.partial {
            for item in &translated {
                let id = item.get("messageId").and_then(Value::as_str).map(str::to_owned);
                match item.get("type").and_then(Value::as_str) {
                    Some("TEXT_MESSAGE_CHUNK") => open.text = id,
                    Some("REASONING_MESSAGE_CHUNK") => open.reasoning = id,
                    _ => {}
                }
            }
            if open.text.is_some() || open.reasoning.is_some() {
                self.messages.insert(key, open);
            }
        }
        translated
    }
}

fn build_runtime_sse_stream<S>(
    mut event_stream: S,
    profile: UiProfile,
    transport: UiTransportMode,
    session_id: String,
    ag_ui_input: Option<AgUiRunInput>,
    decisions: AgUiDecisions,
) -> std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
where
    S: Stream<Item = adk_core::Result<adk_core::Event>> + Send + 'static + Unpin,
{
    let decision_updates: Vec<Value> =
        decisions.updates.iter().cloned().map(decision_event).collect();
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
        let mut ag_ui_stream = AgUiStream::default();
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
                            if let Some(parent_run_id) = selected_parent_run_id.clone()
                                && let Some(object) = started_event.as_object_mut()
                            {
                                object.insert("parentRunId".to_string(), Value::String(parent_run_id));
                            }
                            if let Some(run_input) = selected_run_input.clone()
                                && let Ok(value) = serde_json::to_value(run_input)
                                && let Some(object) = started_event.as_object_mut()
                            {
                                object.insert("input".to_string(), value);
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
                            for update in &decision_updates {
                                yield Ok(Event::default().data(update.to_string()));
                            }
                            started = true;
                        }

                        for payload in ag_ui_stream.translate(&event, &selected_thread_id, &run_id) {
                            yield Ok(Event::default().data(payload.to_string()));
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
                            for update in &decision_updates {
                                yield Ok(Event::default().data(update.to_string()));
                            }
                        }
                        yield Ok(Event::default().data(json!({
                            "type": "RUN_ERROR",
                            "threadId": selected_thread_id,
                            "runId": run_id,
                            "message": error.to_string(),
                        }).to_string()));
                    } else if let Some(payload) = serialize_runtime_error(&error, profile) {
                        yield Ok(Event::default().data(payload));
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
                for update in &decision_updates {
                    yield Ok(Event::default().data(update.to_string()));
                }
            }
            let mut finished = json!({
                "type": "RUN_FINISHED",
                "threadId": selected_thread_id,
                "runId": run_id,
            });
            // A run that stopped on held calls is not a success (spec 3.3 §10 S2).
            if !ag_ui_stream.held.is_empty() {
                match decisions.interrupts(&ag_ui_stream.held).await {
                    Ok(interrupts) => {
                        finished["outcome"] = json!({ "type": "interrupt", "interrupts": interrupts });
                    }
                    // An interrupt without its inbox entry would say the call cannot be
                    // decided here, so the failure is reported instead.
                    Err(error) => {
                        yield Ok(Event::default().data(json!({
                            "type": "RUN_ERROR",
                            "threadId": selected_thread_id,
                            "runId": run_id,
                            "message": "This run is waiting for a decision that could not be \
                                        loaded; answer it in your decisions inbox",
                            "code": error.code,
                        }).to_string()));
                        return;
                    }
                }
            }
            yield Ok(Event::default().data(finished.to_string()));
        }
    })
}

/// The stream of an AG-UI resume refused before its run started: `RUN_STARTED`, the
/// decision updates, and a `RUN_ERROR` naming the refusal (spec 3.3 §10 S2, S-I2).
fn refused_resume_stream(
    thread_id: String,
    run_id: String,
    updates: Vec<Value>,
    code: &'static str,
    message: String,
) -> std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
    let mut events = vec![json!({ "type": "RUN_STARTED", "threadId": thread_id, "runId": run_id })];
    events.extend(updates.into_iter().map(decision_event));
    events.push(json!({
        "type": "RUN_ERROR",
        "threadId": thread_id,
        "runId": run_id,
        "message": message,
        "code": code,
    }));
    Box::pin(futures::stream::iter(
        events.into_iter().map(|event| Ok(Event::default().data(event.to_string()))),
    ))
}

/// What a request's AG-UI stream needs for decisions: the approval service only for
/// protocol-native AG-UI runs, whose held calls become interrupts (spec 3.3 §10 S2).
fn ag_ui_decisions(
    controller: &RuntimeController,
    profile: UiProfile,
    transport: UiTransportMode,
    scope: adk_core::ApprovalScope,
    caller: Option<&RequestContext>,
) -> AgUiDecisions {
    if profile != UiProfile::AgUi || transport != UiTransportMode::ProtocolNative {
        return AgUiDecisions::default();
    }
    AgUiDecisions {
        service: controller.approval_service.clone(),
        // The caller the extractor authenticated is the session's owner: the session was
        // looked up under its user ID.
        caller: caller.map(|caller| AgUiCaller {
            approver: adk_core::principal::SubjectRef::human(scope.user_id.clone()),
            groups: caller.scopes.clone(),
        }),
        scope: Some(scope),
        updates: Vec::new(),
        audit: controller.config.governance.audit(),
    }
}

pub async fn run_sse(
    State(controller): State<RuntimeController>,
    path: Path<(String, String, String)>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, RuntimeError> {
    run_sse_for_request(State(controller), path, parts_from_headers(&headers)?, Json(req)).await
}

/// `POST /run/{app_name}/{user_id}/{session_id}` as the server routes it: the request
/// context extractor sees the whole request, including its peer address, rather than only
/// its headers.
pub(crate) async fn run_sse_for_request(
    State(controller): State<RuntimeController>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
    parts: Parts,
    Json(req): Json<RunRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, RuntimeError> {
    let headers = parts.headers.clone();
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
        let request_context =
            extract_request_context(controller.config.request_context_extractor.as_deref(), &parts)
                .await?;

        // Explicit authenticated user override: when an auth extractor is
        // configured and succeeds, the authenticated user_id takes precedence
        // over the path parameter. This prevents callers from impersonating
        // other users via the URL while keeping the path param as a fallback
        // for unauthenticated deployments (no extractor configured).
        let effective_user_id = request_context.as_ref().map_or(user_id, |rc| rc.user_id.clone());
        let decisions = ag_ui_decisions(
            &controller,
            ui_profile,
            transport,
            adk_core::ApprovalScope::new(&app_name, &effective_user_id, &session_id),
            request_context.as_ref(),
        );

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
            .map_err(adk_err_to_runtime)?;

        // Load agent
        let agent = controller
            .config
            .agent_loader
            .load_agent(&app_name)
            .await
            .map_err(adk_err_to_runtime)?;

        // Create runner
        let mut runner_builder = adk_runner::Runner::builder()
            .app_name(app_name.clone())
            .agent(agent)
            .session_service(controller.config.session_service.clone())
            .governance(controller.config.governance.clone());
        if let Some(ref artifact_service) = controller.config.artifact_service {
            runner_builder = runner_builder.artifact_service(artifact_service.clone());
        }
        if let Some(ref memory_service) = controller.config.memory_service {
            runner_builder = runner_builder.memory_service(memory_service.clone());
        }
        if let Some(ref compaction_config) = controller.config.compaction_config {
            runner_builder = runner_builder.compaction_config(compaction_config.clone());
        }
        if let Some(ref context_cache_config) = controller.config.context_cache_config {
            runner_builder = runner_builder.context_cache_config(context_cache_config.clone());
        }
        if let Some(ref cache_capable) = controller.config.cache_capable {
            runner_builder = runner_builder.cache_capable(cache_capable.clone());
        }
        if let Some(request_context) = request_context {
            runner_builder = runner_builder.request_context(request_context);
        }
        if let Some(service) = decisions.service.clone() {
            runner_builder = runner_builder
                .run_config(adk_core::RunConfig::builder().approval_service(service).build());
        }
        let runner = runner_builder.build().map_err(adk_err_to_runtime)?;

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
        let sse_stream = build_runtime_sse_stream(
            event_stream,
            ui_profile,
            transport,
            session_id.clone(),
            None,
            decisions,
        );

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
    run_sse_compat_for_request(State(controller), parts_from_headers(&headers)?, Json(req)).await
}

/// `POST /run_sse` as the server routes it: the request context extractor sees the whole
/// request, including its peer address, rather than only its headers.
pub(crate) async fn run_sse_compat_for_request(
    State(controller): State<RuntimeController>,
    parts: Parts,
    Json(req): Json<RunSseRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, RuntimeError> {
    let headers = parts.headers.clone();
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
        extract_request_context(controller.config.request_context_extractor.as_deref(), &parts)
            .await?;

    // Explicit authenticated user override: when an auth extractor is
    // configured and succeeds, the authenticated user_id takes precedence
    // over the request body value. This prevents callers from impersonating
    // other users via the JSON payload while keeping the body param as a
    // fallback for unauthenticated deployments (no extractor configured).
    let effective_user_id = request_context.as_ref().map_or(user_id, |rc| rc.user_id.clone());
    let mut decisions = ag_ui_decisions(
        &controller,
        ui_profile,
        transport,
        adk_core::ApprovalScope::new(&app_name, &effective_user_id, &session_id),
        request_context.as_ref(),
    );

    // Answers to the previous run's interrupts are checked now and recorded only once the run
    // is about to start, so a request that fails decides nothing and the run resumes the calls
    // they allow (spec 3.3 §10 S2).
    let resume = ag_ui_input.as_ref().and_then(|input| input.resume.as_ref());
    let resuming = resume.is_some_and(|resume| !resume.entries().is_empty());
    let mut resume_plan = None;
    let mut resume_in_progress = None;
    if let Some(resume) = resume.filter(|_| resuming) {
        let Some(service) = decisions.service.clone() else {
            return Err((
                StatusCode::BAD_REQUEST,
                "an AG-UI resume needs the ag_ui profile with the protocol_native transport and \
                 a server with an approval service (ServerBuilder::with_approval_service)"
                    .to_string(),
            ));
        };
        let Some(caller) = request_context.as_ref() else {
            return Err((
                StatusCode::UNAUTHORIZED,
                "an AG-UI resume decides approvals, which need an authenticated caller; \
                 configure a RequestContextExtractor"
                    .to_string(),
            ));
        };
        let scope = adk_core::ApprovalScope::new(&app_name, &effective_user_id, &session_id);
        let refused = |updates, code, message| {
            let input = ag_ui_input.as_ref();
            let thread_id = input
                .and_then(|input| input.thread_id.clone())
                .unwrap_or_else(|| session_id.clone());
            let run_id = input
                .and_then(|input| input.run_id.clone())
                .unwrap_or_else(|| format!("run-{}", Uuid::new_v4()));
            let stream = refused_resume_stream(thread_id, run_id, updates, code, message);
            Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
        };
        // One resume of a session at a time, until its run ends: a second one would start a
        // second run resuming the same held calls (spec 3.3 S-I2).
        let Some(in_progress) = controller.resumes.begin(&scope) else {
            return refused(
                Vec::new(),
                RESUME_IN_PROGRESS,
                "An earlier answer on this thread is still being applied; try again when its \
                 run finishes"
                    .to_string(),
            );
        };
        resume_in_progress = Some(in_progress);
        // The session's owner decides: the extractor authenticated the user it runs for.
        let approver = adk_core::principal::SubjectRef::human(effective_user_id.clone());
        match plan_resume(
            service.as_ref(),
            &scope,
            &approver,
            &caller.scopes,
            resume.entries(),
            decisions.audit.as_ref(),
        )
        .await
        .map_err(|reason| (StatusCode::BAD_REQUEST, reason))?
        {
            ResumeOutcome::Run(plan) => resume_plan = Some((service, plan)),
            ResumeOutcome::Refused { updates, code, message } => {
                return refused(updates, code, message);
            }
        }
    }

    // A resume carries the client's whole history, whose last user message this session
    // already answered, so it starts no new user turn unless one is sent as `newMessage`.
    let content = match req.new_message.clone().or_else(|| {
        if resuming { None } else { ag_ui_input.as_ref().and_then(new_message_from_ag_ui_input) }
    }) {
        Some(resolved_new_message) => {
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
            // Build content from message parts (includes both text and inline_data)
            build_content_from_parts(&resolved_new_message.parts)?
        }
        // A blank turn resumes the held calls that now have a decision.
        None if resuming => adk_core::Content::new("user").with_text(""),
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                "newMessage is required unless protocol-native ag_ui input supplies a user message"
                    .to_string(),
            ));
        }
    };

    let merged_state_delta = merge_runtime_state_delta(
        body_state_delta(req.state_delta.as_ref())?,
        ag_ui_input.as_ref().map(ag_ui_state_delta).unwrap_or_default(),
    );

    ensure_session(
        &controller.config.session_service,
        &app_name,
        &effective_user_id,
        &session_id,
        merged_state_delta.clone(),
    )
    .await?;

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

    let mut run_config = adk_core::RunConfig::builder().streaming_mode(streaming_mode);
    if let Some(service) = decisions.service.clone() {
        run_config = run_config.approval_service(service);
    }
    let mut runner_builder = adk_runner::Runner::builder()
        .app_name(app_name)
        .agent(agent)
        .session_service(controller.config.session_service.clone())
        .governance(controller.config.governance.clone())
        .run_config(run_config.build());
    if let Some(ref artifact_service) = controller.config.artifact_service {
        runner_builder = runner_builder.artifact_service(artifact_service.clone());
    }
    if let Some(ref memory_service) = controller.config.memory_service {
        runner_builder = runner_builder.memory_service(memory_service.clone());
    }
    if let Some(ref compaction_config) = controller.config.compaction_config {
        runner_builder = runner_builder.compaction_config(compaction_config.clone());
    }
    if let Some(ref context_cache_config) = controller.config.context_cache_config {
        runner_builder = runner_builder.context_cache_config(context_cache_config.clone());
    }
    if let Some(ref cache_capable) = controller.config.cache_capable {
        runner_builder = runner_builder.cache_capable(cache_capable.clone());
    }
    if let Some(request_context) = request_context {
        runner_builder = runner_builder.request_context(request_context);
    }
    let runner = runner_builder.build().map_err(adk_err_to_runtime)?;
    // A frozen server records no answer: the run would not start.
    if resume_plan.is_some() {
        controller.config.governance.check().map_err(adk_err_to_runtime)?;
    }

    // Run agent with full content (text + inline data)
    let typed_user_id =
        UserId::new(effective_user_id).map_err(|err| adk_err_to_runtime(err.into()))?;
    let typed_session_id =
        SessionId::new(session_id.clone()).map_err(|err| adk_err_to_runtime(err.into()))?;
    // `run` refuses a request context of another user before returning; the agent runs only
    // once the stream is read, after the answers are recorded below.
    let event_stream =
        runner.run(typed_user_id, typed_session_id, content).await.map_err(adk_err_to_runtime)?;
    if let Some((service, plan)) = resume_plan {
        decisions.updates = commit_resume(service.as_ref(), plan, decisions.audit.as_ref()).await;
    }

    // Convert to SSE stream
    let sse_stream = build_runtime_sse_stream(
        event_stream,
        ui_profile,
        transport,
        session_id.clone(),
        ag_ui_input,
        decisions,
    );
    // The resume stays in progress until its run's stream ends or is dropped.
    let sse_stream: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        Box::pin(sse_stream.map(move |event| {
            let _resume = &resume_in_progress;
            event
        }));

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}

/// POST /run - plain-JSON, non-streaming endpoint (Google ADK api_server parity)
///
/// Accepts the same JSON body as `POST /run_sse`, runs the agent to
/// completion, and returns every produced event as a JSON array instead of
/// an SSE stream. The `streaming` body field is ignored — the run always
/// uses `StreamingMode::None`.
pub async fn run_collect(
    State(controller): State<RuntimeController>,
    headers: HeaderMap,
    Json(req): Json<RunSseRequest>,
) -> Result<Json<Vec<adk_core::Event>>, RuntimeError> {
    run_collect_for_request(State(controller), parts_from_headers(&headers)?, Json(req)).await
}

/// `POST /run` as the server routes it: the request context extractor sees the whole
/// request, including its peer address, rather than only its headers.
pub(crate) async fn run_collect_for_request(
    State(controller): State<RuntimeController>,
    parts: Parts,
    Json(req): Json<RunSseRequest>,
) -> Result<Json<Vec<adk_core::Event>>, RuntimeError> {
    let app_name = req.app_name.clone();
    let user_id = req.user_id.clone();
    let session_id = req.session_id.clone();

    info!(
        app_name = %app_name,
        user_id = %user_id,
        session_id = %session_id,
        "POST /run request received"
    );

    // Extract request context from auth middleware bridge if configured.
    // This returns Err (401/500) when the extractor is present but auth
    // fails, ensuring authorization checks are never bypassed.
    let request_context =
        extract_request_context(controller.config.request_context_extractor.as_deref(), &parts)
            .await?;

    // Explicit authenticated user override: when an auth extractor is
    // configured and succeeds, the authenticated user_id takes precedence
    // over the request body value. This prevents callers from impersonating
    // other users via the JSON payload while keeping the body param as a
    // fallback for unauthenticated deployments (no extractor configured).
    let effective_user_id = request_context.as_ref().map_or(user_id, |rc| rc.user_id.clone());

    let new_message = req
        .new_message
        .as_ref()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "newMessage is required".to_string()))?;

    // Build content from message parts (includes both text and inline_data)
    let content = build_content_from_parts(&new_message.parts)?;

    let state_delta = body_state_delta(req.state_delta.as_ref())?;

    // Same behavior as /run_sse.
    ensure_session(
        &controller.config.session_service,
        &app_name,
        &effective_user_id,
        &session_id,
        state_delta,
    )
    .await?;

    // Load agent
    let agent =
        controller.config.agent_loader.load_agent(&app_name).await.map_err(adk_err_to_runtime)?;

    // Create runner — always non-streaming for the plain-JSON endpoint
    let mut runner_builder = adk_runner::Runner::builder()
        .app_name(app_name)
        .agent(agent)
        .session_service(controller.config.session_service.clone())
        .governance(controller.config.governance.clone())
        .run_config(
            adk_core::RunConfig::builder().streaming_mode(adk_core::StreamingMode::None).build(),
        );
    if let Some(ref artifact_service) = controller.config.artifact_service {
        runner_builder = runner_builder.artifact_service(artifact_service.clone());
    }
    if let Some(ref memory_service) = controller.config.memory_service {
        runner_builder = runner_builder.memory_service(memory_service.clone());
    }
    if let Some(ref compaction_config) = controller.config.compaction_config {
        runner_builder = runner_builder.compaction_config(compaction_config.clone());
    }
    if let Some(ref context_cache_config) = controller.config.context_cache_config {
        runner_builder = runner_builder.context_cache_config(context_cache_config.clone());
    }
    if let Some(ref cache_capable) = controller.config.cache_capable {
        runner_builder = runner_builder.cache_capable(cache_capable.clone());
    }
    if let Some(request_context) = request_context {
        runner_builder = runner_builder.request_context(request_context);
    }
    let runner = runner_builder.build().map_err(adk_err_to_runtime)?;

    // Run agent to completion, collecting every event
    let typed_user_id =
        UserId::new(effective_user_id).map_err(|err| adk_err_to_runtime(err.into()))?;
    let typed_session_id =
        SessionId::new(session_id).map_err(|err| adk_err_to_runtime(err.into()))?;
    let mut event_stream =
        runner.run(typed_user_id, typed_session_id, content).await.map_err(adk_err_to_runtime)?;

    let mut events = Vec::new();
    while let Some(event) = event_stream.next().await {
        events.push(event.map_err(adk_err_to_runtime)?);
    }

    info!(event_count = events.len(), "POST /run completed");

    Ok(Json(events))
}

#[cfg(test)]
#[path = "runtime_stream_tests.rs"]
mod stream_tests;

#[cfg(test)]
mod error_status_tests {
    use super::*;
    use adk_core::{AdkError, ErrorCategory, ErrorComponent};

    #[test]
    fn conflict_errors_map_to_409_with_their_code() {
        let error = AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::Conflict,
            "run_store.stale_epoch",
            "run is no longer held",
        );
        let (status, body) = adk_err_to_runtime(error);
        assert_eq!(status, StatusCode::CONFLICT);
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["error"]["category"], "conflict");
        assert_eq!(body["error"]["code"], "run_store.stale_epoch");
    }
}
