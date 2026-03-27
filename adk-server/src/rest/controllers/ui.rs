use crate::ui_protocol::{
    TOOL_ENVELOPE_VERSION, UI_DEFAULT_PROTOCOL, UI_PROTOCOL_CAPABILITIES,
    UiProtocolDeprecationSpec, UiProtocolImplementationTier, UiProtocolSpecTrack,
};
use crate::ui_types::{
    McpAppsRenderOptions, McpUiBridgeSnapshot, McpUiHostCapabilities, McpUiHostInfo,
    McpUiPermissions, McpUiResourceCsp, default_mcp_ui_host_capabilities, default_mcp_ui_host_info,
    validate_mcp_apps_render_options,
};
use axum::{Json, extract::Query, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{OnceLock, RwLock};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize)]
pub struct UiProtocolCapability {
    pub protocol: &'static str,
    pub versions: Vec<&'static str>,
    #[serde(rename = "implementationTier")]
    pub implementation_tier: UiProtocolImplementationTier,
    #[serde(rename = "specTrack")]
    pub spec_track: UiProtocolSpecTrack,
    pub summary: &'static str,
    pub features: Vec<&'static str>,
    pub limitations: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecation: Option<UiProtocolDeprecation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiProtocolDeprecation {
    pub stage: &'static str,
    pub announced_on: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sunset_target_on: Option<&'static str>,
    pub replacement_protocols: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

fn map_deprecation(
    spec: Option<&'static UiProtocolDeprecationSpec>,
) -> Option<UiProtocolDeprecation> {
    let spec = spec?;
    Some(UiProtocolDeprecation {
        stage: spec.stage,
        announced_on: spec.announced_on,
        sunset_target_on: spec.sunset_target_on,
        replacement_protocols: spec.replacement_protocols.to_vec(),
        note: spec.note,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct UiCapabilities {
    pub default_protocol: &'static str,
    pub protocols: Vec<UiProtocolCapability>,
    pub tool_envelope_version: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiResource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub mime_type: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiResourceContent {
    pub uri: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiResourceListResponse {
    pub resources: Vec<UiResource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiResourceReadResponse {
    pub contents: Vec<UiResourceContent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterUiResourceRequest {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub mime_type: String,
    pub text: String,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadUiResourceQuery {
    pub uri: String,
}

#[derive(Debug, Clone)]
struct UiResourceEntry {
    resource: UiResource,
    content: UiResourceContent,
}

static UI_RESOURCE_REGISTRY: OnceLock<RwLock<HashMap<String, UiResourceEntry>>> = OnceLock::new();

const MCP_UI_DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

fn resource_registry() -> &'static RwLock<HashMap<String, UiResourceEntry>> {
    UI_RESOURCE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct McpUiBridgeSessionKey {
    app_name: String,
    user_id: String,
    session_id: String,
}

#[derive(Debug, Clone)]
struct McpUiBridgeSessionEntry {
    protocol_version: String,
    initialized: bool,
    app_info: Option<Value>,
    app_capabilities: Option<Value>,
    host_info: McpUiHostInfo,
    host_capabilities: McpUiHostCapabilities,
    host_context: Value,
    message_count: u64,
    last_message: Option<Value>,
    model_context: Vec<Value>,
    model_context_revision: u64,
    resource_list_revision: u64,
    tool_list_revision: u64,
    notification_count: u64,
    pending_notifications: Vec<McpUiBridgeNotification>,
}

static MCP_UI_BRIDGE_REGISTRY: OnceLock<
    RwLock<HashMap<McpUiBridgeSessionKey, McpUiBridgeSessionEntry>>,
> = OnceLock::new();

fn bridge_registry() -> &'static RwLock<HashMap<McpUiBridgeSessionKey, McpUiBridgeSessionEntry>> {
    MCP_UI_BRIDGE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiBridgeNotification {
    pub notification_id: u64,
    pub method: String,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

fn bridge_session_key(app_name: &str, user_id: &str, session_id: &str) -> McpUiBridgeSessionKey {
    McpUiBridgeSessionKey {
        app_name: app_name.to_string(),
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
    }
}

fn default_host_context(app_name: &str, user_id: &str, session_id: &str) -> Value {
    json!({
        "appName": app_name,
        "userId": user_id,
        "sessionId": session_id,
        "theme": "light",
        "locale": "en-US",
        "timeZone": "UTC",
        "platform": "adk-server",
        "displayMode": "inline",
        "availableDisplayModes": ["inline"]
    })
}

fn merge_host_context(
    target: &mut Value,
    patch: Option<Value>,
) -> Result<(), (StatusCode, String)> {
    let Some(patch) = patch else {
        return Ok(());
    };
    let patch_object = patch.as_object().ok_or_else(|| {
        (StatusCode::BAD_REQUEST, "hostContext must be a JSON object".to_string())
    })?;
    let target_object = target.as_object_mut().ok_or_else(|| {
        (StatusCode::INTERNAL_SERVER_ERROR, "host context store invalid".to_string())
    })?;
    for (key, value) in patch_object {
        target_object.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn ensure_bridge_session<'a>(
    registry: &'a mut HashMap<McpUiBridgeSessionKey, McpUiBridgeSessionEntry>,
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> &'a mut McpUiBridgeSessionEntry {
    registry.entry(bridge_session_key(app_name, user_id, session_id)).or_insert_with(|| {
        McpUiBridgeSessionEntry {
            protocol_version: MCP_UI_DEFAULT_PROTOCOL_VERSION.to_string(),
            initialized: false,
            app_info: None,
            app_capabilities: None,
            host_info: default_mcp_ui_host_info(),
            host_capabilities: default_mcp_ui_host_capabilities(),
            host_context: default_host_context(app_name, user_id, session_id),
            message_count: 0,
            last_message: None,
            model_context: vec![],
            model_context_revision: 0,
            resource_list_revision: 0,
            tool_list_revision: 0,
            notification_count: 0,
            pending_notifications: vec![],
        }
    })
}

fn validate_ui_resource_uri(uri: &str) -> Result<(), (StatusCode, String)> {
    if !uri.starts_with("ui://") {
        return Err((
            StatusCode::BAD_REQUEST,
            "ui resource uri must start with 'ui://'".to_string(),
        ));
    }
    Ok(())
}

fn validate_ui_resource_mime(mime_type: &str) -> Result<(), (StatusCode, String)> {
    if mime_type != "text/html;profile=mcp-app" {
        return Err((
            StatusCode::BAD_REQUEST,
            "mimeType must be 'text/html;profile=mcp-app'".to_string(),
        ));
    }
    Ok(())
}

fn parse_ui_meta_options(
    meta: &Option<Value>,
) -> Result<McpAppsRenderOptions, (StatusCode, String)> {
    let Some(meta_value) = meta else {
        return Ok(McpAppsRenderOptions::default());
    };
    let meta_object = meta_value
        .as_object()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "_meta must be a JSON object".to_string()))?;
    let Some(ui_value) = meta_object.get("ui") else {
        return Ok(McpAppsRenderOptions::default());
    };
    let ui_object = ui_value
        .as_object()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "_meta.ui must be a JSON object".to_string()))?;

    let domain = ui_object
        .get("domain")
        .map(|domain_value| {
            domain_value.as_str().ok_or_else(|| {
                (StatusCode::BAD_REQUEST, "_meta.ui.domain must be a string".to_string())
            })
        })
        .transpose()?
        .map(ToString::to_string);

    let prefers_border = ui_object
        .get("prefersBorder")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                (StatusCode::BAD_REQUEST, "_meta.ui.prefersBorder must be a boolean".to_string())
            })
        })
        .transpose()?;

    let csp = ui_object
        .get("csp")
        .map(|value| {
            serde_json::from_value::<McpUiResourceCsp>(value.clone()).map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("_meta.ui.csp must be an object with domain arrays: {}", error),
                )
            })
        })
        .transpose()?;

    let permissions = ui_object
        .get("permissions")
        .map(|value| {
            serde_json::from_value::<McpUiPermissions>(value.clone()).map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("_meta.ui.permissions must be an object: {}", error),
                )
            })
        })
        .transpose()?;

    Ok(McpAppsRenderOptions { domain, prefers_border, csp, permissions })
}

fn validate_ui_meta(meta: &Option<Value>) -> Result<McpAppsRenderOptions, (StatusCode, String)> {
    let options = parse_ui_meta_options(meta)?;
    validate_mcp_apps_render_options(&options).map_err(|error| {
        (StatusCode::BAD_REQUEST, format!("Invalid _meta.ui options for mcp_apps: {}", error))
    })?;
    Ok(options)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiInitializeParams {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    pub app_info: Option<Value>,
    #[serde(default)]
    pub app_capabilities: Option<Value>,
    #[serde(default)]
    pub host_context: Option<Value>,
    #[serde(default)]
    pub host_info: Option<McpUiHostInfo>,
    #[serde(default)]
    pub host_capabilities: Option<McpUiHostCapabilities>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiMessageParams {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub host_context: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpUiModelContextUpdateMode {
    Append,
    #[default]
    Replace,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiUpdateModelContextParams {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub structured_content: Option<Value>,
    #[serde(default)]
    pub host_context: Option<Value>,
    #[serde(default)]
    pub mode: McpUiModelContextUpdateMode,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiInitializeResult {
    pub initialized: bool,
    pub protocol_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_info: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_capabilities: Option<Value>,
    pub host_info: McpUiHostInfo,
    pub host_capabilities: McpUiHostCapabilities,
    pub host_context: Value,
    pub message_count: u64,
    pub model_context: Vec<Value>,
    pub model_context_revision: u64,
    pub resource_list_revision: u64,
    pub tool_list_revision: u64,
    pub notifications: Vec<McpUiBridgeNotification>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiMessageResult {
    pub accepted: bool,
    pub initialized: bool,
    pub protocol_version: String,
    pub message_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<Value>,
    pub resource_list_revision: u64,
    pub tool_list_revision: u64,
    pub notifications: Vec<McpUiBridgeNotification>,
    pub host_info: McpUiHostInfo,
    pub host_capabilities: McpUiHostCapabilities,
    pub host_context: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiUpdateModelContextResult {
    pub accepted: bool,
    pub initialized: bool,
    pub protocol_version: String,
    pub model_context: Vec<Value>,
    pub model_context_revision: u64,
    pub resource_list_revision: u64,
    pub tool_list_revision: u64,
    pub notifications: Vec<McpUiBridgeNotification>,
    pub host_info: McpUiHostInfo,
    pub host_capabilities: McpUiHostCapabilities,
    pub host_context: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiPollNotificationsParams {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    #[serde(default = "default_true")]
    pub drain: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiPollNotificationsResult {
    pub initialized: bool,
    pub protocol_version: String,
    pub resource_list_revision: u64,
    pub tool_list_revision: u64,
    pub notifications: Vec<McpUiBridgeNotification>,
    pub host_info: McpUiHostInfo,
    pub host_capabilities: McpUiHostCapabilities,
    pub host_context: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiListChangedParams {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiListChangedResult {
    pub accepted: bool,
    pub initialized: bool,
    pub protocol_version: String,
    pub method: String,
    pub revision: u64,
    pub pending_notification_count: usize,
    pub resource_list_revision: u64,
    pub tool_list_revision: u64,
    pub host_info: McpUiHostInfo,
    pub host_capabilities: McpUiHostCapabilities,
    pub host_context: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum McpUiBridgeInput<T> {
    Direct(T),
    Rpc(McpUiBridgeRpcRequest<T>),
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct McpUiBridgeRpcRequest<T> {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    params: T,
}

enum McpUiBridgeResponseMode {
    Direct,
    Rpc { id: Option<Value> },
}

fn parse_bridge_input<T>(
    input: McpUiBridgeInput<T>,
    expected_method: &str,
) -> Result<(T, McpUiBridgeResponseMode), (StatusCode, String)> {
    match input {
        McpUiBridgeInput::Direct(params) => Ok((params, McpUiBridgeResponseMode::Direct)),
        McpUiBridgeInput::Rpc(request) => {
            if request.method != expected_method {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!(
                        "unexpected MCP Apps bridge method '{}', expected '{}'",
                        request.method, expected_method
                    ),
                ));
            }
            Ok((request.params, McpUiBridgeResponseMode::Rpc { id: request.id }))
        }
    }
}

fn bridge_result_json<T: Serialize>(
    mode: McpUiBridgeResponseMode,
    result: T,
) -> Result<Json<Value>, (StatusCode, String)> {
    let value = match mode {
        McpUiBridgeResponseMode::Direct => serde_json::to_value(result).map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to serialize bridge response: {}", error),
            )
        })?,
        McpUiBridgeResponseMode::Rpc { id } => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }),
    };
    Ok(Json(value))
}

fn model_context_items(params: &McpUiUpdateModelContextParams) -> Vec<Value> {
    let mut items = params.content.clone();
    if let Some(structured_content) = params.structured_content.clone() {
        items.push(json!({
            "type": "structuredContent",
            "structuredContent": structured_content
        }));
    }
    items
}

fn default_true() -> bool {
    true
}

fn bridge_snapshot(session: &McpUiBridgeSessionEntry) -> McpUiBridgeSnapshot {
    McpUiBridgeSnapshot::new(
        session.protocol_version.clone(),
        session.initialized,
        session.host_info.clone(),
        session.host_capabilities.clone(),
        session.host_context.clone(),
    )
    .with_optional_app_metadata(session.app_info.clone(), session.app_capabilities.clone())
}

fn queued_notifications(session: &McpUiBridgeSessionEntry) -> Vec<McpUiBridgeNotification> {
    session.pending_notifications.clone()
}

fn queue_bridge_notification(
    session: &mut McpUiBridgeSessionEntry,
    method: &str,
    revision: u64,
    params: Option<Value>,
) {
    session.notification_count += 1;
    session.pending_notifications.push(McpUiBridgeNotification {
        notification_id: session.notification_count,
        method: method.to_string(),
        revision,
        params,
    });
}

pub(crate) fn initialize_mcp_ui_bridge(
    params: McpUiInitializeParams,
) -> Result<McpUiInitializeResult, (StatusCode, String)> {
    let app_name = params.app_name.clone();
    let user_id = params.user_id.clone();
    let session_id = params.session_id.clone();
    let mut registry = bridge_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "bridge registry poisoned".to_string()))?;
    let session = ensure_bridge_session(&mut registry, &app_name, &user_id, &session_id);

    if let Some(protocol_version) = params.protocol_version.filter(|value| !value.trim().is_empty())
    {
        session.protocol_version = protocol_version;
    }
    if let Some(app_info) = params.app_info {
        session.app_info = Some(app_info);
    }
    if let Some(app_capabilities) = params.app_capabilities {
        session.app_capabilities = Some(app_capabilities);
    }
    if let Some(host_info) = params.host_info {
        session.host_info = host_info;
    }
    if let Some(host_capabilities) = params.host_capabilities {
        session.host_capabilities = host_capabilities;
    }
    merge_host_context(&mut session.host_context, params.host_context)?;
    session.initialized = true;

    info!(
        app_name = %app_name,
        user_id = %user_id,
        session_id = %session_id,
        protocol_version = %session.protocol_version,
        "mcp ui initialize handled"
    );

    let snapshot = bridge_snapshot(session);
    Ok(McpUiInitializeResult {
        initialized: snapshot.initialized,
        protocol_version: snapshot.protocol_version.clone(),
        app_info: snapshot.app_info.clone(),
        app_capabilities: snapshot.app_capabilities.clone(),
        host_info: snapshot.host_info.clone(),
        host_capabilities: snapshot.host_capabilities.clone(),
        host_context: snapshot.host_context.clone(),
        message_count: session.message_count,
        model_context: session.model_context.clone(),
        model_context_revision: session.model_context_revision,
        resource_list_revision: session.resource_list_revision,
        tool_list_revision: session.tool_list_revision,
        notifications: queued_notifications(session),
    })
}

pub(crate) fn message_mcp_ui_bridge(
    params: McpUiMessageParams,
) -> Result<McpUiMessageResult, (StatusCode, String)> {
    let app_name = params.app_name.clone();
    let user_id = params.user_id.clone();
    let session_id = params.session_id.clone();
    let host_context = params.host_context.clone();
    let mut registry = bridge_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "bridge registry poisoned".to_string()))?;
    let session = ensure_bridge_session(&mut registry, &app_name, &user_id, &session_id);
    merge_host_context(&mut session.host_context, host_context)?;

    session.message_count += 1;
    let mut message = json!({
        "role": params.role.unwrap_or_else(|| "user".to_string()),
        "content": params.content
    });
    if let Some(metadata) = params.metadata {
        let object = message.as_object_mut().ok_or_else(|| {
            (StatusCode::INTERNAL_SERVER_ERROR, "message payload must be an object".to_string())
        })?;
        object.insert("metadata".to_string(), metadata);
    }
    session.last_message = Some(message);

    info!(
        app_name = %app_name,
        user_id = %user_id,
        session_id = %session_id,
        message_count = session.message_count,
        initialized = session.initialized,
        "mcp ui message handled"
    );

    let snapshot = bridge_snapshot(session);
    Ok(McpUiMessageResult {
        accepted: true,
        initialized: snapshot.initialized,
        protocol_version: snapshot.protocol_version,
        message_count: session.message_count,
        last_message: session.last_message.clone(),
        resource_list_revision: session.resource_list_revision,
        tool_list_revision: session.tool_list_revision,
        notifications: queued_notifications(session),
        host_info: snapshot.host_info,
        host_capabilities: snapshot.host_capabilities,
        host_context: snapshot.host_context,
    })
}

pub(crate) fn update_mcp_ui_bridge_model_context(
    params: McpUiUpdateModelContextParams,
) -> Result<McpUiUpdateModelContextResult, (StatusCode, String)> {
    let app_name = params.app_name.clone();
    let user_id = params.user_id.clone();
    let session_id = params.session_id.clone();
    let host_context = params.host_context.clone();
    let items = model_context_items(&params);
    let mut registry = bridge_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "bridge registry poisoned".to_string()))?;
    let session = ensure_bridge_session(&mut registry, &app_name, &user_id, &session_id);
    merge_host_context(&mut session.host_context, host_context)?;

    match params.mode {
        McpUiModelContextUpdateMode::Replace => session.model_context = items,
        McpUiModelContextUpdateMode::Append => session.model_context.extend(items),
    }
    session.model_context_revision += 1;

    info!(
        app_name = %app_name,
        user_id = %user_id,
        session_id = %session_id,
        model_context_revision = session.model_context_revision,
        initialized = session.initialized,
        "mcp ui model context updated"
    );

    let snapshot = bridge_snapshot(session);
    Ok(McpUiUpdateModelContextResult {
        accepted: true,
        initialized: snapshot.initialized,
        protocol_version: snapshot.protocol_version,
        model_context: session.model_context.clone(),
        model_context_revision: session.model_context_revision,
        resource_list_revision: session.resource_list_revision,
        tool_list_revision: session.tool_list_revision,
        notifications: queued_notifications(session),
        host_info: snapshot.host_info,
        host_capabilities: snapshot.host_capabilities,
        host_context: snapshot.host_context,
    })
}

pub(crate) fn poll_mcp_ui_bridge_notifications(
    params: McpUiPollNotificationsParams,
) -> Result<McpUiPollNotificationsResult, (StatusCode, String)> {
    let mut registry = bridge_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "bridge registry poisoned".to_string()))?;
    let session =
        ensure_bridge_session(&mut registry, &params.app_name, &params.user_id, &params.session_id);
    let notifications = queued_notifications(session);
    if params.drain {
        session.pending_notifications.clear();
    }
    let snapshot = bridge_snapshot(session);

    Ok(McpUiPollNotificationsResult {
        initialized: snapshot.initialized,
        protocol_version: snapshot.protocol_version,
        resource_list_revision: session.resource_list_revision,
        tool_list_revision: session.tool_list_revision,
        notifications,
        host_info: snapshot.host_info,
        host_capabilities: snapshot.host_capabilities,
        host_context: snapshot.host_context,
    })
}

fn notify_mcp_ui_bridge_list_changed(
    params: McpUiListChangedParams,
    method: &'static str,
    revision_selector: impl Fn(&mut McpUiBridgeSessionEntry) -> &mut u64,
) -> Result<McpUiListChangedResult, (StatusCode, String)> {
    let mut registry = bridge_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "bridge registry poisoned".to_string()))?;
    let session =
        ensure_bridge_session(&mut registry, &params.app_name, &params.user_id, &params.session_id);
    let revision = {
        let target = revision_selector(session);
        *target += 1;
        *target
    };
    queue_bridge_notification(session, method, revision, params.params);
    let snapshot = bridge_snapshot(session);

    Ok(McpUiListChangedResult {
        accepted: true,
        initialized: snapshot.initialized,
        protocol_version: snapshot.protocol_version,
        method: method.to_string(),
        revision,
        pending_notification_count: session.pending_notifications.len(),
        resource_list_revision: session.resource_list_revision,
        tool_list_revision: session.tool_list_revision,
        host_info: snapshot.host_info,
        host_capabilities: snapshot.host_capabilities,
        host_context: snapshot.host_context,
    })
}

pub(crate) fn notify_mcp_ui_resource_list_changed(
    params: McpUiListChangedParams,
) -> Result<McpUiListChangedResult, (StatusCode, String)> {
    notify_mcp_ui_bridge_list_changed(
        params,
        "ui/notifications/resources/list_changed",
        |session| &mut session.resource_list_revision,
    )
}

pub(crate) fn notify_mcp_ui_tool_list_changed(
    params: McpUiListChangedParams,
) -> Result<McpUiListChangedResult, (StatusCode, String)> {
    notify_mcp_ui_bridge_list_changed(params, "ui/notifications/tools/list_changed", |session| {
        &mut session.tool_list_revision
    })
}

pub(crate) fn mark_mcp_ui_initialized(
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> Result<(), (StatusCode, String)> {
    let mut registry = bridge_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "bridge registry poisoned".to_string()))?;
    let session = ensure_bridge_session(&mut registry, app_name, user_id, session_id);
    session.initialized = true;
    Ok(())
}

/// GET /api/ui/capabilities
pub async fn ui_capabilities() -> Json<UiCapabilities> {
    Json(UiCapabilities {
        default_protocol: UI_DEFAULT_PROTOCOL,
        protocols: UI_PROTOCOL_CAPABILITIES
            .iter()
            .map(|spec| UiProtocolCapability {
                protocol: spec.protocol,
                versions: spec.versions.to_vec(),
                implementation_tier: spec.implementation_tier,
                spec_track: spec.spec_track,
                summary: spec.summary,
                features: spec.features.to_vec(),
                limitations: spec.limitations.to_vec(),
                deprecation: map_deprecation(spec.deprecation),
            })
            .collect(),
        tool_envelope_version: TOOL_ENVELOPE_VERSION,
    })
}

/// POST /api/ui/initialize
pub(crate) async fn ui_initialize(
    Json(input): Json<McpUiBridgeInput<McpUiInitializeParams>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (params, response_mode) = parse_bridge_input(input, "ui/initialize")?;
    let result = initialize_mcp_ui_bridge(params)?;
    bridge_result_json(response_mode, result)
}

/// POST /api/ui/message
pub(crate) async fn ui_message(
    Json(input): Json<McpUiBridgeInput<McpUiMessageParams>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (params, response_mode) = parse_bridge_input(input, "ui/message")?;
    let result = message_mcp_ui_bridge(params)?;
    bridge_result_json(response_mode, result)
}

/// POST /api/ui/update-model-context
pub(crate) async fn ui_update_model_context(
    Json(input): Json<McpUiBridgeInput<McpUiUpdateModelContextParams>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (params, response_mode) = parse_bridge_input(input, "ui/update-model-context")?;
    let result = update_mcp_ui_bridge_model_context(params)?;
    bridge_result_json(response_mode, result)
}

/// POST /api/ui/notifications/poll
pub(crate) async fn ui_poll_notifications(
    Json(input): Json<McpUiBridgeInput<McpUiPollNotificationsParams>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (params, response_mode) = parse_bridge_input(input, "ui/notifications/poll")?;
    let result = poll_mcp_ui_bridge_notifications(params)?;
    bridge_result_json(response_mode, result)
}

/// POST /api/ui/notifications/resources-list-changed
pub(crate) async fn ui_notify_resources_list_changed(
    Json(input): Json<McpUiBridgeInput<McpUiListChangedParams>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (params, response_mode) =
        parse_bridge_input(input, "ui/notifications/resources/list_changed")?;
    let result = notify_mcp_ui_resource_list_changed(params)?;
    bridge_result_json(response_mode, result)
}

/// POST /api/ui/notifications/tools-list-changed
pub(crate) async fn ui_notify_tools_list_changed(
    Json(input): Json<McpUiBridgeInput<McpUiListChangedParams>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (params, response_mode) = parse_bridge_input(input, "ui/notifications/tools/list_changed")?;
    let result = notify_mcp_ui_tool_list_changed(params)?;
    bridge_result_json(response_mode, result)
}

/// GET /api/ui/resources
pub async fn list_ui_resources() -> Json<UiResourceListResponse> {
    let resources: Vec<UiResource> = resource_registry()
        .read()
        .map(|registry| registry.values().map(|entry| entry.resource.clone()).collect())
        .unwrap_or_default();
    info!(resource_count = resources.len(), "ui resource list requested");
    Json(UiResourceListResponse { resources })
}

/// GET /api/ui/resources/read?uri=ui://...
pub async fn read_ui_resource(
    Query(query): Query<ReadUiResourceQuery>,
) -> Result<Json<UiResourceReadResponse>, (StatusCode, String)> {
    validate_ui_resource_uri(&query.uri)?;
    let guard = resource_registry().read().map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "resource registry poisoned".to_string())
    })?;
    let Some(entry) = guard.get(&query.uri) else {
        warn!(uri = %query.uri, "ui resource read failed: not found");
        return Err((StatusCode::NOT_FOUND, format!("resource not found: {}", query.uri)));
    };
    info!(uri = %query.uri, "ui resource read");
    Ok(Json(UiResourceReadResponse { contents: vec![entry.content.clone()] }))
}

/// POST /api/ui/resources/register
pub async fn register_ui_resource(
    Json(req): Json<RegisterUiResourceRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    validate_ui_resource_uri(&req.uri)?;
    validate_ui_resource_mime(&req.mime_type)?;
    let ui_meta_options = validate_ui_meta(&req.meta)?;

    let uri = req.uri.clone();
    let name = req.name.clone();
    let mime_type = req.mime_type.clone();
    let meta = req.meta.clone();
    let domain = ui_meta_options.domain.unwrap_or_else(|| "<none>".to_string());

    let entry = UiResourceEntry {
        resource: UiResource {
            uri: uri.clone(),
            name: name.clone(),
            description: req.description.clone(),
            mime_type: mime_type.clone(),
            meta: meta.clone(),
        },
        content: UiResourceContent {
            uri: uri.clone(),
            mime_type: mime_type.clone(),
            text: Some(req.text),
            blob: None,
            meta,
        },
    };

    resource_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "resource registry poisoned".to_string()))?
        .insert(uri.clone(), entry);
    info!(
        uri = %uri,
        name = %name,
        mime_type = %mime_type,
        ui_domain = %domain,
        "ui resource registered"
    );

    Ok(StatusCode::CREATED)
}
