//! The in-process source (spec 3.3 §6): a kernel, a harness or a runner in the same process
//! as the surfaces, with the session store and the approval service they write.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{
    ApprovalDecision, ApprovalRequest, ApprovalService, ApprovalState, Content, ErrorCategory,
    Event, GovernanceControl, InboxQuery, NewRun, RunCheckpointRef, RunState, SessionId, UserId,
};
use adk_harness::{Harness, HarnessMode, RuleOwner, StandingDecision, StandingRule};
use adk_kernel::proc::LedgerQuery;
use adk_kernel::{Kernel, ProcessFilter, ProcessKind, ProcessSpec, QueueClass, QuietHours};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, SessionService};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;

use crate::error::{Result, SurfaceError};
use crate::origin::Caller;
use crate::source::{AgentState, AlwaysAllowOffer, RuleCommit, Source, SpendToday};
use crate::words;

/// How many inbox entries one page reads.
const INBOX_PAGE: usize = 200;

/// The engine's source for surfaces in the same process as the agents (spec 3.3 §6): the
/// session store the feed numbers events from, the approval service decisions go through,
/// and whichever of an in-process [`Kernel`], a [`Harness`] and a [`Runner`] runs the turns.
///
/// | Part | Gives the engine |
/// |---|---|
/// | [`sessions`](LocalSourceBuilder::sessions) | the feed (required; a harness or runner brings its own) |
/// | [`approvals`](LocalSourceBuilder::approvals) | decisions: the inbox and `decide` (S-I2) |
/// | [`kernel`](LocalSourceBuilder::kernel) | agents from the process table, turns as processes, stop and resume through the kernel's freeze, spend and its daily cap, quiet time, the `/proc` ledger |
/// | [`harness`](LocalSourceBuilder::harness) | the mode and `set_mode`, "Always allow" as a standing rule, turns and resuming decided calls without a kernel |
/// | [`runner`](LocalSourceBuilder::runner) | turns, resuming decided calls, and the runner's kill switch, for apps without a harness |
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
/// use adk_session::InMemorySessionService;
/// use adk_surface::LocalSource;
///
/// let source = LocalSource::builder("adk-rust")
///     .sessions(Arc::new(InMemorySessionService::new()))
///     .approvals(Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))))
///     .build()
///     .unwrap();
/// # let _ = source;
/// ```
#[derive(Clone)]
pub struct LocalSource {
    app: String,
    sessions: Arc<dyn SessionService>,
    approvals: Option<Arc<dyn ApprovalService>>,
    kernel: Option<(Kernel, String)>,
    harness: Option<Arc<Harness>>,
    runner: Option<Arc<Runner>>,
    governance: Option<GovernanceControl>,
    daily_limit: Option<u64>,
}

impl fmt::Debug for LocalSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalSource")
            .field("app", &self.app)
            .field("approvals", &self.approvals.is_some())
            .field("kernel", &self.kernel.as_ref().map(|(_, target)| target))
            .field("harness", &self.harness.is_some())
            .field("runner", &self.runner.is_some())
            .finish_non_exhaustive()
    }
}

/// Builds a [`LocalSource`].
#[derive(Default)]
pub struct LocalSourceBuilder {
    app: String,
    sessions: Option<Arc<dyn SessionService>>,
    approvals: Option<Arc<dyn ApprovalService>>,
    kernel: Option<(Kernel, String)>,
    harness: Option<Arc<Harness>>,
    runner: Option<Arc<Runner>>,
    governance: Option<GovernanceControl>,
    daily_limit: Option<u64>,
}

impl fmt::Debug for LocalSourceBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalSourceBuilder").field("app", &self.app).finish_non_exhaustive()
    }
}

impl LocalSourceBuilder {
    /// The session store the agents write.
    #[must_use]
    pub fn sessions(mut self, sessions: Arc<dyn SessionService>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// The approval service held calls are submitted to.
    #[must_use]
    pub fn approvals(mut self, approvals: Arc<dyn ApprovalService>) -> Self {
        self.approvals = Some(approvals);
        self
    }

    /// An in-process kernel, whose processes of `target` run the turns surfaces send.
    #[must_use]
    pub fn kernel(mut self, kernel: Kernel, target: impl Into<String>) -> Self {
        self.kernel = Some((kernel, target.into()));
        self
    }

    /// A harness: its runner's sessions and kill switch unless set, its modes and standing
    /// rules.
    #[must_use]
    pub fn harness(mut self, harness: Arc<Harness>) -> Self {
        self.harness = Some(harness);
        self
    }

    /// A runner for apps without a harness: its sessions and kill switch unless set.
    #[must_use]
    pub fn runner(mut self, runner: Arc<Runner>) -> Self {
        self.runner = Some(runner);
        self
    }

    /// The kill switch "Stop all" freezes when there is no kernel.
    #[must_use]
    pub fn governance(mut self, governance: GovernanceControl) -> Self {
        self.governance = Some(governance);
        self
    }

    /// The daily spending limit in micro-USD, over the kernel's daily spend cap.
    #[must_use]
    pub fn daily_limit(mut self, micro_usd: u64) -> Self {
        self.daily_limit = Some(micro_usd);
        self
    }

    /// The source.
    ///
    /// # Errors
    ///
    /// Returns `surface.invalid` when no session store was given and no harness or runner
    /// brings one.
    pub fn build(self) -> Result<LocalSource> {
        let runner_sessions = self
            .harness
            .as_ref()
            .map(|harness| harness.runner().session_service().clone())
            .or_else(|| self.runner.as_ref().map(|runner| runner.session_service().clone()));
        let sessions = self.sessions.or(runner_sessions).ok_or_else(|| {
            SurfaceError::Invalid(
                "a local source needs the session store the agents write: pass sessions(), a \
                 harness or a runner"
                    .into(),
            )
        })?;
        let governance = self.governance.or_else(|| {
            self.harness
                .as_ref()
                .map(|harness| harness.runner().governance().clone())
                .or_else(|| self.runner.as_ref().map(|runner| runner.governance().clone()))
        });
        Ok(LocalSource {
            app: self.app,
            sessions,
            approvals: self.approvals,
            kernel: self.kernel,
            harness: self.harness,
            runner: self.runner,
            governance,
            daily_limit: self.daily_limit,
        })
    }
}

impl LocalSource {
    /// A builder for the sessions of `app_name`.
    pub fn builder(app_name: impl Into<String>) -> LocalSourceBuilder {
        LocalSourceBuilder { app: app_name.into(), ..LocalSourceBuilder::default() }
    }

    fn approvals(&self) -> Result<&Arc<dyn ApprovalService>> {
        self.approvals
            .as_ref()
            .ok_or_else(|| SurfaceError::unsupported("decisions without an approval service"))
    }

    fn ids(user: &str, session: &str) -> Result<(UserId, SessionId)> {
        let user = UserId::new(user).map_err(|error| SurfaceError::Invalid(error.to_string()))?;
        let session =
            SessionId::new(session).map_err(|error| SurfaceError::Invalid(error.to_string()))?;
        Ok((user, session))
    }

    /// The user's processes on the kernel, newest change first.
    async fn processes(&self, user: &str) -> Result<Vec<adk_kernel::Process>> {
        let Some((kernel, _)) = &self.kernel else {
            return Ok(Vec::new());
        };
        let mut processes: Vec<_> = kernel
            .processes()
            .ps(ProcessFilter::default())
            .await?
            .into_iter()
            .filter(|process| process.run.user_id == user && process.run.app_name == self.app)
            .collect();
        processes.sort_by_key(|process| std::cmp::Reverse(process.run.updated_at));
        Ok(processes)
    }

    async fn ensure_session(&self, user: &str, session: &str) -> Result<()> {
        if self.transcript(user, session).await?.is_some() {
            return Ok(());
        }
        let created = self
            .sessions
            .create(CreateRequest {
                app_name: self.app.clone(),
                user_id: user.to_string(),
                session_id: Some(session.to_string()),
                state: Default::default(),
            })
            .await;
        match created {
            Ok(_) => Ok(()),
            Err(error) if error.category == ErrorCategory::Conflict => Ok(()),
            Err(error) => Err(SurfaceError::Source(error)),
        }
    }

    /// Runs a turn of the session in the background through the harness or the runner;
    /// `text` empty resumes the session's decided calls.
    fn run_turn(&self, user: &str, session: &str, text: &str) -> Result<()> {
        let (user_id, session_id) = Self::ids(user, session)?;
        let text = text.to_string();
        if let Some(harness) = &self.harness {
            let harness = Arc::clone(harness);
            tokio::spawn(async move {
                let stream = if text.is_empty() {
                    harness.resume(&user_id, &session_id).await
                } else {
                    harness.send(&user_id, &session_id, text).await
                };
                drain(stream, session_id.as_ref()).await;
            });
            return Ok(());
        }
        if let Some(runner) = &self.runner {
            let runner = Arc::clone(runner);
            tokio::spawn(async move {
                let content = if text.is_empty() {
                    Content::new("user")
                } else {
                    Content::new("user").with_text(text)
                };
                let stream = runner.run(user_id, session_id.clone(), content).await;
                drain(stream, session_id.as_ref()).await;
            });
            return Ok(());
        }
        Err(SurfaceError::unsupported("sending a message without a kernel, harness or runner"))
    }

    /// The harness's pending call for `request`, with the standing rule "Always allow" makes.
    async fn always_allow_rule(
        &self,
        caller: Option<&Caller>,
        request: &ApprovalRequest,
    ) -> Option<(String, StandingRule)> {
        let harness = self.harness.as_ref()?;
        let runtime = harness.agent().runtime();
        let pending = runtime.approval_store().pending(&request.scope).await.ok()?;
        let held = pending.into_iter().find(|entry| entry.fingerprint == request.fingerprint)?;
        let suggestions = StandingRule::suggestions(&held.request);
        let suggestion = suggestions
            .iter()
            .find(|suggestion| suggestion.label.contains("within"))
            .or_else(|| suggestions.first())?
            .clone();
        let label = if suggestion.tool == "bash" {
            words::ALWAYS_ALLOW_FOR_THIS_COMMAND
        } else if suggestion.label.contains("within") || suggestion.label.contains("workspace") {
            words::ALWAYS_ALLOW_FOR_THIS_FOLDER
        } else {
            words::ALWAYS_ALLOW_FOR_THIS_PERSON
        };
        let created_by = caller.map_or_else(
            || adk_core::principal::SubjectRef::human(request.scope.user_id.clone()),
            |caller| caller.subject.clone(),
        );
        let owner = RuleOwner {
            app_name: request.scope.app_name.clone(),
            user_id: request.scope.user_id.clone(),
            subject_kind: adk_core::SubjectKind::Human,
            session_id: request.scope.session_id.clone(),
            created_by,
        };
        let rule = harness
            .agent()
            .rule_for_held_call(&held.request, suggestion, StandingDecision::Allow, &owner)
            .ok()?;
        Some((label.to_string(), rule))
    }
}

/// Runs a turn's stream to its end, so its events reach the session store.
async fn drain(stream: adk_core::Result<adk_core::EventStream>, session: &str) {
    match stream {
        Ok(mut stream) => {
            while let Some(event) = stream.next().await {
                if let Err(error) = event {
                    tracing::warn!(session.id = %session, error = %error, "a turn a surface started failed");
                    break;
                }
            }
        }
        Err(error) => {
            tracing::warn!(session.id = %session, error = %error, "a turn a surface started could not run");
        }
    }
}

/// A standing rule granted once the decision it was made for is recorded.
struct GrantRule {
    harness: Arc<Harness>,
    rule: StandingRule,
}

#[async_trait]
impl RuleCommit for GrantRule {
    async fn commit(self: Box<Self>) -> Result<()> {
        self.harness.agent().runtime().standing_rules().grant(self.rule).await?;
        Ok(())
    }
}

#[async_trait]
impl Source for LocalSource {
    fn app_name(&self) -> &str {
        &self.app
    }

    async fn transcript(&self, user: &str, session: &str) -> Result<Option<Vec<Event>>> {
        let found = self
            .sessions
            .get(GetRequest {
                app_name: self.app.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await;
        match found {
            Ok(session) => Ok(Some(session.events().all())),
            Err(error) if error.category == ErrorCategory::NotFound => Ok(None),
            Err(error) if error.category == ErrorCategory::InvalidInput => Ok(None),
            Err(error) => Err(SurfaceError::Source(error)),
        }
    }

    async fn changed(&self, max: Duration) {
        match &self.kernel {
            Some((kernel, _)) => {
                let mut events = kernel.events();
                tokio::select! {
                    _ = tokio::time::sleep(max) => {}
                    _ = events.recv() => {}
                }
            }
            None => tokio::time::sleep(max).await,
        }
    }

    async fn requests(
        &self,
        caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> Result<Vec<ApprovalRequest>> {
        let Some(approvals) = &self.approvals else {
            return Ok(Vec::new());
        };
        let states =
            if open_only { vec![ApprovalState::Pending] } else { ApprovalState::ALL.to_vec() };
        let mut query = InboxQuery::for_approver(caller.subject.clone(), caller.groups.clone())
            .with_states(states)
            .with_app_name(self.app.clone())
            .with_limit(INBOX_PAGE);
        // A session is the caller's: another user's session with the same ID stays out.
        if let Some(session) = session {
            query = query.with_user_id(caller.user_id()).with_session_id(session);
        }
        let mut requests = Vec::new();
        loop {
            let page = approvals.inbox(query.clone()).await?;
            requests.extend(page.items);
            match page.next_cursor {
                Some(cursor) => query = query.with_cursor(Some(cursor)),
                None => break,
            }
        }
        Ok(requests)
    }

    async fn request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        Ok(self.approvals()?.get(id).await?)
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> Result<ApprovalRequest> {
        Ok(self.approvals()?.decide(id, expected_version, decision).await?)
    }

    async fn resolved(&self, request: &ApprovalRequest) -> Result<()> {
        // A durable run parked on the call is woken by the approval service; a call held in
        // a plain run resumes with an empty turn, as `Harness::resume` does.
        if request.run_id.is_some() && self.kernel.is_some() {
            return Ok(());
        }
        if self.harness.is_none() && self.runner.is_none() {
            return Ok(());
        }
        self.run_turn(&request.scope.user_id, &request.scope.session_id, "")
    }

    async fn always_allow_offer(&self, request: &ApprovalRequest) -> Option<AlwaysAllowOffer> {
        self.always_allow_rule(None, request).await.map(|(label, _)| AlwaysAllowOffer { label })
    }

    async fn prepare_always_allow(
        &self,
        caller: &Caller,
        request: &ApprovalRequest,
    ) -> Result<Box<dyn RuleCommit>> {
        let harness =
            self.harness.clone().ok_or_else(|| SurfaceError::unsupported("always allow"))?;
        let (_, rule) = self.always_allow_rule(Some(caller), request).await.ok_or_else(|| {
            SurfaceError::Invalid("no standing rule applies to this call; allow it once".into())
        })?;
        Ok(Box::new(GrantRule { harness, rule }))
    }

    async fn agents(&self, user: &str) -> Result<Vec<AgentState>> {
        if self.kernel.is_some() {
            let mut latest: BTreeMap<String, AgentState> = BTreeMap::new();
            for process in self.processes(user).await? {
                let run = &process.run;
                if latest.contains_key(&run.target) {
                    continue;
                }
                let waiting_on_decision =
                    matches!(run.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. }));
                latest.insert(
                    run.target.clone(),
                    AgentState {
                        name: run.target.clone(),
                        session_id: run.session_id.clone(),
                        state: run.state,
                        waiting_on_decision,
                        wake_at: (run.state == RunState::Waiting && !waiting_on_decision)
                            .then_some(run.not_before)
                            .flatten(),
                        suspended: process.meta.suspended_by.is_some(),
                    },
                );
            }
            return Ok(latest.into_values().collect());
        }
        let name = match (&self.harness, &self.runner) {
            (Some(harness), _) => harness.agent().info().name.clone(),
            (None, Some(_)) => self.app.clone(),
            (None, None) => return Ok(Vec::new()),
        };
        let active = self.active_sessions();
        let waiting = match &self.approvals {
            Some(approvals) => {
                let page = approvals
                    .inbox(
                        InboxQuery::default()
                            .with_app_name(self.app.clone())
                            .with_limit(INBOX_PAGE),
                    )
                    .await?;
                page.items.into_iter().find(|request| request.scope.user_id == user)
            }
            None => None,
        };
        let state = if !active.is_empty() {
            RunState::Running
        } else if waiting.is_some() {
            RunState::Waiting
        } else {
            RunState::Completed
        };
        Ok(vec![AgentState {
            name,
            session_id: waiting
                .map(|request| request.scope.session_id)
                .or_else(|| active.first().cloned()),
            state,
            waiting_on_decision: state == RunState::Waiting,
            wake_at: None,
            suspended: false,
        }])
    }

    async fn busy(&self, user: &str, session: &str) -> Result<bool> {
        if self.kernel.is_some() {
            return Ok(self.processes(user).await?.iter().any(|process| {
                process.run.session_id.as_deref() == Some(session)
                    && matches!(process.run.state, RunState::Running | RunState::Queued)
            }));
        }
        Ok(self.active_sessions().iter().any(|active| active == session))
    }

    async fn start_turn(&self, caller: &Caller, session: &str, text: &str) -> Result<()> {
        let user = caller.user_id();
        self.ensure_session(user, session).await?;
        if let Some((kernel, target)) = &self.kernel {
            let run = NewRun::new(
                QueueClass::Interactive.as_str(),
                self.app.clone(),
                user,
                target.clone(),
                serde_json::json!({ "text": text }),
            )
            .with_session_id(session)
            .with_authenticated_user();
            kernel.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive)).await?;
            return Ok(());
        }
        self.run_turn(user, session, text)
    }

    async fn stop_all(&self, caller: &Caller, reason: &str) -> Result<()> {
        let actor = AuditActor::subject(caller.subject.clone());
        if let Some((kernel, _)) = &self.kernel {
            kernel.freeze(reason, actor).await?;
            return Ok(());
        }
        let governance =
            self.governance.as_ref().ok_or_else(|| SurfaceError::unsupported("stop all"))?;
        governance.freeze_recorded(reason, actor).await?;
        Ok(())
    }

    async fn resume_all(&self, caller: &Caller) -> Result<()> {
        let actor = AuditActor::subject(caller.subject.clone());
        if let Some((kernel, _)) = &self.kernel {
            kernel.unfreeze(actor).await?;
            return Ok(());
        }
        let governance =
            self.governance.as_ref().ok_or_else(|| SurfaceError::unsupported("resume all"))?;
        governance.unfreeze_recorded(actor).await?;
        Ok(())
    }

    async fn stopped(&self) -> Result<bool> {
        if let Some((kernel, _)) = &self.kernel {
            return Ok(kernel.health().frozen);
        }
        Ok(self.governance.as_ref().is_some_and(GovernanceControl::is_frozen))
    }

    async fn mode(&self, user: &str, session: Option<&str>) -> Result<Option<HarnessMode>> {
        let Some(harness) = &self.harness else {
            return Ok(None);
        };
        match session {
            Some(session) => {
                let (user, session) = Self::ids(user, session)?;
                Ok(Some(harness.mode(&user, &session).await?))
            }
            None => Ok(Some(harness.agent().info().default_mode)),
        }
    }

    async fn set_mode(&self, caller: &Caller, session: &str, mode: HarnessMode) -> Result<()> {
        let harness =
            self.harness.as_ref().ok_or_else(|| SurfaceError::unsupported("changing the mode"))?;
        let (user, session) = Self::ids(caller.user_id(), session)?;
        harness.set_mode(&user, &session, mode).await?;
        Ok(())
    }

    async fn spend(&self, user: &str) -> Result<Option<SpendToday>> {
        let Some((kernel, _)) = &self.kernel else {
            return Ok(self.daily_limit.map(|limit| SpendToday { spent: 0, limit: Some(limit) }));
        };
        let spent =
            self.processes(user).await?.iter().map(|process| process.view.spend_today).sum();
        let limit = self.daily_limit.or(kernel.config().quotas.daily_spend_cap);
        Ok(Some(SpendToday { spent, limit }))
    }

    fn quiet_hours(&self) -> Option<QuietHours> {
        self.kernel.as_ref().and_then(|(kernel, _)| kernel.config().quiet_hours)
    }

    async fn ledger(&self, since: DateTime<Utc>) -> Result<Vec<adk_kernel::proc::LedgerEntry>> {
        let Some(ledger) = self.kernel.as_ref().and_then(|(kernel, _)| kernel.ledger()) else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        let mut query = LedgerQuery::default().from(since).limit(500);
        loop {
            let page = ledger.store().list(&query).await?;
            let Some(last) = page.last().map(|entry| entry.seq) else { break };
            let full = page.len() >= 500;
            entries.extend(page);
            if !full {
                break;
            }
            query = query.after(last);
        }
        Ok(entries)
    }
}

impl LocalSource {
    fn active_sessions(&self) -> Vec<String> {
        match (&self.harness, &self.runner) {
            (Some(harness), _) => harness.runner().active_session_ids(),
            (None, Some(runner)) => runner.active_session_ids(),
            (None, None) => Vec::new(),
        }
    }
}
