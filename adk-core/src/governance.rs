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
///     acting: None,
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
    let GovernedCall { tool_name, function_call_id, mut args, effect, requires_confirmation } =
        call;
    let audited =
        AuditedCall { gate, tool_name: &tool_name, function_call_id: &function_call_id, effect };
    if let Err(frozen) = config.check_governance() {
        audited.decision(&args, AuditDecision::Denied, Some(POLICY_REF_GOVERNANCE), None).await?;
        return Err(frozen);
    }

    let policy_ref = config.tool_policy.as_ref().map(|policy| match policy.policy_ref() {
        Some(name) => format!("{POLICY_REF_TOOL_POLICY}:{name}"),
        None => POLICY_REF_TOOL_POLICY.to_string(),
    });
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
        if let Some(store) = &config.approval_store {
            store.record_pending(&gate.scope, &request).await?;
        }
        let reason = match approval_reason {
            Some(reason) => format!("Tool '{tool_name}' requires confirmation: {reason}"),
            None => format!("Tool '{tool_name}' requires confirmation"),
        };
        if let Some(service) = &config.approval_service {
            let call = crate::approval::ApprovalCall {
                scope: gate.scope.clone(),
                request: request.clone(),
                reason: reason.clone(),
                agent_name: gate.agent_name.to_string(),
                invocation_id: gate.invocation_id.to_string(),
                run_id: config.durable_run_id.clone(),
                acting: Some(gate.acting.clone()),
            };
            service.submit_call(call).await?;
        }
        return Ok(ToolAuthorization::Pending { request, reason });
    };
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
    let (gate, tool_name, effect) = (audited.gate, audited.tool_name, audited.effect);
    let policy_ref = gate.run_config.tool_policy.as_ref().map(|policy| match policy.policy_ref() {
        Some(name) => format!("{POLICY_REF_TOOL_POLICY}:{name}"),
        None => POLICY_REF_TOOL_POLICY.to_string(),
    });
    let mut decision = evaluate_policy(gate, tool_name, &edited, effect).await;
    let mut args = edited;
    if !matches!(decision, PolicyDecision::Deny { .. })
        && let Some(screen) = gate.screen
    {
        match screen.screen(tool_name, &args).await {
            Ok(screened) => {
                if screened != args {
                    let again = evaluate_policy(gate, tool_name, &screened, effect).await;
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

/// One call on its way through [`authorize_tool_call`], for its audit records.
struct AuditedCall<'g, 'a> {
    gate: &'g ToolGate<'a>,
    tool_name: &'g str,
    function_call_id: &'g str,
    effect: ToolEffect,
}

impl AuditedCall<'_, '_> {
    fn draft(&self, kind: AuditRecordKind, args: &Value) -> Option<AuditRecordDraft> {
        let trail = self.gate.run_config.audit.as_ref()?;
        Some(
            AuditRecordDraft::new(kind, self.tool_name)
                .args_digest(trail.digest(args))
                .ext("functionCallId", Value::String(self.function_call_id.to_string()))
                .ext("effect", serde_json::to_value(self.effect).unwrap_or(Value::Null)),
        )
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
        acting: Some(gate.acting.clone()),
    };
    policy.evaluate(&request).await
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
        Ok(Ok(decision)) => Ok(Some((bare(decision), "handler"))),
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
///     acting: None,
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
///     acting: None,
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
/// and call-ID decisions do not apply. The fingerprint is
/// [`tool_call_fingerprint`] of the tool name and its configuration. A decision taken
/// from the store applies for the rest of the invocation.
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
                let effect =
                    tools.get(&name).map_or(ToolEffect::NonIdempotent, |tool| tool.effect());
                self.decide(gate, &name, args, effect).await?
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
    ) -> Result<Option<(BuiltinToolWithholding, String)>> {
        let approval_reason = match evaluate_policy(gate, name, &args, effect).await {
            PolicyDecision::Allow => return Ok(None),
            PolicyDecision::Deny { reason } => {
                return Ok(Some((BuiltinToolWithholding::PolicyDenied, reason)));
            }
            PolicyDecision::RequireApproval { reason } => reason,
        };
        let config = gate.run_config;
        let request = ToolConfirmationRequest::new(name, args);
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
        ApprovalService, DeclarativePolicy, PolicyRule, ToolConfirmationHandler,
        tool_call_fingerprint,
    };
    use serde_json::json;

    fn gate(config: &RunConfig) -> ToolGate<'_> {
        ToolGate::new(config, "agent", "inv", ApprovalScope::new("app", "user", "session"))
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
        let fingerprint = tool_call_fingerprint("web_search", &config_args);
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
            acting: None,
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
            acting: None,
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
        let fingerprint = tool_call_fingerprint("pay", &json!({ "amount": 900 }));

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
        let small_fingerprint = tool_call_fingerprint("pay", &json!({ "amount": 100 }));
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
    async fn a_denial_reports_the_approvers_message() {
        let config = RunConfig::builder()
            .tool_approval(
                tool_call_fingerprint("pay", &json!({})),
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
}
