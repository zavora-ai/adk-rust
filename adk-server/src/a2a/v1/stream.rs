//! A2A v1.0.0 stream response helpers.
//!
//! Provides utilities for wrapping streaming events in [`StreamResponse`]
//! and formatting them as SSE `data:` lines for both JSON-RPC and REST
//! transport bindings.

use a2a_protocol_types::events::{StreamResponse, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a_protocol_types::jsonrpc::{JsonRpcSuccessResponse, JsonRpcVersion};
use a2a_protocol_types::task::Task;

/// Wraps a [`Task`] in a [`StreamResponse::Task`] for the first SSE event.
pub fn wrap_task_event(task: Task) -> StreamResponse {
    StreamResponse::Task(task)
}

/// Wraps a [`TaskStatusUpdateEvent`] in a [`StreamResponse::StatusUpdate`].
pub fn wrap_status_event(event: TaskStatusUpdateEvent) -> StreamResponse {
    StreamResponse::StatusUpdate(event)
}

/// Wraps a [`TaskArtifactUpdateEvent`] in a [`StreamResponse::ArtifactUpdate`].
pub fn wrap_artifact_event(event: TaskArtifactUpdateEvent) -> StreamResponse {
    StreamResponse::ArtifactUpdate(event)
}

/// Formats a [`StreamResponse`] as an SSE `data:` line for the JSON-RPC binding.
///
/// Wraps the response in a full `JsonRpcSuccessResponse` with the given
/// `request_id`, then serializes as `data: {json}\n\n`.
pub fn format_sse_line_jsonrpc(
    response: &StreamResponse,
    request_id: &serde_json::Value,
) -> String {
    let rpc_response = JsonRpcSuccessResponse {
        jsonrpc: JsonRpcVersion,
        id: Some(request_id.clone()),
        result: response,
    };
    let json =
        serde_json::to_string(&rpc_response).expect("StreamResponse serialization should not fail");
    format!("data: {json}\n\n")
}

/// Formats a [`StreamResponse`] as an SSE `data:` line for the REST binding.
///
/// Serializes the response directly as `data: {json}\n\n`.
pub fn format_sse_line_rest(response: &StreamResponse) -> String {
    let json =
        serde_json::to_string(response).expect("StreamResponse serialization should not fail");
    format!("data: {json}\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2a_protocol_types::artifact::{Artifact, ArtifactId};
    use a2a_protocol_types::message::Part;
    use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};

    fn sample_status_event() -> TaskStatusUpdateEvent {
        TaskStatusUpdateEvent {
            task_id: TaskId::new("task-1"),
            context_id: ContextId::new("ctx-1"),
            status: TaskStatus::new(TaskState::Working),
            metadata: None,
        }
    }

    fn sample_artifact_event() -> TaskArtifactUpdateEvent {
        TaskArtifactUpdateEvent {
            task_id: TaskId::new("task-1"),
            context_id: ContextId::new("ctx-1"),
            artifact: Artifact::new(ArtifactId::new("art-1"), vec![Part::text("hello")]),
            append: None,
            last_chunk: None,
            metadata: None,
        }
    }

    #[test]
    fn wrap_status_event_produces_status_update_variant() {
        let event = sample_status_event();
        let resp = wrap_status_event(event);
        assert!(matches!(resp, StreamResponse::StatusUpdate(_)), "expected StatusUpdate variant");
    }

    #[test]
    fn wrap_artifact_event_produces_artifact_update_variant() {
        let event = sample_artifact_event();
        let resp = wrap_artifact_event(event);
        assert!(
            matches!(resp, StreamResponse::ArtifactUpdate(_)),
            "expected ArtifactUpdate variant"
        );
    }

    #[test]
    fn format_sse_line_jsonrpc_wraps_in_jsonrpc_response() {
        let event = sample_status_event();
        let resp = wrap_status_event(event);
        let request_id = serde_json::json!(42);
        let line = format_sse_line_jsonrpc(&resp, &request_id);

        // Must start with "data: " and end with "\n\n"
        assert!(line.starts_with("data: "), "SSE line must start with 'data: '");
        assert!(line.ends_with("\n\n"), "SSE line must end with double newline");

        // Parse the JSON payload (strip "data: " prefix and trailing newlines)
        let json_str = &line["data: ".len()..line.len() - 2];
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect("valid JSON");

        // Must have JSON-RPC envelope fields
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 42);
        assert!(parsed.get("result").is_some(), "must have result field");
    }

    #[test]
    fn format_sse_line_rest_outputs_direct_json() {
        let event = sample_artifact_event();
        let resp = wrap_artifact_event(event);
        let line = format_sse_line_rest(&resp);

        assert!(line.starts_with("data: "));
        assert!(line.ends_with("\n\n"));

        let json_str = &line["data: ".len()..line.len() - 2];
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect("valid JSON");

        // REST mode: no JSON-RPC envelope
        assert!(parsed.get("jsonrpc").is_none(), "REST mode should not have jsonrpc field");
        // Should contain the artifact update content directly
        assert!(parsed.get("artifactUpdate").is_some(), "should contain artifactUpdate field");
    }

    #[test]
    fn format_sse_line_jsonrpc_with_string_request_id() {
        let event = sample_status_event();
        let resp = wrap_status_event(event);
        let request_id = serde_json::json!("req-abc");
        let line = format_sse_line_jsonrpc(&resp, &request_id);

        let json_str = &line["data: ".len()..line.len() - 2];
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect("valid JSON");
        assert_eq!(parsed["id"], "req-abc");
    }

    #[test]
    fn format_sse_line_rest_status_update_contains_expected_fields() {
        let event = sample_status_event();
        let resp = wrap_status_event(event);
        let line = format_sse_line_rest(&resp);

        let json_str = &line["data: ".len()..line.len() - 2];
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect("valid JSON");

        let status_update = &parsed["statusUpdate"];
        assert_eq!(status_update["taskId"], "task-1");
        assert_eq!(status_update["contextId"], "ctx-1");
    }

    #[test]
    fn wrap_task_event_produces_task_variant() {
        let task = Task {
            id: TaskId::new("task-1"),
            context_id: ContextId::new("ctx-1"),
            status: TaskStatus::new(TaskState::Submitted),
            history: None,
            artifacts: None,
            metadata: None,
        };
        let resp = wrap_task_event(task);
        assert!(matches!(resp, StreamResponse::Task(_)), "expected Task variant");
    }
}
