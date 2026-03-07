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
        // First try direct lookup by event_id
        if let Some(attributes) = exporter.get_trace_by_event_id(&event_id) {
            return Ok(Json(attributes));
        }

        // If not found, search through all spans for matching session event ID
        let trace_dict = exporter.get_trace_dict();
        for (_, attributes) in trace_dict.iter() {
            // Check if any span has this event_id in its attributes
            if attributes.values().any(|v| v == &event_id) {
                return Ok(Json(attributes.clone()));
            }
        }
    }

    Err(StatusCode::NOT_FOUND)
}

// Convert ADK exporter format to UI-compatible SpanData format
// Field names must match adk-web Trace.ts interface exactly
fn convert_to_span_data(attributes: &HashMap<String, String>) -> serde_json::Value {
    let start_time: u64 = attributes.get("start_time").and_then(|s| s.parse().ok()).unwrap_or(0);
    let end_time: u64 = attributes.get("end_time").and_then(|s| s.parse().ok()).unwrap_or(0);

    // Build JSON object - omit parent_span_id entirely to prevent nesting
    let mut obj = serde_json::json!({
        "name": attributes.get("span_name").map_or("unknown", |v| v.as_str()),
        "span_id": attributes.get("span_id").map_or("", |v| v.as_str()),
        "trace_id": attributes.get("trace_id").map_or("", |v| v.as_str()),
        "start_time": start_time,
        "end_time": end_time,
        "attributes": attributes,
        "invoc_id": attributes.get("adk.agent.invocation_id")
            .or_else(|| attributes.get("gcp.vertex.agent.invocation_id"))
            .map_or("", |v| v.as_str())
    });

    // Add LLM request/response if present (for UI display)
    if let Some(llm_req) = attributes
        .get("adk.agent.llm_request")
        .or_else(|| attributes.get("gcp.vertex.agent.llm_request"))
    {
        obj["adk.agent.llm_request"] = serde_json::Value::String(llm_req.clone());
    }
    if let Some(llm_resp) = attributes
        .get("adk.agent.llm_response")
        .or_else(|| attributes.get("gcp.vertex.agent.llm_response"))
    {
        obj["adk.agent.llm_response"] = serde_json::Value::String(llm_resp.clone());
    }

    obj
}

// Get all spans for a session (UI-compatible format)
pub async fn get_session_traces(
    State(controller): State<DebugController>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    if let Some(exporter) = &controller.config.span_exporter {
        let traces = exporter.get_session_trace(&session_id);
        let span_data: Vec<serde_json::Value> = traces.iter().map(convert_to_span_data).collect();
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

/// Get evaluation sets for an app (stub - returns empty array)
pub async fn get_eval_sets(
    State(_controller): State<DebugController>,
    Path(_app_name): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    // Stub: Return empty array - eval sets not yet implemented
    Ok(Json(Vec::new()))
}

/// Get event data by event_id - returns event with invocationId for trace linking
pub async fn get_event(
    State(controller): State<DebugController>,
    Path((app_name, _user_id, session_id, event_id)): Path<(String, String, String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Try to find trace data for this event_id
    if let Some(exporter) = &controller.config.span_exporter {
        let traces = exporter.get_session_trace(&session_id);

        // Find a trace with matching event_id
        for attrs in traces {
            if let Some(stored_event_id) =
                attrs.get("adk.agent.event_id").or_else(|| attrs.get("gcp.vertex.agent.event_id"))
            {
                if stored_event_id == &event_id {
                    // Found matching trace - return event-like structure
                    let invocation_id = attrs
                        .get("adk.agent.invocation_id")
                        .or_else(|| attrs.get("gcp.vertex.agent.invocation_id"))
                        .cloned()
                        .unwrap_or_default();

                    return Ok(Json(serde_json::json!({
                        "id": event_id,
                        "invocationId": invocation_id,
                        "appName": app_name,
                        "sessionId": session_id,
                        "attributes": attrs,
                        "adk.agent.llm_request": attrs.get("adk.agent.llm_request").or_else(|| attrs.get("gcp.vertex.agent.llm_request")),
                        "adk.agent.llm_response": attrs.get("adk.agent.llm_response").or_else(|| attrs.get("gcp.vertex.agent.llm_response"))
                    })));
                }
            }
        }
    }

    // Event not found - return a minimal stub to prevent UI errors
    Ok(Json(serde_json::json!({
        "id": event_id,
        "invocationId": "",
        "appName": app_name,
        "sessionId": session_id
    })))
}
