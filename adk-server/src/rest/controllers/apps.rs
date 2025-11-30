use crate::ServerConfig;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct AppsController {
    config: ServerConfig,
}

impl AppsController {
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }
}

/// Response format for /api/apps - simple list of agent names
pub async fn list_apps(
    State(controller): State<AppsController>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let apps = controller.config.agent_loader.list_agents();
    Ok(Json(apps))
}

/// Query params for /api/list-apps (adk-go compatible)
#[derive(Debug, Deserialize)]
pub struct ListAppsQuery {
    #[serde(default)]
    pub relative_path: Option<String>,
}

/// App info returned by /api/list-apps (adk-go compatible format)
#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub description: String,
}

/// Response format for /api/list-apps (adk-go compatible)
/// Returns just the agent names as strings - the frontend expects this format
pub async fn list_apps_compat(
    State(controller): State<AppsController>,
    Query(_query): Query<ListAppsQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let apps = controller.config.agent_loader.list_agents();
    Ok(Json(apps))
}
