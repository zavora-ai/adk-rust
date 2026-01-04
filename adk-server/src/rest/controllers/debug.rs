use crate::ServerConfig;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone)]
pub struct DebugController {
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

// ADK-Go compatible trace response (attributes map)
pub async fn get_trace_by_event_id(
    State(controller): State<DebugController>,
    Path(event_id): Path<String>,
) -> Result<Json<HashMap<String, String>>, StatusCode> {
    if let Some(exporter) = &controller.config.span_exporter {
        if let Some(attributes) = exporter.get_trace_by_event_id(&event_id) {
            return Ok(Json(attributes));
        }
    }
    
    Err(StatusCode::NOT_FOUND)
}

// Convert ADK exporter format to UI-compatible SpanData format
fn convert_to_span_data(attributes: &HashMap<String, String>) -> serde_json::Value {
    serde_json::json!({
        "name": attributes.get("span_name").map_or("unknown", |v| v.as_str()),
        "start_time": attributes.get("start_time").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
        "end_time": attributes.get("end_time").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
        "parent_id": null,
        "attributes": attributes,
        "invoc_id": attributes.get("gcp.vertex.agent.invocation_id").map_or("", |v| v.as_str())
    })
}

// Get all spans for a session (UI-compatible format)
pub async fn get_session_traces(
    State(controller): State<DebugController>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    if let Some(exporter) = &controller.config.span_exporter {
        let traces = exporter.get_session_trace(&session_id);
        let span_data: Vec<serde_json::Value> = traces.iter()
            .map(convert_to_span_data)
            .collect();
        return Ok(Json(span_data));
    }
    
    Ok(Json(Vec::new()))
}

pub async fn get_graph(
    State(_controller): State<DebugController>,
    Path((_app_name, _user_id, _session_id, _event_id)): Path<(String, String, String, String)>,
) -> Result<Json<GraphResponse>, StatusCode> {
    // Stub: Return a simple DOT graph
    let dot_src = "digraph G { Agent -> User [label=\"response\"]; }".to_string();
    Ok(Json(GraphResponse { dot_src }))
}
