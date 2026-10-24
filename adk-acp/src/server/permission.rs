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
//! # Always allow
//!
//! With [`StandingPermissions`] configured, a call it may remember is also offered
//! `allow_always` and `reject_always` (spec 3.3 §10 A1). Choosing one decides this call
//! and asks [`StandingPermissions::remember`] to make the answer a standing rule, so the
//! next identical call is not asked about. A call it may never remember — spending money,
//! publishing, leaving plan mode, an unsandboxed retry — is offered allow once and deny
//! only, and an "always" answer to a request that did not offer one is a denial.
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

use adk_core::{ApprovalScope, ToolConfirmationDecision, ToolConfirmationRequest};
use agent_client_protocol::schema::v1::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    SessionId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Client, ConnectionTo};
use async_trait::async_trait;

use super::error::AcpServerError;
use super::streamer::infer_tool_kind;

/// Opaque option id for the one-time allow choice offered to the client.
const ALLOW_OPTION_ID: &str = "acp-allow-once";
/// Opaque option id for the one-time reject choice offered to the client.
const REJECT_OPTION_ID: &str = "acp-reject-once";
/// Opaque option id for the allow choice remembered as a standing rule.
const ALLOW_ALWAYS_OPTION_ID: &str = "acp-allow-always";
/// Opaque option id for the reject choice remembered as a standing rule.
const REJECT_ALWAYS_OPTION_ID: &str = "acp-reject-always";

/// Remembers an ACP client's "always" answer as a standing rule (spec 3.3 §10 A1).
///
/// The server asks [`offer`](Self::offer) before each permission request: an offer adds
/// `allow_always` and `reject_always` to the request, and the client's choice of one is
/// passed to [`remember`](Self::remember). `adk-harness`'s `HarnessAgent` implements it
/// with its standing rules (spec 3.1 §7.5), bound to the tool's exact declaration and the
/// session's person; `AcpServerConfigBuilder::standing_permissions` installs it.
///
/// # Example
///
/// ```rust
/// use adk_acp::server::{StandingOffer, StandingPermissions};
/// use adk_core::{ApprovalScope, ToolConfirmationDecision, ToolConfirmationRequest};
///
/// /// Remembers nothing: every call is offered allow once and deny only.
/// struct Never;
///
/// #[async_trait::async_trait]
/// impl StandingPermissions for Never {
///     async fn offer(
///         &self,
///         _scope: &ApprovalScope,
///         _request: &ToolConfirmationRequest,
///     ) -> Option<StandingOffer> {
///         None
///     }
///
///     async fn remember(
///         &self,
///         _scope: &ApprovalScope,
///         _request: &ToolConfirmationRequest,
///         _decision: ToolConfirmationDecision,
///     ) -> adk_core::Result<()> {
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait StandingPermissions: Send + Sync {
    /// What an "always" answer to `request` would cover for the person in `scope`, or
    /// `None` when the call may never be remembered, such as spending money, publishing,
    /// leaving plan mode, or an unsandboxed retry.
    async fn offer(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Option<StandingOffer>;

    /// Remembers `decision` as a standing rule covering what [`offer`](Self::offer)
    /// described for `request`, owned by the person in `scope`.
    ///
    /// # Errors
    ///
    /// Returns an error when the rule cannot be created or stored; the call is then
    /// decided once.
    async fn remember(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
        decision: ToolConfirmationDecision,
    ) -> adk_core::Result<()>;
}

/// What an "always" answer would cover, offered by [`StandingPermissions::offer`]
/// (spec 3.3 §10 A1).
///
/// # Example
///
/// ```rust
/// use adk_acp::server::StandingOffer;
///
/// let offer = StandingOffer::new("for this command (bash: cargo test …)");
/// assert_eq!(offer.scope, "for this command (bash: cargo test …)");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StandingOffer {
    /// Completes "Always allow …" and "Always deny …" in plain words, such as
    /// `for this command (bash: cargo test …)`.
    pub scope: String,
}

impl StandingOffer {
    /// An offer covering `scope`.
    pub fn new(scope: impl Into<String>) -> Self {
        Self { scope: scope.into() }
    }
}

/// The client's answer to one permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PermissionAnswer {
    /// The decision for this call.
    pub(crate) decision: ToolConfirmationDecision,
    /// Whether the client chose to remember it as a standing rule.
    pub(crate) always: bool,
}

/// Bridges ADK tool confirmations to ACP `session/request_permission`.
pub(crate) struct PermissionBridge;

impl PermissionBridge {
    /// The permission options offered for a request, in the words of spec 3.3 §8.2.
    ///
    /// Allow once and deny are always offered. An `offer` adds the two remembered
    /// answers, worded with what they cover.
    fn options(offer: Option<&StandingOffer>) -> Vec<PermissionOption> {
        let mut options = vec![PermissionOption::new(
            ALLOW_OPTION_ID,
            "Allow once",
            PermissionOptionKind::AllowOnce,
        )];
        if let Some(offer) = offer {
            options.push(PermissionOption::new(
                ALLOW_ALWAYS_OPTION_ID,
                format!("Always allow {}", offer.scope),
                PermissionOptionKind::AllowAlways,
            ));
        }
        options.push(PermissionOption::new(
            REJECT_OPTION_ID,
            "Deny",
            PermissionOptionKind::RejectOnce,
        ));
        if let Some(offer) = offer {
            options.push(PermissionOption::new(
                REJECT_ALWAYS_OPTION_ID,
                format!("Always deny {}", offer.scope),
                PermissionOptionKind::RejectAlways,
            ));
        }
        options
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
        offer: Option<&StandingOffer>,
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
            Self::options(offer),
        )
    }

    /// Map an ACP permission outcome to an ADK decision.
    ///
    /// A selected allow option approves and a selected reject option denies; an
    /// "always" option does so and asks to remember the answer, but only when the
    /// request `offered_always`. A cancelled request denies (Requirement 7.4). An
    /// unrecognized selected option id is treated as a denial so an unauthorized call
    /// never executes.
    fn answer_for(outcome: &RequestPermissionOutcome, offered_always: bool) -> PermissionAnswer {
        let deny = PermissionAnswer { decision: ToolConfirmationDecision::Deny, always: false };
        match outcome {
            RequestPermissionOutcome::Selected(selected) => {
                match (selected.option_id.to_string().as_str(), offered_always) {
                    (ALLOW_OPTION_ID, _) => PermissionAnswer {
                        decision: ToolConfirmationDecision::Approve,
                        always: false,
                    },
                    (ALLOW_ALWAYS_OPTION_ID, true) => PermissionAnswer {
                        decision: ToolConfirmationDecision::Approve,
                        always: true,
                    },
                    (REJECT_ALWAYS_OPTION_ID, true) => {
                        PermissionAnswer { decision: ToolConfirmationDecision::Deny, always: true }
                    }
                    _ => deny,
                }
            }
            // Cancellation and any future outcome variant map to deny so the
            // affected tool call is not executed.
            _ => deny,
        }
    }

    /// Send a `session/request_permission` request to the client, await the
    /// outcome, and return the mapped answer.
    ///
    /// `offer` adds the "always" options (spec 3.3 §10 A1).
    ///
    /// # Errors
    ///
    /// Returns [`AcpServerError::Transport`] if the nested request fails or the
    /// client connection drops before responding.
    pub(crate) async fn request(
        connection: &ConnectionTo<Client>,
        session_id: &SessionId,
        request: &ToolConfirmationRequest,
        offer: Option<&StandingOffer>,
    ) -> Result<PermissionAnswer, AcpServerError> {
        let acp_request = Self::build_request(session_id, request, offer);
        let response = connection
            .send_request(acp_request)
            .block_task()
            .await
            .map_err(|error| AcpServerError::Transport(error.to_string()))?;
        Ok(Self::answer_for(&response.outcome, offer.is_some()))
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
            origin: None,
            acting: None,
        }
    }

    fn selected(id: &str) -> RequestPermissionOutcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(PermissionOptionId::new(
            id,
        )))
    }

    fn once(decision: ToolConfirmationDecision) -> PermissionAnswer {
        PermissionAnswer { decision, always: false }
    }

    #[test]
    fn request_carries_function_call_id_kind_and_args() {
        let session = SessionId::new("session-1");
        let request = PermissionBridge::build_request(&session, &confirmation(), None);
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
        let request = PermissionBridge::build_request(&session, &confirmation, None);
        assert_eq!(request.tool_call.tool_call_id.to_string(), "delete_file-call");
    }

    #[test]
    fn options_use_the_surface_words_and_offer_always_only_with_an_offer() {
        let options =
            |offer: Option<&StandingOffer>| -> Vec<(String, String, PermissionOptionKind)> {
                PermissionBridge::options(offer)
                    .into_iter()
                    .map(|option| (option.option_id.to_string(), option.name, option.kind))
                    .collect()
            };
        assert_eq!(
            options(None),
            [
                (ALLOW_OPTION_ID.into(), "Allow once".into(), PermissionOptionKind::AllowOnce),
                (REJECT_OPTION_ID.into(), "Deny".into(), PermissionOptionKind::RejectOnce),
            ]
        );
        let offer = StandingOffer::new("for this command (bash: cargo test …)");
        assert_eq!(
            options(Some(&offer)),
            [
                (ALLOW_OPTION_ID.into(), "Allow once".into(), PermissionOptionKind::AllowOnce),
                (
                    ALLOW_ALWAYS_OPTION_ID.into(),
                    "Always allow for this command (bash: cargo test …)".into(),
                    PermissionOptionKind::AllowAlways
                ),
                (REJECT_OPTION_ID.into(), "Deny".into(), PermissionOptionKind::RejectOnce),
                (
                    REJECT_ALWAYS_OPTION_ID.into(),
                    "Always deny for this command (bash: cargo test …)".into(),
                    PermissionOptionKind::RejectAlways
                ),
            ]
        );
    }

    #[test]
    fn allow_selection_maps_to_approve() {
        assert_eq!(
            PermissionBridge::answer_for(&selected(ALLOW_OPTION_ID), false),
            once(ToolConfirmationDecision::Approve)
        );
    }

    #[test]
    fn reject_selection_maps_to_deny() {
        assert_eq!(
            PermissionBridge::answer_for(&selected(REJECT_OPTION_ID), true),
            once(ToolConfirmationDecision::Deny)
        );
    }

    #[test]
    fn always_selections_are_remembered_only_when_offered() {
        assert_eq!(
            PermissionBridge::answer_for(&selected(ALLOW_ALWAYS_OPTION_ID), true),
            PermissionAnswer { decision: ToolConfirmationDecision::Approve, always: true }
        );
        assert_eq!(
            PermissionBridge::answer_for(&selected(REJECT_ALWAYS_OPTION_ID), true),
            PermissionAnswer { decision: ToolConfirmationDecision::Deny, always: true }
        );
        // A client answering "always" to a call that may never be remembered is denied.
        for id in [ALLOW_ALWAYS_OPTION_ID, REJECT_ALWAYS_OPTION_ID] {
            assert_eq!(
                PermissionBridge::answer_for(&selected(id), false),
                once(ToolConfirmationDecision::Deny)
            );
        }
    }

    #[test]
    fn cancellation_maps_to_deny() {
        assert_eq!(
            PermissionBridge::answer_for(&RequestPermissionOutcome::Cancelled, true),
            once(ToolConfirmationDecision::Deny)
        );
    }

    #[test]
    fn unknown_selection_maps_to_deny() {
        assert_eq!(
            PermissionBridge::answer_for(&selected("made-up-id"), true),
            once(ToolConfirmationDecision::Deny)
        );
    }
}
