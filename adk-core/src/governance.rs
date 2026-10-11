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
//!
//! A call held without a decision is answered with a [`confirmation_pending_response`]
//! and stays held across runs. At the start of a later run the agent finds its held
//! calls with [`held_tool_calls`] and, for each one with a decision
//! ([`confirmation_decision_available`]), dispatches the original call again through
//! the same path, so the decision authorizes or refuses it without the model issuing
//! it a second time. The call's real response supersedes the pending one.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AdkError, ErrorCategory, ErrorComponent, Event, InvocationContext, Part, PolicyDecision,
    Result, RunConfig, Tool, ToolConfirmationDecision, ToolConfirmationRequest, ToolEffect,
    ToolPolicyRequest, tool_call_fingerprint,
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

    /// Whether a decision for `fingerprint` may be waiting, without consuming it.
    ///
    /// An agent resuming a held call asks this before it runs the call through the
    /// governed path, which then [takes](Self::take_decision) the decision. A `true`
    /// that turns out to have no live decision holds the call again.
    ///
    /// The default reports a decision whenever `fingerprint` is no longer
    /// [pending](Self::pending), because [`decide`](Self::decide) clears the pending
    /// entry. Override it when the store can answer exactly.
    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        Ok(!self.pending(scope).await?.iter().any(|entry| entry.fingerprint == fingerprint))
    }
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

    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        let scopes = self.scopes.read().await;
        Ok(scopes
            .get(scope)
            .and_then(|entries| entries.decisions.get(fingerprint))
            .is_some_and(|approval| approval.is_live(Utc::now())))
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
    /// What the tool declares about repeating a call ([`Tool::effect`]).
    pub effect: ToolEffect,
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
            effect: ToolEffect::NonIdempotent,
            requires_confirmation: false,
        }
    }

    /// A call to `tool`, taking its name and declared [`Tool::effect`] from the tool.
    pub fn for_tool(tool: &dyn Tool, function_call_id: impl Into<String>, args: Value) -> Self {
        Self::new(tool.name(), function_call_id, args).with_effect(tool.effect())
    }

    /// Sets the effect the tool declares.
    #[must_use]
    pub fn with_effect(mut self, effect: ToolEffect) -> Self {
        self.effect = effect;
        self
    }

    /// Marks the tool as read-only, or as having side effects ([`ToolEffect::NonIdempotent`]).
    #[must_use]
    pub fn with_read_only(self, read_only: bool) -> Self {
        self.with_effect(if read_only { ToolEffect::ReadOnly } else { ToolEffect::NonIdempotent })
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
        /// The message reported to the model as the call's error, in a
        /// [`confirmation_pending_response`].
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
    let GovernedCall { tool_name, function_call_id, mut args, effect, requires_confirmation } =
        call;

    let mut decision = evaluate_policy(gate, &tool_name, &args, effect).await;
    if !matches!(decision, PolicyDecision::Deny { .. })
        && let Some(screen) = gate.screen
    {
        match screen.screen(&tool_name, &args).await {
            Ok(screened) => {
                if screened != args {
                    // The policy approved the arguments it saw, not the rewrite.
                    let again = evaluate_policy(gate, &tool_name, &screened, effect).await;
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
    effect: ToolEffect,
) -> PolicyDecision {
    let Some(policy) = gate.run_config.tool_policy.as_ref() else {
        return PolicyDecision::Allow;
    };
    let request = ToolPolicyRequest {
        tool_name: tool_name.to_string(),
        args: args.clone(),
        effect,
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

// ---------------------------------------------------------------------------
// Held calls and resume
// ---------------------------------------------------------------------------

/// The `status` value of the function response that answers a call held for
/// confirmation.
pub const CONFIRMATION_PENDING_STATUS: &str = "confirmation_pending";

/// Builds the function response for a call held for confirmation.
///
/// The response stands in for the call's result while the call waits for a
/// decision, so the history keeps one response per call. When a later run executes
/// or refuses the call, its real response supersedes this one in what the model
/// sees.
///
/// # Example
///
/// ```rust
/// use adk_core::{CONFIRMATION_PENDING_STATUS, confirmation_pending_response};
///
/// let response = confirmation_pending_response("Tool 'pay' requires confirmation");
/// assert_eq!(response["status"], CONFIRMATION_PENDING_STATUS);
/// assert_eq!(response["error"], "Tool 'pay' requires confirmation");
/// ```
pub fn confirmation_pending_response(reason: impl Into<String>) -> Value {
    serde_json::json!({ "error": reason.into(), "status": CONFIRMATION_PENDING_STATUS })
}

/// Returns `true` when `response` is a [`confirmation_pending_response`].
///
/// # Example
///
/// ```rust
/// use adk_core::{confirmation_pending_response, is_confirmation_pending};
///
/// assert!(is_confirmation_pending(&confirmation_pending_response("held")));
/// assert!(!is_confirmation_pending(&serde_json::json!({ "error": "held" })));
/// ```
pub fn is_confirmation_pending(response: &Value) -> bool {
    response.get("status").and_then(Value::as_str) == Some(CONFIRMATION_PENDING_STATUS)
}

/// A tool call an agent held for confirmation that has not been answered since.
///
/// Returned by [`held_tool_calls`]. An agent resumes it at the start of a later run
/// once a decision exists for it.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldToolCall {
    /// The request the agent surfaced. Its arguments are the call's final ones,
    /// after plugin and callback rewrites; its fingerprint is the decision key.
    pub request: ToolConfirmationRequest,
    /// The call's ID as the model issued it; `None` when the provider omitted it.
    pub id: Option<String>,
    /// The arguments as the model issued them, before plugin and callback rewrites.
    pub args: Value,
}

/// Finds the tool calls `agent_name` holds for confirmation in `events`.
///
/// `events` is a session's event log in order. A call is held from the event that
/// carries its [`ToolConfirmationRequest`] until a later response answers it: a
/// response to the same call ID, or to a call with the same tool and arguments. A
/// [`confirmation_pending_response`] answers nothing. A later hold of a call with the
/// same fingerprint replaces an earlier one, so one decision resumes one call.
///
/// Only events visible on `branch` and newer than the latest compaction count, as in
/// the conversation history the agent sees.
///
/// # Example
///
/// ```rust
/// use adk_core::{Event, ToolConfirmationRequest, held_tool_calls};
/// use serde_json::json;
///
/// let mut held = Event::new("inv-1");
/// held.author = "agent".into();
/// held.actions.tool_confirmation = Some(ToolConfirmationRequest {
///     tool_name: "pay".into(),
///     function_call_id: Some("call-1".into()),
///     args: json!({ "amount": 5 }),
/// });
///
/// let calls = held_tool_calls(&[held], "agent", "");
/// assert_eq!(calls.len(), 1);
/// assert_eq!(calls[0].request.tool_name, "pay");
/// ```
pub fn held_tool_calls(events: &[Event], agent_name: &str, branch: &str) -> Vec<HeldToolCall> {
    let boundary = events
        .iter()
        .rev()
        .find_map(|event| event.actions.compaction.as_ref().map(|c| c.end_timestamp));
    // Calls the agent issued, by ID; idless calls by invocation and name.
    let mut issued = HashMap::<String, (String, Value)>::new();
    let mut idless = Vec::<(String, String, Value)>::new();
    let mut held = Vec::<HeldToolCall>::new();

    for event in events {
        if event.llm_response.partial
            || event.actions.compaction.is_some()
            || boundary.is_some_and(|end| event.timestamp <= end)
            || !crate::event_belongs_to_branch(branch, &event.branch)
        {
            continue;
        }
        let own = event.author == agent_name;
        if !own && event.author != "user" {
            continue;
        }
        let parts = event.llm_response.content.iter().flat_map(|content| content.parts.iter());
        for part in parts {
            match part {
                Part::FunctionCall { name, args, id, .. } if own => match id {
                    Some(id) => {
                        issued.insert(id.clone(), (name.clone(), args.clone()));
                    }
                    None => idless.push((event.invocation_id.clone(), name.clone(), args.clone())),
                },
                Part::FunctionResponse { function_response, id, .. }
                    if !is_confirmation_pending(&function_response.response) =>
                {
                    match id {
                        Some(id) => {
                            let answered = issued
                                .get(id)
                                .map(|(name, args)| tool_call_fingerprint(name, args));
                            held.retain(|call| {
                                call.request.function_call_id.as_deref() != Some(id)
                                    && call.id.as_deref() != Some(id)
                                    && answered.as_deref()
                                        != Some(&tool_call_fingerprint(
                                            &call.request.tool_name,
                                            &call.args,
                                        ))
                            });
                        }
                        None => held.retain(|call| {
                            call.id.is_some() || call.request.tool_name != function_response.name
                        }),
                    }
                }
                _ => {}
            }
        }
        if own && let Some(request) = &event.actions.tool_confirmation {
            let (id, args) = match request.function_call_id.as_deref() {
                Some(call_id) if issued.contains_key(call_id) => {
                    (Some(call_id.to_string()), issued[call_id].1.clone())
                }
                _ => match idless.iter().rev().find(|(invocation, name, _)| {
                    *invocation == event.invocation_id && *name == request.tool_name
                }) {
                    Some((_, _, args)) => (None, args.clone()),
                    None => (request.function_call_id.clone(), request.args.clone()),
                },
            };
            let fingerprint = request.fingerprint();
            held.retain(|call| call.request.fingerprint() != fingerprint);
            held.push(HeldToolCall { request: request.clone(), id, args });
        }
    }
    held
}

/// Whether a decision for `request` may be available to the governed path.
///
/// Reports `true` for a call-ID decision that matches the call's fingerprint, a live
/// fingerprint approval, a decision in the run's [`ApprovalStore`] (asked through
/// [`ApprovalStore::has_decision`], which consumes nothing), or a configured
/// [`ToolConfirmationHandler`](crate::ToolConfirmationHandler), which decides when
/// asked. An agent resumes a [`HeldToolCall`] only when this is `true`.
///
/// # Errors
///
/// Returns an error when the approval store cannot be read.
///
/// # Example
///
/// ```rust
/// use adk_core::{
///     ApprovalScope, RunConfig, ToolApproval, ToolConfirmationRequest, ToolGate,
///     confirmation_decision_available,
/// };
/// use serde_json::json;
///
/// # futures::executor::block_on(async {
/// let request = ToolConfirmationRequest {
///     tool_name: "pay".into(),
///     function_call_id: Some("call-1".into()),
///     args: json!({ "amount": 5 }),
/// };
/// let config =
///     RunConfig::builder().tool_approval(request.fingerprint(), ToolApproval::approve()).build();
/// let gate = ToolGate::new(&config, "agent", "inv-2", ApprovalScope::new("app", "u", "s"));
/// assert!(confirmation_decision_available(&gate, &request).await?);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
pub async fn confirmation_decision_available(
    gate: &ToolGate<'_>,
    request: &ToolConfirmationRequest,
) -> Result<bool> {
    let config = gate.run_config;
    let fingerprint = request.fingerprint();
    if let Some(call_id) = request.function_call_id.as_deref()
        && config.tool_confirmation_decisions.contains_key(call_id)
        && config
            .tool_confirmation_fingerprints
            .get(call_id)
            .is_none_or(|bound| *bound == fingerprint)
    {
        return Ok(true);
    }
    if config.tool_approvals.get(&fingerprint).is_some_and(|approval| approval.is_live(Utc::now()))
    {
        return Ok(true);
    }
    if let Some(store) = &config.approval_store
        && store.has_decision(&gate.scope, &fingerprint).await?
    {
        return Ok(true);
    }
    Ok(config.tool_confirmation_handler.is_some())
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
    async fn the_policy_sees_the_effect_the_tool_declares() {
        struct Upsert;
        #[async_trait]
        impl Tool for Upsert {
            fn name(&self) -> &str {
                "upsert"
            }
            fn description(&self) -> &str {
                "writes a record by key"
            }
            fn effect(&self) -> ToolEffect {
                ToolEffect::Idempotent
            }
            async fn execute(
                &self,
                _ctx: Arc<dyn crate::ToolContext>,
                _args: Value,
            ) -> Result<Value> {
                Ok(json!({}))
            }
        }

        let call = GovernedCall::for_tool(&Upsert, "c", json!({}));
        assert_eq!(call.effect, ToolEffect::Idempotent);

        let policy = DeclarativePolicy::builder()
            .rule(PolicyRule::allow("*").with_effect(ToolEffect::Idempotent))
            .build();
        let config = RunConfig::builder().tool_policy(Arc::new(policy)).build();
        assert!(matches!(
            authorize_tool_call(&gate(&config), call).await.unwrap(),
            ToolAuthorization::Execute { .. }
        ));
        assert!(matches!(
            authorize_tool_call(&gate(&config), GovernedCall::new("upsert", "c", json!({})))
                .await
                .unwrap(),
            ToolAuthorization::Refuse { .. }
        ));
    }

    fn event(author: &str, invocation: &str, parts: Vec<Part>) -> Event {
        let mut event = Event::new(invocation);
        event.author = author.to_string();
        event.llm_response.content = Some(crate::Content { role: "model".into(), parts });
        event
    }

    fn function_call(name: &str, args: Value, id: &str) -> Part {
        Part::FunctionCall { name: name.into(), args, id: Some(id.into()), thought_signature: None }
    }

    fn function_response(name: &str, response: Value, id: &str) -> Part {
        Part::FunctionResponse {
            function_response: crate::FunctionResponseData::new(name, response),
            id: Some(id.into()),
            annotations: None,
        }
    }

    fn hold(author: &str, invocation: &str, name: &str, args: Value, id: &str) -> Event {
        let mut event = Event::new(invocation);
        event.author = author.to_string();
        event.actions.tool_confirmation = Some(ToolConfirmationRequest {
            tool_name: name.into(),
            function_call_id: Some(id.into()),
            args,
        });
        event
    }

    /// Run 1 of agent `ops`: `pay` called as `c1`, answered pending, then held.
    fn held_run() -> Vec<Event> {
        vec![
            event("user", "inv-1", vec![Part::Text { text: "pay 5".into() }]),
            event("ops", "inv-1", vec![function_call("pay", json!({ "amount": 5 }), "c1")]),
            event(
                "ops",
                "inv-1",
                vec![function_response("pay", confirmation_pending_response("held"), "c1")],
            ),
            hold("ops", "inv-1", "pay", json!({ "amount": 5 }), "c1"),
        ]
    }

    #[test]
    fn a_held_call_carries_the_models_arguments_and_original_id() {
        let mut events = held_run();
        // A plugin rewrote the arguments before the hold; the request carries the rewrite.
        events[3] = hold("ops", "inv-1", "pay", json!({ "amount": 500 }), "c1");
        assert_eq!(
            held_tool_calls(&events, "ops", ""),
            vec![HeldToolCall {
                request: ToolConfirmationRequest {
                    tool_name: "pay".into(),
                    function_call_id: Some("c1".into()),
                    args: json!({ "amount": 500 }),
                },
                id: Some("c1".into()),
                args: json!({ "amount": 5 }),
            }]
        );
        assert!(held_tool_calls(&events, "other", "").is_empty(), "holds belong to their agent");
    }

    #[test]
    fn a_real_response_releases_a_held_call_and_a_pending_one_does_not() {
        let mut events = held_run();
        events.push(event("user", "inv-2", vec![Part::Text { text: String::new() }]));
        events.push(event(
            "ops",
            "inv-2",
            vec![function_response("pay", confirmation_pending_response("still held"), "c1")],
        ));
        assert_eq!(held_tool_calls(&events, "ops", "").len(), 1);

        events.push(event(
            "ops",
            "inv-2",
            vec![function_response("pay", json!({ "ok": 1 }), "c1")],
        ));
        assert!(held_tool_calls(&events, "ops", "").is_empty());
    }

    #[test]
    fn executing_the_same_call_under_a_new_id_releases_the_hold() {
        let mut events = held_run();
        events.push(event(
            "ops",
            "inv-2",
            vec![function_call("pay", json!({ "amount": 5 }), "c2")],
        ));
        events.push(event(
            "ops",
            "inv-2",
            vec![function_response("pay", json!({ "ok": 1 }), "c2")],
        ));
        assert!(held_tool_calls(&events, "ops", "").is_empty());
    }

    #[test]
    fn a_later_hold_of_the_same_call_replaces_the_earlier_one() {
        let mut events = held_run();
        events.push(event(
            "ops",
            "inv-2",
            vec![function_call("pay", json!({ "amount": 5 }), "c2")],
        ));
        events.push(event(
            "ops",
            "inv-2",
            vec![function_response("pay", confirmation_pending_response("held"), "c2")],
        ));
        events.push(hold("ops", "inv-2", "pay", json!({ "amount": 5 }), "c2"));
        let held = held_tool_calls(&events, "ops", "");
        assert_eq!(held.len(), 1, "one decision resumes one call");
        assert_eq!(held[0].id.as_deref(), Some("c2"));
    }

    #[test]
    fn holds_before_a_compaction_or_on_another_branch_are_ignored() {
        let mut compacted = held_run();
        let mut compaction = Event::new("inv-2");
        compaction.actions.compaction = Some(crate::EventCompaction {
            start_timestamp: compacted[0].timestamp,
            end_timestamp: compacted[3].timestamp,
            compacted_content: crate::Content::new("model").with_text("summary"),
        });
        compacted.push(compaction);
        assert!(held_tool_calls(&compacted, "ops", "").is_empty());

        let mut branched = held_run();
        for event in &mut branched {
            event.branch = "root.left".into();
        }
        assert!(held_tool_calls(&branched, "ops", "root.right").is_empty());
        assert_eq!(held_tool_calls(&branched, "ops", "root.left").len(), 1);
    }

    #[tokio::test]
    async fn a_decision_is_available_from_each_source_without_being_consumed() {
        let request = ToolConfirmationRequest {
            tool_name: "pay".into(),
            function_call_id: Some("c1".into()),
            args: json!({ "amount": 5 }),
        };
        let none = RunConfig::default();
        assert!(!confirmation_decision_available(&gate(&none), &request).await.unwrap());

        let by_id = RunConfig::builder()
            .tool_confirmation_decisions(HashMap::from([(
                "c1".to_string(),
                ToolConfirmationDecision::Deny,
            )]))
            .build();
        assert!(confirmation_decision_available(&gate(&by_id), &request).await.unwrap());

        let mut rebound = by_id.clone();
        rebound
            .tool_confirmation_fingerprints
            .insert("c1".into(), tool_call_fingerprint("pay", &json!({ "amount": 6 })));
        assert!(!confirmation_decision_available(&gate(&rebound), &request).await.unwrap());

        let store = Arc::new(InMemoryApprovalStore::new());
        let stored = RunConfig::builder().approval_store(store.clone()).build();
        let scope = ApprovalScope::new("app", "user", "session");
        store.record_pending(&scope, &request).await.unwrap();
        assert!(!confirmation_decision_available(&gate(&stored), &request).await.unwrap());
        store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await.unwrap();
        assert!(confirmation_decision_available(&gate(&stored), &request).await.unwrap());
        assert_eq!(
            store.take_decision(&scope, &request.fingerprint()).await.unwrap(),
            Some(ToolConfirmationDecision::Approve),
            "asking did not consume the decision"
        );

        let handled =
            RunConfig::builder().tool_confirmation_handler(Arc::new(NeverDecides)).build();
        assert!(confirmation_decision_available(&gate(&handled), &request).await.unwrap());
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
