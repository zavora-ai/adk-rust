//! Governed tool execution.
//!
//! Every agent runs a tool call through [`authorize_tool_call`] after all plugin and
//! callback rewrites and immediately before `Tool::execute`. The order is fixed:
//!
//! 1. The org kill switch ([`GovernanceControl`]) — a frozen run ends with an error.
//! 2. For a held call being resumed ([`GovernedCall::resuming`]), its binding: the acting
//!    agent and the tool's [`ToolOrigin`](crate::ToolOrigin) must be the ones the call was
//!    held for, or the call is refused as not run.
//! 3. The run's [`ToolPolicy`](crate::ToolPolicy), on the final arguments, through
//!    [`ToolPolicy::evaluate_call`](crate::ToolPolicy::evaluate_call) with the tool's
//!    [`ToolOrigin`](crate::ToolOrigin) and the call's ID.
//! 4. The agent's tool screen (its guardrails), on the final arguments. A screen that
//!    rewrites the arguments has the rewrite checked against the policy again.
//! 5. Confirmation, when the agent or the policy asks for it. A decision binds to the
//!    call's [fingerprint](ToolConfirmationRequest::fingerprint) — the acting agent, the
//!    tool's origin and declaration, the tool name, and the canonical arguments — so an
//!    argument rewrite after approval needs a new approval, an approval for one agent's
//!    call never authorizes another agent's, an approval for a tool one server publishes
//!    never authorizes the name on another server, and an approval given in one run
//!    matches the same agent's same call re-issued under a new call ID in the next.
//!
//! Approval decisions come from, in order: a decision keyed by call ID
//! ([`RunConfig::tool_confirmation_decisions`]), a decision keyed by fingerprint
//! ([`RunConfig::tool_approvals`]), the run's [`ApprovalStore`], and the live
//! [`ToolConfirmationHandler`](crate::ToolConfirmationHandler), asked through
//! [`decide_approval`](crate::ToolConfirmationHandler::decide_approval), which is bounded by
//! [`RunConfig::tool_confirmation_timeout`] and denies when it expires.
//!
//! A call held without a decision is answered with a [`confirmation_pending_response`]
//! and stays held across runs. So is a call the handler does not decide in time when the
//! run has an [`ApprovalStore`] to read a later decision from: it waits for a decision
//! instead of being denied (spec 3.3 §7.3), unless
//! [`RunConfig::deny_on_confirmation_timeout`] is set. At the start of a later run the
//! agent finds its held calls with [`held_tool_calls`] and, for each one with a decision
//! ([`confirmation_decision_available`]), dispatches the original call again through
//! the same path, so the decision authorizes or refuses it without the model issuing
//! it a second time. A call waiting in the store resumes only on a decision recorded for
//! it; the handler is not asked again, so the run goes on to the model. The call's real
//! response supersedes the pending one ([`supersede_pending_responses`]).
//!
//! A decision from any source closes what the call left waiting: its pending entry in the
//! store and its open request in the [`ApprovalService`](crate::ApprovalService), which is
//! withdrawn ([`ApprovalService::withdraw`](crate::ApprovalService::withdraw)), so neither
//! can authorize a later call with the same fingerprint. A request an approver resolved at
//! the same moment is waited for until its outcome is in the store, and that outcome is
//! absorbed rather than left to authorize a later call, as is an outcome already applied or
//! recorded while the handler was asked.
//!
//! A held call survives what happens to the session while it waits: compaction never
//! covers it ([`held_call_compaction_limit`]), a workflow agent that it stopped resumes at
//! the sub-agent holding it ([`workflow_resume_positions`]), and a call its provider sent
//! without an ID carries a generated one
//! ([`GENERATED_CALL_ID_PREFIX`](crate::GENERATED_CALL_ID_PREFIX)), so two calls to one
//! tool keep their own arguments.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approval::{ApprovalDelegation, ApprovalRequest, ApprovalRequestQuery};
use crate::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, AuditTrail,
};
use crate::governance_state::{GovernanceScope, GovernanceState, GovernanceStateStore};
use crate::principal::SubjectRef;
use crate::{
    AdkError, BUILTIN_TOOL_ORIGIN, ErrorCategory, ErrorComponent, Event, InvocationContext, Part,
    PolicyCall, PolicyDecision, Result, RunConfig, Tool, ToolConfirmationDecision,
    ToolConfirmationRequest, ToolEffect, ToolOrigin, ToolPolicyRequest, tool_call_fingerprint,
};

/// How long a [`ToolConfirmationHandler`](crate::ToolConfirmationHandler) may take to
/// decide before the call waits in the run's approval store, or is denied when the run has
/// none (spec 3.3 §7.3).
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
/// # Replicated kill switch
///
/// [`GovernanceControl::new`] is process-local. [`GovernanceControl::mirrored`] builds a
/// control that mirrors a [`GovernanceStateStore`] shared by every replica:
///
/// | Mechanism | Behaviour |
/// |-----------|-----------|
/// | [`spawn_mirror`](Self::spawn_mirror) | Reads the mirrored scopes every `poll_interval`, and at once when the optional [`GovernanceWatch`] reports a change |
/// | Staleness lease | Past `max_staleness` without a successful read, [`check`](Self::check) fails closed with `governance.stale`, unless the mirror is configured fail-open |
/// | [`freeze_replicated`](Self::freeze_replicated) / [`unfreeze_replicated`](Self::unfreeze_replicated) | Compare-and-swap the store, then apply the new state locally |
///
/// A local [`freeze`](Self::freeze) still works on a mirrored control and holds until a
/// local [`unfreeze`](Self::unfreeze); the store never lifts it.
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
    /// The reason of a freeze read from the mirrored store, while one is in force.
    replicated: std::sync::RwLock<Option<String>>,
    mirror: Option<Mirror>,
    audit: std::sync::RwLock<Option<AuditTrail>>,
}

/// Error code returned while a mirrored control has not read its store within `max_staleness`.
pub const GOVERNANCE_STALE_CODE: &str = "governance.stale";

/// Error code returned while execution is frozen.
pub const GOVERNANCE_FROZEN_CODE: &str = "governance.frozen";

/// Notifies a mirrored [`GovernanceControl`] that the store changed, so it reads it
/// before the next poll.
///
/// Implement it over a backend's change channel (PostgreSQL `LISTEN`, Redis pub/sub, a
/// MongoDB change stream). The store remains the truth: a notification only shortens the
/// time until the control reads it, and a lost notification is covered by the poll.
///
/// # Example
///
/// ```rust
/// use adk_core::governance::GovernanceWatch;
///
/// #[derive(Default)]
/// struct NotifyWatch(tokio::sync::Notify);
///
/// #[async_trait::async_trait]
/// impl GovernanceWatch for NotifyWatch {
///     async fn changed(&self) -> adk_core::Result<()> {
///         self.0.notified().await;
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait GovernanceWatch: Send + Sync {
    /// Resolves when the store reports a change.
    ///
    /// # Errors
    ///
    /// Returns an error when the change channel failed; the mirror then waits for the
    /// next poll before listening again.
    async fn changed(&self) -> Result<()>;
}

/// How a [`GovernanceControl`] mirrors a [`GovernanceStateStore`].
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::governance::GovernanceMirrorConfig;
/// use adk_core::governance_state::GovernanceScope;
///
/// let config = GovernanceMirrorConfig::default()
///     .with_poll_interval(Duration::from_secs(1))
///     .with_max_staleness(Duration::from_secs(10))
///     .with_scopes([GovernanceScope::Org, GovernanceScope::Tenant("acme".into())]);
/// assert!(!config.fail_open);
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct GovernanceMirrorConfig {
    /// How often the store is read. Defaults to 2 seconds.
    pub poll_interval: Duration,
    /// How long the control trusts its last successful read. Defaults to 30 seconds.
    pub max_staleness: Duration,
    /// Keep running when the store cannot be read past `max_staleness`. Defaults to `false`.
    pub fail_open: bool,
    /// The scopes mirrored; execution is frozen when any of them is. The first scope is
    /// the one [`freeze_replicated`](GovernanceControl::freeze_replicated) writes.
    /// Defaults to [`GovernanceScope::Org`].
    pub scopes: Vec<GovernanceScope>,
    /// An optional change notification that triggers a read before the next poll.
    pub watch: Option<Arc<dyn GovernanceWatch>>,
}

impl Default for GovernanceMirrorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Self::DEFAULT_POLL_INTERVAL,
            max_staleness: Self::DEFAULT_MAX_STALENESS,
            fail_open: false,
            scopes: vec![GovernanceScope::Org],
            watch: None,
        }
    }
}

impl std::fmt::Debug for GovernanceMirrorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernanceMirrorConfig")
            .field("poll_interval", &self.poll_interval)
            .field("max_staleness", &self.max_staleness)
            .field("fail_open", &self.fail_open)
            .field("scopes", &self.scopes)
            .field("watch", &self.watch.is_some())
            .finish()
    }
}

impl GovernanceMirrorConfig {
    /// The default `poll_interval`.
    pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
    /// The default `max_staleness`.
    pub const DEFAULT_MAX_STALENESS: Duration = Duration::from_secs(30);

    /// Sets how often the store is read.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Sets how long the control trusts its last successful read.
    pub fn with_max_staleness(mut self, staleness: Duration) -> Self {
        self.max_staleness = staleness;
        self
    }

    /// Keeps execution running when the store cannot be read past `max_staleness`.
    pub fn with_fail_open(mut self, fail_open: bool) -> Self {
        self.fail_open = fail_open;
        self
    }

    /// Sets the mirrored scopes; the first is the one replicated freezes write.
    pub fn with_scopes(mut self, scopes: impl IntoIterator<Item = GovernanceScope>) -> Self {
        self.scopes = scopes.into_iter().collect();
        self
    }

    /// Sets the change notification.
    pub fn with_watch(mut self, watch: Arc<dyn GovernanceWatch>) -> Self {
        self.watch = Some(watch);
        self
    }
}

struct Mirror {
    store: Arc<dyn GovernanceStateStore>,
    config: GovernanceMirrorConfig,
    last_refresh: std::sync::Mutex<Option<tokio::time::Instant>>,
    wake: tokio::sync::Notify,
}

impl Mirror {
    fn primary_scope(&self) -> GovernanceScope {
        self.config.scopes.first().cloned().unwrap_or(GovernanceScope::Org)
    }

    fn staleness(&self) -> Option<Duration> {
        self.last_refresh
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|refreshed| refreshed.elapsed())
    }

    fn is_stale(&self) -> bool {
        self.staleness().is_none_or(|age| age > self.config.max_staleness)
    }
}

impl std::fmt::Debug for GovernanceControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernanceControl")
            .field("frozen", &self.is_frozen())
            .field("reason", &self.reason())
            .field("mirrored", &self.is_mirrored())
            .finish()
    }
}

/// The most compare-and-swap attempts a replicated freeze makes before reporting the conflict.
const REPLICATED_WRITE_ATTEMPTS: usize = 5;

impl GovernanceControl {
    /// Creates an unfrozen, process-local control.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an unfrozen control that mirrors `store`.
    ///
    /// The control is stale until its first successful read, so it fails closed until
    /// [`refresh`](Self::refresh) or [`spawn_mirror`](Self::spawn_mirror) reads the store
    /// (unless `config.fail_open`).
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_core::GovernanceControl;
    /// use adk_core::governance::GovernanceMirrorConfig;
    /// use adk_core::governance_state::InMemoryGovernanceStateStore;
    ///
    /// # futures::executor::block_on(async {
    /// let store = Arc::new(InMemoryGovernanceStateStore::new());
    /// let a = GovernanceControl::mirrored(store.clone(), GovernanceMirrorConfig::default());
    /// let b = GovernanceControl::mirrored(store, GovernanceMirrorConfig::default());
    /// assert!(a.check().is_err(), "stale until the first read");
    ///
    /// a.freeze_replicated("incident 7", None).await?;
    /// b.refresh().await?;
    /// assert!(b.is_frozen());
    /// # Ok::<(), adk_core::AdkError>(())
    /// # }).unwrap();
    /// ```
    pub fn mirrored(store: Arc<dyn GovernanceStateStore>, config: GovernanceMirrorConfig) -> Self {
        Self {
            inner: Arc::new(ControlState {
                mirror: Some(Mirror {
                    store,
                    config,
                    last_refresh: std::sync::Mutex::new(None),
                    wake: tokio::sync::Notify::new(),
                }),
                ..ControlState::default()
            }),
        }
    }

    /// Whether the control mirrors a [`GovernanceStateStore`].
    pub fn is_mirrored(&self) -> bool {
        self.inner.mirror.is_some()
    }

    /// Freezes execution everywhere this control is attached, in this process only.
    ///
    /// With an [attached audit trail](Self::attach_audit), the freeze is recorded in the
    /// background; use [`freeze_recorded`](Self::freeze_recorded) to wait for the record.
    pub fn freeze(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.set_frozen(reason.clone());
        self.record_in_background(governance_draft("governance.freeze", Some(reason)));
    }

    /// Lifts a local freeze. A freeze read from a mirrored store stays in force.
    ///
    /// With an [attached audit trail](Self::attach_audit), the change is recorded in the
    /// background; use [`unfreeze_recorded`](Self::unfreeze_recorded) to lift the
    /// freeze only once it is recorded.
    pub fn unfreeze(&self) {
        self.clear_frozen();
        self.record_in_background(governance_draft("governance.unfreeze", None));
    }

    /// Records freezes and unfreezes of this control, and of every clone, through `audit`.
    pub fn attach_audit(&self, audit: AuditTrail) {
        *self.inner.audit.write().unwrap_or_else(|e| e.into_inner()) = Some(audit);
    }

    /// The attached audit trail, if any.
    pub fn audit(&self) -> Option<AuditTrail> {
        self.inner.audit.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Freezes execution, then records the freeze as done by `actor`.
    ///
    /// A mirrored control freezes every replica, as
    /// [`freeze_replicated`](Self::freeze_replicated) does, with `actor`'s subject as the
    /// issuer. The freeze takes effect whether or not the record is written.
    ///
    /// # Errors
    ///
    /// Returns the replicated store's error, as `freeze_replicated` does (this replica
    /// is frozen regardless), and `audit.record_failed` when the attached trail is in
    /// [`Block`](crate::audit_log::AuditFailureMode::Block) mode and the record cannot be
    /// written; execution stays frozen.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_core::GovernanceControl;
    /// use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
    /// use adk_core::principal::SubjectRef;
    ///
    /// # futures::executor::block_on(async {
    /// let recorder = InMemoryAuditRecorder::new();
    /// let control = GovernanceControl::new();
    /// control.attach_audit(AuditTrail::new(Arc::new(recorder.clone())));
    ///
    /// let admin = AuditActor::subject(SubjectRef::human("oncall"));
    /// control.freeze_recorded("incident 4012", admin.clone()).await?;
    /// control.unfreeze_recorded(admin).await?;
    /// assert_eq!(recorder.records().len(), 2);
    /// # Ok::<(), adk_core::AdkError>(())
    /// # }).unwrap();
    /// ```
    pub async fn freeze_recorded(
        &self,
        reason: impl Into<String>,
        actor: AuditActor,
    ) -> Result<()> {
        let reason = reason.into();
        let replicated = self.replicate_freeze(reason.clone(), actor.on_behalf_of.clone()).await;
        if let Some(trail) = self.audit() {
            let draft = governance_draft("governance.freeze", Some(reason))
                .actor(actor)
                .ext("replicated", Value::Bool(self.is_mirrored() && replicated.is_ok()));
            trail.record(draft).await?;
        }
        replicated.map(drop)
    }

    /// Records the unfreeze as done by `actor`, then lifts the freeze; on every replica
    /// for a mirrored control, as [`unfreeze_replicated`](Self::unfreeze_replicated) does.
    ///
    /// # Errors
    ///
    /// Returns the replicated store's error, as `unfreeze_replicated` does, and
    /// `audit.record_failed` when the attached trail is in
    /// [`Block`](crate::audit_log::AuditFailureMode::Block) mode and the record cannot be
    /// written; execution stays frozen.
    pub async fn unfreeze_recorded(&self, actor: AuditActor) -> Result<()> {
        let issued_by = actor.on_behalf_of.clone();
        if let Some(trail) = self.audit() {
            let draft = governance_draft("governance.unfreeze", None)
                .actor(actor)
                .ext("replicated", Value::Bool(self.is_mirrored()));
            trail.record(draft).await?;
        }
        self.replicate_unfreeze(issued_by).await.map(drop)
    }

    fn set_frozen(&self, reason: String) {
        tracing::warn!(governance.reason = %reason, "execution frozen");
        *self.inner.reason.write().unwrap_or_else(|e| e.into_inner()) = Some(reason);
        self.inner.frozen.store(true, Ordering::SeqCst);
    }

    fn clear_frozen(&self) {
        self.inner.frozen.store(false, Ordering::SeqCst);
        *self.inner.reason.write().unwrap_or_else(|e| e.into_inner()) = None;
        tracing::info!("execution unfrozen");
    }

    /// Records `draft` without waiting, when a trail is attached and a runtime is running.
    fn record_in_background(&self, draft: AuditRecordDraft) {
        let Some(trail) = self.audit() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    // A failure is logged by the trail; the change has already happened.
                    let _ = trail.record(draft).await;
                });
            }
            Err(_) => tracing::warn!(
                audit.action = %draft.action,
                "no async runtime to record the governance change; use the *_recorded methods"
            ),
        }
    }

    /// Whether execution is frozen, locally or by a mirrored store.
    pub fn is_frozen(&self) -> bool {
        self.inner.frozen.load(Ordering::SeqCst) || self.replicated_reason().is_some()
    }

    fn replicated_reason(&self) -> Option<String> {
        self.inner.replicated.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The reason given for the current freeze, if frozen.
    pub fn reason(&self) -> Option<String> {
        if self.inner.frozen.load(Ordering::SeqCst) {
            return self.inner.reason.read().unwrap_or_else(|e| e.into_inner()).clone();
        }
        self.replicated_reason()
    }

    /// Whether `other` shares this control's flag.
    pub fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// How long ago the mirrored store was last read successfully; `None` for a
    /// process-local control or before the first read.
    pub fn staleness(&self) -> Option<Duration> {
        self.inner.mirror.as_ref().and_then(Mirror::staleness)
    }

    /// Whether a mirrored control has gone past `max_staleness` without a successful
    /// read. Always `false` for a process-local control.
    pub fn is_stale(&self) -> bool {
        self.inner.mirror.as_ref().is_some_and(Mirror::is_stale)
    }

    /// Returns an error when frozen, or when a mirrored, fail-closed control is stale.
    ///
    /// # Errors
    ///
    /// Returns a non-retryable [`ErrorCategory::Forbidden`] error with code
    /// `governance.frozen` while frozen, and a retryable [`ErrorCategory::Unavailable`]
    /// error with code [`GOVERNANCE_STALE_CODE`] while a fail-closed mirror is stale.
    pub fn check(&self) -> Result<()> {
        if self.is_frozen() {
            let reason = self.reason().unwrap_or_else(|| "no reason given".to_string());
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Forbidden,
                GOVERNANCE_FROZEN_CODE,
                format!(
                    "execution is frozen by the governance kill switch ({reason}); no model call \
                     or tool runs until an operator unfreezes it"
                ),
            ));
        }
        if let Some(mirror) = &self.inner.mirror
            && !mirror.config.fail_open
            && mirror.is_stale()
        {
            let age = mirror
                .staleness()
                .map_or_else(|| "never".to_string(), |age| format!("{}s ago", age.as_secs()));
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Unavailable,
                GOVERNANCE_STALE_CODE,
                format!(
                    "the governance kill switch state was last read {age}, past its {}s \
                     staleness limit, so execution is refused until the governance store is \
                     reachable again",
                    mirror.config.max_staleness.as_secs()
                ),
            ));
        }
        Ok(())
    }

    /// Reads the mirrored scopes once and applies them. A no-op for a process-local control.
    ///
    /// # Errors
    ///
    /// Returns the store's error; the previous state stays in force and its age keeps growing.
    pub async fn refresh(&self) -> Result<()> {
        let Some(mirror) = &self.inner.mirror else { return Ok(()) };
        let mut frozen_by = None;
        for scope in &mirror.config.scopes {
            if let Some(state) = mirror.store.get(scope).await?
                && state.frozen
                && frozen_by.is_none()
            {
                frozen_by = Some(state);
            }
        }
        *mirror.last_refresh.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(tokio::time::Instant::now());
        let reason = frozen_by.as_ref().map(|state| {
            state.reason.clone().unwrap_or_else(|| format!("{} frozen", state.scope.key()))
        });
        let previous = std::mem::replace(
            &mut *self.inner.replicated.write().unwrap_or_else(|e| e.into_inner()),
            reason.clone(),
        );
        match (&previous, &frozen_by) {
            (None, Some(state)) => tracing::warn!(
                governance.scope = %state.scope.key(),
                governance.epoch = state.epoch,
                governance.reason = reason.as_deref().unwrap_or_default(),
                "execution frozen by the replicated kill switch"
            ),
            (Some(_), None) => tracing::info!("replicated kill switch lifted"),
            _ => {}
        }
        Ok(())
    }

    /// Spawns the task that keeps a mirrored control in step with its store, and returns
    /// its handle; `None` for a process-local control.
    ///
    /// The task reads the store at once, then every `poll_interval` and whenever the
    /// configured [`GovernanceWatch`] reports a change. It ends when every clone of the
    /// control has been dropped, or when the handle is aborted.
    pub fn spawn_mirror(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.inner.mirror.as_ref()?;
        let weak = Arc::downgrade(&self.inner);
        Some(tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { return };
                let control = GovernanceControl { inner };
                if let Err(error) = control.refresh().await {
                    tracing::warn!(error = %error, "could not read the governance store");
                }
                let Some(mirror) = control.inner.mirror.as_ref() else { return };
                let poll = tokio::time::sleep(mirror.config.poll_interval);
                let watch = mirror.config.watch.clone();
                tokio::select! {
                    () = poll => {}
                    () = mirror.wake.notified() => {}
                    changed = async {
                        match &watch {
                            Some(watch) => watch.changed().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if let Err(error) = changed {
                            tracing::warn!(error = %error, "governance change notification failed");
                            tokio::time::sleep(mirror.config.poll_interval).await;
                        }
                    }
                }
            }
        }))
    }

    /// Freezes execution on every replica that mirrors the same store, and returns the
    /// stored state.
    ///
    /// On a mirrored control this writes the first mirrored scope by compare-and-swap,
    /// retrying when another change lands first, then applies the state locally. When
    /// the store cannot be written, the control freezes locally so this replica stops
    /// regardless, and the error is returned. On a process-local control it is
    /// [`freeze`](Self::freeze) and returns `None`.
    ///
    /// # Errors
    ///
    /// Returns the store's error, or `governance.conflict` when every attempt lost the race.
    pub async fn freeze_replicated(
        &self,
        reason: impl Into<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<Option<GovernanceState>> {
        let reason = reason.into();
        let result = self.replicate_freeze(reason.clone(), issued_by).await;
        self.record_in_background(governance_draft("governance.freeze", Some(reason)));
        result
    }

    /// [`freeze_replicated`](Self::freeze_replicated) without an audit record.
    async fn replicate_freeze(
        &self,
        reason: String,
        issued_by: Option<SubjectRef>,
    ) -> Result<Option<GovernanceState>> {
        let Some(mirror) = &self.inner.mirror else {
            self.set_frozen(reason);
            return Ok(None);
        };
        match self.write_replicated(mirror, true, Some(reason.clone()), issued_by).await {
            Ok(state) => Ok(state),
            Err(error) => {
                tracing::error!(error = %error, "could not write the replicated freeze; freezing this replica locally");
                self.set_frozen(reason);
                Err(error)
            }
        }
    }

    /// Lifts a replicated freeze, and any local one, and returns the stored state.
    ///
    /// On a mirrored control this writes the first mirrored scope by compare-and-swap and
    /// clears the local freeze only once the store is written. On a process-local control
    /// it is [`unfreeze`](Self::unfreeze) and returns `None`.
    ///
    /// # Errors
    ///
    /// Returns the store's error, or `governance.conflict` when every attempt lost the
    /// race; the freeze then stays in force.
    pub async fn unfreeze_replicated(
        &self,
        issued_by: Option<SubjectRef>,
    ) -> Result<Option<GovernanceState>> {
        let state = self.replicate_unfreeze(issued_by).await?;
        self.record_in_background(governance_draft("governance.unfreeze", None));
        Ok(state)
    }

    /// [`unfreeze_replicated`](Self::unfreeze_replicated) without an audit record.
    async fn replicate_unfreeze(
        &self,
        issued_by: Option<SubjectRef>,
    ) -> Result<Option<GovernanceState>> {
        let Some(mirror) = &self.inner.mirror else {
            self.clear_frozen();
            return Ok(None);
        };
        let state = self.write_replicated(mirror, false, None, issued_by).await?;
        self.clear_frozen();
        Ok(state)
    }

    async fn write_replicated(
        &self,
        mirror: &Mirror,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<Option<GovernanceState>> {
        let scope = mirror.primary_scope();
        let mut last_error = None;
        for _ in 0..REPLICATED_WRITE_ATTEMPTS {
            let current = mirror.store.get(&scope).await?;
            let epoch = current.as_ref().map_or(0, |state| state.epoch);
            if !frozen && current.as_ref().is_none_or(|state| !state.frozen) {
                self.refresh().await?;
                return Ok(current);
            }
            match mirror
                .store
                .set(scope.clone(), epoch, frozen, reason.clone(), issued_by.clone())
                .await
            {
                Ok(state) => {
                    tracing::warn!(
                        governance.scope = %scope.key(),
                        governance.epoch = state.epoch,
                        governance.frozen = frozen,
                        governance.issued_by = ?state.issued_by,
                        "replicated kill switch written"
                    );
                    self.refresh().await?;
                    mirror.wake.notify_waiters();
                    return Ok(Some(state));
                }
                Err(error) if error.is_conflict() => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last_error
            .unwrap_or_else(|| crate::governance_state::governance_conflict_error(&scope, 0, 0)))
    }
}

/// A governance record for `action` with the freeze reason, if any.
fn governance_draft(action: &str, reason: Option<String>) -> AuditRecordDraft {
    let draft = AuditRecordDraft::new(AuditRecordKind::Governance, action)
        .outcome(AuditRecordOutcome::Succeeded);
    match reason {
        Some(reason) => draft.ext("reason", Value::String(reason)),
        None => draft,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApproval {
    /// The decision.
    pub decision: ToolConfirmationDecision,
    /// When the decision stops applying. `None` never expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Arguments an approver substituted for the call's own.
    ///
    /// An approval with edited arguments executes the call with them, after the
    /// governed path checks them against the [`ToolPolicy`](crate::ToolPolicy) and the
    /// agent's tool screen again. Ignored on a denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_args: Option<Value>,
    /// What the approver said: a rejection reason or a response that stands in for the
    /// call's result. A denial reports it to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ToolApproval {
    /// An approval that does not expire.
    pub fn approve() -> Self {
        Self {
            decision: ToolConfirmationDecision::Approve,
            expires_at: None,
            edited_args: None,
            message: None,
        }
    }

    /// A denial that does not expire.
    pub fn deny() -> Self {
        Self {
            decision: ToolConfirmationDecision::Deny,
            expires_at: None,
            edited_args: None,
            message: None,
        }
    }

    /// Executes the call with `args` instead of its own. See
    /// [`edited_args`](Self::edited_args).
    #[must_use]
    pub fn with_edited_args(mut self, args: Value) -> Self {
        self.edited_args = Some(args);
        self
    }

    /// Sets what the approver said. See [`message`](Self::message).
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
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
/// authorize the same call in another. Within a session, the decision's
/// [fingerprint](ToolConfirmationRequest::fingerprint) names the agent and the tool it
/// was granted for, so agents sharing a session never share approvals.
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
/// [`decide`](Self::decide), and the next run that makes the same call — same agent,
/// same tool origin and declaration, same canonical arguments, any call ID — takes the
/// decision. Taking consumes it, so one approval authorizes one execution.
///
/// Stores key entries by the fingerprint string and need no knowledge of what it
/// contains.
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
///     acting: None,
///     origin: None,
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

    /// Records a call waiting for a decision under `fingerprint` instead of the one
    /// `request` computes.
    ///
    /// A wrapper that stores the request with its arguments sealed (`adk-session`'s
    /// `EncryptedApprovalStore`, spec 3.1 §10 S5) passes the fingerprint of the call as
    /// the model made it, so the decision the governed path records later is found. The
    /// default reports `approval.store_unsupported`; the stores in adk-core and adk-session
    /// implement it.
    async fn record_pending_keyed(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        let _ = (scope, fingerprint, request);
        Err(crate::approval::store_unsupported("record_pending_keyed"))
    }

    /// Takes the live decision for `fingerprint` with what the approver attached to it —
    /// edited arguments or a message — consuming it.
    ///
    /// The governed path takes decisions through this method. The default wraps
    /// [`take_decision`](Self::take_decision), so a store that predates edits reports
    /// bare decisions. An expired decision is discarded and reported as `None`.
    async fn take_approval(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolApproval>> {
        Ok(self.take_decision(scope, fingerprint).await?.map(|decision| match decision {
            ToolConfirmationDecision::Approve => ToolApproval::approve(),
            ToolConfirmationDecision::Deny => ToolApproval::deny(),
        }))
    }

    /// Stores a new [`ApprovalRequest`] unless one with the same ID exists, and returns
    /// the stored request: `request` when it was created, otherwise the existing one,
    /// unchanged.
    ///
    /// Backs [`ApprovalService::submit`](crate::ApprovalService::submit). The default
    /// reports `approval.store_unsupported`.
    async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let _ = request;
        Err(crate::approval::store_unsupported("create_request"))
    }

    /// Reads the request with `id`.
    ///
    /// The default reports `approval.store_unsupported`.
    async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        let _ = id;
        Err(crate::approval::store_unsupported("get_request"))
    }

    /// Replaces the stored request with the same ID by `request` when the stored
    /// [`version`](ApprovalRequest::version) equals `expected_version`, atomically.
    ///
    /// Returns `false`, writing nothing, when the request is missing or its version
    /// differs, so of several writers that read the same version exactly one succeeds.
    /// The default reports `approval.store_unsupported`.
    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> Result<bool> {
        let _ = (request, expected_version);
        Err(crate::approval::store_unsupported("replace_request"))
    }

    /// Lists stored requests matching `query`, ordered by creation time and then ID.
    ///
    /// The default reports `approval.store_unsupported`.
    async fn list_requests(&self, query: &ApprovalRequestQuery) -> Result<Vec<ApprovalRequest>> {
        let _ = query;
        Err(crate::approval::store_unsupported("list_requests"))
    }

    /// Stores `delegation`, replacing one with the same ID.
    ///
    /// The default reports `approval.store_unsupported`.
    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
        let _ = delegation;
        Err(crate::approval::store_unsupported("put_delegation"))
    }

    /// Lists the delegations granted to `to`, live or not, ordered by ID.
    ///
    /// The default reports `approval.store_unsupported`.
    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        let _ = to;
        Err(crate::approval::store_unsupported("delegations_to"))
    }

    /// Removes the delegation with `id`. Returns whether one was removed.
    ///
    /// The default reports `approval.store_unsupported`.
    async fn delete_delegation(&self, id: &str) -> Result<bool> {
        let _ = id;
        Err(crate::approval::store_unsupported("delete_delegation"))
    }
}

#[derive(Debug, Default)]
struct ScopeApprovals {
    pending: HashMap<String, PendingApproval>,
    decisions: HashMap<String, ToolApproval>,
}

/// An [`ApprovalStore`] held in process memory.
///
/// Pending requests, decisions, approval requests, and delegations survive between
/// runs of one process; they do not survive a restart.
#[derive(Debug, Default)]
pub struct InMemoryApprovalStore {
    scopes: tokio::sync::RwLock<HashMap<ApprovalScope, ScopeApprovals>>,
    requests: tokio::sync::RwLock<std::collections::BTreeMap<String, ApprovalRequest>>,
    delegations: tokio::sync::RwLock<std::collections::BTreeMap<String, ApprovalDelegation>>,
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
        self.record_pending_keyed(scope, &request.fingerprint(), request).await
    }

    async fn record_pending_keyed(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        let fingerprint = fingerprint.to_string();
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
        Ok(self.take_approval(scope, fingerprint).await?.map(|approval| approval.decision))
    }

    async fn take_approval(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolApproval>> {
        let mut scopes = self.scopes.write().await;
        let Some(entries) = scopes.get_mut(scope) else {
            return Ok(None);
        };
        Ok(entries.decisions.remove(fingerprint).filter(|approval| approval.is_live(Utc::now())))
    }

    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        let scopes = self.scopes.read().await;
        Ok(scopes
            .get(scope)
            .and_then(|entries| entries.decisions.get(fingerprint))
            .is_some_and(|approval| approval.is_live(Utc::now())))
    }

    async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let mut requests = self.requests.write().await;
        Ok(requests.entry(request.id.clone()).or_insert_with(|| request.clone()).clone())
    }

    async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        Ok(self.requests.read().await.get(id).cloned())
    }

    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> Result<bool> {
        let mut requests = self.requests.write().await;
        match requests.get_mut(&request.id) {
            Some(stored) if stored.version == expected_version => {
                *stored = request.clone();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn list_requests(&self, query: &ApprovalRequestQuery) -> Result<Vec<ApprovalRequest>> {
        let requests = self.requests.read().await;
        let mut matching: Vec<ApprovalRequest> =
            requests.values().filter(|request| query.matches(request)).cloned().collect();
        matching.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        matching.truncate(query.limit);
        Ok(matching)
    }

    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
        self.delegations.write().await.insert(delegation.id.clone(), delegation.clone());
        Ok(())
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        Ok(self
            .delegations
            .read()
            .await
            .values()
            .filter(|delegation| delegation.to == *to)
            .cloned()
            .collect())
    }

    async fn delete_delegation(&self, id: &str) -> Result<bool> {
        Ok(self.delegations.write().await.remove(id).is_some())
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
    /// Who published the tool and what it declared. [`for_tool`](Self::for_tool) sets it;
    /// a request held for this call records it.
    pub origin: Option<ToolOrigin>,
    /// The held request this call resumes, if it is one. See [`resuming`](Self::resuming).
    pub resumes: Option<ToolConfirmationRequest>,
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
            origin: None,
            resumes: None,
        }
    }

    /// A call to `tool`, taking its name, declared [`Tool::effect`], and
    /// [`ToolOrigin`] from the tool.
    pub fn for_tool(tool: &dyn Tool, function_call_id: impl Into<String>, args: Value) -> Self {
        let mut call = Self::new(tool.name(), function_call_id, args).with_effect(tool.effect());
        call.origin = Some(ToolOrigin::of(tool));
        call
    }

    /// Marks the call as the resumption of `held`, a call held for confirmation in an
    /// earlier run.
    ///
    /// The governed path refuses it, as not run, unless the acting agent is the one that
    /// held it and the tool has the origin and declaration it was held for: another MCP
    /// server answering the tool's name, or a tool redefined after the approval, is not
    /// the call the approver saw. The refusal consumes any decision for `held` in the
    /// run's [`ApprovalStore`].
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{
    ///     ApprovalScope, GovernedCall, RunConfig, ToolAuthorization, ToolConfirmationRequest,
    ///     ToolGate, ToolOrigin,
    /// };
    /// use serde_json::json;
    ///
    /// # futures::executor::block_on(async {
    /// let config = RunConfig::default();
    /// let gate = ToolGate::new(&config, "agent", "inv-2", ApprovalScope::new("app", "u", "s"));
    /// let declared = json!({ "name": "wire" });
    /// let held = ToolConfirmationRequest::new("wire", json!({ "amount": 40 }))
    ///     .with_acting_context(gate.acting_context().clone())
    ///     .with_origin(ToolOrigin::new("mcp:bank_a", &declared, None));
    ///
    /// // The tool name now resolves to another server.
    /// let mut call = GovernedCall::new("wire", "call-1", json!({ "amount": 40 })).resuming(held);
    /// call.origin = Some(ToolOrigin::new("mcp:bank_b", &declared, None));
    /// assert!(matches!(
    ///     adk_core::authorize_tool_call(&gate, call).await?,
    ///     ToolAuthorization::Refuse { .. }
    /// ));
    /// # Ok::<(), adk_core::AdkError>(())
    /// # }).unwrap();
    /// ```
    #[must_use]
    pub fn resuming(mut self, held: ToolConfirmationRequest) -> Self {
        self.resumes = Some(held);
        self
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
    acting: crate::ActingContext,
    screen: Option<&'a dyn ToolCallScreen>,
    abandon: Option<tokio::sync::watch::Receiver<bool>>,
    actor: Option<AuditActor>,
}

impl<'a> ToolGate<'a> {
    /// A gate over `run_config` for calls made by `agent_name` in `invocation_id`.
    ///
    /// The calls carry the acting context
    /// [`RunConfig::acting_context_for`] gives for the agent and scope; replace it with
    /// [`with_acting_context`](Self::with_acting_context).
    pub fn new(
        run_config: &'a RunConfig,
        agent_name: &'a str,
        invocation_id: &'a str,
        scope: ApprovalScope,
    ) -> Self {
        let acting = run_config.acting_context_for(&scope.app_name, agent_name, &scope.user_id);
        Self {
            run_config,
            agent_name,
            invocation_id,
            scope,
            acting,
            screen: None,
            abandon: None,
            actor: None,
        }
    }

    /// A gate reading the run config and identity of `ctx`.
    pub fn for_context(ctx: &'a dyn InvocationContext) -> Self {
        Self::new(
            ctx.run_config(),
            ctx.agent_name(),
            ctx.invocation_id(),
            ApprovalScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id()),
        )
        .with_acting_context(ctx.acting_context())
    }

    /// Sets the acting context every policy request, held call, and audit record of
    /// this gate's calls carries.
    #[must_use]
    pub fn with_acting_context(mut self, acting: crate::ActingContext) -> Self {
        self.acting = acting;
        self
    }

    /// The acting context calls are evaluated and recorded under.
    pub fn acting_context(&self) -> &crate::ActingContext {
        &self.acting
    }

    /// Sets who the audit records of this gate's calls name as acting.
    ///
    /// Without one, the actor is the gate's [acting context](Self::acting_context).
    #[must_use]
    pub fn with_actor(mut self, actor: AuditActor) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Records one governed decision through the run's [`RunConfig::audit`], if any.
    async fn record(&self, draft: AuditRecordDraft) -> Result<()> {
        let Some(trail) = &self.run_config.audit else {
            return Ok(());
        };
        let actor =
            self.actor.clone().unwrap_or_else(|| AuditActor::new(&self.acting, self.invocation_id));
        trail
            .record(
                draft.actor(actor).ext("sessionId", Value::String(self.scope.session_id.clone())),
            )
            .await
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
    let GovernedCall {
        tool_name,
        function_call_id,
        mut args,
        effect,
        requires_confirmation,
        origin,
        resumes,
    } = call;
    let audited = AuditedCall {
        gate,
        tool_name: &tool_name,
        function_call_id: &function_call_id,
        effect,
        origin: origin.as_ref(),
    };
    if let Err(frozen) = config.check_governance() {
        audited.decision(&args, AuditDecision::Denied, Some(POLICY_REF_GOVERNANCE), None).await?;
        return Err(frozen);
    }

    if let Some(held) = &resumes
        && let Some(mismatch) = resume_mismatch(held, &gate.acting, origin.as_ref())
    {
        tracing::warn!(
            tool.name = %tool_name,
            agent.id = %gate.acting.agent.id,
            function_call.id = %function_call_id,
            reason = %mismatch,
            "held call not resumed: it is not the call it was held for"
        );
        // The decision was for the call the approver saw, so it is spent with it.
        if let Some(store) = &config.approval_store {
            store.take_approval(&gate.scope, &held.fingerprint()).await?;
        }
        audited
            .decision(&args, AuditDecision::Denied, Some(POLICY_REF_CONFIRMATION), Some(&mismatch))
            .await?;
        return Ok(ToolAuthorization::Refuse {
            reason: format!(
                "Tool '{tool_name}' was not run: {mismatch}. Call it again if it is still \
                 needed; the new call needs its own approval."
            ),
            confirmation: None,
        });
    }

    let policy_ref = config.tool_policy.as_ref().map(|policy| match policy.policy_ref() {
        Some(name) => format!("{POLICY_REF_TOOL_POLICY}:{name}"),
        None => POLICY_REF_TOOL_POLICY.to_string(),
    });
    let mut decision = audited.evaluate_policy(&args).await;
    if !matches!(decision, PolicyDecision::Deny { .. })
        && let Some(screen) = gate.screen
    {
        match screen.screen(&tool_name, &args).await {
            Ok(screened) => {
                if screened != args {
                    // The policy approved the arguments it saw, not the rewrite.
                    let again = audited.evaluate_policy(&screened).await;
                    decision = decision.stricter(again);
                    args = screened;
                }
            }
            Err(reason) => {
                // The screen's reason can quote the arguments, so it stays out of the record.
                audited
                    .decision(&args, AuditDecision::Denied, Some(POLICY_REF_GUARDRAIL), None)
                    .await?;
                return Ok(ToolAuthorization::Refuse { reason, confirmation: None });
            }
        }
    }

    let approval_reason = match decision {
        PolicyDecision::Allow => None,
        PolicyDecision::RequireApproval { reason } => Some(reason),
        PolicyDecision::Deny { reason } => {
            tracing::info!(
                tool.name = %tool_name,
                agent.id = %gate.acting.agent.id,
                policy.reason = %reason,
                "tool call denied by policy"
            );
            audited
                .decision(&args, AuditDecision::Denied, policy_ref.as_deref(), Some(&reason))
                .await?;
            return Ok(ToolAuthorization::Refuse {
                reason: format!("Tool '{tool_name}' denied by policy: {reason}"),
                confirmation: None,
            });
        }
    };

    if !requires_confirmation && approval_reason.is_none() {
        tracing::debug!(
            tool.name = %tool_name,
            agent.id = %gate.acting.agent.id,
            "tool call allowed by policy"
        );
        audited.decision(&args, AuditDecision::Allowed, policy_ref.as_deref(), None).await?;
        return Ok(ToolAuthorization::Execute { args, confirmation: None });
    }

    let request = ToolConfirmationRequest {
        tool_name: tool_name.clone(),
        function_call_id: Some(function_call_id.clone()),
        args: args.clone(),
        acting: Some(Box::new(gate.acting.clone())),
        origin: origin.clone(),
    };
    let Some((approval, source)) = resolve_confirmation(gate, &request).await? else {
        let held_by = if approval_reason.is_some() {
            policy_ref.as_deref()
        } else {
            Some(POLICY_REF_CONFIRMATION)
        };
        audited
            .decision(&args, AuditDecision::ApprovalRequired, held_by, approval_reason.as_deref())
            .await?;
        let reason = match approval_reason {
            Some(reason) => format!("Tool '{tool_name}' requires confirmation: {reason}"),
            None => format!("Tool '{tool_name}' requires confirmation"),
        };
        record_held_call(gate, &request, &reason).await?;
        return Ok(ToolAuthorization::Pending { request, reason });
    };
    close_waiting_call(gate, &request, source).await?;
    match approval.decision {
        ToolConfirmationDecision::Approve => {
            let edited = approval.edited_args.filter(|edited| *edited != args);
            audited.approval(&args, AuditDecision::Approved, source, edited.as_ref()).await?;
            // A freeze can land while a person is deciding.
            if let Err(frozen) = config.check_governance() {
                audited
                    .decision(&args, AuditDecision::Denied, Some(POLICY_REF_GOVERNANCE), None)
                    .await?;
                return Err(frozen);
            }
            match edited {
                Some(edited) => authorize_edited_args(&audited, edited).await,
                None => {
                    audited
                        .decision(
                            &args,
                            AuditDecision::Allowed,
                            Some(POLICY_REF_CONFIRMATION),
                            None,
                        )
                        .await?;
                    Ok(ToolAuthorization::Execute {
                        args,
                        confirmation: Some(ToolConfirmationDecision::Approve),
                    })
                }
            }
        }
        ToolConfirmationDecision::Deny => {
            // The approver's message is free text, so it stays out of the record.
            audited.approval(&args, AuditDecision::Rejected, source, None).await?;
            audited
                .decision(&args, AuditDecision::Denied, Some(POLICY_REF_CONFIRMATION), None)
                .await?;
            Ok(ToolAuthorization::Refuse {
                reason: match approval.message {
                    Some(message) => format!(
                        "Tool '{tool_name}' was not run; the approver declined it: {message}"
                    ),
                    None => format!("Tool '{tool_name}' execution denied by confirmation policy"),
                },
                confirmation: Some(ToolConfirmationDecision::Deny),
            })
        }
    }
}

/// Records `request` as waiting for a decision: pending in the run's [`ApprovalStore`]
/// and submitted to its [`ApprovalService`](crate::ApprovalService), when it has them.
async fn record_held_call(
    gate: &ToolGate<'_>,
    request: &ToolConfirmationRequest,
    reason: &str,
) -> Result<()> {
    let config = gate.run_config;
    if let Some(store) = &config.approval_store {
        store.record_pending(&gate.scope, request).await?;
    }
    if let Some(service) = &config.approval_service {
        let call = crate::approval::ApprovalCall {
            scope: gate.scope.clone(),
            request: request.clone(),
            reason: reason.to_string(),
            agent_name: gate.agent_name.to_string(),
            invocation_id: gate.invocation_id.to_string(),
            run_id: config.durable_run_id.clone(),
            acting: Some(gate.acting.clone()),
        };
        service.submit_call(call).await?;
    }
    Ok(())
}

/// How long [`close_waiting_call`] waits for the outcome of a request another decision
/// resolved to reach the store before it fails the call rather than race that write.
const SETTLE_WITHIN: Duration = Duration::from_secs(10);

/// Closes what a decided call left waiting: its open requests in the run's
/// [`ApprovalService`](crate::ApprovalService), which are withdrawn, and its pending entry
/// in the run's [`ApprovalStore`], so neither can authorize a later call with the same
/// fingerprint (spec 3.3 §7.3, §11 S-I1). A decision taken from the store already cleared
/// the entry.
///
/// A request another decision resolved first writes its outcome to the store after its
/// state changes; that outcome is waited for and absorbed, never overwritten, so it cannot
/// land after the entry is cleared. So is an outcome already applied, and anything recorded
/// for the call while the handler was asked: the store was read before it, so a decision
/// there now was made for this call (spec 3.3 §7.3).
async fn close_waiting_call(
    gate: &ToolGate<'_>,
    request: &ToolConfirmationRequest,
    source: &str,
) -> Result<()> {
    let config = gate.run_config;
    let fingerprint = request.fingerprint();
    let note = format!("the call was decided another way ({source})");
    // Whether a decision for the call may have reached the store: the store was read
    // without one before this decision, or through a request for the call.
    let mut absorb = matches!(source, "handler" | "handler_timeout" | "withdrawn");
    if let Some(service) = &config.approval_service {
        // Every request not yet applied: pending, or resolved with its outcome on the way. A
        // request already applied wrote its outcome, which may not have been taken yet.
        let mut query = crate::InboxQuery::default()
            .with_session(&gate.scope)
            .with_states(crate::ApprovalState::ALL);
        let mut open = Vec::new();
        loop {
            let page = service.inbox(query.clone()).await?;
            for item in page.items.into_iter().filter(|item| item.fingerprint == fingerprint) {
                if item.is_open() {
                    open.push(item.id);
                } else {
                    absorb = true;
                }
            }
            match page.next_cursor {
                Some(cursor) => query = query.with_cursor(Some(cursor)),
                None => break,
            }
        }
        for id in open {
            absorb |= settle_request(service.as_ref(), &id, &note).await?;
        }
    }
    if let Some(store) = &config.approval_store {
        let pending = source != "approval_store"
            && store
                .pending(&gate.scope)
                .await?
                .iter()
                .any(|entry| entry.fingerprint == fingerprint);
        if pending {
            store
                .decide(&gate.scope, &fingerprint, ToolApproval::deny().with_message(note))
                .await?;
        }
        if pending || absorb {
            store.take_approval(&gate.scope, &fingerprint).await?;
        }
    }
    Ok(())
}

/// Withdraws request `id`, or, when another decision resolved it first, waits until its
/// outcome is in the store. Returns whether an outcome may have been written for the call.
///
/// A version conflict means the request is still pending (a partial quorum or a fired
/// escalation step moved it), so the withdrawal is retried on the request as it now is.
async fn settle_request(
    service: &dyn crate::ApprovalService,
    id: &str,
    note: &str,
) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + SETTLE_WITHIN;
    let mut pause = Duration::from_millis(5);
    loop {
        match service.withdraw(id, Some(note.to_string())).await {
            Ok(_) => return Ok(true),
            Err(error) if error.code == "approval.not_found" => return Ok(false),
            Err(error)
                if error.code == "approval.version_conflict"
                    || error.code == "approval.already_resolved" => {}
            Err(error) => return Err(error),
        }
        let still_pending = match service.get(id).await? {
            None => return Ok(false),
            Some(current) if current.outcome_applied => return Ok(true),
            // Still pending at a newer version: withdraw it again.
            Some(current)
                if !current.state.is_terminal() && tokio::time::Instant::now() < deadline =>
            {
                continue;
            }
            // Resolved by another decision whose outcome is still being written, or still
            // pending at the deadline.
            Some(current) => !current.state.is_terminal(),
        };
        if tokio::time::Instant::now() >= deadline {
            let why = if still_pending {
                "it kept changing while it was being withdrawn and is still pending; the call \
                 is not run, so retry it"
            } else {
                "another decision resolved it, but its outcome has not reached the approval \
                 store; the call is not run, so retry it once the approval service has \
                 recorded the outcome"
            };
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Unavailable,
                "approval.outcome_unsettled",
                format!("approval request '{id}' did not settle within {SETTLE_WITHIN:?}: {why}"),
            ));
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_millis(200));
    }
}

/// Withdraws a call the governed path held when it will not wait for its decision: its
/// open requests in the run's [`ApprovalService`](crate::ApprovalService) are withdrawn and
/// its pending entry in the run's [`ApprovalStore`] is cleared, so a later decision on
/// them cannot authorize a call with the same fingerprint (spec 3.3 §7.3).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{
///     ApprovalScope, ApprovalStore, InMemoryApprovalStore, RunConfig, ToolConfirmationRequest,
///     ToolGate, withdraw_held_call,
/// };
/// use serde_json::json;
///
/// # futures::executor::block_on(async {
/// let store = Arc::new(InMemoryApprovalStore::new());
/// let config = RunConfig::builder().approval_store(store.clone()).build();
/// let scope = ApprovalScope::new("app", "user", "session");
/// let gate = ToolGate::new(&config, "agent", "inv-1", scope.clone());
/// let request = ToolConfirmationRequest::new("pay", json!({ "cents": 500 }))
///     .with_acting_context(gate.acting_context().clone());
/// store.record_pending(&scope, &request).await.unwrap();
///
/// withdraw_held_call(&gate, &request).await.unwrap();
/// assert!(store.pending(&scope).await.unwrap().is_empty());
/// assert!(!store.has_decision(&scope, &request.fingerprint()).await.unwrap());
/// # });
/// ```
///
/// # Errors
///
/// Returns the store's or the service's error, or `approval.outcome_unsettled` when a
/// request for the call was resolved but its outcome did not reach the store in time.
pub async fn withdraw_held_call(
    gate: &ToolGate<'_>,
    request: &ToolConfirmationRequest,
) -> Result<()> {
    close_waiting_call(gate, request, "withdrawn").await
}

/// Decides a confirmation that a tool asks for while it executes, through the decision
/// sources of the governed path.
///
/// A tool whose own checks escalate a call — a payment that a payment policy sends for
/// review — asks here, so the escalation is answered like a call held before execution:
/// by a decision for the call ID (bound to the request's fingerprint), a live approval
/// keyed by fingerprint ([`RunConfig::tool_approvals`]), a decision in the run's
/// [`ApprovalStore`], which this call consumes, or the confirmation handler, bounded by
/// [`RunConfig::tool_confirmation_timeout`]. Build `request` with the acting context and
/// the tool's [`ToolOrigin`], so the decision binds to the agent and the tool that asked.
///
/// Without a decision, `request` is recorded as pending in the approval store and
/// submitted to the approval service, and `None` is returned. The tool then sets
/// `request` as its event's
/// [`tool_confirmation`](crate::EventActions::tool_confirmation) and does not act: the
/// agent holds the call, and dispatches it again once a decision exists. The caller
/// records its own outcome; this function writes no audit record.
///
/// # Errors
///
/// Returns an error when the run is frozen before or after the decision, or when the
/// handler, the approval store, or the approval service fails.
///
/// # Example
///
/// ```rust
/// use adk_core::{
///     ApprovalScope, ApprovalStore, InMemoryApprovalStore, RunConfig, ToolApproval,
///     ToolConfirmationDecision, ToolConfirmationRequest, ToolGate,
/// };
/// use serde_json::json;
/// use std::sync::Arc;
///
/// # futures::executor::block_on(async {
/// let store = Arc::new(InMemoryApprovalStore::new());
/// let config = RunConfig::builder().approval_store(store.clone()).build();
/// let scope = ApprovalScope::new("shop", "user-1", "session-1");
/// let gate = ToolGate::new(&config, "buyer", "inv-1", scope.clone());
/// let request = ToolConfirmationRequest::new("pay", json!({ "cents": 75_000 }))
///     .with_function_call_id("call-1")
///     .with_acting_context(gate.acting_context().clone());
///
/// // No decision yet: the request waits in the approval store.
/// assert!(adk_core::confirm_tool_call(&gate, &request, "over $50").await?.is_none());
/// assert_eq!(store.pending(&scope).await?.len(), 1);
///
/// // An approver decides it; the next attempt consumes the approval.
/// store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await?;
/// let approval = adk_core::confirm_tool_call(&gate, &request, "over $50").await?;
/// assert_eq!(approval.map(|a| a.decision), Some(ToolConfirmationDecision::Approve));
/// assert!(!store.has_decision(&scope, &request.fingerprint()).await?);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
pub async fn confirm_tool_call(
    gate: &ToolGate<'_>,
    request: &ToolConfirmationRequest,
    reason: &str,
) -> Result<Option<ToolApproval>> {
    let config = gate.run_config;
    config.check_governance()?;
    let Some((approval, source)) = resolve_confirmation(gate, request).await? else {
        tracing::info!(
            tool.name = %request.tool_name,
            agent.id = %gate.acting.agent.id,
            "tool call held for confirmation during execution"
        );
        record_held_call(gate, request, reason).await?;
        return Ok(None);
    };
    close_waiting_call(gate, request, source).await?;
    tracing::info!(
        tool.name = %request.tool_name,
        agent.id = %gate.acting.agent.id,
        confirmation.source = source,
        decision = ?approval.decision,
        "confirmation decided during execution"
    );
    // A freeze can land while a person is deciding.
    config.check_governance()?;
    Ok(Some(approval))
}

/// Why `held` cannot resume as a call by `acting`'s agent to a tool with `origin`, or
/// `None` when it can.
fn resume_mismatch(
    held: &ToolConfirmationRequest,
    acting: &crate::ActingContext,
    origin: Option<&ToolOrigin>,
) -> Option<String> {
    let Some(held_by) = held.acting.as_deref() else {
        return Some("the hold does not record which agent made the call".to_string());
    };
    if held_by.agent.id != acting.agent.id {
        return Some(format!(
            "it was held for agent '{}', and '{}' is resuming it",
            held_by.agent.id, acting.agent.id
        ));
    }
    let (Some(held_origin), Some(origin)) = (held.origin.as_ref(), origin) else {
        return Some("the hold does not record which tool the call was made to".to_string());
    };
    if held_origin.source != origin.source {
        return Some(format!(
            "it was held for the tool '{}' publishes, and the name now resolves to '{}'",
            held_origin.source, origin.source
        ));
    }
    if held_origin.declaration_digest != origin.declaration_digest {
        return Some(format!(
            "'{}' changed its description, parameters, or annotations after the call was held",
            held_origin.source
        ));
    }
    None
}

/// Re-authorizes a call whose approver substituted `edited` for its arguments.
///
/// The edit is a new call with a new fingerprint, so it passes the policy and the tool
/// screen again; the approval stands in for confirmation, because the approver chose
/// exactly these arguments. A policy denial of the edit, or of a screen's rewrite of
/// it, refuses the call.
async fn authorize_edited_args(
    audited: &AuditedCall<'_, '_>,
    edited: Value,
) -> Result<ToolAuthorization> {
    let (gate, tool_name) = (audited.gate, audited.tool_name);
    let policy_ref = gate.run_config.tool_policy.as_ref().map(|policy| match policy.policy_ref() {
        Some(name) => format!("{POLICY_REF_TOOL_POLICY}:{name}"),
        None => POLICY_REF_TOOL_POLICY.to_string(),
    });
    let mut decision = audited.evaluate_policy(&edited).await;
    let mut args = edited;
    if !matches!(decision, PolicyDecision::Deny { .. })
        && let Some(screen) = gate.screen
    {
        match screen.screen(tool_name, &args).await {
            Ok(screened) => {
                if screened != args {
                    let again = audited.evaluate_policy(&screened).await;
                    decision = decision.stricter(again);
                    args = screened;
                }
            }
            Err(reason) => {
                audited
                    .decision(&args, AuditDecision::Denied, Some(POLICY_REF_GUARDRAIL), None)
                    .await?;
                return Ok(ToolAuthorization::Refuse {
                    reason,
                    confirmation: Some(ToolConfirmationDecision::Approve),
                });
            }
        }
    }
    if let PolicyDecision::Deny { reason } = decision {
        tracing::info!(tool.name = %tool_name, policy.reason = %reason, "edited tool call denied by policy");
        audited
            .decision(&args, AuditDecision::Denied, policy_ref.as_deref(), Some(&reason))
            .await?;
        return Ok(ToolAuthorization::Refuse {
            reason: format!(
                "Tool '{tool_name}' with the approver's edited arguments denied by policy: {reason}"
            ),
            confirmation: Some(ToolConfirmationDecision::Approve),
        });
    }
    tracing::info!(tool.name = %tool_name, "executing tool call with the approver's edited arguments");
    audited.decision(&args, AuditDecision::Allowed, Some(POLICY_REF_CONFIRMATION), None).await?;
    Ok(ToolAuthorization::Execute { args, confirmation: Some(ToolConfirmationDecision::Approve) })
}

/// `policy_ref` of a decision made by the kill switch.
const POLICY_REF_GOVERNANCE: &str = "governance";
/// `policy_ref` of a decision made by the run's [`ToolPolicy`](crate::ToolPolicy), followed
/// by `:<name>` when the policy names itself.
const POLICY_REF_TOOL_POLICY: &str = "tool_policy";
/// `policy_ref` of a refusal by the agent's tool screen.
const POLICY_REF_GUARDRAIL: &str = "guardrail";
/// `policy_ref` of a call decided by its confirmation.
const POLICY_REF_CONFIRMATION: &str = "confirmation";

/// One call on its way through [`authorize_tool_call`], for its policy evaluations and
/// audit records.
struct AuditedCall<'g, 'a> {
    gate: &'g ToolGate<'a>,
    tool_name: &'g str,
    function_call_id: &'g str,
    effect: ToolEffect,
    origin: Option<&'g ToolOrigin>,
}

impl AuditedCall<'_, '_> {
    fn draft(&self, kind: AuditRecordKind, args: &Value) -> Option<AuditRecordDraft> {
        let trail = self.gate.run_config.audit.as_ref()?;
        let mut draft = AuditRecordDraft::new(kind, self.tool_name)
            .args_digest(trail.digest(args))
            .ext("functionCallId", Value::String(self.function_call_id.to_string()))
            .ext("effect", serde_json::to_value(self.effect).unwrap_or(Value::Null));
        // Which publisher's tool, as declared, the decision was for.
        if let Some(origin) = self.origin {
            draft = draft
                .ext("toolSource", Value::String(origin.source.clone()))
                .ext("declarationDigest", Value::String(origin.declaration_digest.clone()));
        }
        Some(draft)
    }

    /// Asks the run's policy about the call with `args`, passing its origin and ID.
    async fn evaluate_policy(&self, args: &Value) -> PolicyDecision {
        let call = PolicyCall {
            origin: self.origin,
            function_call_id: Some(self.function_call_id),
            durable_run_id: self.gate.run_config.durable_run_id.as_deref(),
        };
        evaluate_policy(self.gate, self.tool_name, args, self.effect, call).await
    }

    /// Records the governed path's decision for the call.
    async fn decision(
        &self,
        args: &Value,
        decision: AuditDecision,
        policy_ref: Option<&str>,
        reason: Option<&str>,
    ) -> Result<()> {
        let Some(mut draft) = self.draft(AuditRecordKind::ToolCall, args) else {
            return Ok(());
        };
        draft = draft.decision(decision);
        if decision != AuditDecision::Allowed {
            draft = draft.outcome(AuditRecordOutcome::NotRun);
        }
        if let Some(policy_ref) = policy_ref {
            draft = draft.policy_ref(policy_ref);
        }
        if let Some(reason) = reason {
            draft = draft.ext("reason", Value::String(reason.to_string()));
        }
        self.gate.record(draft).await
    }

    /// Records the approval decision the call was given, where it came from, and the
    /// digest of arguments the approver substituted.
    async fn approval(
        &self,
        args: &Value,
        decision: AuditDecision,
        source: &str,
        edited: Option<&Value>,
    ) -> Result<()> {
        let Some(mut draft) = self.draft(AuditRecordKind::Approval, args) else {
            return Ok(());
        };
        draft = draft.decision(decision).ext("source", Value::String(source.to_string()));
        if let (Some(edited), Some(trail)) = (edited, &self.gate.run_config.audit) {
            draft = draft.ext("editedArgsDigest", Value::String(trail.digest(edited)));
        }
        self.gate.record(draft).await
    }
}

/// Asks the run's policy about a call to `tool_name` with `args`, described by `call`.
async fn evaluate_policy(
    gate: &ToolGate<'_>,
    tool_name: &str,
    args: &Value,
    effect: ToolEffect,
    call: PolicyCall<'_>,
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
        acting: Some(gate.acting.clone()),
    };
    policy.evaluate_call(&request, &call).await
}

/// Finds the decision for `request`, asking the live handler last.
async fn resolve_confirmation(
    gate: &ToolGate<'_>,
    request: &ToolConfirmationRequest,
) -> Result<Option<(ToolApproval, &'static str)>> {
    let config = gate.run_config;
    let fingerprint = request.fingerprint();
    let bare = |decision: ToolConfirmationDecision| match decision {
        ToolConfirmationDecision::Approve => ToolApproval::approve(),
        ToolConfirmationDecision::Deny => ToolApproval::deny(),
    };

    if let Some(call_id) = request.function_call_id.as_deref()
        && let Some(decision) = config.tool_confirmation_decisions.get(call_id)
    {
        match config.tool_confirmation_fingerprints.get(call_id) {
            Some(expected) if *expected != fingerprint => tracing::warn!(
                tool.name = %request.tool_name,
                function_call.id = %call_id,
                "confirmation decision does not match this call's arguments, treating as unconfirmed"
            ),
            _ => return Ok(Some((bare(*decision), "call_id"))),
        }
    }

    if let Some(approval) = config.tool_approvals.get(&fingerprint) {
        if approval.is_live(Utc::now()) {
            return Ok(Some((approval.clone(), "fingerprint")));
        }
        tracing::debug!(tool.name = %request.tool_name, "approval for this call has expired");
    }

    if let Some(store) = &config.approval_store
        && let Some(approval) = store.take_approval(&gate.scope, &fingerprint).await?
    {
        return Ok(Some((approval, "approval_store")));
    }

    let Some(handler) = config.tool_confirmation_handler.as_ref() else {
        return Ok(None);
    };
    let timeout = config.tool_confirmation_timeout;
    let decide = Box::pin(tokio::time::timeout(timeout, handler.decide_approval(request)));
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
    // Nothing is decided by silence where a later decision can be read back (spec 3.3 §7.3).
    if decided.is_err()
        && !config.deny_on_confirmation_timeout
        && let Some(store) = &config.approval_store
    {
        // A decision may have reached the store while the handler was asked.
        if let Some(approval) = store.take_approval(&gate.scope, &fingerprint).await? {
            return Ok(Some((approval, "approval_store")));
        }
        tracing::info!(
            tool.name = %request.tool_name,
            timeout = ?config.tool_confirmation_timeout,
            "confirmation handler did not decide in time; the call waits for a decision"
        );
        return Ok(None);
    }
    match decided {
        Ok(Ok(approval)) => Ok(Some((approval, "handler"))),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            tracing::warn!(
                tool.name = %request.tool_name,
                timeout_secs = timeout.as_secs(),
                "confirmation handler did not decide in time, denying the call"
            );
            Ok(Some((ToolApproval::deny(), "handler_timeout")))
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
///
/// The held call is usually the call the request describes. An agent tool whose
/// delegated agent held a call of its own holds the agent-tool call instead: its
/// request describes the delegated agent's call, so `name` and `args` name the
/// agent-tool call the agent resumes, while the request's fingerprint stays the key
/// of the decision the delegated call needs.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldToolCall {
    /// The request the agent surfaced. Its arguments are the call's final ones,
    /// after plugin and callback rewrites; its fingerprint is the decision key.
    pub request: ToolConfirmationRequest,
    /// The call's ID: the one the model issued, or the one the agent generated for a
    /// call its provider sent without one ([`GENERATED_CALL_ID_PREFIX`](crate::GENERATED_CALL_ID_PREFIX)).
    /// `None` only for a call held before agents generated IDs.
    pub id: Option<String>,
    /// The tool the model called, the one the agent resumes.
    pub name: String,
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
/// A call is matched to its request by call ID. An agent gives every call its
/// provider sent without an ID a generated one before saving it, so two calls to one
/// tool in one turn keep their own arguments. Calls saved without an ID by an earlier
/// release are matched by tool name within their invocation.
///
/// Only events visible on `branch` and newer than the latest compaction count, as in
/// the conversation history the agent sees. The runner never compacts a held call
/// (see [`held_call_compaction_limit`]).
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
///     acting: None,
///     origin: None,
/// });
///
/// let calls = held_tool_calls(&[held], "agent", "");
/// assert_eq!(calls.len(), 1);
/// assert_eq!(calls[0].request.tool_name, "pay");
/// ```
pub fn held_tool_calls(events: &[Event], agent_name: &str, branch: &str) -> Vec<HeldToolCall> {
    scan_held_calls(events, agent_name, branch).into_iter().map(|held| held.call).collect()
}

/// A held call with the positions of the events that issued and held it.
struct ScannedHold {
    call: HeldToolCall,
    /// The event whose function call the hold resumes, or the hold itself when that
    /// call is not in the log.
    issued_at: usize,
    /// The event that carries the request.
    held_at: usize,
}

fn scan_held_calls(events: &[Event], agent_name: &str, branch: &str) -> Vec<ScannedHold> {
    let boundary = events
        .iter()
        .rev()
        .find_map(|event| event.actions.compaction.as_ref().map(|c| c.end_timestamp));
    // Calls the agent issued, by ID; calls saved without one by invocation and name.
    let mut issued = HashMap::<String, (String, Value, usize)>::new();
    let mut idless = Vec::<(String, String, Value, usize)>::new();
    let mut held = Vec::<ScannedHold>::new();

    for (index, event) in events.iter().enumerate() {
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
                        issued.insert(id.clone(), (name.clone(), args.clone(), index));
                    }
                    None => idless.push((
                        event.invocation_id.clone(),
                        name.clone(),
                        args.clone(),
                        index,
                    )),
                },
                Part::FunctionResponse { function_response, id, .. }
                    if !is_confirmation_pending(&function_response.response) =>
                {
                    match id {
                        Some(id) => {
                            let answered = issued
                                .get(id)
                                .map(|(name, args, _)| tool_call_fingerprint(name, args));
                            held.retain(|hold| {
                                let call = &hold.call;
                                call.request.function_call_id.as_deref() != Some(id)
                                    && call.id.as_deref() != Some(id)
                                    && answered.as_deref()
                                        != Some(&tool_call_fingerprint(&call.name, &call.args))
                            });
                        }
                        None => held.retain(|hold| {
                            hold.call.id.is_some() || hold.call.name != function_response.name
                        }),
                    }
                }
                _ => {}
            }
        }
        if own && let Some(request) = &event.actions.tool_confirmation {
            let (id, name, args, issued_at) = match request.function_call_id.as_deref() {
                Some(call_id) if issued.contains_key(call_id) => {
                    let (name, args, at) = &issued[call_id];
                    (Some(call_id.to_string()), name.clone(), args.clone(), *at)
                }
                _ => match idless.iter().rev().find(|(invocation, name, _, _)| {
                    *invocation == event.invocation_id && *name == request.tool_name
                }) {
                    Some((_, name, args, at)) => (None, name.clone(), args.clone(), *at),
                    None => (
                        request.function_call_id.clone(),
                        request.tool_name.clone(),
                        request.args.clone(),
                        index,
                    ),
                },
            };
            let fingerprint = request.fingerprint();
            held.retain(|hold| hold.call.request.fingerprint() != fingerprint);
            held.push(ScannedHold {
                call: HeldToolCall { request: request.clone(), id, name, args },
                issued_at,
                held_at: index,
            });
        }
    }
    held
}

/// Every hold in `events`, by any agent on any branch.
fn scan_all_held_calls(events: &[Event]) -> Vec<ScannedHold> {
    let mut holders = Vec::<(&str, &str)>::new();
    for event in events {
        if event.actions.tool_confirmation.is_some()
            && !holders.contains(&(event.author.as_str(), event.branch.as_str()))
        {
            holders.push((event.author.as_str(), event.branch.as_str()));
        }
    }
    let mut held = Vec::<ScannedHold>::new();
    for (agent, branch) in holders {
        for hold in scan_held_calls(events, agent, branch) {
            if !held.iter().any(|known| known.held_at == hold.held_at) {
                held.push(hold);
            }
        }
    }
    held
}

/// Returns how many leading events of `events`, at most `end`, a compaction may cover
/// without hiding a call still held for confirmation.
///
/// A compaction hides every event up to its end timestamp, and an agent finds held
/// calls only among the events a compaction left visible, so compacting a held call
/// would leave its approval with nothing to resume. The limit therefore stops before
/// the first event that is not older than the earliest event that issued a held call,
/// by any agent on any branch. The runner applies it to session compaction,
/// intra-invocation compaction and context compaction alike.
///
/// # Example
///
/// ```rust
/// use adk_core::{Content, Event, Part, ToolConfirmationRequest, held_call_compaction_limit};
/// use serde_json::json;
///
/// let mut ask = Event::new("inv-1");
/// ask.author = "user".into();
/// ask.llm_response.content = Some(Content::new("user").with_text("pay 5"));
/// let mut call = Event::new("inv-1");
/// call.author = "ops".into();
/// call.llm_response.content = Some(Content {
///     role: "model".into(),
///     parts: vec![Part::FunctionCall {
///         name: "pay".into(),
///         args: json!({ "amount": 5 }),
///         id: Some("c1".into()),
///         thought_signature: None,
///     }],
/// });
/// let mut hold = Event::new("inv-1");
/// hold.author = "ops".into();
/// hold.actions.tool_confirmation = Some(ToolConfirmationRequest {
///     tool_name: "pay".into(),
///     function_call_id: Some("c1".into()),
///     args: json!({ "amount": 5 }),
///     acting: None,
///     origin: None,
/// });
///
/// let events = vec![ask, call, hold];
/// // Only the user message before the held call may be compacted.
/// assert_eq!(held_call_compaction_limit(&events, events.len()), 1);
/// assert_eq!(held_call_compaction_limit(&events[..1], 1), 1);
/// ```
pub fn held_call_compaction_limit(events: &[Event], end: usize) -> usize {
    let end = end.min(events.len());
    let earliest = scan_all_held_calls(events)
        .iter()
        .map(|hold| events[hold.issued_at].timestamp.min(events[hold.held_at].timestamp))
        .min();
    match earliest {
        Some(from) => events[..end].iter().position(|event| event.timestamp >= from).unwrap_or(end),
        None => end,
    }
}

/// Returns where `workflow` resumes: the positions it recorded on the holds that
/// stopped its last run, while those calls are still held.
///
/// A workflow agent records its [`WorkflowPosition`](crate::WorkflowPosition) on each
/// confirmation event that passes through it ([`EventActions::workflow_positions`](crate::EventActions::workflow_positions)).
/// A recorded hold counts while its call is still held and the agent that held it has
/// not run in a later invocation; of those, the holds from the latest invocation are
/// the ones that stopped the workflow. An empty result means the workflow starts over.
///
/// `branch` is the workflow's own branch; holds recorded on it or below it count.
///
/// # Example
///
/// ```rust
/// use adk_core::{Event, ToolConfirmationRequest, WorkflowPosition, workflow_resume_positions};
/// use serde_json::json;
///
/// let mut hold = Event::new("inv-1");
/// hold.author = "ops".into();
/// hold.actions.tool_confirmation = Some(ToolConfirmationRequest {
///     tool_name: "pay".into(),
///     function_call_id: Some("c1".into()),
///     args: json!({ "amount": 5 }),
///     acting: None,
///     origin: None,
/// });
/// hold.actions.workflow_positions.insert("pipeline".into(), WorkflowPosition::new(0, "ops"));
///
/// let mut events = vec![hold];
/// assert_eq!(workflow_resume_positions(&events, "pipeline", ""), vec![WorkflowPosition::new(0, "ops")]);
///
/// // Once `ops` runs again in a later invocation, the hold no longer stops the workflow.
/// let mut later = Event::new("inv-2");
/// later.author = "ops".into();
/// events.push(later);
/// assert!(workflow_resume_positions(&events, "pipeline", "").is_empty());
/// ```
pub fn workflow_resume_positions(
    events: &[Event],
    workflow: &str,
    branch: &str,
) -> Vec<crate::WorkflowPosition> {
    let within = |event_branch: &str| {
        branch.is_empty()
            || event_branch == branch
            || event_branch.strip_prefix(branch).is_some_and(|rest| rest.starts_with('.'))
    };
    let stopping = scan_all_held_calls(events)
        .into_iter()
        .filter(|hold| {
            let event = &events[hold.held_at];
            event.actions.workflow_positions.contains_key(workflow)
                && within(&event.branch)
                && !events[hold.held_at + 1..].iter().any(|later| {
                    later.author == event.author && later.invocation_id != event.invocation_id
                })
        })
        .map(|hold| hold.held_at)
        .collect::<Vec<_>>();
    let Some(last) = stopping.iter().max().map(|&index| &events[index].invocation_id) else {
        return Vec::new();
    };
    let mut held_at = stopping
        .iter()
        .copied()
        .filter(|&index| events[index].invocation_id == *last)
        .collect::<Vec<_>>();
    held_at.sort_unstable();
    let mut positions = Vec::new();
    for index in held_at {
        let position = &events[index].actions.workflow_positions[workflow];
        if !positions.contains(position) {
            positions.push(position.clone());
        }
    }
    positions
}

/// Puts each function response in `content` in the place of the pending response to
/// the same call in `history`, and returns the parts that answered no pending call.
///
/// A call held for confirmation is answered with a [`confirmation_pending_response`];
/// when a later run resumes it, its new response takes the pending one's place, so the
/// history keeps one response per call and providers that reject a second response
/// accept it. A response matches the latest pending response with its call ID; one
/// without an ID, as saved by an earlier release, matches the latest pending response
/// to the same tool that has none either.
///
/// # Example
///
/// ```rust
/// use adk_core::{
///     Content, FunctionResponseData, Part, confirmation_pending_response,
///     supersede_pending_responses,
/// };
/// use serde_json::json;
///
/// let response = |value| Part::FunctionResponse {
///     function_response: FunctionResponseData::new("pay", value),
///     id: Some("c1".into()),
///     annotations: None,
/// };
/// let mut history =
///     vec![Content { role: "function".into(), parts: vec![response(confirmation_pending_response("held"))] }];
/// let resumed = Content { role: "function".into(), parts: vec![response(json!({ "paid": 5 }))] };
///
/// assert!(supersede_pending_responses(&mut history, resumed).is_none());
/// assert_eq!(history.len(), 1);
/// assert!(matches!(&history[0].parts[0], Part::FunctionResponse { function_response, .. }
///     if function_response.response == json!({ "paid": 5 })));
/// ```
pub fn supersede_pending_responses(
    history: &mut [crate::Content],
    content: crate::Content,
) -> Option<crate::Content> {
    let mut unplaced = Vec::new();
    for part in content.parts {
        let slot = match &part {
            Part::FunctionResponse { function_response, id, .. } => history
                .iter_mut()
                .rev()
                .flat_map(|held| held.parts.iter_mut().rev())
                .find(|candidate| match candidate {
                    Part::FunctionResponse {
                        function_response: pending, id: pending_id, ..
                    } => {
                        is_confirmation_pending(&pending.response)
                            && pending_id == id
                            && (id.is_some() || pending.name == function_response.name)
                    }
                    _ => false,
                }),
            _ => None,
        };
        match slot {
            Some(slot) => *slot = part,
            None => unplaced.push(part),
        }
    }
    (!unplaced.is_empty()).then_some(crate::Content { role: content.role, parts: unplaced })
}

/// Whether a decision for `request` may be available to the governed path.
///
/// Reports `true` for a call-ID decision that matches the call's fingerprint, a live
/// fingerprint approval, a decision in the run's [`ApprovalStore`] (asked through
/// [`ApprovalStore::has_decision`], which consumes nothing), or a configured
/// [`ToolConfirmationHandler`](crate::ToolConfirmationHandler), which decides when
/// asked. A call waiting in the store's pending entries is not put to the handler again:
/// it waits for a decision recorded for it, so a run that brings a new message goes on to
/// the model instead of asking about the waiting call (spec 3.3 §7.3, §7.4). An agent
/// resumes a [`HeldToolCall`] only when this is `true`.
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
///     acting: None,
///     origin: None,
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
    let Some(store) = &config.approval_store else {
        return Ok(config.tool_confirmation_handler.is_some());
    };
    if store.has_decision(&gate.scope, &fingerprint).await? {
        return Ok(true);
    }
    if config.tool_confirmation_handler.is_none() {
        return Ok(false);
    }
    // A call waiting in the store resumes on its decision, never by asking the handler again.
    Ok(!store.pending(&gate.scope).await?.iter().any(|entry| entry.fingerprint == fingerprint))
}

// ---------------------------------------------------------------------------
// Provider-executed built-in tools
// ---------------------------------------------------------------------------

/// Event-level `provider_metadata` key listing the built-in tools withheld from the
/// model request that produced the event, as a JSON array of [`WithheldBuiltinTool`].
pub const BUILTIN_TOOLS_WITHHELD_KEY: &str = "adk.builtin_tools.withheld";

/// Declaration keys that carry a provider-native tool definition.
const BUILTIN_TOOL_MARKERS: [&str; 3] =
    ["x-adk-gemini-tool", "x-adk-openai-tool", "x-adk-anthropic-tool"];

/// Returns the provider-native definition a tool declaration carries, if any.
///
/// Built-in tools declare themselves to the provider adapters under
/// `x-adk-gemini-tool`, `x-adk-openai-tool` or `x-adk-anthropic-tool`. The value is
/// the tool's static configuration — allowed domains, maximum uses, store names — and
/// is what a [`ToolPolicy`](crate::ToolPolicy) sees as the arguments of a built-in tool.
///
/// # Example
///
/// ```rust
/// use adk_core::builtin_tool_configuration;
/// use serde_json::json;
///
/// let declaration = json!({
///     "name": "web_search",
///     "x-adk-anthropic-tool": { "type": "web_search_20250305", "max_uses": 3 }
/// });
/// assert_eq!(builtin_tool_configuration(&declaration).unwrap()["max_uses"], 3);
/// assert!(builtin_tool_configuration(&json!({ "name": "lookup" })).is_none());
/// ```
pub fn builtin_tool_configuration(declaration: &Value) -> Option<&Value> {
    BUILTIN_TOOL_MARKERS.iter().find_map(|marker| declaration.get(*marker))
}

/// Why a built-in tool was left out of a model request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuiltinToolWithholding {
    /// The run's policy denied the tool.
    PolicyDenied,
    /// The policy requires approval and no decision exists yet.
    ApprovalRequired,
    /// An approver denied the tool.
    ApprovalDenied,
    /// The run's tool-call budget has no calls left.
    BudgetExhausted,
}

/// A built-in tool left out of a model request, and why.
///
/// # Example
///
/// ```rust
/// use adk_core::{BuiltinToolWithholding, WithheldBuiltinTool};
///
/// let withheld = WithheldBuiltinTool {
///     tool_name: "web_search".into(),
///     cause: BuiltinToolWithholding::PolicyDenied,
///     reason: "no web access".into(),
/// };
/// let json = serde_json::to_value(&withheld).unwrap();
/// assert_eq!(json["cause"], "policyDenied");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WithheldBuiltinTool {
    /// Name of the withheld tool.
    pub tool_name: String,
    /// Why it was withheld.
    pub cause: BuiltinToolWithholding,
    /// Human-readable explanation.
    pub reason: String,
}

/// Governs the provider-executed built-in tools of each model request in one invocation.
///
/// A built-in tool — Gemini Google Search, OpenAI web search, Anthropic web search and
/// the rest — runs inside the provider, so no call reaches [`authorize_tool_call`].
/// The governor applies the same controls before the request is sent, to every
/// declaration carrying a provider-native definition (see
/// [`builtin_tool_configuration`]) whose tool reports [`Tool::is_builtin`] or is not
/// among the agent's tools:
///
/// 1. The kill switch — a frozen run ends with an error.
/// 2. The run's tool-call budget — with no calls left, every built-in tool is withheld.
/// 3. The run's [`ToolPolicy`](crate::ToolPolicy), with the tool's name, its declared
///    [`Tool::effect`] and its provider-native configuration as the arguments.
///
/// | Decision | Result |
/// |----------|--------|
/// | `Allow` | Attached |
/// | `Deny` | Withheld |
/// | `RequireApproval` | Attached only when a decision already exists: a live fingerprint approval ([`RunConfig::tool_approvals`]) or a decision in the run's [`ApprovalStore`]. Otherwise withheld and recorded as pending in the store, so an approver can decide before the next run. |
///
/// The provider executes a built-in tool mid-response, so the confirmation handler
/// and call-ID decisions do not apply. The fingerprint is that of a
/// [`ToolConfirmationRequest`] by the gate's acting agent for the tool name with its
/// configuration as the arguments, carrying the tool's [`ToolOrigin`] (or
/// [`BUILTIN_TOOL_ORIGIN`] with the declaration when the agent
/// has no tool for it). A decision taken from the store applies for the rest of the
/// invocation.
///
/// Without a policy and without a tool-call limit every built-in tool is attached.
///
/// # Example
///
/// ```rust
/// use adk_core::{
///     ApprovalScope, BuiltinToolGovernor, DeclarativePolicy, RunConfig, Tool, ToolGate,
/// };
/// use serde_json::json;
/// use std::collections::HashMap;
/// use std::sync::Arc;
///
/// # futures::executor::block_on(async {
/// let config = RunConfig::builder()
///     .tool_policy(Arc::new(DeclarativePolicy::builder().allow("lookup").build()))
///     .build();
/// let gate = ToolGate::new(&config, "agent", "inv-1", ApprovalScope::new("app", "u", "s"));
/// let mut declarations = HashMap::from([
///     ("lookup".to_string(), json!({ "name": "lookup" })),
///     ("google_search".to_string(), json!({ "x-adk-gemini-tool": { "google_search": {} } })),
/// ]);
///
/// let tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
/// let withheld = BuiltinToolGovernor::new().govern(&gate, &mut declarations, &tools).await?;
/// assert_eq!(withheld[0].tool_name, "google_search");
/// assert!(declarations.contains_key("lookup"));
/// assert!(!declarations.contains_key("google_search"));
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Debug, Default)]
pub struct BuiltinToolGovernor {
    /// Approval decisions by fingerprint, so a consumed store decision lasts the invocation.
    decisions: HashMap<String, ToolConfirmationDecision>,
    /// Tools whose withholding has been logged.
    reported: HashSet<String>,
}

impl BuiltinToolGovernor {
    /// Creates a governor with no decisions; use one per invocation.
    pub fn new() -> Self {
        Self::default()
    }

    /// Removes every built-in tool the run may not use from `declarations` and
    /// returns what was removed, sorted by tool name.
    ///
    /// `tools` is the agent's tool map; it supplies each tool's declared effect and
    /// tells client-executed tools with a provider-native declaration (Anthropic
    /// bash and text editor) apart from built-in ones. Client-executed tools are
    /// governed when they are called, not here.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is frozen or the approval store fails.
    pub async fn govern(
        &mut self,
        gate: &ToolGate<'_>,
        declarations: &mut HashMap<String, Value>,
        tools: &HashMap<String, Arc<dyn Tool>>,
    ) -> Result<Vec<WithheldBuiltinTool>> {
        let config = gate.run_config;
        config.check_governance()?;
        let mut builtin: Vec<String> = declarations
            .iter()
            .filter(|(name, declaration)| {
                builtin_tool_configuration(declaration).is_some()
                    && tools.get(name.as_str()).is_none_or(|tool| tool.is_builtin())
            })
            .map(|(name, _)| name.clone())
            .collect();
        if builtin.is_empty() {
            return Ok(Vec::new());
        }
        builtin.sort_unstable();
        let budget_exhausted =
            config.budget_tracker.as_ref().and_then(|tracker| tracker.tool_calls_remaining())
                == Some(0);

        let mut withheld = Vec::new();
        for name in builtin {
            let outcome = if budget_exhausted {
                Some((
                    BuiltinToolWithholding::BudgetExhausted,
                    "the run's tool-call budget has no calls left".to_string(),
                ))
            } else {
                let args = declarations
                    .get(&name)
                    .and_then(builtin_tool_configuration)
                    .cloned()
                    .unwrap_or(Value::Null);
                let tool = tools.get(&name);
                let effect = tool.map_or(ToolEffect::NonIdempotent, |tool| tool.effect());
                let origin = match tool {
                    Some(tool) => ToolOrigin::of(tool.as_ref()),
                    None => ToolOrigin::new(
                        BUILTIN_TOOL_ORIGIN,
                        declarations.get(&name).unwrap_or(&Value::Null),
                        None,
                    ),
                };
                self.decide(gate, &name, args, effect, origin).await?
            };
            let Some((cause, reason)) = outcome else {
                continue;
            };
            declarations.remove(&name);
            if self.reported.insert(name.clone()) {
                tracing::info!(
                    tool.name = %name,
                    builtin_tool.cause = ?cause,
                    policy.reason = %reason,
                    "built-in tool withheld from the model request"
                );
            }
            withheld.push(WithheldBuiltinTool { tool_name: name, cause, reason });
        }
        Ok(withheld)
    }

    async fn decide(
        &mut self,
        gate: &ToolGate<'_>,
        name: &str,
        args: Value,
        effect: ToolEffect,
        origin: ToolOrigin,
    ) -> Result<Option<(BuiltinToolWithholding, String)>> {
        let call = PolicyCall {
            origin: Some(&origin),
            function_call_id: None,
            durable_run_id: gate.run_config.durable_run_id.as_deref(),
        };
        let approval_reason = match evaluate_policy(gate, name, &args, effect, call).await {
            PolicyDecision::Allow => return Ok(None),
            PolicyDecision::Deny { reason } => {
                return Ok(Some((BuiltinToolWithholding::PolicyDenied, reason)));
            }
            PolicyDecision::RequireApproval { reason } => reason,
        };
        let config = gate.run_config;
        let request = ToolConfirmationRequest::new(name, args)
            .with_acting_context(gate.acting.clone())
            .with_origin(origin);
        let fingerprint = request.fingerprint();
        let mut decision = self.decisions.get(&fingerprint).copied();
        if decision.is_none()
            && let Some(approval) = config.tool_approvals.get(&fingerprint)
            && approval.is_live(Utc::now())
        {
            decision = Some(approval.decision);
        }
        if decision.is_none()
            && let Some(store) = &config.approval_store
        {
            decision = store.take_decision(&gate.scope, &fingerprint).await?;
        }
        match decision {
            Some(decision) => {
                self.decisions.insert(fingerprint, decision);
                Ok(match decision {
                    ToolConfirmationDecision::Approve => None,
                    ToolConfirmationDecision::Deny => Some((
                        BuiltinToolWithholding::ApprovalDenied,
                        "an approver denied this built-in tool".to_string(),
                    )),
                })
            }
            None => {
                if let Some(store) = &config.approval_store {
                    store.record_pending(&gate.scope, &request).await?;
                }
                Ok(Some((BuiltinToolWithholding::ApprovalRequired, approval_reason)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActingContext, AgentPrincipal, ApprovalService, DeclarativePolicy, PolicyRule,
        ToolConfirmationHandler,
    };
    use serde_json::json;

    fn gate(config: &RunConfig) -> ToolGate<'_> {
        ToolGate::new(config, "agent", "inv", ApprovalScope::new("app", "user", "session"))
    }

    /// The acting context of `agent` in the test app.
    fn acting(agent: &str) -> ActingContext {
        ActingContext::new(AgentPrincipal::derived("app", agent))
    }

    /// The decision key of a call by the test gate's agent to a tool known only by name.
    fn key(name: &str, args: &Value) -> String {
        ToolConfirmationRequest::new(name, args.clone())
            .with_acting_context(acting("agent"))
            .fingerprint()
    }

    struct Native {
        name: &'static str,
        builtin: bool,
        effect: ToolEffect,
    }

    #[async_trait]
    impl Tool for Native {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "native"
        }
        fn is_builtin(&self) -> bool {
            self.builtin
        }
        fn effect(&self) -> ToolEffect {
            self.effect
        }
        async fn execute(&self, _ctx: Arc<dyn crate::ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }

    fn native_tools() -> (HashMap<String, Value>, HashMap<String, Arc<dyn Tool>>) {
        let declarations = HashMap::from([
            ("lookup".to_string(), json!({ "name": "lookup" })),
            (
                "web_search".to_string(),
                json!({ "x-adk-anthropic-tool": { "type": "web_search_20250305", "max_uses": 2 } }),
            ),
            (
                "code_exec".to_string(),
                json!({ "x-adk-openai-tool": { "type": "code_interpreter" } }),
            ),
            ("bash".to_string(), json!({ "x-adk-anthropic-tool": { "type": "bash_20250124" } })),
        ]);
        let tools: HashMap<String, Arc<dyn Tool>> = HashMap::from([
            (
                "web_search".to_string(),
                Arc::new(Native { name: "web_search", builtin: true, effect: ToolEffect::ReadOnly })
                    as Arc<dyn Tool>,
            ),
            (
                "bash".to_string(),
                Arc::new(Native { name: "bash", builtin: false, effect: ToolEffect::NonIdempotent })
                    as Arc<dyn Tool>,
            ),
        ]);
        (declarations, tools)
    }

    fn names(declarations: &HashMap<String, Value>) -> Vec<&str> {
        let mut names: Vec<&str> = declarations.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    #[tokio::test]
    async fn builtin_tools_stay_attached_without_a_policy() {
        let config = RunConfig::default();
        let (mut declarations, tools) = native_tools();
        let withheld = BuiltinToolGovernor::new()
            .govern(&gate(&config), &mut declarations, &tools)
            .await
            .unwrap();
        assert!(withheld.is_empty());
        assert_eq!(names(&declarations), ["bash", "code_exec", "lookup", "web_search"]);
    }

    #[tokio::test]
    async fn default_deny_withholds_builtin_tools_and_leaves_client_tools_to_call_time() {
        let policy = DeclarativePolicy::builder().allow("lookup").build();
        let config = RunConfig::builder().tool_policy(Arc::new(policy)).build();
        let (mut declarations, tools) = native_tools();
        let withheld = BuiltinToolGovernor::new()
            .govern(&gate(&config), &mut declarations, &tools)
            .await
            .unwrap();
        assert_eq!(names(&declarations), ["bash", "lookup"]);
        assert_eq!(
            withheld.iter().map(|w| (w.tool_name.as_str(), w.cause)).collect::<Vec<_>>(),
            [
                ("code_exec", BuiltinToolWithholding::PolicyDenied),
                ("web_search", BuiltinToolWithholding::PolicyDenied),
            ]
        );
    }

    #[tokio::test]
    async fn effect_and_configuration_reach_the_policy() {
        let policy = DeclarativePolicy::builder()
            .rule(
                PolicyRule::allow("*")
                    .with_effect(ToolEffect::ReadOnly)
                    .when(crate::ArgPredicate::at_most("/max_uses", 3.0)),
            )
            .build();
        let config = RunConfig::builder().tool_policy(Arc::new(policy)).build();
        let (mut declarations, tools) = native_tools();
        BuiltinToolGovernor::new().govern(&gate(&config), &mut declarations, &tools).await.unwrap();
        // `code_exec` has no local tool, so it is treated as non-idempotent.
        assert_eq!(names(&declarations), ["bash", "lookup", "web_search"]);
    }

    #[tokio::test]
    async fn require_approval_withholds_until_a_decision_exists() {
        let policy = DeclarativePolicy::builder()
            .require_approval("web_search", "web access needs sign-off")
            .allow("*")
            .build();
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder()
            .tool_policy(Arc::new(policy))
            .approval_store(store.clone())
            .build();
        let (mut declarations, tools) = native_tools();
        let withheld = BuiltinToolGovernor::new()
            .govern(&gate(&config), &mut declarations, &tools)
            .await
            .unwrap();
        assert_eq!(withheld.len(), 1);
        assert_eq!(withheld[0].cause, BuiltinToolWithholding::ApprovalRequired);
        assert_eq!(withheld[0].reason, "web access needs sign-off");
        assert!(!declarations.contains_key("web_search"));

        let scope = ApprovalScope::new("app", "user", "session");
        let pending = store.pending(&scope).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request.function_call_id, None);
        store.decide(&scope, &pending[0].fingerprint, ToolApproval::approve()).await.unwrap();

        // The next invocation takes the decision once and keeps it for every turn.
        let mut governor = BuiltinToolGovernor::new();
        for _ in 0..2 {
            let (mut declarations, tools) = native_tools();
            let withheld =
                governor.govern(&gate(&config), &mut declarations, &tools).await.unwrap();
            assert!(withheld.is_empty());
            assert!(declarations.contains_key("web_search"));
        }
    }

    #[tokio::test]
    async fn a_fingerprint_approval_attaches_and_a_denial_withholds() {
        let policy =
            DeclarativePolicy::builder().require_approval("web_search", "sign-off").build();
        let config_args = json!({ "type": "web_search_20250305", "max_uses": 2 });
        let (_, tools) = native_tools();
        let fingerprint =
            ToolConfirmationRequest::for_tool(tools["web_search"].as_ref(), config_args.clone())
                .with_acting_context(acting("agent"))
                .fingerprint();
        for (approval, attached) in [(ToolApproval::approve(), true), (ToolApproval::deny(), false)]
        {
            let config = RunConfig::builder()
                .tool_policy(Arc::new(policy.clone()))
                .tool_approval(fingerprint.clone(), approval)
                .build();
            let (mut declarations, tools) = native_tools();
            let withheld = BuiltinToolGovernor::new()
                .govern(&gate(&config), &mut declarations, &tools)
                .await
                .unwrap();
            assert_eq!(declarations.contains_key("web_search"), attached);
            if !attached {
                assert!(withheld.iter().any(|w| w.tool_name == "web_search"
                    && w.cause == BuiltinToolWithholding::ApprovalDenied));
            }
        }
    }

    #[tokio::test]
    async fn an_exhausted_tool_budget_withholds_and_a_frozen_run_fails() {
        let tracker =
            Arc::new(crate::BudgetTracker::new(crate::RunBudget::new().max_tool_calls(1)));
        tracker.begin_tool_calls(1).unwrap();
        let control = GovernanceControl::new();
        let mut config = RunConfig::builder().governance(control.clone()).build();
        config.budget_tracker = Some(tracker);
        let (mut declarations, tools) = native_tools();
        let withheld = BuiltinToolGovernor::new()
            .govern(&gate(&config), &mut declarations, &tools)
            .await
            .unwrap();
        assert!(withheld.iter().all(|w| w.cause == BuiltinToolWithholding::BudgetExhausted));
        assert_eq!(names(&declarations), ["bash", "lookup"]);

        control.freeze("drill");
        let (mut declarations, tools) = native_tools();
        let error = BuiltinToolGovernor::new()
            .govern(&gate(&config), &mut declarations, &tools)
            .await
            .unwrap_err();
        assert_eq!(error.code, "governance.frozen");
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

    /// A timeout with an approval store holds the call there; a decision recorded later
    /// authorizes the call once (spec 3.3 §7.3, §10 C2).
    #[tokio::test]
    async fn a_timeout_with_an_approval_store_leaves_the_call_waiting() {
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(NeverDecides))
            .tool_confirmation_timeout(Duration::from_millis(20))
            .approval_store(store.clone())
            .build();
        let call = || GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);

        let ToolAuthorization::Pending { request, reason } =
            authorize_tool_call(&gate(&config), call()).await.unwrap()
        else {
            panic!("a timed-out call with a store waits for a decision");
        };
        assert_eq!(reason, "Tool 'tool' requires confirmation");
        let scope = ApprovalScope::new("app", "user", "session");
        let pending = store.pending(&scope).await.unwrap();
        assert_eq!(pending.len(), 1, "the call waits in the store");
        assert_eq!(pending[0].request, request);

        store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await.unwrap();
        assert_eq!(
            authorize_tool_call(&gate(&config), call()).await.unwrap(),
            ToolAuthorization::Execute {
                args: json!({}),
                confirmation: Some(ToolConfirmationDecision::Approve),
            },
            "the stored decision authorizes the call before the handler is asked"
        );
        assert!(
            matches!(
                authorize_tool_call(&gate(&config), call()).await.unwrap(),
                ToolAuthorization::Pending { .. }
            ),
            "the decision was used once"
        );
    }

    /// The service the timeout waits in for the tests below: an in-memory store under a
    /// service whose requests the `ops` group decides.
    fn ops_inbox() -> (Arc<InMemoryApprovalStore>, Arc<crate::StoreApprovalService>) {
        let store = Arc::new(InMemoryApprovalStore::new());
        let service = Arc::new(
            crate::StoreApprovalService::new(store.clone())
                .with_router(crate::ApprovalPolicy::new([crate::ApproverSelector::group("ops")])),
        );
        (store, service)
    }

    fn ops_approval() -> crate::ApprovalDecision {
        crate::ApprovalDecision::approve(SubjectRef::human("ana")).with_groups(["ops"])
    }

    /// A timeout with an approval service, whose store the builder installs, submits the
    /// call to its inbox and waits there.
    #[tokio::test]
    async fn a_timeout_with_an_approval_service_waits_in_its_inbox() {
        let (_, service) = ops_inbox();
        let config = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(NeverDecides))
            .tool_confirmation_timeout(Duration::from_millis(20))
            .approval_service(service.clone())
            .build();
        let call = GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        assert!(matches!(
            authorize_tool_call(&gate(&config), call).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
        let inbox = service.inbox(crate::InboxQuery::for_group("ops")).await.unwrap();
        assert_eq!(inbox.items.len(), 1, "the call waits in the service's inbox");
        assert_eq!(inbox.items[0].tool_name, "tool");
    }

    /// Review finding 5: with an approval service and no store to read its decisions from,
    /// a timeout denies, as no decision could ever reach the call.
    #[tokio::test]
    async fn a_timeout_with_only_an_approval_service_is_denied() {
        let (_, service) = ops_inbox();
        let config = RunConfig {
            approval_store: None,
            ..RunConfig::builder()
                .tool_confirmation_handler(Arc::new(NeverDecides))
                .tool_confirmation_timeout(Duration::from_millis(20))
                .approval_service(service.clone())
                .build()
        };
        let call = GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        assert_eq!(
            authorize_tool_call(&gate(&config), call).await.unwrap(),
            ToolAuthorization::Refuse {
                reason: "Tool 'tool' execution denied by confirmation policy".into(),
                confirmation: Some(ToolConfirmationDecision::Deny),
            }
        );
        let inbox = service.inbox(crate::InboxQuery::for_group("ops")).await.unwrap();
        assert!(inbox.items.is_empty(), "nothing waits where no decision can be read back");
    }

    /// Review finding 1: a call waiting in the store is not put to the handler again; it
    /// resumes on a decision recorded for it. A held call with no pending entry still goes
    /// to the handler.
    #[tokio::test]
    async fn a_call_waiting_in_the_store_resumes_only_on_its_decision() {
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(NeverDecides))
            .approval_store(store.clone())
            .build();
        let gate = gate(&config);
        let waiting = ToolConfirmationRequest::new("pay", json!({ "cents": 500 }))
            .with_function_call_id("c1")
            .with_acting_context(gate.acting_context().clone());
        let stranded = ToolConfirmationRequest::new("pay", json!({ "cents": 700 }))
            .with_function_call_id("c2")
            .with_acting_context(gate.acting_context().clone());
        store.record_pending(gate.scope(), &waiting).await.unwrap();

        assert!(
            !confirmation_decision_available(&gate, &waiting).await.unwrap(),
            "the handler is not asked about a call waiting in the store"
        );
        assert!(
            confirmation_decision_available(&gate, &stranded).await.unwrap(),
            "a held call with no pending entry is still asked about"
        );
        store.decide(gate.scope(), &waiting.fingerprint(), ToolApproval::approve()).await.unwrap();
        assert!(confirmation_decision_available(&gate, &waiting).await.unwrap());
    }

    /// Answers every call with `Approve` at once.
    #[derive(Debug)]
    struct Approves;

    #[async_trait]
    impl ToolConfirmationHandler for Approves {
        async fn decide(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> Result<ToolConfirmationDecision> {
            Ok(ToolConfirmationDecision::Approve)
        }
    }

    /// Review finding 2: a call decided by the handler, by a call-ID decision or by a
    /// fingerprint approval leaves no pending entry and no open inbox request that could
    /// authorize a second identical call.
    #[tokio::test]
    async fn a_decision_from_any_source_closes_the_waiting_entries() {
        let call =
            || GovernedCall::new("tool", "c1", json!({ "n": 1 })).requiring_confirmation(true);
        let fingerprint = key("tool", &json!({ "n": 1 }));
        let scope = ApprovalScope::new("app", "user", "session");
        let mut sources =
            Vec::<Box<dyn Fn(crate::RunConfigBuilder) -> crate::RunConfigBuilder>>::new();
        sources.push(Box::new(|builder| builder.tool_confirmation_handler(Arc::new(Approves))));
        sources.push(Box::new(|builder| {
            builder.tool_confirmation_decisions(HashMap::from([(
                "c1".to_string(),
                ToolConfirmationDecision::Approve,
            )]))
        }));
        let approval = fingerprint.clone();
        sources.push(Box::new(move |builder| {
            builder.tool_approval(approval.clone(), ToolApproval::approve())
        }));
        for (index, source) in sources.iter().enumerate() {
            let (store, service) = ops_inbox();
            let held = RunConfig::builder().approval_service(service.clone()).build();
            assert!(matches!(
                authorize_tool_call(&gate(&held), call()).await.unwrap(),
                ToolAuthorization::Pending { .. }
            ));
            let open = service.inbox(crate::InboxQuery::for_group("ops")).await.unwrap().items;
            assert_eq!(open.len(), 1, "source {index}: the call waits as one request");

            let decided = source(RunConfig::builder().approval_service(service.clone())).build();
            assert!(matches!(
                authorize_tool_call(&gate(&decided), call()).await.unwrap(),
                ToolAuthorization::Execute { .. }
            ));
            assert!(store.pending(&scope).await.unwrap().is_empty(), "source {index}: no entry");
            assert!(!store.has_decision(&scope, &fingerprint).await.unwrap(), "source {index}");
            let request = service.get(&open[0].id).await.unwrap().unwrap();
            assert_eq!(request.state, crate::ApprovalState::Cancelled, "source {index}");
            assert!(
                service.decide(&request.id, request.version, ops_approval()).await.is_err(),
                "source {index}: the stale request cannot be approved"
            );
            assert!(
                matches!(
                    authorize_tool_call(&gate(&held), call()).await.unwrap(),
                    ToolAuthorization::Pending { .. }
                ),
                "source {index}: the next identical call needs its own decision"
            );
        }
    }

    /// Records an inbox decision for the call while it is being asked, then never answers.
    #[derive(Debug)]
    struct DecidedInTheInbox(Arc<crate::StoreApprovalService>);

    #[async_trait]
    impl ToolConfirmationHandler for DecidedInTheInbox {
        async fn decide(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> Result<ToolConfirmationDecision> {
            let open = self.0.inbox(crate::InboxQuery::for_group("ops")).await?.items;
            self.0.decide(&open[0].id, open[0].version, ops_approval()).await?;
            std::future::pending().await
        }
    }

    /// Review finding 3: a decision the inbox records while the handler is asked is used
    /// when the handler times out, instead of a second pending entry and request.
    #[tokio::test]
    async fn a_decision_made_in_the_inbox_during_the_wait_is_used() {
        let (store, service) = ops_inbox();
        let call = || GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        let held = RunConfig::builder().approval_service(service.clone()).build();
        assert!(matches!(
            authorize_tool_call(&gate(&held), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
        let asked = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(DecidedInTheInbox(service.clone())))
            .tool_confirmation_timeout(Duration::from_millis(20))
            .approval_service(service.clone())
            .build();
        assert!(matches!(
            authorize_tool_call(&gate(&asked), call()).await.unwrap(),
            ToolAuthorization::Execute { .. }
        ));
        let scope = ApprovalScope::new("app", "user", "session");
        assert!(store.pending(&scope).await.unwrap().is_empty(), "nothing is held again");
        let all = crate::InboxQuery::for_group("ops").with_states(crate::ApprovalState::ALL);
        let requests = service.inbox(all).await.unwrap().items;
        assert_eq!(requests.len(), 1, "no second request");
        assert_eq!(requests[0].state, crate::ApprovalState::Approved);
    }

    /// The service under `ops_inbox`'s, except that the first cancel or withdrawal loses to
    /// an approval that leaves the request pending, as a partial quorum does.
    #[derive(Debug)]
    struct MovedOnce {
        inner: Arc<crate::StoreApprovalService>,
        moved: AtomicBool,
    }

    impl MovedOnce {
        /// Records `ana`'s approval on `id` the first time, and reports the conflict.
        async fn lose_once(&self, id: &str) -> Result<()> {
            if self.moved.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            let current = self.inner.get(id).await?.expect("the request exists");
            self.inner.decide(id, current.version, ops_approval()).await?;
            Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Conflict,
                "approval.version_conflict",
                "another decision landed first",
            ))
        }
    }

    #[async_trait]
    impl crate::ApprovalService for MovedOnce {
        async fn submit(&self, request: ApprovalRequest) -> Result<ApprovalRequest> {
            self.inner.submit(request).await
        }
        async fn submit_call(
            &self,
            call: crate::approval::ApprovalCall,
        ) -> Result<Option<ApprovalRequest>> {
            self.inner.submit_call(call).await
        }
        async fn get(&self, id: &str) -> Result<Option<ApprovalRequest>> {
            self.inner.get(id).await
        }
        async fn inbox(&self, query: crate::InboxQuery) -> Result<crate::InboxPage> {
            self.inner.inbox(query).await
        }
        async fn decide(
            &self,
            id: &str,
            expected_version: u64,
            decision: crate::ApprovalDecision,
        ) -> Result<ApprovalRequest> {
            self.inner.decide(id, expected_version, decision).await
        }
        async fn delegate(&self, delegation: ApprovalDelegation) -> Result<ApprovalDelegation> {
            self.inner.delegate(delegation).await
        }
        async fn revoke_delegation(&self, id: &str) -> Result<bool> {
            self.inner.revoke_delegation(id).await
        }
        async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
            self.inner.delegations_to(to).await
        }
        async fn cancel(&self, id: &str, reason: Option<String>) -> Result<ApprovalRequest> {
            self.lose_once(id).await?;
            self.inner.cancel(id, reason).await
        }
        async fn withdraw(&self, id: &str, reason: Option<String>) -> Result<ApprovalRequest> {
            self.lose_once(id).await?;
            self.inner.withdraw(id, reason).await
        }
        async fn tick(&self, now: DateTime<Utc>) -> Result<crate::ApprovalTick> {
            self.inner.tick(now).await
        }
        fn approval_store(&self) -> Option<Arc<dyn ApprovalStore>> {
            self.inner.approval_store()
        }
    }

    /// Review findings 3 and 5 of #1044: a request a partial quorum moves while the call
    /// is decided another way is still pending, so it is withdrawn at its new version; the
    /// remaining approver cannot then authorize a later identical call.
    #[tokio::test]
    async fn a_request_moved_by_a_partial_quorum_is_still_withdrawn() {
        let store = Arc::new(InMemoryApprovalStore::new());
        let inner = Arc::new(crate::StoreApprovalService::new(store.clone()).with_router(
            crate::ApprovalPolicy::new([crate::ApproverSelector::group("ops")]).with_quorum(2),
        ));
        let service = Arc::new(MovedOnce { inner: inner.clone(), moved: AtomicBool::new(false) });
        let call = || GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        let held = RunConfig::builder().approval_service(service.clone()).build();
        assert!(matches!(
            authorize_tool_call(&gate(&held), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
        let id =
            inner.inbox(crate::InboxQuery::for_group("ops")).await.unwrap().items[0].id.clone();

        let decided = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(Approves))
            .approval_service(service.clone())
            .build();
        assert!(matches!(
            authorize_tool_call(&gate(&decided), call()).await.unwrap(),
            ToolAuthorization::Execute { .. }
        ));
        let request = inner.get(&id).await.unwrap().unwrap();
        assert_eq!(request.state, crate::ApprovalState::Cancelled, "withdrawn at its new version");
        let bob = crate::ApprovalDecision::approve(SubjectRef::human("bob")).with_groups(["ops"]);
        assert!(inner.decide(&id, request.version, bob).await.is_err());
        let scope = ApprovalScope::new("app", "user", "session");
        assert!(!store.has_decision(&scope, &key("tool", &json!({}))).await.unwrap());
        assert!(matches!(
            authorize_tool_call(&gate(&held), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
    }

    /// Review finding 2 of #1044: an approver's decision that resolved the request but has
    /// not yet written its outcome when the call is decided another way is waited for and
    /// absorbed, so it cannot land afterwards and authorize a later identical call.
    #[tokio::test]
    async fn an_inbox_outcome_still_being_written_is_absorbed() {
        let (store, service) = ops_inbox();
        let call = || GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        let held = RunConfig::builder().approval_service(service.clone()).build();
        assert!(matches!(
            authorize_tool_call(&gate(&held), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
        let scope = ApprovalScope::new("app", "user", "session");
        let fingerprint = key("tool", &json!({}));

        // The approver's decision has moved the request to approved; its outcome is
        // written to the store 100 ms later.
        let open = service.inbox(crate::InboxQuery::for_group("ops")).await.unwrap().items;
        let mut resolved = open[0].clone();
        resolved.decisions.push(ops_approval());
        resolved.state = crate::ApprovalState::Approved;
        resolved.resolved_at = Some(Utc::now());
        resolved.version += 1;
        assert!(store.replace_request(&resolved, open[0].version).await.unwrap());
        let writer = {
            let (store, scope, fingerprint) = (store.clone(), scope.clone(), fingerprint.clone());
            let mut applied = resolved.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                store.decide(&scope, &fingerprint, ToolApproval::approve()).await.unwrap();
                applied.outcome_applied = true;
                applied.version += 1;
                assert!(store.replace_request(&applied, resolved.version).await.unwrap());
            })
        };

        let decided = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(Approves))
            .approval_service(service.clone())
            .build();
        assert!(matches!(
            authorize_tool_call(&gate(&decided), call()).await.unwrap(),
            ToolAuthorization::Execute { .. }
        ));
        writer.await.unwrap();
        assert!(!store.has_decision(&scope, &fingerprint).await.unwrap(), "nothing left behind");
        assert!(store.pending(&scope).await.unwrap().is_empty());
        assert!(matches!(
            authorize_tool_call(&gate(&held), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
    }

    /// Approves the call in the inbox while it is being asked — the request is resolved and
    /// its outcome applied before the handler answers — then answers with its own decision.
    #[derive(Debug)]
    struct AnsweredAfterTheInbox {
        service: Arc<crate::StoreApprovalService>,
        answer: ToolConfirmationDecision,
    }

    #[async_trait]
    impl ToolConfirmationHandler for AnsweredAfterTheInbox {
        async fn decide(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> Result<ToolConfirmationDecision> {
            let open = self.service.inbox(crate::InboxQuery::for_group("ops")).await?.items;
            let applied = self.service.decide(&open[0].id, open[0].version, ops_approval()).await?;
            assert!(applied.outcome_applied, "the inbox outcome is in the store");
            Ok(self.answer)
        }
    }

    /// Wave 2 review findings 4–6 (spec 3.3 §7.3): an inbox outcome applied while the
    /// handler is asked, or before a call-ID decision decides the call, is absorbed, so it
    /// cannot authorize the next identical call without anyone being asked.
    #[tokio::test]
    async fn an_inbox_outcome_applied_before_another_decision_is_absorbed() {
        let call = || GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        let scope = ApprovalScope::new("app", "user", "session");
        let fingerprint = key("tool", &json!({}));
        for answer in [ToolConfirmationDecision::Approve, ToolConfirmationDecision::Deny] {
            let (store, service) = ops_inbox();
            let held = RunConfig::builder().approval_service(service.clone()).build();
            assert!(matches!(
                authorize_tool_call(&gate(&held), call()).await.unwrap(),
                ToolAuthorization::Pending { .. }
            ));
            let asked = RunConfig::builder()
                .tool_confirmation_handler(Arc::new(AnsweredAfterTheInbox {
                    service: service.clone(),
                    answer,
                }))
                .approval_service(service.clone())
                .build();
            let decided = authorize_tool_call(&gate(&asked), call()).await.unwrap();
            assert_eq!(
                matches!(decided, ToolAuthorization::Execute { .. }),
                answer == ToolConfirmationDecision::Approve,
                "the handler's answer decides the call: {decided:?}"
            );
            assert!(!store.has_decision(&scope, &fingerprint).await.unwrap(), "{answer:?}");
            assert!(store.pending(&scope).await.unwrap().is_empty());
            assert!(
                matches!(
                    authorize_tool_call(&gate(&held), call()).await.unwrap(),
                    ToolAuthorization::Pending { .. }
                ),
                "{answer:?}: the next identical call needs its own decision"
            );
        }

        // A call-ID decision resumes the call after the inbox approved its request.
        let (store, service) = ops_inbox();
        let held = RunConfig::builder().approval_service(service.clone()).build();
        assert!(matches!(
            authorize_tool_call(&gate(&held), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
        let open = service.inbox(crate::InboxQuery::for_group("ops")).await.unwrap().items;
        service.decide(&open[0].id, open[0].version, ops_approval()).await.unwrap();
        let by_id = RunConfig::builder()
            .tool_confirmation_decisions(HashMap::from([(
                "c1".to_string(),
                ToolConfirmationDecision::Deny,
            )]))
            .approval_service(service.clone())
            .build();
        assert!(matches!(
            authorize_tool_call(&gate(&by_id), call()).await.unwrap(),
            ToolAuthorization::Refuse { .. }
        ));
        assert!(!store.has_decision(&scope, &fingerprint).await.unwrap(), "nothing left behind");
        assert!(matches!(
            authorize_tool_call(&gate(&held), call()).await.unwrap(),
            ToolAuthorization::Pending { .. }
        ));
    }

    /// Wave 2 review finding 15 (spec 3.3 §7.3): a request another decision resolved but
    /// whose outcome never reaches the store fails the call with `approval.outcome_unsettled`
    /// after ten seconds, and the store keeps the call waiting rather than cleared.
    #[tokio::test(start_paused = true)]
    async fn a_request_that_never_settles_fails_the_call() {
        let (store, service) = ops_inbox();
        let call = || GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        let held = RunConfig::builder().approval_service(service.clone()).build();
        let ToolAuthorization::Pending { request, .. } =
            authorize_tool_call(&gate(&held), call()).await.unwrap()
        else {
            panic!("the call waits");
        };
        let open = service.inbox(crate::InboxQuery::for_group("ops")).await.unwrap().items;
        let mut resolved = open[0].clone();
        resolved.decisions.push(ops_approval());
        resolved.state = crate::ApprovalState::Approved;
        resolved.resolved_at = Some(Utc::now());
        resolved.version += 1;
        assert!(store.replace_request(&resolved, open[0].version).await.unwrap());

        let started = tokio::time::Instant::now();
        let error = withdraw_held_call(&gate(&held), &request).await.unwrap_err();
        assert_eq!(error.code, "approval.outcome_unsettled");
        assert!(error.message.contains("has not reached the approval store"), "{error}");
        assert!(started.elapsed() >= SETTLE_WITHIN, "it waited {:?}", started.elapsed());
        let scope = ApprovalScope::new("app", "user", "session");
        assert_eq!(store.pending(&scope).await.unwrap().len(), 1, "the call still waits");
    }

    /// The service under `ops_inbox`'s, except that every withdrawal loses to a change that
    /// leaves the request pending, as an escalation step that keeps firing would.
    #[derive(Debug)]
    struct AlwaysMoved(Arc<crate::StoreApprovalService>);

    #[async_trait]
    impl crate::ApprovalService for AlwaysMoved {
        async fn submit(&self, request: ApprovalRequest) -> Result<ApprovalRequest> {
            self.0.submit(request).await
        }
        async fn submit_call(
            &self,
            call: crate::approval::ApprovalCall,
        ) -> Result<Option<ApprovalRequest>> {
            self.0.submit_call(call).await
        }
        async fn get(&self, id: &str) -> Result<Option<ApprovalRequest>> {
            self.0.get(id).await
        }
        async fn inbox(&self, query: crate::InboxQuery) -> Result<crate::InboxPage> {
            self.0.inbox(query).await
        }
        async fn decide(
            &self,
            id: &str,
            expected_version: u64,
            decision: crate::ApprovalDecision,
        ) -> Result<ApprovalRequest> {
            self.0.decide(id, expected_version, decision).await
        }
        async fn delegate(&self, delegation: ApprovalDelegation) -> Result<ApprovalDelegation> {
            self.0.delegate(delegation).await
        }
        async fn revoke_delegation(&self, id: &str) -> Result<bool> {
            self.0.revoke_delegation(id).await
        }
        async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
            self.0.delegations_to(to).await
        }
        async fn cancel(&self, id: &str, reason: Option<String>) -> Result<ApprovalRequest> {
            self.0.cancel(id, reason).await
        }
        async fn withdraw(&self, _id: &str, _reason: Option<String>) -> Result<ApprovalRequest> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Conflict,
                "approval.version_conflict",
                "another change landed first",
            ))
        }
        async fn tick(&self, now: DateTime<Utc>) -> Result<crate::ApprovalTick> {
            self.0.tick(now).await
        }
        fn approval_store(&self) -> Option<Arc<dyn ApprovalStore>> {
            self.0.approval_store()
        }
    }

    /// Wave 2 review finding 15: a request that is still pending at the deadline, because
    /// every withdrawal lost to another change, fails the call and says it is still pending.
    #[tokio::test(start_paused = true)]
    async fn a_request_that_keeps_changing_fails_the_call_as_still_pending() {
        let (_, inner) = ops_inbox();
        let service = Arc::new(AlwaysMoved(inner));
        let call = GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        let held = RunConfig::builder().approval_service(service).build();
        let ToolAuthorization::Pending { request, .. } =
            authorize_tool_call(&gate(&held), call).await.unwrap()
        else {
            panic!("the call waits");
        };
        let error = withdraw_held_call(&gate(&held), &request).await.unwrap_err();
        assert_eq!(error.code, "approval.outcome_unsettled");
        assert!(error.message.contains("still pending"), "{error}");
    }

    /// `deny_on_confirmation_timeout` restores the denial with a store, and records nothing.
    #[tokio::test]
    async fn the_opt_out_denies_a_timeout_even_with_an_approval_store() {
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(NeverDecides))
            .tool_confirmation_timeout(Duration::from_millis(20))
            .approval_store(store.clone())
            .deny_on_confirmation_timeout(true)
            .build();
        let call = GovernedCall::new("tool", "c1", json!({})).requiring_confirmation(true);
        assert_eq!(
            authorize_tool_call(&gate(&config), call).await.unwrap(),
            ToolAuthorization::Refuse {
                reason: "Tool 'tool' execution denied by confirmation policy".into(),
                confirmation: Some(ToolConfirmationDecision::Deny),
            }
        );
        let scope = ApprovalScope::new("app", "user", "session");
        assert!(store.pending(&scope).await.unwrap().is_empty(), "nothing waits");
    }

    /// A tool's own confirmation that times out with a store waits there too.
    #[tokio::test]
    async fn a_timeout_during_execution_waits_with_an_approval_store() {
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(NeverDecides))
            .tool_confirmation_timeout(Duration::from_millis(20))
            .approval_store(store.clone())
            .build();
        let gate = gate(&config);
        let request = ToolConfirmationRequest::new("pay", json!({ "cents": 75_000 }))
            .with_function_call_id("c1")
            .with_acting_context(gate.acting_context().clone());
        assert_eq!(confirm_tool_call(&gate, &request, "over $50").await.unwrap(), None);
        let scope = ApprovalScope::new("app", "user", "session");
        assert_eq!(store.pending(&scope).await.unwrap().len(), 1);
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
        let fingerprint = key("pay", &args);
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
            acting: None,
            origin: None,
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
                    acting: None,
                    origin: None,
                },
                id: Some("c1".into()),
                name: "pay".into(),
                args: json!({ "amount": 5 }),
            }]
        );
        assert!(held_tool_calls(&events, "other", "").is_empty(), "holds belong to their agent");
    }

    #[test]
    fn an_agent_tool_call_held_for_its_delegates_call_resumes_as_the_agent_tool_call() {
        let events = vec![
            event("user", "inv-1", vec![Part::Text { text: "clean up".into() }]),
            event(
                "ops",
                "inv-1",
                vec![function_call("helper", json!({ "request": "delete a.txt" }), "c1")],
            ),
            event(
                "ops",
                "inv-1",
                vec![function_response("helper", confirmation_pending_response("held"), "c1")],
            ),
            // The request describes the delegate's call; its ID is the agent-tool call's.
            hold("ops", "inv-1", "delete_file", json!({ "path": "a.txt" }), "c1"),
        ];
        let held = held_tool_calls(&events, "ops", "");
        assert_eq!(
            held,
            vec![HeldToolCall {
                request: ToolConfirmationRequest {
                    tool_name: "delete_file".into(),
                    function_call_id: Some("c1".into()),
                    args: json!({ "path": "a.txt" }),
                    acting: None,
                    origin: None,
                },
                id: Some("c1".into()),
                name: "helper".into(),
                args: json!({ "request": "delete a.txt" }),
            }]
        );

        let mut answered = events;
        answered.push(event(
            "ops",
            "inv-2",
            vec![function_response("helper", json!({ "response": "deleted" }), "c1")],
        ));
        assert!(held_tool_calls(&answered, "ops", "").is_empty());
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

    /// Run 1 of agent `ops`: two `transfer` calls with generated IDs, both held.
    fn two_held_transfers() -> Vec<Event> {
        let a = "adk-call-a";
        let b = "adk-call-b";
        vec![
            event("user", "inv-1", vec![Part::Text { text: "send 10 and 20".into() }]),
            event(
                "ops",
                "inv-1",
                vec![
                    function_call("transfer", json!({ "amount": 10 }), a),
                    function_call("transfer", json!({ "amount": 20 }), b),
                ],
            ),
            event(
                "ops",
                "inv-1",
                vec![
                    function_response("transfer", confirmation_pending_response("held"), a),
                    function_response("transfer", confirmation_pending_response("held"), b),
                ],
            ),
            hold("ops", "inv-1", "transfer", json!({ "amount": 10 }), a),
            hold("ops", "inv-1", "transfer", json!({ "amount": 20 }), b),
        ]
    }

    #[test]
    fn two_calls_to_one_tool_keep_their_arguments_and_are_answered_apart() {
        let mut events = two_held_transfers();
        let amounts = |events: &[Event]| {
            held_tool_calls(events, "ops", "")
                .into_iter()
                .map(|call| (call.id.unwrap_or_default(), call.args["amount"].clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            amounts(&events),
            vec![("adk-call-a".to_string(), json!(10)), ("adk-call-b".to_string(), json!(20))]
        );

        events.push(event("user", "inv-2", vec![Part::Text { text: String::new() }]));
        events.push(event(
            "ops",
            "inv-2",
            vec![function_response("transfer", json!({ "status": "sent" }), "adk-call-a")],
        ));
        assert_eq!(amounts(&events), vec![("adk-call-b".to_string(), json!(20))]);
    }

    /// `events` one millisecond apart, in order, as a session saves them.
    fn timed(mut events: Vec<Event>) -> Vec<Event> {
        let start = chrono::Utc::now();
        for (index, event) in events.iter_mut().enumerate() {
            event.timestamp = start + chrono::Duration::milliseconds(index as i64);
        }
        events
    }

    #[test]
    fn compaction_stops_before_the_earliest_held_call() {
        let mut events = vec![event("user", "inv-0", vec![Part::Text { text: "hi".into() }])];
        events.push(event("ops", "inv-0", vec![Part::Text { text: "hello".into() }]));
        events.extend(held_run());
        let mut events = timed(events);
        // The held call's model event is at index 3, after `user` at index 2.
        assert_eq!(held_call_compaction_limit(&events, events.len()), 3);
        assert_eq!(held_call_compaction_limit(&events, 1), 1, "never past the requested end");

        // Once answered, nothing holds compaction back.
        events.push(event(
            "ops",
            "inv-2",
            vec![function_response("pay", json!({ "ok": 1 }), "c1")],
        ));
        assert_eq!(held_call_compaction_limit(&events, events.len()), events.len());
    }

    #[test]
    fn compaction_limit_counts_holds_of_every_agent_and_branch() {
        let mut left = held_run();
        for event in &mut left {
            event.branch = "fan.left".into();
        }
        let mut events = vec![event("user", "inv-0", vec![Part::Text { text: "hi".into() }])];
        events.extend(left);
        let events = timed(events);
        assert_eq!(held_call_compaction_limit(&events, events.len()), 2);
    }

    #[test]
    fn compaction_limit_stops_at_an_event_newer_than_the_held_call() {
        // Concurrent branches can save an event before the held call with a later time;
        // compacting it would move the boundary past the held call.
        let mut events = timed(held_run());
        let mut late = event("fast", "inv-1", vec![Part::Text { text: "done".into() }]);
        late.timestamp = events[1].timestamp + chrono::Duration::milliseconds(5);
        events.insert(1, late);
        assert_eq!(held_call_compaction_limit(&events, events.len()), 1);
    }

    fn stamped(mut event: Event, workflow: &str, position: crate::WorkflowPosition) -> Event {
        event.actions.workflow_positions.insert(workflow.into(), position);
        event
    }

    #[test]
    fn a_workflow_resumes_where_its_last_run_held_a_call() {
        let mut events = held_run();
        events[3] = stamped(events[3].clone(), "pipeline", crate::WorkflowPosition::new(1, "ops"));
        assert_eq!(
            workflow_resume_positions(&events, "pipeline", ""),
            vec![crate::WorkflowPosition::new(1, "ops")]
        );
        assert!(workflow_resume_positions(&events, "other", "").is_empty());
        assert!(
            workflow_resume_positions(&events, "pipeline", "elsewhere").is_empty(),
            "a hold on another branch does not resume this workflow"
        );
    }

    #[test]
    fn a_workflow_starts_over_once_the_hold_is_answered_or_its_agent_ran_again() {
        let mut stopped = held_run();
        stopped[3] =
            stamped(stopped[3].clone(), "pipeline", crate::WorkflowPosition::new(0, "ops"));

        let mut answered = stopped.clone();
        answered.push(event(
            "ops",
            "inv-2",
            vec![function_response("pay", json!({ "ok": 1 }), "c1")],
        ));
        assert!(workflow_resume_positions(&answered, "pipeline", "").is_empty());

        // The resumed agent ran without a decision and the workflow went on: the call is
        // still held, but it no longer stops the workflow.
        let mut moved_on = stopped;
        moved_on.push(event("ops", "inv-2", vec![Part::Text { text: "still waiting".into() }]));
        assert_eq!(held_tool_calls(&moved_on, "ops", "").len(), 1);
        assert!(workflow_resume_positions(&moved_on, "pipeline", "").is_empty());
    }

    #[test]
    fn a_parallel_workflow_resumes_every_branch_its_last_run_held() {
        let branch = |events: Vec<Event>, name: &str| {
            events
                .into_iter()
                .map(|mut event| {
                    event.branch = format!("fan.{name}");
                    event.author = event.author.replace("ops", name);
                    if event.actions.tool_confirmation.is_some() {
                        event = stamped(event, "fan", crate::WorkflowPosition::new(0, name));
                    }
                    event
                })
                .collect::<Vec<_>>()
        };
        let mut events = branch(held_run(), "left");
        events.extend(branch(held_run().split_off(1), "right"));
        assert_eq!(
            workflow_resume_positions(&events, "fan", ""),
            vec![crate::WorkflowPosition::new(0, "left"), crate::WorkflowPosition::new(0, "right")]
        );
    }

    #[test]
    fn a_resumed_response_takes_the_pending_responses_place() {
        let pending = |id: &str| crate::Content {
            role: "function".into(),
            parts: vec![function_response("transfer", confirmation_pending_response("held"), id)],
        };
        let mut history = vec![pending("adk-call-a"), pending("adk-call-b")];
        let resumed = crate::Content {
            role: "function".into(),
            parts: vec![function_response("transfer", json!({ "status": "sent" }), "adk-call-a")],
        };
        assert!(supersede_pending_responses(&mut history, resumed).is_none());
        assert_eq!(
            history[0].parts,
            vec![function_response("transfer", json!({ "status": "sent" }), "adk-call-a")]
        );
        assert_eq!(history[1].parts, pending("adk-call-b").parts, "the other call stays pending");

        let unrelated = crate::Content {
            role: "function".into(),
            parts: vec![function_response("lookup", json!({ "rows": 1 }), "c9")],
        };
        assert_eq!(
            supersede_pending_responses(&mut history, unrelated.clone()).map(|rest| rest.parts),
            Some(unrelated.parts),
            "a response to no pending call is returned"
        );
    }

    #[tokio::test]
    async fn a_decision_is_available_from_each_source_without_being_consumed() {
        let request = ToolConfirmationRequest {
            tool_name: "pay".into(),
            function_call_id: Some("c1".into()),
            args: json!({ "amount": 5 }),
            acting: None,
            origin: None,
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
            .insert("c1".into(), key("pay", &json!({ "amount": 6 })));
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

    fn transfer_policy() -> DeclarativePolicy {
        // Up to 1000 needs approval; anything larger is denied outright.
        DeclarativePolicy::builder()
            .rule(
                PolicyRule::require_approval("pay", "money moves")
                    .when(crate::ArgPredicate::at_most("/amount", 1000.0)),
            )
            .deny("pay", "over the transfer limit")
            .build()
    }

    #[tokio::test]
    async fn an_edited_approval_is_checked_against_the_policy_again() {
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder()
            .tool_policy(Arc::new(transfer_policy()))
            .approval_store(store.clone())
            .build();
        let scope = ApprovalScope::new("app", "user", "session");
        let call = || GovernedCall::new("pay", "c1", json!({ "amount": 900 }));
        let fingerprint = key("pay", &json!({ "amount": 900 }));

        // An edit the policy allows executes with the edited arguments.
        let edit = ToolApproval::approve().with_edited_args(json!({ "amount": 400 }));
        store.decide(&scope, &fingerprint, edit).await.unwrap();
        assert_eq!(
            authorize_tool_call(&gate(&config), call()).await.unwrap(),
            ToolAuthorization::Execute {
                args: json!({ "amount": 400 }),
                confirmation: Some(ToolConfirmationDecision::Approve),
            }
        );

        // An edit past the limit is a new call the policy denies.
        let edit = ToolApproval::approve().with_edited_args(json!({ "amount": 5000 }));
        store.decide(&scope, &fingerprint, edit).await.unwrap();
        let ToolAuthorization::Refuse { reason, confirmation } =
            authorize_tool_call(&gate(&config), call()).await.unwrap()
        else {
            panic!("expected the edited call to be refused");
        };
        assert!(reason.contains("over the transfer limit"), "{reason}");
        assert_eq!(confirmation, Some(ToolConfirmationDecision::Approve));

        // A screen that refuses the edited arguments refuses the call.
        struct NoLargeAmounts;
        #[async_trait]
        impl ToolCallScreen for NoLargeAmounts {
            async fn screen(
                &self,
                _tool: &str,
                args: &Value,
            ) -> std::result::Result<Value, String> {
                if args["amount"].as_f64().unwrap_or(0.0) > 500.0 {
                    Err("screened out".into())
                } else {
                    Ok(args.clone())
                }
            }
        }
        let screen = NoLargeAmounts;
        let screened = gate(&config).with_screen(&screen);
        let small = || GovernedCall::new("pay", "c2", json!({ "amount": 100 }));
        let edit = ToolApproval::approve().with_edited_args(json!({ "amount": 800 }));
        let small_fingerprint = key("pay", &json!({ "amount": 100 }));
        store.decide(&scope, &small_fingerprint, edit).await.unwrap();
        assert_eq!(
            authorize_tool_call(&screened, small()).await.unwrap(),
            ToolAuthorization::Refuse {
                reason: "screened out".into(),
                confirmation: Some(ToolConfirmationDecision::Approve),
            }
        );
    }

    #[tokio::test]
    async fn approvals_bind_to_the_acting_agent_and_the_tool_origin() {
        let args = json!({ "amount": 40 });
        let declared = json!({ "name": "pay", "parameters": { "type": "object" } });
        let origin = ToolOrigin::new("mcp:bank_a", &declared, None);
        let owner = ToolConfirmationRequest::new("pay", args.clone())
            .with_acting_context(acting("agent"))
            .with_origin(origin.clone());
        let call = |origin: ToolOrigin| {
            let mut call =
                GovernedCall::new("pay", "c-new", args.clone()).requiring_confirmation(true);
            call.origin = Some(origin);
            call
        };
        let other_server = ToolOrigin::new("mcp:bank_b", &declared, None);
        let redefined = ToolOrigin::new(
            "mcp:bank_a",
            &json!({ "name": "pay", "parameters": { "type": "object", "required": ["memo"] } }),
            None,
        );

        // A run-wide approval for the owner's call.
        let config = RunConfig::builder()
            .tool_approval(owner.fingerprint(), ToolApproval::approve())
            .build();
        let scope = || ApprovalScope::new("app", "user", "session");
        let intern = ToolGate::new(&config, "intern", "inv", scope());
        for (gate, origin, why) in [
            (&intern, origin.clone(), "another agent"),
            (&gate(&config), other_server.clone(), "another server"),
            (&gate(&config), redefined.clone(), "a redefined tool"),
        ] {
            assert!(
                matches!(
                    authorize_tool_call(gate, call(origin)).await.unwrap(),
                    ToolAuthorization::Pending { .. }
                ),
                "{why} is not authorized"
            );
        }
        assert!(matches!(
            authorize_tool_call(&gate(&config), call(origin.clone())).await.unwrap(),
            ToolAuthorization::Execute {
                confirmation: Some(ToolConfirmationDecision::Approve),
                ..
            }
        ));

        // A stored decision for the owner's call is left for the owner.
        let store = Arc::new(InMemoryApprovalStore::new());
        let stored = RunConfig::builder().approval_store(store.clone()).build();
        store.decide(&scope(), &owner.fingerprint(), ToolApproval::approve()).await.unwrap();
        let intern = ToolGate::new(&stored, "intern", "inv", scope());
        let ToolAuthorization::Pending { request, .. } =
            authorize_tool_call(&intern, call(origin.clone())).await.unwrap()
        else {
            panic!("another agent's call must wait for its own approval");
        };
        assert_eq!(request.acting.map(|acting| acting.agent), Some(acting("intern").agent));
        assert_eq!(request.origin.as_ref(), Some(&origin));
        assert!(matches!(
            authorize_tool_call(&gate(&stored), call(origin)).await.unwrap(),
            ToolAuthorization::Execute {
                confirmation: Some(ToolConfirmationDecision::Approve),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_resumed_call_runs_only_for_its_agent_and_tool() {
        let args = json!({ "amount": 40 });
        let declared = json!({ "name": "pay" });
        let origin = ToolOrigin::new("mcp:bank_a", &declared, None);
        let held = ToolConfirmationRequest::new("pay", args.clone())
            .with_function_call_id("c1")
            .with_acting_context(acting("agent"))
            .with_origin(origin.clone());
        let resume = |origin: Option<ToolOrigin>, held: ToolConfirmationRequest| {
            let mut call = GovernedCall::new("pay", "c1", args.clone())
                .requiring_confirmation(true)
                .resuming(held);
            call.origin = origin;
            call
        };
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder().approval_store(store.clone()).build();
        let scope = ApprovalScope::new("app", "user", "session");

        let unrecorded = ToolConfirmationRequest { origin: None, ..held.clone() };
        let other_agent =
            ToolConfirmationRequest { acting: Some(Box::new(acting("intern"))), ..held.clone() };
        for (origin, held, expected) in [
            (
                Some(ToolOrigin::new("mcp:bank_b", &declared, None)),
                held.clone(),
                "now resolves to 'mcp:bank_b'",
            ),
            (
                Some(ToolOrigin::new("mcp:bank_a", &json!({ "name": "pay", "x": 1 }), None)),
                held.clone(),
                "changed its description",
            ),
            (Some(origin.clone()), other_agent, "held for agent 'urn:adk:agent:app/intern'"),
            (Some(origin.clone()), unrecorded, "does not record which tool"),
            (None, held.clone(), "does not record which tool"),
        ] {
            store.decide(&scope, &held.fingerprint(), ToolApproval::approve()).await.unwrap();
            let ToolAuthorization::Refuse { reason, confirmation } =
                authorize_tool_call(&gate(&config), resume(origin, held.clone())).await.unwrap()
            else {
                panic!("a mismatched resume must be refused ({expected})");
            };
            assert!(reason.contains("was not run") && reason.contains(expected), "{reason}");
            assert_eq!(confirmation, None);
            assert!(
                !store.has_decision(&scope, &held.fingerprint()).await.unwrap(),
                "the refusal spends the decision ({expected})"
            );
        }

        store.decide(&scope, &held.fingerprint(), ToolApproval::approve()).await.unwrap();
        assert_eq!(
            authorize_tool_call(&gate(&config), resume(Some(origin), held)).await.unwrap(),
            ToolAuthorization::Execute {
                args,
                confirmation: Some(ToolConfirmationDecision::Approve),
            }
        );
    }

    #[tokio::test]
    async fn a_builtin_tool_approval_binds_to_its_agent() {
        let policy =
            DeclarativePolicy::builder().require_approval("web_search", "sign-off").build();
        let (_, tools) = native_tools();
        let config_args = json!({ "type": "web_search_20250305", "max_uses": 2 });
        let fingerprint =
            ToolConfirmationRequest::for_tool(tools["web_search"].as_ref(), config_args)
                .with_acting_context(acting("agent"))
                .fingerprint();
        let config = RunConfig::builder()
            .tool_policy(Arc::new(policy))
            .tool_approval(fingerprint, ToolApproval::approve())
            .build();
        let intern =
            ToolGate::new(&config, "intern", "inv", ApprovalScope::new("app", "user", "session"));
        let (mut declarations, tools) = native_tools();
        let withheld =
            BuiltinToolGovernor::new().govern(&intern, &mut declarations, &tools).await.unwrap();
        assert!(
            withheld.iter().any(|w| w.tool_name == "web_search"
                && w.cause == BuiltinToolWithholding::ApprovalRequired)
        );
    }

    #[tokio::test]
    async fn a_denial_reports_the_approvers_message() {
        let config = RunConfig::builder()
            .tool_approval(
                key("pay", &json!({})),
                ToolApproval::deny().with_message("the approver responded instead: use INV-43"),
            )
            .build();
        let call = GovernedCall::new("pay", "c", json!({})).requiring_confirmation(true);
        let ToolAuthorization::Refuse { reason, .. } =
            authorize_tool_call(&gate(&config), call).await.unwrap()
        else {
            panic!("expected a refusal");
        };
        assert_eq!(
            reason,
            "Tool 'pay' was not run; the approver declined it: the approver responded instead: use INV-43"
        );
    }

    #[tokio::test]
    async fn a_held_call_is_submitted_to_the_approval_service_once() {
        let store = Arc::new(InMemoryApprovalStore::new());
        let service = Arc::new(
            crate::StoreApprovalService::new(store.clone())
                .with_router(crate::ApprovalPolicy::new([crate::ApproverSelector::group("ops")])),
        );
        let config =
            RunConfig::builder().approval_service(service.clone()).durable_run_id("run-7").build();
        let call =
            || GovernedCall::new("pay", "c1", json!({ "amount": 3 })).requiring_confirmation(true);
        for _ in 0..2 {
            assert!(matches!(
                authorize_tool_call(&gate(&config), call()).await.unwrap(),
                ToolAuthorization::Pending { .. }
            ));
        }
        let inbox = service.inbox(crate::InboxQuery::for_group("ops")).await.unwrap();
        assert_eq!(inbox.items.len(), 1, "holding the call again reuses its request");
        let request = &inbox.items[0];
        assert_eq!(request.run_id.as_deref(), Some("run-7"));
        assert_eq!(request.requester, Some(SubjectRef::human("user")));
        assert_eq!(request.summary, "Tool 'pay' requires confirmation");

        service
            .decide(
                &request.id,
                request.version,
                crate::ApprovalDecision::approve(SubjectRef::human("ana")).with_groups(["ops"]),
            )
            .await
            .unwrap();
        assert!(matches!(
            authorize_tool_call(&gate(&config), call()).await.unwrap(),
            ToolAuthorization::Execute { .. }
        ));
    }

    /// Records the facts each `evaluate_call` receives and denies every tool `mcp:evil`
    /// publishes. `evaluate` alone would require approval, so a path that skips
    /// `evaluate_call` shows up as a held call.
    #[derive(Debug, Default)]
    struct OriginPolicy {
        seen: std::sync::Mutex<Vec<SeenCall>>,
    }

    /// The origin, call ID, and arguments one `evaluate_call` received.
    type SeenCall = (Option<ToolOrigin>, Option<String>, Value);

    #[async_trait]
    impl crate::ToolPolicy for OriginPolicy {
        async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
            PolicyDecision::require_approval("evaluate_call was not used")
        }

        async fn evaluate_call(
            &self,
            request: &ToolPolicyRequest,
            call: &PolicyCall<'_>,
        ) -> PolicyDecision {
            self.seen.lock().unwrap().push((
                call.origin.cloned(),
                call.function_call_id.map(str::to_string),
                request.args.clone(),
            ));
            match call.origin {
                Some(origin) if origin.source == "mcp:evil" => {
                    PolicyDecision::deny("tools from mcp:evil are blocked")
                }
                _ => PolicyDecision::Allow,
            }
        }
    }

    /// Answers every request with a fixed approval through `decide_approval`; its `decide`
    /// must never be consulted.
    #[derive(Debug)]
    struct ApprovalHandler(ToolApproval);

    #[async_trait]
    impl ToolConfirmationHandler for ApprovalHandler {
        async fn decide(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> Result<ToolConfirmationDecision> {
            Err(AdkError::tool("decide must not be called when decide_approval is overridden"))
        }

        async fn decide_approval(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> Result<ToolApproval> {
            Ok(self.0.clone())
        }
    }

    fn origin_call(source: &str, id: &str, args: Value) -> (GovernedCall, ToolOrigin) {
        let origin = ToolOrigin::new(source, &json!({ "name": "search" }), None);
        let mut call = GovernedCall::new("search", id, args);
        call.origin = Some(origin.clone());
        (call, origin)
    }

    #[tokio::test]
    async fn the_policy_sees_the_calls_origin_and_id_on_every_evaluation() {
        let policy = Arc::new(OriginPolicy::default());
        let handler = ApprovalHandler(ToolApproval::approve().with_edited_args(json!({ "q": 2 })));
        let config = RunConfig::builder()
            .tool_policy(policy.clone())
            .tool_confirmation_handler(Arc::new(handler))
            .build();

        let (evil_call, evil) = origin_call("mcp:evil", "c1", json!({ "q": 1 }));
        assert_eq!(
            authorize_tool_call(&gate(&config), evil_call).await.unwrap(),
            ToolAuthorization::Refuse {
                reason: "Tool 'search' denied by policy: tools from mcp:evil are blocked".into(),
                confirmation: None,
            }
        );

        // An approver's edit is a new evaluation of the same call, with the same facts.
        let (good_call, good) = origin_call("mcp:good", "c2", json!({ "q": 1 }));
        assert_eq!(
            authorize_tool_call(&gate(&config), good_call.requiring_confirmation(true))
                .await
                .unwrap(),
            ToolAuthorization::Execute {
                args: json!({ "q": 2 }),
                confirmation: Some(ToolConfirmationDecision::Approve),
            }
        );

        // A built-in tool is evaluated before any call exists, so it has an origin but no ID.
        let (mut declarations, tools) = native_tools();
        BuiltinToolGovernor::new().govern(&gate(&config), &mut declarations, &tools).await.unwrap();

        let seen = policy.seen.lock().unwrap().clone();
        assert_eq!(
            seen[..3],
            [
                (Some(evil), Some("c1".to_string()), json!({ "q": 1 })),
                (Some(good.clone()), Some("c2".to_string()), json!({ "q": 1 })),
                (Some(good), Some("c2".to_string()), json!({ "q": 2 })),
            ]
        );
        let builtin: Vec<_> =
            seen[3..].iter().map(|(origin, id, _)| (origin.is_some(), id)).collect();
        assert_eq!(builtin, [(true, &None), (true, &None)], "{seen:?}");
    }

    /// Spec 3.2 §7.8: a policy scopes a crossing approval to one durable run, which spans
    /// the invocations that resume it.
    #[tokio::test]
    async fn the_policy_sees_the_durable_run_the_call_belongs_to() {
        #[derive(Debug, Default)]
        struct RunPolicy(std::sync::Mutex<Vec<Option<String>>>);
        #[async_trait]
        impl crate::ToolPolicy for RunPolicy {
            async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
                PolicyDecision::require_approval("evaluate_call was not used")
            }

            async fn evaluate_call(
                &self,
                _request: &ToolPolicyRequest,
                call: &PolicyCall<'_>,
            ) -> PolicyDecision {
                self.0.lock().unwrap().push(call.durable_run_id.map(str::to_string));
                PolicyDecision::Allow
            }
        }

        let policy = Arc::new(RunPolicy::default());
        let mut config = RunConfig::builder().tool_policy(policy.clone()).build();
        let (call, _) = origin_call("mcp:good", "c1", json!({ "q": 1 }));
        authorize_tool_call(&gate(&config), call).await.unwrap();
        config.durable_run_id = Some("run-7".into());
        let (call, _) = origin_call("mcp:good", "c2", json!({ "q": 1 }));
        authorize_tool_call(&gate(&config), call).await.unwrap();
        let (mut declarations, tools) = native_tools();
        BuiltinToolGovernor::new().govern(&gate(&config), &mut declarations, &tools).await.unwrap();

        let seen = policy.0.lock().unwrap().clone();
        assert_eq!(seen[..2], [None, Some("run-7".to_string())]);
        assert!(seen[2..].iter().all(|run| run.as_deref() == Some("run-7")), "{seen:?}");
    }

    #[tokio::test]
    async fn the_default_decide_approval_maps_the_handlers_decision() {
        #[derive(Debug)]
        struct Fixed(ToolConfirmationDecision);
        #[async_trait]
        impl ToolConfirmationHandler for Fixed {
            async fn decide(
                &self,
                _request: &ToolConfirmationRequest,
            ) -> Result<ToolConfirmationDecision> {
                Ok(self.0)
            }
        }
        let request = ToolConfirmationRequest::new("pay", json!({}));
        let approve = Fixed(ToolConfirmationDecision::Approve).decide_approval(&request);
        assert_eq!(approve.await.unwrap(), ToolApproval::approve());
        let deny = Fixed(ToolConfirmationDecision::Deny).decide_approval(&request);
        assert_eq!(deny.await.unwrap(), ToolApproval::deny());
    }

    #[tokio::test]
    async fn a_handlers_edited_arguments_are_checked_against_the_policy_again() {
        let config = |approval: ToolApproval| {
            RunConfig::builder()
                .tool_policy(Arc::new(transfer_policy()))
                .tool_confirmation_handler(Arc::new(ApprovalHandler(approval)))
                .build()
        };
        let call = || GovernedCall::new("pay", "c1", json!({ "amount": 900 }));

        let allowed = config(ToolApproval::approve().with_edited_args(json!({ "amount": 400 })));
        assert_eq!(
            authorize_tool_call(&gate(&allowed), call()).await.unwrap(),
            ToolAuthorization::Execute {
                args: json!({ "amount": 400 }),
                confirmation: Some(ToolConfirmationDecision::Approve),
            }
        );

        let over = config(ToolApproval::approve().with_edited_args(json!({ "amount": 5000 })));
        let ToolAuthorization::Refuse { reason, confirmation } =
            authorize_tool_call(&gate(&over), call()).await.unwrap()
        else {
            panic!("expected the edited call to be refused");
        };
        assert!(reason.contains("over the transfer limit"), "{reason}");
        assert_eq!(confirmation, Some(ToolConfirmationDecision::Approve));

        let declined = config(ToolApproval::deny().with_message("use the treasury account"));
        assert_eq!(
            authorize_tool_call(&gate(&declined), call()).await.unwrap(),
            ToolAuthorization::Refuse {
                reason:
                    "Tool 'pay' was not run; the approver declined it: use the treasury account"
                        .into(),
                confirmation: Some(ToolConfirmationDecision::Deny),
            }
        );
    }

    #[tokio::test]
    async fn governed_records_name_the_tools_origin() {
        let recorder = crate::audit_log::InMemoryAuditRecorder::new();
        let config =
            RunConfig::builder().audit(AuditTrail::new(Arc::new(recorder.clone()))).build();
        let (call, origin) = origin_call("mcp:good", "c1", json!({ "q": 1 }));
        authorize_tool_call(&gate(&config), call).await.unwrap();
        authorize_tool_call(&gate(&config), GovernedCall::new("search", "c2", json!({})))
            .await
            .unwrap();

        let records = recorder.records();
        assert_eq!(records.len(), 2, "{records:?}");
        assert_eq!(records[0].ext.get("toolSource"), Some(&json!("mcp:good")));
        assert_eq!(
            records[0].ext.get("declarationDigest"),
            Some(&json!(origin.declaration_digest))
        );
        assert!(!records[1].ext.contains_key("toolSource"), "{:?}", records[1]);
        assert!(!records[1].ext.contains_key("declarationDigest"), "{:?}", records[1]);
    }
}
