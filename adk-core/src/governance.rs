//! Governed tool execution.
//!
//! Every agent runs a tool call through [`authorize_tool_call`] after all plugin and
//! callback rewrites and immediately before `Tool::execute`. The order is fixed:
//!
//! 1. The org kill switch ([`GovernanceControl`]) — a frozen run ends with an error.
//! 2. The run's [`ToolPolicy`](crate::ToolPolicy), on the final arguments.
//! 3. The agent's tool screen (its guardrails), on the final arguments. A screen that
//!    rewrites the arguments has the rewrite checked against the policy again.
//! 4. Confirmation, when the agent or the policy asks for it. A decision binds to the
//!    call's fingerprint — its tool name and canonical arguments — so an argument
//!    rewrite after approval needs a new approval, and an approval given in one run
//!    matches the same call re-issued under a new call ID in the next.
//!
//! Approval decisions come from, in order: a decision keyed by call ID
//! ([`RunConfig::tool_confirmation_decisions`]), a decision keyed by fingerprint
//! ([`RunConfig::tool_approvals`]), the run's [`ApprovalStore`], and the live
//! [`ToolConfirmationHandler`](crate::ToolConfirmationHandler), which is bounded by
//! [`RunConfig::tool_confirmation_timeout`] and denies when it expires.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AdkError, ErrorCategory, ErrorComponent, InvocationContext, PolicyDecision, Result, RunConfig,
    Tool, ToolConfirmationDecision, ToolConfirmationRequest, ToolPolicyRequest,
};

/// How long a [`ToolConfirmationHandler`](crate::ToolConfirmationHandler) may take to
/// decide before the call is denied.
pub const DEFAULT_TOOL_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Kill switch
// ---------------------------------------------------------------------------

/// An organisation-wide kill switch for agent execution.
///
/// Clones share one flag. While frozen, a run fails at its start, an agent stops
/// before its next model call, and no tool executes: each check ends the run with an
/// error carrying the freeze reason.
///
/// Attach a control to a runner with its builder's `governance`, or to a single run
/// with [`RunConfigBuilder::governance`](crate::RunConfigBuilder::governance).
///
/// # Example
///
/// ```rust
/// use adk_core::GovernanceControl;
///
/// let control = GovernanceControl::new();
/// assert!(control.check().is_ok());
///
/// control.freeze("incident 4012: unexpected payouts");
/// let error = control.check().unwrap_err();
/// assert!(error.to_string().contains("incident 4012"));
///
/// control.unfreeze();
/// assert!(!control.is_frozen());
/// ```
#[derive(Clone, Default)]
pub struct GovernanceControl {
    inner: Arc<ControlState>,
}

#[derive(Default)]
struct ControlState {
    frozen: AtomicBool,
    reason: std::sync::RwLock<Option<String>>,
}

impl std::fmt::Debug for GovernanceControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernanceControl")
            .field("frozen", &self.is_frozen())
            .field("reason", &self.reason())
            .finish()
    }
}

impl GovernanceControl {
    /// Creates an unfrozen control.
    pub fn new() -> Self {
        Self::default()
    }

    /// Freezes execution everywhere this control is attached.
    pub fn freeze(&self, reason: impl Into<String>) {
        let reason = reason.into();
        tracing::warn!(governance.reason = %reason, "execution frozen");
        *self.inner.reason.write().unwrap_or_else(|e| e.into_inner()) = Some(reason);
        self.inner.frozen.store(true, Ordering::SeqCst);
    }

    /// Lifts a freeze.
    pub fn unfreeze(&self) {
        self.inner.frozen.store(false, Ordering::SeqCst);
        *self.inner.reason.write().unwrap_or_else(|e| e.into_inner()) = None;
        tracing::info!("execution unfrozen");
    }

    /// Whether execution is frozen.
    pub fn is_frozen(&self) -> bool {
        self.inner.frozen.load(Ordering::SeqCst)
    }

    /// The reason given for the current freeze, if frozen.
    pub fn reason(&self) -> Option<String> {
        if !self.is_frozen() {
            return None;
        }
        self.inner.reason.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether `other` shares this control's flag.
    pub fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Returns an error when frozen.
    ///
    /// # Errors
    ///
    /// Returns a non-retryable [`ErrorCategory::Forbidden`] error with code
    /// `governance.frozen` while the control is frozen.
    pub fn check(&self) -> Result<()> {
        if !self.is_frozen() {
            return Ok(());
        }
        let reason = self.reason().unwrap_or_else(|| "no reason given".to_string());
        Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::Forbidden,
            "governance.frozen",
            format!(
                "execution is frozen by the governance kill switch ({reason}); no model call or \
                 tool runs until an operator unfreezes it"
            ),
        ))
    }
}

// ---------------------------------------------------------------------------
// Durable approvals
// ---------------------------------------------------------------------------

/// A decision for one tool-call fingerprint, with an optional expiry.
///
/// # Example
///
/// ```rust
/// use adk_core::{ToolApproval, ToolConfirmationDecision};
/// use std::time::Duration;
///
/// let approval = ToolApproval::approve().expires_in(Duration::from_secs(600));
/// assert_eq!(approval.decision, ToolConfirmationDecision::Approve);
/// assert!(approval.is_live(chrono::Utc::now()));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApproval {
    /// The decision.
    pub decision: ToolConfirmationDecision,
    /// When the decision stops applying. `None` never expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl ToolApproval {
    /// An approval that does not expire.
    pub fn approve() -> Self {
        Self { decision: ToolConfirmationDecision::Approve, expires_at: None }
    }

    /// A denial that does not expire.
    pub fn deny() -> Self {
        Self { decision: ToolConfirmationDecision::Deny, expires_at: None }
    }

    /// Sets the instant the decision stops applying.
    #[must_use]
    pub fn expires_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Sets the decision to stop applying `ttl` from now.
    #[must_use]
    pub fn expires_in(self, ttl: Duration) -> Self {
        let ttl = chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::MAX);
        self.expires_at(Utc::now().checked_add_signed(ttl).unwrap_or(DateTime::<Utc>::MAX_UTC))
    }

    /// Whether the decision still applies at `now`.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|at| now < at)
    }
}

/// The identity an approval belongs to.
///
/// Approvals never cross sessions: a decision recorded for one session does not
/// authorize the same call in another.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalScope {
    /// Application name.
    pub app_name: String,
    /// User ID.
    pub user_id: String,
    /// Session ID.
    pub session_id: String,
}

impl ApprovalScope {
    /// Creates a scope.
    pub fn new(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self { app_name: app_name.into(), user_id: user_id.into(), session_id: session_id.into() }
    }
}

/// A call waiting for a decision, as recorded in an [`ApprovalStore`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    /// The call's fingerprint, the key a decision is recorded under.
    pub fingerprint: String,
    /// The request shown to the approver.
    pub request: ToolConfirmationRequest,
    /// When the call was first held for approval.
    pub requested_at: DateTime<Utc>,
}

/// Holds pending confirmation requests and decisions between runs.
///
/// When a run holds a call for approval, the governed path records it here. An
/// approver lists [`pending`](Self::pending) requests, records a decision with
/// [`decide`](Self::decide), and the next run that makes the same call — same tool,
/// same canonical arguments, any call ID — takes the decision. Taking consumes it, so
/// one approval authorizes one execution.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalScope, ApprovalStore, InMemoryApprovalStore, ToolApproval,
///     ToolConfirmationDecision, ToolConfirmationRequest};
/// use serde_json::json;
///
/// # futures::executor::block_on(async {
/// let store = InMemoryApprovalStore::new();
/// let scope = ApprovalScope::new("app", "user-1", "session-1");
/// let request = ToolConfirmationRequest {
///     tool_name: "transfer".into(),
///     function_call_id: Some("call-1".into()),
///     args: json!({ "amount": 40 }),
/// };
///
/// store.record_pending(&scope, &request).await?;
/// let pending = store.pending(&scope).await?;
/// store.decide(&scope, &pending[0].fingerprint, ToolApproval::approve()).await?;
///
/// assert_eq!(
///     store.take_decision(&scope, &request.fingerprint()).await?,
///     Some(ToolConfirmationDecision::Approve)
/// );
/// assert_eq!(store.take_decision(&scope, &request.fingerprint()).await?, None);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[async_trait]
pub trait ApprovalStore: std::fmt::Debug + Send + Sync {
    /// Records a call waiting for a decision. Recording the same fingerprint again
    /// keeps the original request time.
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Result<()>;

    /// Lists the calls in `scope` waiting for a decision, oldest first.
    async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>>;

    /// Records a decision for the call with `fingerprint` and clears its pending entry.
    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> Result<()>;

    /// Takes the live decision for `fingerprint`, consuming it.
    ///
    /// An expired decision is discarded and reported as `None`.
    async fn take_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolConfirmationDecision>>;
}

#[derive(Debug, Default)]
struct ScopeApprovals {
    pending: HashMap<String, PendingApproval>,
    decisions: HashMap<String, ToolApproval>,
}

/// An [`ApprovalStore`] held in process memory.
///
/// Pending requests and decisions survive between runs of one process; they do not
/// survive a restart.
#[derive(Debug, Default)]
pub struct InMemoryApprovalStore {
    scopes: tokio::sync::RwLock<HashMap<ApprovalScope, ScopeApprovals>>,
}

impl InMemoryApprovalStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ApprovalStore for InMemoryApprovalStore {
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        let fingerprint = request.fingerprint();
        let mut scopes = self.scopes.write().await;
        scopes.entry(scope.clone()).or_default().pending.entry(fingerprint.clone()).or_insert_with(
            || PendingApproval { fingerprint, request: request.clone(), requested_at: Utc::now() },
        );
        Ok(())
    }

    async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>> {
        let scopes = self.scopes.read().await;
        let mut pending: Vec<PendingApproval> = scopes
            .get(scope)
            .map(|entries| entries.pending.values().cloned().collect())
            .unwrap_or_default();
        pending.sort_by_key(|entry| entry.requested_at);
        Ok(pending)
    }

    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> Result<()> {
        let mut scopes = self.scopes.write().await;
        let entries = scopes.entry(scope.clone()).or_default();
        entries.pending.remove(fingerprint);
        entries.decisions.insert(fingerprint.to_string(), approval);
        Ok(())
    }

    async fn take_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolConfirmationDecision>> {
        let mut scopes = self.scopes.write().await;
        let Some(entries) = scopes.get_mut(scope) else {
            return Ok(None);
        };
        Ok(entries
            .decisions
            .remove(fingerprint)
            .filter(|approval| approval.is_live(Utc::now()))
            .map(|approval| approval.decision))
    }
}

// ---------------------------------------------------------------------------
// The governed path
// ---------------------------------------------------------------------------

/// Screens a tool call's final arguments, as an agent's tool guardrails do.
///
/// # Example
///
/// ```rust
/// use adk_core::{ToolCallScreen, async_trait};
/// use serde_json::Value;
///
/// struct NoRoot;
///
/// #[async_trait]
/// impl ToolCallScreen for NoRoot {
///     async fn screen(&self, _tool: &str, args: &Value) -> Result<Value, String> {
///         if args["path"] == "/" { Err("refusing to touch /".into()) } else { Ok(args.clone()) }
///     }
/// }
/// ```
#[async_trait]
pub trait ToolCallScreen: Send + Sync {
    /// Returns the arguments to continue with, possibly rewritten, or the reason the
    /// call is refused.
    async fn screen(&self, tool_name: &str, args: &Value) -> std::result::Result<Value, String>;
}

/// One tool call presented to [`authorize_tool_call`].
///
/// # Example
///
/// ```rust
/// use adk_core::GovernedCall;
/// use serde_json::json;
///
/// let call = GovernedCall::new("delete_file", "call-1", json!({ "path": "/tmp/x" }))
///     .requiring_confirmation(true);
/// assert!(call.requires_confirmation);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct GovernedCall {
    /// Name of the tool.
    pub tool_name: String,
    /// The call's function call ID.
    pub function_call_id: String,
    /// Final arguments, after plugin and callback rewrites.
    pub args: Value,
    /// Whether the tool is read-only.
    pub read_only: bool,
    /// Whether the agent itself requires confirmation for this call, independent of
    /// the policy.
    pub requires_confirmation: bool,
}

impl GovernedCall {
    /// A call to `tool_name` that is not read-only and needs no agent-level confirmation.
    pub fn new(
        tool_name: impl Into<String>,
        function_call_id: impl Into<String>,
        args: Value,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            function_call_id: function_call_id.into(),
            args,
            read_only: false,
            requires_confirmation: false,
        }
    }

    /// A call to `tool`, taking its name and read-only flag from the tool.
    pub fn for_tool(tool: &dyn Tool, function_call_id: impl Into<String>, args: Value) -> Self {
        Self::new(tool.name(), function_call_id, args).with_read_only(tool.is_read_only())
    }

    /// Sets the read-only flag.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Sets whether the agent requires confirmation for this call.
    #[must_use]
    pub fn requiring_confirmation(mut self, required: bool) -> Self {
        self.requires_confirmation = required;
        self
    }
}

/// The run-wide inputs [`authorize_tool_call`] reads.
pub struct ToolGate<'a> {
    run_config: &'a RunConfig,
    agent_name: &'a str,
    invocation_id: &'a str,
    scope: ApprovalScope,
    screen: Option<&'a dyn ToolCallScreen>,
    abandon: Option<tokio::sync::watch::Receiver<bool>>,
}

impl<'a> ToolGate<'a> {
    /// A gate over `run_config` for calls made by `agent_name` in `invocation_id`.
    pub fn new(
        run_config: &'a RunConfig,
        agent_name: &'a str,
        invocation_id: &'a str,
        scope: ApprovalScope,
    ) -> Self {
        Self { run_config, agent_name, invocation_id, scope, screen: None, abandon: None }
    }

    /// A gate reading the run config and identity of `ctx`.
    pub fn for_context(ctx: &'a dyn InvocationContext) -> Self {
        Self::new(
            ctx.run_config(),
            ctx.agent_name(),
            ctx.invocation_id(),
            ApprovalScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id()),
        )
    }

    /// Screens every call's final arguments with `screen`.
    #[must_use]
    pub fn with_screen(mut self, screen: &'a dyn ToolCallScreen) -> Self {
        self.screen = Some(screen);
        self
    }

    /// Stops waiting on the confirmation handler once `signal` becomes `true`.
    ///
    /// Agents dispatching a batch of calls use this so that one failed approval
    /// releases the calls still waiting for theirs.
    #[must_use]
    pub fn with_abandon_signal(mut self, signal: tokio::sync::watch::Receiver<bool>) -> Self {
        self.abandon = Some(signal);
        self
    }

    /// The approval scope calls are recorded under.
    pub fn scope(&self) -> &ApprovalScope {
        &self.scope
    }
}

/// What [`authorize_tool_call`] concluded for one call.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolAuthorization {
    /// Execute the tool with `args`.
    Execute {
        /// The arguments to execute with, after any guardrail rewrite.
        args: Value,
        /// The confirmation decision that authorized the call, if one was needed.
        confirmation: Option<ToolConfirmationDecision>,
    },
    /// Do not execute. `reason` is reported to the model as the call's error.
    Refuse {
        /// Why the call was refused.
        reason: String,
        /// The confirmation decision that refused it, if a person or handler did.
        confirmation: Option<ToolConfirmationDecision>,
    },
    /// The call needs a decision that does not exist yet. It has been recorded in the
    /// run's [`ApprovalStore`], if any; the agent reports `reason` and surfaces
    /// `request` so an approver can decide.
    Pending {
        /// The request to show the approver; its arguments are the final ones.
        request: ToolConfirmationRequest,
        /// The message reported to the model as the call's error.
        reason: String,
    },
}

/// Runs one tool call through the governed path.
///
/// Call it with the call's final arguments, after plugin and callback rewrites, and
/// execute the tool only on [`ToolAuthorization::Execute`], with the arguments it
/// returns. See the [module documentation](self) for the order of checks.
///
/// # Errors
///
/// Returns an error, which ends the run, when the run is frozen, when the
/// confirmation handler or the approval store fails, or when the wait for the handler
/// is abandoned through [`ToolGate::with_abandon_signal`].
///
/// # Example
///
/// ```rust
/// use adk_core::{
///     ApprovalScope, DeclarativePolicy, GovernedCall, RunConfig, ToolAuthorization, ToolGate,
/// };
/// use serde_json::json;
/// use std::sync::Arc;
///
/// # futures::executor::block_on(async {
/// let config = RunConfig::builder()
///     .tool_policy(Arc::new(DeclarativePolicy::builder().allow("search").build()))
///     .build();
/// let gate = ToolGate::new(&config, "agent", "inv-1", ApprovalScope::new("app", "u", "s"));
///
/// let search = GovernedCall::new("search", "call-1", json!({ "q": "rust" }));
/// assert!(matches!(
///     adk_core::authorize_tool_call(&gate, search).await?,
///     ToolAuthorization::Execute { .. }
/// ));
///
/// let delete = GovernedCall::new("delete", "call-2", json!({}));
/// assert!(matches!(
///     adk_core::authorize_tool_call(&gate, delete).await?,
///     ToolAuthorization::Refuse { .. }
/// ));
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
pub async fn authorize_tool_call(
    gate: &ToolGate<'_>,
    call: GovernedCall,
) -> Result<ToolAuthorization> {
    let config = gate.run_config;
    config.check_governance()?;
    let GovernedCall { tool_name, function_call_id, mut args, read_only, requires_confirmation } =
        call;

    let mut decision = evaluate_policy(gate, &tool_name, &args, read_only).await;
    if !matches!(decision, PolicyDecision::Deny { .. })
        && let Some(screen) = gate.screen
    {
        match screen.screen(&tool_name, &args).await {
            Ok(screened) => {
                if screened != args {
                    // The policy approved the arguments it saw, not the rewrite.
                    let again = evaluate_policy(gate, &tool_name, &screened, read_only).await;
                    decision = decision.stricter(again);
                    args = screened;
                }
            }
            Err(reason) => return Ok(ToolAuthorization::Refuse { reason, confirmation: None }),
        }
    }

    let approval_reason = match decision {
        PolicyDecision::Allow => None,
        PolicyDecision::RequireApproval { reason } => Some(reason),
        PolicyDecision::Deny { reason } => {
            tracing::info!(tool.name = %tool_name, policy.reason = %reason, "tool call denied by policy");
            return Ok(ToolAuthorization::Refuse {
                reason: format!("Tool '{tool_name}' denied by policy: {reason}"),
                confirmation: None,
            });
        }
    };

    if !requires_confirmation && approval_reason.is_none() {
        return Ok(ToolAuthorization::Execute { args, confirmation: None });
    }

    let request = ToolConfirmationRequest {
        tool_name: tool_name.clone(),
        function_call_id: Some(function_call_id),
        args: args.clone(),
    };
    match resolve_confirmation(gate, &request).await? {
        Some(ToolConfirmationDecision::Approve) => {
            // A freeze can land while a person is deciding.
            config.check_governance()?;
            Ok(ToolAuthorization::Execute {
                args,
                confirmation: Some(ToolConfirmationDecision::Approve),
            })
        }
        Some(ToolConfirmationDecision::Deny) => Ok(ToolAuthorization::Refuse {
            reason: format!("Tool '{tool_name}' execution denied by confirmation policy"),
            confirmation: Some(ToolConfirmationDecision::Deny),
        }),
        None => {
            if let Some(store) = &config.approval_store {
                store.record_pending(&gate.scope, &request).await?;
            }
            let reason = match approval_reason {
                Some(reason) => format!("Tool '{tool_name}' requires confirmation: {reason}"),
                None => format!("Tool '{tool_name}' requires confirmation"),
            };
            Ok(ToolAuthorization::Pending { request, reason })
        }
    }
}

async fn evaluate_policy(
    gate: &ToolGate<'_>,
    tool_name: &str,
    args: &Value,
    read_only: bool,
) -> PolicyDecision {
    let Some(policy) = gate.run_config.tool_policy.as_ref() else {
        return PolicyDecision::Allow;
    };
    let request = ToolPolicyRequest {
        tool_name: tool_name.to_string(),
        args: args.clone(),
        read_only,
        agent_name: gate.agent_name.to_string(),
        app_name: gate.scope.app_name.clone(),
        user_id: gate.scope.user_id.clone(),
        session_id: gate.scope.session_id.clone(),
        invocation_id: gate.invocation_id.to_string(),
    };
    policy.evaluate(&request).await
}

/// Finds the decision for `request`, asking the live handler last.
async fn resolve_confirmation(
    gate: &ToolGate<'_>,
    request: &ToolConfirmationRequest,
) -> Result<Option<ToolConfirmationDecision>> {
    let config = gate.run_config;
    let fingerprint = request.fingerprint();

    if let Some(call_id) = request.function_call_id.as_deref()
        && let Some(decision) = config.tool_confirmation_decisions.get(call_id)
    {
        match config.tool_confirmation_fingerprints.get(call_id) {
            Some(expected) if *expected != fingerprint => tracing::warn!(
                tool.name = %request.tool_name,
                function_call.id = %call_id,
                "confirmation decision does not match this call's arguments, treating as unconfirmed"
            ),
            _ => return Ok(Some(*decision)),
        }
    }

    if let Some(approval) = config.tool_approvals.get(&fingerprint) {
        if approval.is_live(Utc::now()) {
            return Ok(Some(approval.decision));
        }
        tracing::debug!(tool.name = %request.tool_name, "approval for this call has expired");
    }

    if let Some(store) = &config.approval_store
        && let Some(decision) = store.take_decision(&gate.scope, &fingerprint).await?
    {
        return Ok(Some(decision));
    }

    let Some(handler) = config.tool_confirmation_handler.as_ref() else {
        return Ok(None);
    };
    let timeout = config.tool_confirmation_timeout;
    let decide = Box::pin(tokio::time::timeout(timeout, handler.decide(request)));
    let decided = match gate.abandon.clone() {
        Some(mut abandon) => {
            let abandoned = Box::pin(async move {
                if abandon.wait_for(|failed| *failed).await.is_err() {
                    // The batch ended without failing; never abandon.
                    std::future::pending::<()>().await;
                }
            });
            match futures::future::select(abandoned, decide).await {
                futures::future::Either::Left(_) => {
                    return Err(AdkError::tool(format!(
                        "approval for tool '{}' was abandoned because another approval in the \
                         same batch failed",
                        request.tool_name
                    )));
                }
                futures::future::Either::Right((decided, _)) => decided,
            }
        }
        None => decide.await,
    };
    match decided {
        Ok(Ok(decision)) => Ok(Some(decision)),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            tracing::warn!(
                tool.name = %request.tool_name,
                timeout_secs = timeout.as_secs(),
                "confirmation handler did not decide in time, denying the call"
            );
            Ok(Some(ToolConfirmationDecision::Deny))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeclarativePolicy, PolicyRule, ToolConfirmationHandler, tool_call_fingerprint};
    use serde_json::json;

    fn gate(config: &RunConfig) -> ToolGate<'_> {
        ToolGate::new(config, "agent", "inv", ApprovalScope::new("app", "user", "session"))
    }

    #[tokio::test]
    async fn no_policy_allows_and_a_frozen_run_fails() {
        let control = GovernanceControl::new();
        let config = RunConfig::builder().governance(control.clone()).build();
        let call = GovernedCall::new("tool", "c1", json!({}));
        assert_eq!(
            authorize_tool_call(&gate(&config), call.clone()).await.unwrap(),
            ToolAuthorization::Execute { args: json!({}), confirmation: None }
        );

        control.freeze("drill");
        let error = authorize_tool_call(&gate(&config), call).await.unwrap_err();
        assert_eq!(error.code, "governance.frozen");
        assert!(!error.is_retryable());
    }

    #[tokio::test]
    async fn a_screen_rewrite_is_checked_against_the_policy_again() {
        struct Inflate;
        #[async_trait]
        impl ToolCallScreen for Inflate {
            async fn screen(
                &self,
                _tool: &str,
                _args: &Value,
            ) -> std::result::Result<Value, String> {
                Ok(json!({ "amount": 5000 }))
            }
        }
        let policy = DeclarativePolicy::builder()
            .rule(PolicyRule::allow("pay").when(crate::ArgPredicate::at_most("/amount", 100.0)))
            .build();
        let config = RunConfig::builder().tool_policy(Arc::new(policy)).build();
        let screen = Inflate;
        let gate = gate(&config).with_screen(&screen);
        let outcome =
            authorize_tool_call(&gate, GovernedCall::new("pay", "c1", json!({ "amount": 5 })))
                .await
                .unwrap();
        assert!(matches!(outcome, ToolAuthorization::Refuse { .. }), "{outcome:?}");
    }

    #[derive(Debug)]
    struct NeverDecides;

    #[async_trait]
    impl ToolConfirmationHandler for NeverDecides {
        async fn decide(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> Result<ToolConfirmationDecision> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn a_handler_that_never_decides_is_denied_after_the_timeout() {
        let config = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(NeverDecides))
            .tool_confirmation_timeout(Duration::from_millis(20))
            .build();
        let call = GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        let outcome = authorize_tool_call(&gate(&config), call).await.unwrap();
        assert_eq!(
            outcome,
            ToolAuthorization::Refuse {
                reason: "Tool 'tool' execution denied by confirmation policy".into(),
                confirmation: Some(ToolConfirmationDecision::Deny),
            }
        );
    }

    #[tokio::test]
    async fn an_abandon_signal_releases_a_waiting_handler() {
        let config = RunConfig::builder().tool_confirmation_handler(Arc::new(NeverDecides)).build();
        let (failed, signal) = tokio::sync::watch::channel(false);
        let gate = gate(&config).with_abandon_signal(signal);
        let call = GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        let waiting = authorize_tool_call(&gate, call);
        failed.send_replace(true);
        assert!(waiting.await.unwrap_err().to_string().contains("abandoned"));
    }

    #[tokio::test]
    async fn approvals_bind_to_the_fingerprint_and_expire() {
        let args = json!({ "amount": 40 });
        let fingerprint = tool_call_fingerprint("pay", &args);
        let config = RunConfig::builder()
            .tool_approval(fingerprint.clone(), ToolApproval::approve())
            .build();
        let approved = GovernedCall::new("pay", "new-id", args).requiring_confirmation(true);
        assert!(matches!(
            authorize_tool_call(&gate(&config), approved).await.unwrap(),
            ToolAuthorization::Execute {
                confirmation: Some(ToolConfirmationDecision::Approve),
                ..
            }
        ));

        let other = GovernedCall::new("pay", "new-id", json!({ "amount": 41 }))
            .requiring_confirmation(true);
        assert!(matches!(
            authorize_tool_call(&gate(&config), other).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));

        let expired = RunConfig::builder()
            .tool_approval(
                fingerprint,
                ToolApproval::approve().expires_at(Utc::now() - chrono::Duration::seconds(1)),
            )
            .build();
        let call =
            GovernedCall::new("pay", "id", json!({ "amount": 40 })).requiring_confirmation(true);
        assert!(matches!(
            authorize_tool_call(&gate(&expired), call).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
    }

    #[tokio::test]
    async fn a_pending_call_is_recorded_and_a_stored_decision_is_used_once() {
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder().approval_store(store.clone()).build();
        let call =
            || GovernedCall::new("pay", "c1", json!({ "amount": 1 })).requiring_confirmation(true);

        let ToolAuthorization::Pending { request, .. } =
            authorize_tool_call(&gate(&config), call()).await.unwrap()
        else {
            panic!("expected a pending call");
        };
        let scope = ApprovalScope::new("app", "user", "session");
        let pending = store.pending(&scope).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request, request);

        // A decision in another session does not apply here.
        let elsewhere = ApprovalScope::new("app", "user", "other");
        store.decide(&elsewhere, &request.fingerprint(), ToolApproval::approve()).await.unwrap();
        assert!(matches!(
            authorize_tool_call(&gate(&config), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));

        store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await.unwrap();
        assert!(store.pending(&scope).await.unwrap().is_empty());
        assert!(matches!(
            authorize_tool_call(&gate(&config), call()).await.unwrap(),
            ToolAuthorization::Execute { .. }
        ));
        assert!(matches!(
            authorize_tool_call(&gate(&config), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
    }

    #[tokio::test]
    async fn the_policy_can_require_approval() {
        let policy = DeclarativePolicy::builder().require_approval("pay", "money moves").build();
        let config = RunConfig::builder().tool_policy(Arc::new(policy)).build();
        let outcome = authorize_tool_call(&gate(&config), GovernedCall::new("pay", "c", json!({})))
            .await
            .unwrap();
        let ToolAuthorization::Pending { reason, .. } = outcome else {
            panic!("expected a pending call, got {outcome:?}");
        };
        assert_eq!(reason, "Tool 'pay' requires confirmation: money moves");
    }
}
