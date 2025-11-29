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
pub async fn list_apps_compat(
    State(controller): State<AppsController>,
    Query(_query): Query<ListAppsQuery>,
) -> Result<Json<Vec<AppInfo>>, StatusCode> {
    let agent_names = controller.config.agent_loader.list_agents();
    
    // Get agent info for each agent
    let mut apps = Vec::new();
    for name in agent_names {
        if let Ok(agent) = controller.config.agent_loader.load_agent(&name).await {
            apps.push(AppInfo {
                name: agent.name().to_string(),
                description: agent.description().to_string(),
            });
        }
    }
    
    Ok(Json(apps))
}
