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
use tracing::{Instrument, debug, info, warn};

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
    #[serde(default)]
    pub base64: Option<String>,
    #[serde(default, alias = "fileUri", alias = "file_uri")]
    pub url: Option<String>,
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
    #[serde(default, rename = "fileData")]
    pub file_data: Option<FileData>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InlineData {
    pub display_name: Option<String>,
    pub data: String,
    pub mime_type: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileData {
    pub display_name: Option<String>,
    pub file_uri: String,
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

enum InlineBase64ValidationError {
    Invalid,
    TooLarge,
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

fn validate_inline_base64(data_base64: &str) -> Result<(), InlineBase64ValidationError> {
    let data =
        BASE64_STANDARD.decode(data_base64).map_err(|_| InlineBase64ValidationError::Invalid)?;
    if data.len() > adk_core::MAX_INLINE_DATA_SIZE {
        return Err(InlineBase64ValidationError::TooLarge);
    }
    Ok(())
}

fn validate_file_uri(file_uri: &str, name_or_field: &str) -> Result<(), RuntimeError> {
    if file_uri.is_empty() {
        return Err((StatusCode::BAD_REQUEST, format!("Missing file URL for {name_or_field}")));
    }
    if reqwest::Url::parse(file_uri).is_err() {
        return Err((StatusCode::BAD_REQUEST, format!("Invalid file URL for {name_or_field}")));
    }
    Ok(())
}

/// Build Content from message text and attachments
fn build_content_with_attachments(
    text: &str,
    attachments: &[Attachment],
) -> Result<adk_core::Content, RuntimeError> {
    let mut content = adk_core::Content::new("user");
    let mut inline_data_base64_parts = 0usize;
    let mut file_data_parts = 0usize;

    // Add the text part
    content.parts.push(adk_core::Part::Text { text: text.to_string() });

    // Add attachment parts
    for attachment in attachments {
        match (attachment.base64.as_ref(), attachment.url.as_ref()) {
            (Some(data_base64), None) => {
                match validate_inline_base64(data_base64) {
                    Ok(()) => {}
                    Err(InlineBase64ValidationError::Invalid) => {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            format!("Invalid base64 data for attachment '{}'", attachment.name),
                        ));
                    }
                    Err(InlineBase64ValidationError::TooLarge) => {
                        return Err((
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!(
                                "Attachment '{}' exceeds max inline size of {} bytes",
                                attachment.name,
                                adk_core::MAX_INLINE_DATA_SIZE
                            ),
                        ));
                    }
                }
                // Keep canonical base64 payload to avoid decode/re-encode in provider adapters.
                content.parts.push(adk_core::Part::InlineDataBase64 {
                    mime_type: attachment.mime_type.clone(),
                    data_base64: data_base64.clone(),
                });
                inline_data_base64_parts += 1;
            }
            (None, Some(file_uri)) => {
                validate_file_uri(file_uri, &format!("attachment '{}'", attachment.name))?;
                content.parts.push(adk_core::Part::FileData {
                    mime_type: attachment.mime_type.clone(),
                    file_uri: file_uri.clone(),
                });
                file_data_parts += 1;
            }
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!(
                        "Attachment '{}' must contain exactly one of 'base64' or 'url'",
                        attachment.name
                    ),
                ));
            }
        }
    }

    debug!(
        inline_data_base64_parts,
        inline_data_bytes_parts = 0usize,
        file_data_parts,
        "built content with attachment parts"
    );

    Ok(content)
}

/// Build Content from message parts (for /run_sse endpoint)
fn build_content_from_parts(parts: &[MessagePart]) -> Result<adk_core::Content, RuntimeError> {
    let mut content = adk_core::Content::new("user");
    let mut inline_data_base64_parts = 0usize;
    let mut file_data_parts = 0usize;

    for (index, part) in parts.iter().enumerate() {
        // Add text part if present
        if let Some(text) = &part.text {
            content.parts.push(adk_core::Part::Text { text: text.clone() });
        }

        // Add inline data part if present
        if let Some(inline_data) = &part.inline_data {
            match validate_inline_base64(&inline_data.data) {
                Ok(()) => {
                    // Keep canonical base64 payload to avoid decode/re-encode in provider adapters.
                    content.parts.push(adk_core::Part::InlineDataBase64 {
                        mime_type: inline_data.mime_type.clone(),
                        data_base64: inline_data.data.clone(),
                    });
                    inline_data_base64_parts += 1;
                }
                Err(InlineBase64ValidationError::Invalid) => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "Invalid base64 data in inline_data".to_string(),
                    ));
                }
                Err(InlineBase64ValidationError::TooLarge) => {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!(
                            "inline_data exceeds max inline size of {} bytes",
                            adk_core::MAX_INLINE_DATA_SIZE
                        ),
                    ));
                }
            }
        }

        if let Some(file_data) = &part.file_data {
            let field_name = format!("file_data at part index {index}");
            validate_file_uri(&file_data.file_uri, &field_name)?;
            content.parts.push(adk_core::Part::FileData {
                mime_type: file_data.mime_type.clone(),
                file_uri: file_data.file_uri.clone(),
            });
            file_data_parts += 1;
        }
    }

    debug!(
        inline_data_base64_parts,
        inline_data_bytes_parts = 0usize,
        file_data_parts,
        "built content from message parts"
    );

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

    // Build content from message parts (includes text, inline_data, and file_data)
    let content = build_content_from_parts(&req.new_message.parts)?;

    // Log part info
    let text_parts: Vec<_> = req.new_message.parts.iter().filter(|p| p.text.is_some()).collect();
    let data_parts: Vec<_> =
        req.new_message.parts.iter().filter(|p| p.inline_data.is_some()).collect();
    let file_parts: Vec<_> =
        req.new_message.parts.iter().filter(|p| p.file_data.is_some()).collect();
    if !data_parts.is_empty() || !file_parts.is_empty() {
        info!(
            text_parts = text_parts.len(),
            inline_data_parts = data_parts.len(),
            file_data_parts = file_parts.len(),
            "processing request with non-text parts"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn png_base64() -> String {
        base64::engine::general_purpose::STANDARD.encode([0x89, 0x50, 0x4E, 0x47])
    }

    #[test]
    fn build_content_with_attachments_keeps_base64_variant() {
        let attachments = vec![Attachment {
            name: "image.png".to_string(),
            mime_type: "image/png".to_string(),
            base64: Some(png_base64()),
            url: None,
        }];

        let content =
            build_content_with_attachments("describe", &attachments).expect("content should build");

        assert!(matches!(content.parts[1], adk_core::Part::InlineDataBase64 { .. }));
    }

    #[test]
    fn build_content_with_attachments_accepts_url_variant() {
        let attachments = vec![Attachment {
            name: "doc.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            base64: None,
            url: Some("https://example.com/doc.pdf".to_string()),
        }];

        let content = build_content_with_attachments("summarize", &attachments)
            .expect("content should build");

        assert!(matches!(content.parts[1], adk_core::Part::FileData { .. }));
    }

    #[test]
    fn build_content_with_attachments_accepts_any_url_scheme() {
        let attachments = vec![Attachment {
            name: "doc.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            base64: None,
            url: Some("gs://bucket/doc.pdf".to_string()),
        }];

        let content = build_content_with_attachments("summarize", &attachments)
            .expect("content should build");

        assert!(matches!(content.parts[1], adk_core::Part::FileData { .. }));
    }

    #[test]
    fn build_content_with_attachments_rejects_invalid_base64() {
        let attachments = vec![Attachment {
            name: "invalid.png".to_string(),
            mime_type: "image/png".to_string(),
            base64: Some("not-valid-base64!!!".to_string()),
            url: None,
        }];

        let err =
            build_content_with_attachments("describe", &attachments).expect_err("should fail");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_content_from_parts_keeps_inline_data_base64_variant() {
        let parts = vec![MessagePart {
            text: Some("describe".to_string()),
            inline_data: Some(InlineData {
                display_name: None,
                data: png_base64(),
                mime_type: "image/png".to_string(),
            }),
            file_data: None,
        }];

        let content = build_content_from_parts(&parts).expect("content should build");
        assert!(matches!(content.parts[1], adk_core::Part::InlineDataBase64 { .. }));
    }

    #[test]
    fn build_content_from_parts_accepts_file_data() {
        let parts = vec![MessagePart {
            text: Some("summarize".to_string()),
            inline_data: None,
            file_data: Some(FileData {
                display_name: None,
                file_uri: "https://example.com/doc.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
            }),
        }];

        let content = build_content_from_parts(&parts).expect("content should build");
        assert!(matches!(content.parts[1], adk_core::Part::FileData { .. }));
    }
}
