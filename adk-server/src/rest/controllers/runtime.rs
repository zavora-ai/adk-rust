use crate::ServerConfig;
use adk_ui::{SUPPORTED_UI_PROTOCOLS, UI_PROTOCOL_CAPABILITIES, normalize_runtime_ui_protocol};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures::{
    StreamExt,
    stream::{self, Stream},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::convert::Infallible;
use tracing::{Instrument, info, warn};

fn default_streaming_true() -> bool {
    true
}

const UI_PROTOCOL_HEADER: &str = "x-adk-ui-protocol";

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
    pub attachments: Vec<Attachment>,
}

/// Request format for /run_sse (adk-go compatible)
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunSseRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub new_message: NewMessage,
    #[serde(default = "default_streaming_true")]
    pub streaming: bool,
    #[serde(default)]
    pub state_delta: Option<serde_json::Value>,
    #[serde(default, alias = "ui_protocol")]
    pub ui_protocol: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NewMessage {
    pub role: String,
    pub parts: Vec<MessagePart>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MessagePart {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "inlineData")]
    pub inline_data: Option<InlineData>,
}

#[derive(Serialize, Deserialize, Debug)]
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
        warn!(
            requested = %raw,
            header = %UI_PROTOCOL_HEADER,
            supported = ?SUPPORTED_UI_PROTOCOLS,
            "unsupported ui protocol requested"
        );
        (
            StatusCode::BAD_REQUEST,
            format!(
                "Unsupported ui protocol '{}'. Supported profiles: {}",
                raw,
                SUPPORTED_UI_PROTOCOLS.join(", ")
            ),
        )
    })
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

pub async fn run_sse(
    State(controller): State<RuntimeController>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, RuntimeError> {
    let ui_profile = resolve_ui_profile(&headers, req.ui_protocol.as_deref())?;
    let span = tracing::info_span!("run_sse", session_id = %session_id, app_name = %app_name, user_id = %user_id);

    async move {
        log_profile_deprecation(ui_profile);
        info!(
            ui_protocol = %ui_profile.as_str(),
            "resolved ui protocol profile for runtime request"
        );

        // Validate session exists
        controller
            .config
            .session_service
            .get(adk_session::GetRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .map_err(|_| (StatusCode::NOT_FOUND, "session not found".to_string()))?;

        // Load agent
        let agent =
            controller.config.agent_loader.load_agent(&app_name).await.map_err(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "failed to load agent".to_string())
            })?;

        // Create runner
        let runner = adk_runner::Runner::new(adk_runner::RunnerConfig {
            app_name: app_name.clone(),
            agent,
            session_service: controller.config.session_service.clone(),
            artifact_service: controller.config.artifact_service.clone(),
            memory_service: None,
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
        })
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to create runner".to_string()))?;

        // Build content with attachments
        let content = build_content_with_attachments(&req.new_message, &req.attachments)?;

        // Log attachment info
        if !req.attachments.is_empty() {
            info!(attachment_count = req.attachments.len(), "processing request with attachments");
        }

        // Run agent
        let event_stream = runner
            .run(user_id, session_id, content)
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to run agent".to_string()))?;

        // Convert to SSE stream
        let selected_profile = ui_profile;
        let sse_stream = stream::unfold(event_stream, move |mut stream| async move {
            match stream.next().await {
                Some(Ok(event)) => {
                    let json = serialize_runtime_event(&event, selected_profile)?;
                    Some((Ok(Event::default().data(json)), stream))
                }
                _ => None,
            }
        });

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
    let ui_profile = resolve_ui_profile(&headers, req.ui_protocol.as_deref())?;
    let app_name = req.app_name;
    let user_id = req.user_id;
    let session_id = req.session_id;

    info!(
        app_name = %app_name,
        user_id = %user_id,
        session_id = %session_id,
        ui_protocol = %ui_profile.as_str(),
        "POST /run_sse request received"
    );
    log_profile_deprecation(ui_profile);

    // Build content from message parts (includes both text and inline_data)
    let content = build_content_from_parts(&req.new_message.parts)?;

    // Log part info
    let text_parts: Vec<_> = req.new_message.parts.iter().filter(|p| p.text.is_some()).collect();
    let data_parts: Vec<_> =
        req.new_message.parts.iter().filter(|p| p.inline_data.is_some()).collect();
    if !data_parts.is_empty() {
        info!(
            text_parts = text_parts.len(),
            inline_data_parts = data_parts.len(),
            "processing request with inline data"
        );
    }

    // Validate session exists or create it
    let session_result = controller
        .config
        .session_service
        .get(adk_session::GetRequest {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
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
                user_id: user_id.clone(),
                session_id: Some(session_id.clone()),
                state: std::collections::HashMap::new(),
            })
            .await
            .map_err(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "failed to create session".to_string())
            })?;
    }

    // Load agent
    let agent = controller
        .config
        .agent_loader
        .load_agent(&app_name)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to load agent".to_string()))?;

    // Create runner with streaming config from request
    let streaming_mode =
        if req.streaming { adk_core::StreamingMode::SSE } else { adk_core::StreamingMode::None };

    let runner = adk_runner::Runner::new(adk_runner::RunnerConfig {
        app_name,
        agent,
        session_service: controller.config.session_service.clone(),
        artifact_service: controller.config.artifact_service.clone(),
        memory_service: None,
        plugin_manager: None,
        run_config: Some(adk_core::RunConfig { streaming_mode, ..adk_core::RunConfig::default() }),
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to create runner".to_string()))?;

    // Run agent with full content (text + inline data)
    let event_stream = runner
        .run(user_id, session_id, content)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to run agent".to_string()))?;

    // Convert to SSE stream
    let selected_profile = ui_profile;
    let sse_stream = stream::unfold(event_stream, move |mut stream| async move {
        match stream.next().await {
            Some(Ok(event)) => {
                let json = serialize_runtime_event(&event, selected_profile)?;
                Some((Ok(Event::default().data(json)), stream))
            }
            _ => None,
        }
    });

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}
