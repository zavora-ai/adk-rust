//! Forking, rewinding, and editing session history under the runtime's rules.
//!
//! [`HistoryControl`] wraps the [`SessionService`] history operations with what the store
//! cannot know:
//!
//! | Rule | Error |
//! |------|-------|
//! | No history operation runs on a session while a turn runs on it: a run of any runner sharing the [`SessionActivity`], or a run a durable run store records as running | `session.history.run_active` (`Conflict`) |
//! | Each operation is recorded in the audit log before it is applied, and its outcome after; a decision record that cannot be written refuses the operation under [`AuditFailureMode::Block`](adk_core::audit_log::AuditFailureMode::Block) | `audit.record_failed` |
//!
//! The store's own rules hold underneath: a held call blocks rewinds and edits, and no result
//! separates a tool call from its response (see [`adk_session::history`]). History operations
//! run no agent, tool, or model, so the kill switch does not stop them: rewinding a session past
//! a bad turn is part of what a freeze is for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, AuditTrail,
};
use adk_core::identity::AdkIdentity;
use adk_core::{
    AdkError, DurableRunStore, ErrorCategory, ErrorComponent, Result, RunFilter, RunState,
};
use adk_session::{
    ForkRequest, HistoryChange, HistoryEdit, HistoryOutcome, HistoryRewrite, RewriteHistoryRequest,
    SessionHistory, SessionService,
};
use serde_json::{Value, json};

/// The sessions with a turn in flight, shared by every runner that holds a clone.
///
/// A [`Runner`](crate::Runner) registers each run here for as long as its event stream lives.
/// A host that builds one runner per request, such as `adk-server`, gives every runner the same
/// registry, so a [`HistoryControl`] sees the runs of all of them.
///
/// # Example
///
/// ```rust
/// use adk_core::identity::AdkIdentity;
/// use adk_runner::SessionActivity;
///
/// let activity = SessionActivity::new();
/// let identity = AdkIdentity::new(
///     "app".try_into().unwrap(),
///     "alice".try_into().unwrap(),
///     "s1".try_into().unwrap(),
/// );
/// let guard = activity.enter(identity.clone());
/// assert!(activity.is_active(&identity));
/// drop(guard);
/// assert!(!activity.is_active(&identity));
/// ```
#[derive(Clone, Default)]
pub struct SessionActivity {
    runs: Arc<Mutex<HashMap<AdkIdentity, usize>>>,
}

impl std::fmt::Debug for SessionActivity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("SessionActivity").field("active_sessions", &runs.len()).finish()
    }
}

impl SessionActivity {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a turn on `identity` until the returned guard drops.
    pub fn enter(&self, identity: AdkIdentity) -> ActivityGuard {
        *self
            .runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(identity.clone())
            .or_default() += 1;
        ActivityGuard { runs: self.runs.clone(), identity }
    }

    /// Whether a turn runs on `identity`.
    pub fn is_active(&self, identity: &AdkIdentity) -> bool {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).contains_key(identity)
    }
}

/// A turn recorded in a [`SessionActivity`]; dropping it ends the record.
#[must_use = "the turn is recorded only while the guard lives"]
pub struct ActivityGuard {
    runs: Arc<Mutex<HashMap<AdkIdentity, usize>>>,
    identity: AdkIdentity,
}

impl std::fmt::Debug for ActivityGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityGuard").field("session_id", &self.identity.session_id).finish()
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = runs.get_mut(&self.identity) {
            *count -= 1;
            if *count == 0 {
                runs.remove(&self.identity);
            }
        }
    }
}

/// Where a rewind stops.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RewindTarget {
    /// Keep the events up to and including this one.
    Event(String),
    /// Remove this many events from the end.
    Steps(usize),
    /// Remove the invocation with this ID and every event after it: the turn it ran is undone.
    BeforeInvocation(String),
}

/// Session fork, rewind, and edit under the runtime's rules: refused while a turn runs on the
/// session, and recorded in the audit log.
///
/// Get one from [`Runner::history_control`](crate::Runner::history_control), or build one over
/// a session service for a host without a runner.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::AuditActor;
/// use adk_core::principal::SubjectRef;
/// use adk_runner::{HistoryControl, RewindTarget, SessionActivity};
/// use adk_session::{CreateRequest, ForkRequest, InMemorySessionService, SessionService};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let sessions = Arc::new(InMemorySessionService::new());
/// let session = sessions
///     .create(CreateRequest {
///         app_name: "app".into(),
///         user_id: "alice".into(),
///         session_id: Some("main".into()),
///         state: Default::default(),
///     })
///     .await?;
/// let control = HistoryControl::new(sessions).with_activity(SessionActivity::new());
/// let actor = AuditActor::subject(SubjectRef::human("alice"));
/// let fork = control.fork(&actor, ForkRequest::new(session.try_identity()?)).await?;
/// let rewound = control
///     .rewind(&actor, &fork.session.try_identity()?, RewindTarget::Steps(0), None)
///     .await?;
/// assert_eq!(rewound.session.events().len(), 0);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct HistoryControl {
    sessions: Arc<dyn SessionService>,
    activity: SessionActivity,
    audit: Option<AuditTrail>,
    run_store: Option<Arc<dyn DurableRunStore>>,
}

impl std::fmt::Debug for HistoryControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryControl")
            .field("activity", &self.activity)
            .field("audit", &self.audit.is_some())
            .field("run_store", &self.run_store.is_some())
            .finish()
    }
}

impl HistoryControl {
    /// History operations on `sessions`, with an empty activity registry and no audit log.
    pub fn new(sessions: Arc<dyn SessionService>) -> Self {
        Self { sessions, activity: SessionActivity::new(), audit: None, run_store: None }
    }

    /// Refuses operations on sessions with a turn recorded in `activity`.
    #[must_use]
    pub fn with_activity(mut self, activity: SessionActivity) -> Self {
        self.activity = activity;
        self
    }

    /// Records every operation in `audit`.
    #[must_use]
    pub fn with_audit(mut self, audit: AuditTrail) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Also refuses operations on sessions a durable run in `store` is running on, whichever
    /// process runs it.
    #[must_use]
    pub fn with_run_store(mut self, store: Arc<dyn DurableRunStore>) -> Self {
        self.run_store = Some(store);
        self
    }

    /// The session's history and its version.
    ///
    /// # Errors
    ///
    /// Returns `session.not_found` or a backend error.
    pub async fn history(&self, identity: &AdkIdentity) -> Result<SessionHistory> {
        self.sessions.history(identity).await
    }

    /// Forks a session; see [`SessionService::fork`].
    ///
    /// # Errors
    ///
    /// Returns `session.history.run_active` while a turn runs on the source, `audit.record_failed`
    /// when the decision cannot be recorded, and the errors of [`SessionService::fork`].
    pub async fn fork(&self, actor: &AuditActor, mut req: ForkRequest) -> Result<HistoryOutcome> {
        self.refuse_running(&req.source).await?;
        // A generated ID is chosen once, so the recorded plan and the fork name the same
        // session.
        req.session_id.get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
        let (_, planned) = self.sessions.history(&req.source).await?.fork(&req)?;
        let args = json!({
            "source": req.source.session_id.as_ref(),
            "atEventId": req.at_event_id,
            "sessionId": req.session_id,
        });
        self.decide(actor, &req.source, &planned, &args).await?;
        let outcome = self.sessions.fork(req.clone()).await;
        self.record_outcome(actor, &req.source, &planned, &args, &outcome).await;
        outcome
    }

    /// Rewinds a session to `target`; see [`SessionService::rewrite_history`].
    ///
    /// # Errors
    ///
    /// Returns `session.history.run_active` while a turn runs on the session,
    /// `session.invocation_not_found` for a [`RewindTarget::BeforeInvocation`] the session does
    /// not hold, `audit.record_failed` when the decision cannot be recorded, and the errors of
    /// [`SessionService::rewrite_history`].
    pub async fn rewind(
        &self,
        actor: &AuditActor,
        identity: &AdkIdentity,
        target: RewindTarget,
        expected_version: Option<String>,
    ) -> Result<HistoryOutcome> {
        self.refuse_running(identity).await?;
        let history = self.sessions.history(identity).await?;
        let rewrite = match &target {
            RewindTarget::Event(event_id) => {
                HistoryRewrite::RewindTo { event_id: event_id.clone() }
            }
            RewindTarget::Steps(steps) => HistoryRewrite::RewindSteps { steps: *steps },
            RewindTarget::BeforeInvocation(invocation_id) => {
                let first = history
                    .events
                    .iter()
                    .position(|event| &event.invocation_id == invocation_id)
                    .ok_or_else(|| {
                        AdkError::not_found(
                            ErrorComponent::Session,
                            "session.invocation_not_found",
                            format!(
                                "invocation '{invocation_id}' has no events in session '{}'",
                                identity.session_id
                            ),
                        )
                    })?;
                match first.checked_sub(1) {
                    Some(last) => {
                        HistoryRewrite::RewindTo { event_id: history.events[last].id.clone() }
                    }
                    None => HistoryRewrite::RewindSteps { steps: history.events.len() },
                }
            }
        };
        let args = match &target {
            RewindTarget::Event(event_id) => json!({ "eventId": event_id }),
            RewindTarget::Steps(steps) => json!({ "steps": steps }),
            RewindTarget::BeforeInvocation(id) => json!({ "beforeInvocationId": id }),
        };
        self.rewrite(actor, identity, history, rewrite, expected_version, args).await
    }

    /// Replaces or removes events of a session; see [`HistoryEdit`].
    ///
    /// # Errors
    ///
    /// Returns `session.history.run_active` while a turn runs on the session,
    /// `audit.record_failed` when the decision cannot be recorded, and the errors of
    /// [`SessionService::rewrite_history`].
    pub async fn edit(
        &self,
        actor: &AuditActor,
        identity: &AdkIdentity,
        edits: Vec<HistoryEdit>,
        expected_version: Option<String>,
    ) -> Result<HistoryOutcome> {
        self.refuse_running(identity).await?;
        let history = self.sessions.history(identity).await?;
        let args = json!({
            "replace": edits.iter().filter_map(|edit| match edit {
                HistoryEdit::Replace { event } => Some(event.id.clone()),
                _ => None,
            }).collect::<Vec<_>>(),
            "remove": edits.iter().filter_map(|edit| match edit {
                HistoryEdit::Remove { event_id } => Some(event_id.clone()),
                _ => None,
            }).collect::<Vec<_>>(),
        });
        let rewrite = HistoryRewrite::Edit { edits };
        self.rewrite(actor, identity, history, rewrite, expected_version, args).await
    }

    /// Plans `rewrite` against `history` and applies it only to that history.
    async fn rewrite(
        &self,
        actor: &AuditActor,
        identity: &AdkIdentity,
        history: SessionHistory,
        rewrite: HistoryRewrite,
        expected_version: Option<String>,
        args: Value,
    ) -> Result<HistoryOutcome> {
        let planned = history.rewrite(identity, &rewrite, expected_version.as_deref())?.change;
        self.decide(actor, identity, &planned, &args).await?;
        // The planned history is the one the decision names, so the write applies to it alone.
        let request =
            RewriteHistoryRequest::new(identity.clone(), rewrite).expect_version(history.version());
        let outcome = self.sessions.rewrite_history(request).await;
        self.record_outcome(actor, identity, &planned, &args, &outcome).await;
        outcome
    }

    async fn refuse_running(&self, identity: &AdkIdentity) -> Result<()> {
        let mut running = self.activity.is_active(identity);
        if !running && let Some(store) = &self.run_store {
            let filter = RunFilter::default()
                .app_name(identity.app_name.as_ref())
                .user_id(identity.user_id.as_ref())
                .session_id(identity.session_id.as_ref())
                .states([RunState::Running]);
            running = !store.list(filter).await?.is_empty();
        }
        if running {
            return Err(AdkError::new(
                ErrorComponent::Session,
                ErrorCategory::Conflict,
                "session.history.run_active",
                format!(
                    "a turn is running on session '{}'; wait for it to finish, or interrupt it, \
                     before forking, rewinding, or editing the session",
                    identity.session_id
                ),
            ));
        }
        Ok(())
    }

    fn draft(
        &self,
        actor: &AuditActor,
        identity: &AdkIdentity,
        change: &HistoryChange,
        args: &Value,
        trail: &AuditTrail,
    ) -> AuditRecordDraft {
        let mut draft =
            AuditRecordDraft::new(AuditRecordKind::SessionHistory, change.operation.action())
                .actor(actor.clone())
                .args_digest(trail.digest(args))
                .decision(AuditDecision::Allowed)
                .ext("app", json!(identity.app_name.as_ref()))
                .ext("user", json!(identity.user_id.as_ref()))
                .ext("session", json!(identity.session_id.as_ref()))
                .ext("versionBefore", json!(change.version_before))
                .ext("eventsAfter", json!(change.events_after));
        if change.session_id != identity.session_id.as_ref() {
            draft = draft.ext("fork", json!(change.session_id));
        }
        if !change.removed.is_empty() {
            draft = draft.ext("removed", json!(change.removed));
        }
        if !change.replaced.is_empty() {
            draft = draft.ext("replaced", json!(change.replaced));
        }
        if let Some(digest) = &change.changed_digest {
            draft = draft.payload_ref(digest.clone());
        }
        draft
    }

    /// Records the decision to apply `change`; a refused record refuses the operation.
    async fn decide(
        &self,
        actor: &AuditActor,
        identity: &AdkIdentity,
        change: &HistoryChange,
        args: &Value,
    ) -> Result<()> {
        let Some(trail) = &self.audit else { return Ok(()) };
        trail.record(self.draft(actor, identity, change, args, trail)).await
    }

    /// Records what happened; the change is already applied, so a failed record is logged.
    async fn record_outcome(
        &self,
        actor: &AuditActor,
        identity: &AdkIdentity,
        planned: &HistoryChange,
        args: &Value,
        outcome: &Result<HistoryOutcome>,
    ) {
        let Some(trail) = &self.audit else { return };
        let draft = match outcome {
            Ok(done) => self
                .draft(actor, identity, &done.change, args, trail)
                .outcome(AuditRecordOutcome::Succeeded)
                .ext("versionAfter", json!(done.change.version_after)),
            Err(error) => self
                .draft(actor, identity, planned, args, trail)
                .outcome(AuditRecordOutcome::Failed)
                .ext("error", json!(error.code)),
        };
        if let Err(error) = trail.recorder().record(draft).await {
            tracing::error!(
                session.id = %identity.session_id,
                error = %error,
                "history operation applied but its outcome record failed"
            );
        }
    }
}
