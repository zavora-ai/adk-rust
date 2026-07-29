use crate::runtime::VerificationOutcome;
use crate::runtime::binding::{
    validate_envelope_freshness, validate_lease, validate_receipt, validate_reservation,
};
use crate::{
    ActionEnvelope, ActionPreview, ComputerUseError, ComputerUseRuntime, ControlLease,
    ExecutionReceipt, SessionDeletionResult, SessionFollowUp, SessionFollowUpPage,
    TargetReservation,
};
use adk_tool::McpToolset;
use async_trait::async_trait;
use rmcp::{RoleClient, service::Service};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::Instrument;

/// Static configuration binding a [`ComputerUseMcpRuntime`] to one authenticated session.
#[derive(Debug, Clone)]
pub struct ComputerUseMcpConfig {
    /// The runtime session identifier this adapter operates within.
    pub session_id: String,
    /// The principal the MCP server authenticated; every payload is checked against it.
    pub expected_principal_id: String,
    /// The tool used when discovering execution capabilities.
    pub capability_tool: String,
    /// Optional bundle/app identifier scoping observation and capability queries.
    pub target_app: Option<String>,
    /// Optional window identifier scoping the semantic observation.
    pub target_window_id: Option<u64>,
    /// Trace identifiers correlating ADK spans with runtime session events.
    pub correlation: TraceCorrelation,
}

/// ADK ↔ runtime trace correlation identifiers attached to every MCP span.
#[derive(Debug, Clone, Default)]
pub struct TraceCorrelation {
    /// The ADK session identifier, if the graph runs inside a session.
    pub adk_session_id: Option<String>,
    /// The ADK invocation identifier for the current run.
    pub adk_invocation_id: Option<String>,
    /// The `adk-graph` thread identifier for durable resume correlation.
    pub adk_graph_thread_id: Option<String>,
    /// An external distributed-trace identifier, if one is propagated.
    pub trace_id: Option<String>,
}

/// [`ComputerUseRuntime`] implementation backed by a live `computer-use-mcp` server.
///
/// The adapter is bound to a single authenticated session via
/// [`ComputerUseMcpConfig`]. It forwards every observation through the governed
/// `execute_action` shadow facade and re-checks the principal/session identity
/// on each response so that graph or model state can never substitute a different
/// authority.
///
/// # Errors
///
/// Methods return [`ComputerUseError`]: transport faults become
/// [`ComputerUseError::Mcp`], decode failures become [`ComputerUseError::Decode`],
/// and identity re-checks become [`ComputerUseError::IdentityMismatch`].
pub struct ComputerUseMcpRuntime<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    toolset: Arc<McpToolset<S>>,
    config: ComputerUseMcpConfig,
    proposed: Mutex<HashMap<String, ProposedAction>>,
}

#[derive(Clone)]
struct ProposedAction {
    arguments: Map<String, Value>,
    preview: ActionPreview,
}

impl<S> ComputerUseMcpRuntime<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    /// Create an adapter bound to the supplied MCP toolset and session config.
    pub fn new(toolset: Arc<McpToolset<S>>, config: ComputerUseMcpConfig) -> Self {
        Self { toolset, config, proposed: Mutex::new(HashMap::new()) }
    }

    /// Run a read-only desktop tool through the governed shadow facade.
    ///
    /// Useful for bootstrapping exact target evidence before graph execution.
    ///
    /// # Errors
    ///
    /// Returns [`ComputerUseError::Mcp`] if the underlying MCP call fails.
    pub async fn observe_tool(
        &self,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ComputerUseError> {
        self.observe_through_shadow(tool, arguments).await
    }

    /// Permanently remove this adapter's terminal runtime session through the
    /// authenticated MCP principal. The server, not graph state, owns identity.
    ///
    /// # Errors
    ///
    /// Returns [`ComputerUseError::Mcp`] or [`ComputerUseError::Decode`] if the
    /// call fails or the deletion result cannot be decoded.
    pub async fn delete_terminal_session(&self) -> Result<SessionDeletionResult, ComputerUseError> {
        let value = output(
            self.call(
                "delete_session",
                object(json!({
                    "session_id": self.config.session_id,
                    "confirm": true,
                }))?,
            )
            .await?,
        );
        Ok(serde_json::from_value(value.get("deletion").cloned().unwrap_or(value))?)
    }

    /// Explicitly prune caller-owned terminal sessions before the supplied
    /// timestamp. The MCP server enforces principal ownership and the limit.
    ///
    /// # Errors
    ///
    /// Returns [`ComputerUseError::Mcp`] or [`ComputerUseError::Decode`] on
    /// transport or decoding failure.
    pub async fn prune_terminal_sessions(
        &self,
        older_than: &str,
        limit: u32,
    ) -> Result<Vec<SessionDeletionResult>, ComputerUseError> {
        let value = output(
            self.call(
                "prune_sessions",
                object(json!({
                    "older_than": older_than,
                    "limit": limit,
                    "confirm": true,
                }))?,
            )
            .await?,
        );
        Ok(serde_json::from_value(value.get("deletions").cloned().unwrap_or_else(|| json!([])))?)
    }

    /// Consume remote/local steering with a monotonic cursor. The server binds
    /// both the session and returned instructions to the authenticated principal.
    ///
    /// # Errors
    ///
    /// Returns [`ComputerUseError::InvalidRequest`] if `limit` is outside
    /// `1..=1000`, [`ComputerUseError::IdentityMismatch`] if any returned
    /// follow-up does not match the authenticated context, or a transport/decode
    /// error otherwise.
    pub async fn get_follow_ups(
        &self,
        after_sequence: u64,
        limit: u32,
    ) -> Result<SessionFollowUpPage, ComputerUseError> {
        if limit == 0 || limit > 1000 {
            return Err(ComputerUseError::InvalidRequest(
                "follow-up limit must be between 1 and 1000".into(),
            ));
        }
        let value = output(
            self.call(
                "get_follow_ups",
                object(json!({
                    "session_id": self.config.session_id,
                    "after_sequence": after_sequence,
                    "limit": limit,
                }))?,
            )
            .await?,
        );
        let page: SessionFollowUpPage = serde_json::from_value(value)?;
        if page.follow_ups.iter().any(|item| {
            item.session_id != self.config.session_id
                || item.principal_id != self.config.expected_principal_id
        }) {
            return Err(ComputerUseError::IdentityMismatch(
                "follow-up identity does not match authenticated ADK context".into(),
            ));
        }
        Ok(page)
    }

    /// Submit steering through the same principal-bound MCP boundary. Hosts
    /// normally call this; exposing it here supports supervisor/graph tests.
    ///
    /// # Errors
    ///
    /// Returns [`ComputerUseError::IdentityMismatch`] if the returned follow-up
    /// does not match the authenticated context, or a transport/decode error
    /// otherwise.
    pub async fn submit_follow_up(
        &self,
        instruction: &str,
    ) -> Result<SessionFollowUp, ComputerUseError> {
        let value = output(
            self.call(
                "submit_follow_up",
                object(json!({
                    "session_id": self.config.session_id,
                    "instruction": instruction,
                }))?,
            )
            .await?,
        );
        let follow_up: SessionFollowUp =
            serde_json::from_value(value.get("follow_up").cloned().unwrap_or(value))?;
        if follow_up.session_id != self.config.session_id
            || follow_up.principal_id != self.config.expected_principal_id
        {
            return Err(ComputerUseError::IdentityMismatch(
                "follow-up identity does not match authenticated ADK context".into(),
            ));
        }
        Ok(follow_up)
    }

    async fn call(
        &self,
        name: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, ComputerUseError> {
        let span = tracing::info_span!(
            "computer_use.mcp",
            mcp.tool = name,
            runtime.session_id = %self.config.session_id,
            adk.session_id = ?self.config.correlation.adk_session_id,
            adk.invocation_id = ?self.config.correlation.adk_invocation_id,
            adk.graph_thread_id = ?self.config.correlation.adk_graph_thread_id,
            trace_id = ?self.config.correlation.trace_id,
        );
        self.toolset
            .call_tool_value(name, arguments)
            .instrument(span)
            .await
            .map_err(|error| ComputerUseError::Mcp(error.to_string()))
    }

    async fn observe_through_shadow(
        &self,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ComputerUseError> {
        self.call(
            "execute_action",
            object(json!({
                "session_id": self.config.session_id,
                "action_id": uuid::Uuid::new_v4().to_string(),
                "tool": tool,
                "arguments": arguments,
                "mode": "shadow",
                "data_labels": ["private"],
            }))?,
        )
        .await
    }
}

fn output(value: Value) -> Value {
    value
        .get("response")
        .and_then(|value| value.get("output"))
        .or_else(|| value.get("output"))
        .cloned()
        .unwrap_or(value)
}

fn object(value: Value) -> Result<Map<String, Value>, ComputerUseError> {
    let mut value = value.as_object().cloned().ok_or_else(|| {
        ComputerUseError::InvalidRequest("proposed action must be an object".into())
    })?;
    value.retain(|_, entry| !entry.is_null());
    Ok(value)
}

/// Decides what a committed receipt proves about its declared postcondition.
///
/// Looks for a `verification` object on the receipt result carrying the observed digest, and
/// requires it to match the postcondition's expected digest. Anything less is reported as
/// committed-but-unverified rather than as verification, so the absence of evidence is never
/// mistaken for evidence of success.
fn evaluate_postcondition_evidence(
    receipt: &ExecutionReceipt,
    postcondition: &crate::ActionPostcondition,
) -> VerificationOutcome {
    let expected = expected_digest(postcondition);

    let Some(verification) = receipt.result.as_ref().and_then(|result| result.get("verification"))
    else {
        return VerificationOutcome::CommittedUnverified {
            reason: "the receipt carried no verification evidence for the declared postcondition"
                .to_string(),
        };
    };

    // An explicit negative observation is a failure, not merely missing evidence.
    if verification.get("satisfied").and_then(Value::as_bool) == Some(false) {
        return VerificationOutcome::Failed {
            reason: "the runtime reported the postcondition was not satisfied".to_string(),
        };
    }

    let observed = verification.get("observedDigest").and_then(Value::as_str);

    match (expected, observed) {
        (Some(expected), Some(observed)) if expected == observed => VerificationOutcome::Verified,
        (Some(expected), Some(observed)) => VerificationOutcome::Failed {
            reason: format!(
                "observed digest {observed:?} does not match the expected postcondition digest \
                 {expected:?}"
            ),
        },
        (Some(_), None) => VerificationOutcome::CommittedUnverified {
            reason: "verification evidence carried no observed digest to compare".to_string(),
        },
        // A digest-free postcondition (existence only) is satisfied by an explicit
        // affirmative observation.
        (None, _) => match verification.get("satisfied").and_then(Value::as_bool) {
            Some(true) => VerificationOutcome::Verified,
            _ => VerificationOutcome::CommittedUnverified {
                reason: "the postcondition declares no digest and the evidence made no explicit \
                         satisfied claim"
                    .to_string(),
            },
        },
    }
}

/// The digest a postcondition expects, where it declares one.
fn expected_digest(postcondition: &crate::ActionPostcondition) -> Option<&str> {
    use crate::ActionPostcondition;
    match postcondition {
        ActionPostcondition::UiElement { value_digest, .. } => value_digest.as_deref(),
        ActionPostcondition::Filesystem { content_digest, .. } => content_digest.as_deref(),
        ActionPostcondition::Registry { value_digest, .. } => value_digest.as_deref(),
        _ => None,
    }
}

#[async_trait]
impl<S> ComputerUseRuntime for ComputerUseMcpRuntime<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    async fn discover_capabilities(&self) -> Result<Value, ComputerUseError> {
        self.call(
            "get_execution_capabilities",
            object(json!({
                "tool": self.config.capability_tool,
                "app_id": self.config.target_app,
            }))?,
        )
        .await
    }

    async fn observe_visual(&self) -> Result<Value, ComputerUseError> {
        self.observe_through_shadow(
            "snapshot",
            json!({
                "use_vision": true,
                "use_annotation": true,
                "target_app": self.config.target_app,
            }),
        )
        .await
    }

    async fn observe_semantic(&self) -> Result<Value, ComputerUseError> {
        let (name, args) = match self.config.target_window_id {
            Some(window_id) => ("get_ui_tree", json!({ "window_id": window_id })),
            None => ("list_windows", json!({ "bundle_id": self.config.target_app })),
        };
        self.observe_through_shadow(name, args).await
    }

    async fn preview_action(
        &self,
        proposed_action: Value,
    ) -> Result<ActionPreview, ComputerUseError> {
        let mut args = object(proposed_action)?;
        args.insert("session_id".into(), json!(self.config.session_id));
        let preview: ActionPreview =
            serde_json::from_value(output(self.call("preview_action", args.clone()).await?))?;
        if preview.envelope.session_id != self.config.session_id {
            return Err(ComputerUseError::IdentityMismatch(
                "preview returned a different session identity".into(),
            ));
        }
        if preview.envelope.principal_id != self.config.expected_principal_id {
            return Err(ComputerUseError::IdentityMismatch(
                "preview principal does not match authenticated ADK identity".into(),
            ));
        }
        self.proposed.lock().await.insert(
            preview.envelope.action_id.clone(),
            ProposedAction { arguments: args, preview: preview.clone() },
        );
        Ok(preview)
    }

    async fn acquire_lease(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<ControlLease, ComputerUseError> {
        let kind = if envelope.requested_mode == crate::ExecutionMode::Foreground {
            "exclusive"
        } else {
            "cooperative"
        };
        let value = output(
            self.call(
                "acquire_control_lease",
                object(json!({
                    "session_id": envelope.session_id,
                    "agent_id": envelope.agent_id,
                    "kind": kind,
                    "mode": envelope.requested_mode,
                    "ttl_ms": 30_000,
                    "action_budget": 1,
                    "app_ids": envelope.target.as_ref().map(|target| vec![target.app_id.clone()]),
                    "window_ids": envelope.target.as_ref().and_then(|target| target.window_id.clone()).map(|id| vec![id]),
                }))?,
            )
            .await?,
        );
        let lease: ControlLease =
            serde_json::from_value(value.get("lease").cloned().unwrap_or(value))?;
        // Deserialization proves shape, not provenance: a well-formed lease for another
        // session or principal parses cleanly. Bind it before it reaches graph state.
        validate_lease(&lease, envelope)?;
        Ok(lease)
    }

    async fn reserve_target(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<Option<TargetReservation>, ComputerUseError> {
        let Some(target) = envelope.target.as_ref() else {
            return Ok(None);
        };
        let value = output(
            self.call(
                "reserve_target",
                object(json!({
                    "session_id": envelope.session_id,
                    "intent_id": envelope.action_id,
                    "execution_group_id": envelope.execution_group_id,
                    "agent_id": envelope.agent_id,
                    "app_id": target.app_id,
                    "window_id": target.window_id,
                    "ttl_ms": 30_000,
                }))?,
            )
            .await?,
        );
        let reservation: TargetReservation =
            serde_json::from_value(value.get("reservation").cloned().unwrap_or(value))?;
        validate_reservation(&reservation, envelope)?;
        Ok(Some(reservation))
    }

    async fn release_target(
        &self,
        reservation: &TargetReservation,
    ) -> Result<(), ComputerUseError> {
        self.call(
            "release_target_reservation",
            object(json!({
                "session_id": reservation.session_id,
                "reservation_id": reservation.reservation_id,
            }))?,
        )
        .await?;
        Ok(())
    }

    async fn execute_action(
        &self,
        envelope: &ActionEnvelope,
        lease: &ControlLease,
        approval_grant_id: Option<&str>,
    ) -> Result<ExecutionReceipt, ComputerUseError> {
        validate_envelope_freshness(envelope)?;
        validate_lease(lease, envelope)?;
        let proposed =
            self.proposed.lock().await.get(&envelope.action_id).cloned().ok_or_else(|| {
                ComputerUseError::Runtime("missing exact proposed action for execution".into())
            })?;
        if proposed.preview.envelope != *envelope {
            return Err(ComputerUseError::IdentityMismatch(format!(
                "action envelope {} changed after preview; exact preview binding is required",
                envelope.action_id
            )));
        }
        let mut args = proposed.arguments;
        args.insert("action_id".into(), json!(envelope.action_id));
        args.insert("lease_id".into(), json!(lease.lease_id));
        if let Some(grant) = approval_grant_id {
            args.insert("approval_grant_id".into(), json!(grant));
        }
        let value = output(self.call("execute_action", args).await?);
        let receipt: ExecutionReceipt =
            serde_json::from_value(value.get("receipt").cloned().unwrap_or(value))?;
        tracing::info!(
            runtime.session_id = %receipt.session_id,
            runtime.action_id = %receipt.action_id,
            runtime.receipt_id = %receipt.receipt_id,
            runtime.action_digest = %receipt.action_digest,
            "computer-use action receipt"
        );
        // The digest covers the envelope, so a receipt carrying a different one describes
        // different work even when the identifiers line up.
        validate_receipt(&receipt, envelope, &envelope.args_digest)?;
        Ok(receipt)
    }

    async fn verify(
        &self,
        receipt: &ExecutionReceipt,
        postcondition: Option<&crate::ActionPostcondition>,
    ) -> Result<VerificationOutcome, ComputerUseError> {
        // A committed receipt says the runtime accepted and performed the action. It does not
        // say the intended effect occurred. Returning `status == Committed` as "verified"
        // reported a committed-but-ineffective action as completed.
        if receipt.status != crate::ReceiptStatus::Committed {
            return Ok(VerificationOutcome::Failed {
                reason: format!("receipt status is {:?}, not committed", receipt.status),
            });
        }

        let Some(postcondition) = postcondition else {
            return Ok(VerificationOutcome::CommittedUnverified {
                reason: "the action declared no postcondition, so there is nothing to verify"
                    .to_string(),
            });
        };

        // `computer-use-mcp` may verify before issuing a receipt, but that is its contract,
        // not something this adapter can assume. Evidence has to be present and bound to the
        // postcondition to count.
        Ok(evaluate_postcondition_evidence(receipt, postcondition))
    }

    async fn pause_session(&self, session_id: &str, reason: &str) -> Result<(), ComputerUseError> {
        self.call("pause_session", object(json!({ "session_id": session_id, "reason": reason }))?)
            .await?;
        Ok(())
    }

    async fn stop_session(&self, session_id: &str, reason: &str) -> Result<(), ComputerUseError> {
        self.call("stop_session", object(json!({ "session_id": session_id, "reason": reason }))?)
            .await?;
        Ok(())
    }

    async fn emergency_stop(&self, reason: &str) -> Result<(), ComputerUseError> {
        self.call("emergency_stop", object(json!({ "reason": reason }))?).await?;
        Ok(())
    }
}
