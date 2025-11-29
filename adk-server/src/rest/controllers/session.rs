use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
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
pub struct SessionResponse {
    pub id: String,
    pub app_name: String,
    pub user_id: String,
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
    
    info!(generated_session_id = %session_id, "Session ID resolved");
    
    let session = controller
        .session_service
        .create(adk_session::CreateRequest {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: Some(session_id),
            state: std::collections::HashMap::new(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let response = SessionResponse {
        id: session.id().to_string(),
        app_name: session.app_name().to_string(),
        user_id: session.user_id().to_string(),
    };
    
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

    Ok(Json(SessionResponse {
        id: session.id().to_string(),
        app_name: session.app_name().to_string(),
        user_id: session.user_id().to_string(),
    }))
}

pub async fn delete_session(
    State(controller): State<SessionController>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
    controller
        .session_service
        .delete(adk_session::DeleteRequest {
            app_name,
            user_id,
            session_id,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}
