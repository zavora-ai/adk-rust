use crate::ServerConfig;
use axum::{extract::State, http::StatusCode, Json};

#[derive(Clone)]
pub struct AppsController {
    config: ServerConfig,
}

impl AppsController {
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }
}

pub async fn list_apps(
    State(controller): State<AppsController>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let apps = controller.config.agent_loader.list_agents();
    Ok(Json(apps))
}
