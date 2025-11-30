use crate::ServerConfig;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone)]
pub struct DebugController {
    #[allow(dead_code)] // Reserved for future debug functionality
    config: ServerConfig,
}

impl DebugController {
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }
}

#[derive(Serialize)]
pub struct GraphResponse {
    #[serde(rename = "dotSrc")]
    pub dot_src: String,
}

pub async fn get_trace(
    State(_controller): State<DebugController>,
    Path(_event_id): Path<String>,
) -> Result<Json<HashMap<String, String>>, StatusCode> {
    // Stub: Return empty map or mock data
    let mut trace = HashMap::new();
    trace.insert("status".to_string(), "not_implemented".to_string());
    Ok(Json(trace))
}

pub async fn get_graph(
    State(_controller): State<DebugController>,
    Path((_app_name, _user_id, _session_id, _event_id)): Path<(String, String, String, String)>,
) -> Result<Json<GraphResponse>, StatusCode> {
    // Stub: Return a simple DOT graph
    let dot_src = "digraph G { Agent -> User [label=\"response\"]; }".to_string();
    Ok(Json(GraphResponse { dot_src }))
}
