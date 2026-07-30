//! Server-side permission bridge.
//!
//! Maps an ADK [`ToolConfirmationRequest`] (surfaced on
//! `event.actions.tool_confirmation` when an agent pauses pending human
//! approval of a tool call) to an ACP `session/request_permission` request,
//! awaits the client's outcome, and maps that outcome back to an ADK
//! [`ToolConfirmationDecision`].
//!
//! # Correlation
//!
//! Each confirmation carries the LLM's `function_call_id`. The bridge threads
//! that identifier onto the outbound `session/request_permission` request as the
//! tool-call id, so the client sees the exact call it is approving and the
//! outcome is correlated to that call (Requirement 7.5, Property P8).
//!
//! The resume API
//! ([`RunConfig::tool_confirmation_decisions`](adk_core::RunConfig::tool_confirmation_decisions))
//! is keyed by
//! *function-call ID*, so the decision the bridge derives is recorded against the
//! exact call the client approved and cannot be replayed onto another call of the
//! same tool. A confirmation request that carries no call ID is rejected rather
//! than approved under a weaker key.
//!
//! # SDK safety
//!
//! `session/request_permission` is a nested, agent-to-client request issued
//! *while* the server is still handling the outer `session/prompt` request. The
//! SDK's [`block_task`](agent_client_protocol::ConnectionTo) is only safe to
//! await inside a task spawned with `ConnectionTo::spawn`. The prompt handler
//! runs inside exactly such a spawned task (see `transport/stdio.rs`), so the
//! nested request does not block the connection's dispatch loop and the outer
//! prompt response still completes.

use adk_core::{ToolConfirmationDecision, ToolConfirmationRequest};
use agent_client_protocol::schema::v1::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    SessionId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Client, ConnectionTo};

use super::error::AcpServerError;
use super::streamer::infer_tool_kind;

/// Opaque option id for the one-time allow choice offered to the client.
const ALLOW_OPTION_ID: &str = "acp-allow-once";
/// Opaque option id for the one-time reject choice offered to the client.
const REJECT_OPTION_ID: &str = "acp-reject-once";

/// Bridges ADK tool confirmations to ACP `session/request_permission`.
pub(crate) struct PermissionBridge;

impl PermissionBridge {
    /// The permission options offered to the client for every request.
    ///
    /// A one-time allow and a one-time reject are sufficient to express the ADK
    /// [`ToolConfirmationDecision`] set (`Approve` / `Deny`); persistent
    /// allow/reject are not offered because ADK applies a decision per resume,
    /// not persistently.
    fn options() -> Vec<PermissionOption> {
        vec![
            PermissionOption::new(ALLOW_OPTION_ID, "Allow", PermissionOptionKind::AllowOnce),
            PermissionOption::new(REJECT_OPTION_ID, "Reject", PermissionOptionKind::RejectOnce),
        ]
    }

    /// Build the ACP `session/request_permission` request describing a tool call
    /// awaiting confirmation.
    ///
    /// The tool-call id is the confirmation's `function_call_id` when present so
    /// the client can correlate the request to the exact call; otherwise a
    /// deterministic `"{tool_name}-call"` fallback mirrors the streamer's tool
    /// call id derivation.
    fn build_request(
        session_id: &SessionId,
        request: &ToolConfirmationRequest,
    ) -> RequestPermissionRequest {
        let call_id = request
            .function_call_id
            .clone()
            .unwrap_or_else(|| format!("{}-call", request.tool_name));
        let fields = ToolCallUpdateFields::new()
            .kind(infer_tool_kind(&request.tool_name))
            .status(ToolCallStatus::Pending)
            .title(format!("Run tool '{}'", request.tool_name))
            .raw_input(request.args.clone());
        RequestPermissionRequest::new(
            session_id.clone(),
            ToolCallUpdate::new(call_id, fields),
            Self::options(),
        )
    }

    /// Map an ACP permission outcome to an ADK decision.
    ///
    /// A selected allow option (one-time or persistent) approves; a selected
    /// reject option denies; a cancelled request denies (Requirement 7.4). An
    /// unrecognized selected option id is treated as a denial so an
    /// unauthorized call never executes.
    fn decision_for(outcome: &RequestPermissionOutcome) -> ToolConfirmationDecision {
        match outcome {
            RequestPermissionOutcome::Selected(selected) => {
                let id = selected.option_id.to_string();
                if id == ALLOW_OPTION_ID {
                    ToolConfirmationDecision::Approve
                } else {
                    ToolConfirmationDecision::Deny
                }
            }
            // Cancellation and any future outcome variant map to deny so the
            // affected tool call is not executed.
            _ => ToolConfirmationDecision::Deny,
        }
    }

    /// Send a `session/request_permission` request to the client, await the
    /// outcome, and return the mapped decision.
    ///
    /// # Errors
    ///
    /// Returns [`AcpServerError::Transport`] if the nested request fails or the
    /// client connection drops before responding.
    pub(crate) async fn request(
        connection: &ConnectionTo<Client>,
        session_id: &SessionId,
        request: &ToolConfirmationRequest,
    ) -> Result<ToolConfirmationDecision, AcpServerError> {
        let acp_request = Self::build_request(session_id, request);
        let response = connection
            .send_request(acp_request)
            .block_task()
            .await
            .map_err(|error| AcpServerError::Transport(error.to_string()))?;
        Ok(Self::decision_for(&response.outcome))
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{
        PermissionOptionId, SelectedPermissionOutcome, ToolKind,
    };

    use super::*;

    fn confirmation() -> ToolConfirmationRequest {
        ToolConfirmationRequest {
            tool_name: "delete_file".into(),
            function_call_id: Some("call-42".into()),
            args: serde_json::json!({"path": "/tmp/x"}),
        }
    }

    #[test]
    fn request_carries_function_call_id_kind_and_args() {
        let session = SessionId::new("session-1");
        let request = PermissionBridge::build_request(&session, &confirmation());
        assert_eq!(request.tool_call.tool_call_id.to_string(), "call-42");
        assert_eq!(request.tool_call.fields.kind, Some(ToolKind::Delete));
        assert_eq!(request.tool_call.fields.status, Some(ToolCallStatus::Pending));
        assert_eq!(request.tool_call.fields.raw_input, Some(serde_json::json!({"path": "/tmp/x"})));
        assert_eq!(request.options.len(), 2);
    }

    #[test]
    fn request_falls_back_to_tool_name_call_id_when_absent() {
        let session = SessionId::new("session-1");
        let mut confirmation = confirmation();
        confirmation.function_call_id = None;
        let request = PermissionBridge::build_request(&session, &confirmation);
        assert_eq!(request.tool_call.tool_call_id.to_string(), "delete_file-call");
    }

    #[test]
    fn allow_selection_maps_to_approve() {
        let outcome = RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            PermissionOptionId::new(ALLOW_OPTION_ID),
        ));
        assert_eq!(PermissionBridge::decision_for(&outcome), ToolConfirmationDecision::Approve);
    }

    #[test]
    fn reject_selection_maps_to_deny() {
        let outcome = RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            PermissionOptionId::new(REJECT_OPTION_ID),
        ));
        assert_eq!(PermissionBridge::decision_for(&outcome), ToolConfirmationDecision::Deny);
    }

    #[test]
    fn cancellation_maps_to_deny() {
        assert_eq!(
            PermissionBridge::decision_for(&RequestPermissionOutcome::Cancelled),
            ToolConfirmationDecision::Deny
        );
    }

    #[test]
    fn unknown_selection_maps_to_deny() {
        let outcome = RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            PermissionOptionId::new("made-up-id"),
        ));
        assert_eq!(PermissionBridge::decision_for(&outcome), ToolConfirmationDecision::Deny);
    }
}
