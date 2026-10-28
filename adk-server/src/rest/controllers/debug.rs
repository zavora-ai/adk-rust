use crate::ServerConfig;
use axum::{
    Extension, Json,
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

fn authorize_path_user_id(
    request_context: &Option<adk_core::RequestContext>,
    user_id: &str,
) -> Result<(), StatusCode> {
    match request_context {
        Some(context) if context.user_id != user_id => Err(StatusCode::FORBIDDEN),
        _ => Ok(()),
    }
}

/// Returns the attributes of the span recorded for an ADK event ID (ADK-Go compatible).
///
/// Mounted when no `RequestContextExtractor` is configured, or when
/// `SecurityConfig::expose_admin_debug` is set. Only the span's `gcp.vertex.agent.event_id`
/// is matched, so a session ID or any other attribute value answers `404 Not Found`. An
/// authenticated caller is answered `403 Forbidden` unless the span's `adk.user_id` is
/// theirs; the owner reads every attribute, the `gcp.vertex.agent.llm_request` and
/// `gcp.vertex.agent.llm_response` payloads included.
pub async fn get_trace_by_event_id(
    State(controller): State<DebugController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path(event_id): Path<String>,
) -> Result<Json<HashMap<String, String>>, StatusCode> {
    if request_context.is_some() && !controller.config.security.expose_admin_debug {
        return Err(StatusCode::NOT_FOUND);
    }

    // The exporter also answers a span ID, so the event ID itself is checked.
    let attributes = controller
        .config
        .span_exporter
        .as_ref()
        .and_then(|exporter| exporter.get_trace_by_event_id(&event_id))
        .filter(|attributes| {
            attributes.get("gcp.vertex.agent.event_id").is_some_and(|id| id == &event_id)
        })
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(context) = &request_context
        && attributes.get("adk.user_id") != Some(&context.user_id)
    {
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(Json(attributes))
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
        "invoc_id": attributes.get("gcp.vertex.agent.invocation_id").map_or("", |v| v.as_str())
    });

    // Add LLM request/response if present (for UI display)
    if let Some(llm_req) = attributes.get("gcp.vertex.agent.llm_request") {
        obj["gcp.vertex.agent.llm_request"] = serde_json::Value::String(llm_req.clone());
    }
    if let Some(llm_resp) = attributes.get("gcp.vertex.agent.llm_response") {
        obj["gcp.vertex.agent.llm_response"] = serde_json::Value::String(llm_resp.clone());
    }

    obj
}

// Get all spans for a session (UI-compatible format)
pub async fn get_session_traces(
    State(controller): State<DebugController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    if let Some(exporter) = &controller.config.span_exporter {
        let traces = exporter.get_session_trace(&session_id);
        if let Some(context) = request_context.as_ref() {
            for trace in &traces {
                if let Some(owner) = trace.get("adk.user_id")
                    && owner != &context.user_id
                {
                    return Err(StatusCode::FORBIDDEN);
                }
            }
        }
        let span_data: Vec<serde_json::Value> = traces.iter().map(convert_to_span_data).collect();
        return Ok(Json(span_data));
    }

    Ok(Json(Vec::new()))
}

pub async fn get_graph(
    State(_controller): State<DebugController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path((_app_name, user_id, _session_id, _event_id)): Path<(String, String, String, String)>,
) -> Result<Json<GraphResponse>, (StatusCode, Json<serde_json::Value>)> {
    authorize_path_user_id(&request_context, &user_id)
        .map_err(|s| (s, Json(serde_json::json!({"error": "forbidden"}))))?;

    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "graph generation is not yet implemented"})),
    ))
}

/// Get evaluation sets for an app
pub async fn get_eval_sets(
    State(_controller): State<DebugController>,
    Extension(_request_context): Extension<Option<adk_core::RequestContext>>,
    Path(_app_name): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "eval sets are not yet implemented"})),
    ))
}

/// Get event data by event_id - returns event with invocationId for trace linking
///
/// Answers `403 Forbidden` when the path user is not the authenticated caller, or
/// when any span recorded for `session_id` belongs to a different user — traces are
/// indexed by session ID alone, and session IDs are chosen by their creators.
pub async fn get_event(
    State(controller): State<DebugController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path((app_name, user_id, session_id, event_id)): Path<(String, String, String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    authorize_path_user_id(&request_context, &user_id)?;

    // Try to find trace data for this event_id
    if let Some(exporter) = &controller.config.span_exporter {
        let traces = exporter.get_session_trace(&session_id);
        if traces
            .iter()
            .any(|trace| trace.get("adk.user_id").is_some_and(|owner| owner != &user_id))
        {
            return Err(StatusCode::FORBIDDEN);
        }

        // Find a trace with matching event_id
        for attrs in traces {
            if let Some(stored_event_id) = attrs.get("gcp.vertex.agent.event_id")
                && stored_event_id == &event_id
            {
                // Found matching trace - return event-like structure
                let invocation_id =
                    attrs.get("gcp.vertex.agent.invocation_id").cloned().unwrap_or_default();

                return Ok(Json(serde_json::json!({
                    "id": event_id,
                    "invocationId": invocation_id,
                    "appName": app_name,
                    "sessionId": session_id,
                    "attributes": attrs,
                    "gcp.vertex.agent.llm_request": attrs.get("gcp.vertex.agent.llm_request"),
                    "gcp.vertex.agent.llm_response": attrs.get("gcp.vertex.agent.llm_response")
                })));
            }
        }
    }

    // Event not found
    Err(StatusCode::NOT_FOUND)
}
