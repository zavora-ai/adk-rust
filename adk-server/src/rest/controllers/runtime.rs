use crate::ServerConfig;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tracing::{info, error};

#[derive(Clone)]
pub struct RuntimeController {
    config: ServerConfig,
}

impl RuntimeController {
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }
}

#[derive(Serialize, Deserialize)]
pub struct RunRequest {
    pub new_message: String,
}

/// Request format for /run_sse (adk-go compatible)
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunSseRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub new_message: NewMessage,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub state_delta: Option<serde_json::Value>,
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

pub async fn run_sse(
    State(controller): State<RuntimeController>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
    Json(req): Json<RunRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
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
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Load agent
    let agent = controller
        .config
        .agent_loader
        .load_agent(&app_name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Create runner
    let runner = adk_runner::Runner::new(adk_runner::RunnerConfig {
        app_name,
        agent,
        session_service: controller.config.session_service.clone(),
        artifact_service: controller.config.artifact_service.clone(),
        memory_service: None,
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Run agent
    let event_stream = runner
        .run(
            user_id,
            session_id,
            adk_core::Content::new("user").with_text(&req.new_message),
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Convert to SSE stream
    let sse_stream = stream::unfold(event_stream, |mut stream| async move {
        use futures::StreamExt;
        match stream.next().await {
            Some(Ok(event)) => {
                let json = serde_json::to_string(&event).ok()?;
                Some((Ok(Event::default().data(json)), stream))
            }
            _ => None,
        }
    });

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}


/// POST /run_sse - adk-go compatible endpoint
/// Accepts JSON body with appName, userId, sessionId, newMessage
pub async fn run_sse_compat(
    State(controller): State<RuntimeController>,
    Json(req): Json<RunSseRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    let app_name = req.app_name;
    let user_id = req.user_id;
    let session_id = req.session_id;

    info!(
        app_name = %app_name,
        user_id = %user_id,
        session_id = %session_id,
        "POST /run_sse request received"
    );

    // Extract text from message parts
    let message_text = req
        .new_message
        .parts
        .iter()
        .filter_map(|p| p.text.as_ref())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");

    info!(message = %message_text, "Extracted message text");

    // Validate session exists
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
    
    if let Err(ref e) = session_result {
        error!(error = ?e, "Session not found");
        return Err(StatusCode::NOT_FOUND);
    }
    
    info!("Session validated successfully");

    // Load agent
    let agent = controller
        .config
        .agent_loader
        .load_agent(&app_name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Create runner
    let runner = adk_runner::Runner::new(adk_runner::RunnerConfig {
        app_name,
        agent,
        session_service: controller.config.session_service.clone(),
        artifact_service: controller.config.artifact_service.clone(),
        memory_service: None,
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Run agent
    let event_stream = runner
        .run(
            user_id,
            session_id,
            adk_core::Content::new("user").with_text(&message_text),
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Convert to SSE stream
    let sse_stream = stream::unfold(event_stream, |mut stream| async move {
        use futures::StreamExt;
        match stream.next().await {
            Some(Ok(event)) => {
                let json = serde_json::to_string(&event).ok()?;
                Some((Ok(Event::default().data(json)), stream))
            }
            _ => None,
        }
    });

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}
