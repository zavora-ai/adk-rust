use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

#[derive(Clone)]
pub struct SessionController {
    session_service: Arc<dyn adk_session::SessionService>,
}

impl SessionController {
    pub fn new(session_service: Arc<dyn adk_session::SessionService>) -> Self {
        Self { session_service }
    }

    /// Helper function to convert a session to SessionResponse with actual events and state
    fn session_to_response(session: &dyn adk_session::Session) -> SessionResponse {
        // Convert events to JSON values, capping at a reasonable limit to prevent
        // uncontrolled allocation from very large session histories.
        const MAX_EVENTS: usize = 10_000;
        let events: Vec<serde_json::Value> = session
            .events()
            .all()
            .into_iter()
            .take(MAX_EVENTS)
            .map(|event| serde_json::to_value(event).unwrap_or(serde_json::Value::Null))
            .collect();

        SessionResponse {
            id: session.id().to_string(),
            app_name: session.app_name().to_string(),
            user_id: session.user_id().to_string(),
            last_update_time: session.last_update_time().timestamp(),
            events,
            state: session.state().all(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct CreateSessionRequest {
    #[serde(rename = "appName")]
    pub app_name: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub id: String,
    pub app_name: String,
    pub user_id: String,
    pub last_update_time: i64,
    pub events: Vec<serde_json::Value>,
    pub state: std::collections::HashMap<String, serde_json::Value>,
}

pub async fn create_session(
    State(controller): State<SessionController>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, StatusCode> {
    info!(
        app_name = %req.app_name,
        user_id = %req.user_id,
        session_id = ?req.session_id,
        "POST /sessions - Creating session"
    );

    // Generate session ID if not provided
    let session_id = req.session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let session = controller
        .session_service
        .create(adk_session::CreateRequest {
            app_name: req.app_name.clone(),
            user_id: req.user_id.clone(),
            session_id: Some(session_id),
            state: std::collections::HashMap::new(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let response = SessionController::session_to_response(session.as_ref());

    info!(session_id = %response.id, "Session created successfully");

    Ok(Json(response))
}

pub async fn get_session(
    State(controller): State<SessionController>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
) -> Result<Json<SessionResponse>, StatusCode> {
    let session = controller
        .session_service
        .get(adk_session::GetRequest {
            app_name,
            user_id,
            session_id,
            num_recent_events: None,
            after: None,
        })
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    Ok(Json(SessionController::session_to_response(session.as_ref())))
}

pub async fn delete_session(
    State(controller): State<SessionController>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
    controller
        .session_service
        .delete(adk_session::DeleteRequest { app_name, user_id, session_id })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Maximum number of state entries accepted in a create-session request body.
/// Prevents uncontrolled allocation from user-provided input.
const MAX_STATE_ENTRIES: usize = 1_000;

/// Maximum number of events accepted in a create-session request body.
const MAX_BODY_EVENTS: usize = 10_000;

fn deserialize_bounded_state<'de, D>(
    deserializer: D,
) -> Result<std::collections::HashMap<String, serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let full: std::collections::HashMap<String, serde_json::Value> =
        serde::Deserialize::deserialize(deserializer)?;
    if full.len() <= MAX_STATE_ENTRIES {
        Ok(full)
    } else {
        Ok(full.into_iter().take(MAX_STATE_ENTRIES).collect())
    }
}

fn deserialize_bounded_events<'de, D>(deserializer: D) -> Result<Vec<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let full: Vec<serde_json::Value> = serde::Deserialize::deserialize(deserializer)?;
    if full.len() <= MAX_BODY_EVENTS {
        Ok(full)
    } else {
        Ok(full.into_iter().take(MAX_BODY_EVENTS).collect())
    }
}

/// Request body for creating session (optional, can be empty)
#[derive(Serialize, Deserialize, Default)]
pub struct CreateSessionBodyRequest {
    #[serde(default, deserialize_with = "deserialize_bounded_state")]
    pub state: std::collections::HashMap<String, serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_bounded_events")]
    pub events: Vec<serde_json::Value>,
}

/// Path parameters for session routes
#[derive(Deserialize)]
pub struct SessionPathParams {
    pub app_name: String,
    pub user_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Create session from URL path parameters (adk-go compatible)
/// POST /apps/{app_name}/users/{user_id}/sessions
/// POST /apps/{app_name}/users/{user_id}/sessions/{session_id}
pub async fn create_session_from_path(
    State(controller): State<SessionController>,
    Path(params): Path<SessionPathParams>,
    body: Option<Json<CreateSessionBodyRequest>>,
) -> Result<Json<SessionResponse>, StatusCode> {
    let session_id = params.session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let session = controller
        .session_service
        .create(adk_session::CreateRequest {
            app_name: params.app_name.clone(),
            user_id: params.user_id.clone(),
            session_id: Some(session_id),
            state: body.map(|b| b.0.state).unwrap_or_default(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(SessionController::session_to_response(session.as_ref())))
}

/// Get session from URL path parameters (adk-go compatible)
pub async fn get_session_from_path(
    State(controller): State<SessionController>,
    Path(params): Path<SessionPathParams>,
) -> Result<Json<SessionResponse>, StatusCode> {
    let session_id = params.session_id.ok_or(StatusCode::BAD_REQUEST)?;

    let session = controller
        .session_service
        .get(adk_session::GetRequest {
            app_name: params.app_name,
            user_id: params.user_id,
            session_id,
            num_recent_events: None,
            after: None,
        })
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    Ok(Json(SessionController::session_to_response(session.as_ref())))
}

/// Delete session from URL path parameters (adk-go compatible)
pub async fn delete_session_from_path(
    State(controller): State<SessionController>,
    Path(params): Path<SessionPathParams>,
) -> Result<StatusCode, StatusCode> {
    let session_id = params.session_id.ok_or(StatusCode::BAD_REQUEST)?;

    controller
        .session_service
        .delete(adk_session::DeleteRequest {
            app_name: params.app_name,
            user_id: params.user_id,
            session_id,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

/// List sessions for a user (adk-go compatible)
pub async fn list_sessions(
    State(controller): State<SessionController>,
    Path(params): Path<SessionPathParams>,
) -> Result<Json<Vec<SessionResponse>>, StatusCode> {
    tracing::info!(
        "list_sessions called with app_name: {}, user_id: {}",
        params.app_name,
        params.user_id
    );

    let sessions = controller
        .session_service
        .list(adk_session::ListRequest {
            app_name: params.app_name.clone(),
            user_id: params.user_id.clone(),
        })
        .await
        .map_err(|e| {
            tracing::error!("Failed to list sessions: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!("Found {} sessions", sessions.len());

    let responses: Vec<SessionResponse> =
        sessions.into_iter().map(|s| SessionController::session_to_response(s.as_ref())).collect();

    Ok(Json(responses))
}
