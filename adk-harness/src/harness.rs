//! `Harness`: a harness agent with its own `Runner` (spec 3.1 §7.3, feature `runner`).

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::audit_log::AuditActor;
use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    AdkError, ApprovalScope, Content, ErrorCategory, ErrorComponent, Event, EventStream,
    FunctionResponseData, Part, PendingApproval, RequestContext, Result, RunConfig, SessionId,
    ToolApproval, UserId, held_tool_calls,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, SessionService};
use serde_json::{Value, json};

use crate::agent::HarnessAgent;
use crate::approvals::{
    RuleOwner, RuleSuggestion, StandingDecision, StandingRuleStore, rule_audit_fields,
};
use crate::audit::HarnessAudit;
use crate::goal::{GOAL_STATE_KEY, GOAL_TARGET, GoalSpec, GoalState};

/// The queue durable goals go to unless [`Harness::with_goal_queue`] names another.
const DEFAULT_GOAL_QUEUE: &str = "default";
use crate::instructions::{TrustDecision, WorkspaceTrust};
use crate::mode::{HarnessMode, MODE_STATE_KEY, ModeChange, ModeSource, write_mode_change};
use crate::tools::{TODOS_STATE_KEY, TodoList};

/// Status of the response that cancels a held call on a mode loosening (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::CANCELLED_BY_MODE_CHANGE_STATUS, "cancelled_by_mode_change");
/// ```
pub const CANCELLED_BY_MODE_CHANGE_STATUS: &str = "cancelled_by_mode_change";

/// What closing the harness could not finish (spec 3.1 §7.15.2).
///
/// # Example
///
/// ```rust
/// let report = adk_harness::CloseReport::default();
/// assert!(report.distillation_failures.is_empty());
/// ```
#[derive(Debug, Default)]
pub struct CloseReport {
    /// Sessions whose turns were not distilled into long-term memory, by session id, with
    /// the error.
    pub distillation_failures: Vec<(String, AdkError)>,
}

/// A person's decision on a pending call, through [`Harness::decide`] (spec 3.1 §7.3).
///
/// # Example
///
/// ```rust
/// use adk_harness::HarnessDecision;
///
/// let decision = HarnessDecision::Deny { message: Some("not on main".into()) };
/// # let _ = decision;
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum HarnessDecision {
    /// Run the call once.
    Approve,
    /// Run the call once with these arguments; the policy checks them again.
    ApproveWithArgs(Value),
    /// Run the call once and remember the rule.
    ApproveAndRemember(RuleSuggestion),
    /// Do not run the call.
    Deny {
        /// Reported to the model.
        message: Option<String>,
    },
    /// Do not run the call and remember a Deny rule.
    DenyAndRemember(RuleSuggestion),
}

/// A harness agent with the `Runner` that serves it (spec 3.1 §7.3).
///
/// `Runner::run` never creates a session, hence [`open_session`](Self::open_session).
///
/// # Example
///
/// ```rust,ignore
/// let harness = HarnessAgent::builder("harness").model(model).workspace(ws).build()?
///     .into_harness("adk-rust", Arc::new(InMemorySessionService::new()))?;
/// let session = harness.open_session(&user, None).await?;
/// let events = harness.send(&user, &session, "fix the failing test").await?;
/// ```
pub struct Harness {
    runner: Arc<Runner>,
    agent: Arc<HarnessAgent>,
    sessions: Arc<dyn SessionService>,
    app_name: String,
    run_store: Option<Arc<dyn adk_core::DurableRunStore>>,
    goal_queue: String,
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Harness")
            .field("app_name", &self.app_name)
            .field("agent", &self.agent)
            .finish()
    }
}

/// The note `Harness::decide` withdraws a call's inbox requests with. Under a kernel the
/// withdrawal wakes nothing; the person's decision is reported once it is recorded
/// (spec 3.3 §7.3).
pub(crate) const DECIDED_IN_THE_HARNESS: &str = "decided in the harness";

fn store_error(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::InvalidInput,
        "harness.decision.invalid",
        message,
    )
}

/// A call [`Harness::decide`] is recording a decision for: its approval store, by address,
/// its scope and its fingerprint.
type DecidingKey = (usize, ApprovalScope, String);

/// The calls being decided in this process. The approval store's decision write is not
/// conditional, so two decisions for one call that both found it pending would both be
/// written; the second is refused instead (spec 3.3 §7.3, §11 S-I2).
static DECIDING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<DecidingKey>>> =
    std::sync::LazyLock::new(Default::default);

/// Holds a call in [`DECIDING`] until the decision is recorded or refused.
struct Deciding(DecidingKey);

impl Deciding {
    fn claim(key: DecidingKey) -> Result<Self> {
        let mut deciding = DECIDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !deciding.insert(key.clone()) {
            return Err(store_error(
                "another decision for this call is being recorded; the first one stands",
            ));
        }
        Ok(Self(key))
    }
}

impl Drop for Deciding {
    fn drop(&mut self) {
        DECIDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.0);
    }
}

impl Harness {
    pub(crate) fn new(
        agent: Arc<HarnessAgent>,
        app_name: String,
        sessions: Arc<dyn SessionService>,
    ) -> Result<Self> {
        let builder = Runner::builder()
            .app_name(app_name.clone())
            .agent(agent.clone())
            .session_service(sessions.clone());
        let builder = agent.configure_runner(builder);
        // Every agent of a partition's harness carries its class and tenant (spec 3.2 §7.8).
        #[cfg(feature = "kernel")]
        let builder = match agent.partition() {
            Some(partition) => builder.identity_source(
                partition.identity_source(&app_name, adk_core::Agent::name(agent.as_ref())),
            ),
            None => builder,
        };
        let runner = builder.build()?;
        #[cfg(feature = "memory")]
        attach_facts(&agent, &runner, &sessions, &app_name);
        Ok(Self {
            runner: Arc::new(runner),
            agent,
            sessions,
            app_name,
            run_store: None,
            goal_queue: DEFAULT_GOAL_QUEUE.to_string(),
        })
    }

    /// Wraps a runner built for `agent`, for callers that configure the runner themselves —
    /// for example to attach a signed audit log with `RunnerConfigBuilder::audit_log`.
    /// Build it with [`HarnessAgent::configure_runner`] and `agent` as its root.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agent = Arc::new(harness_agent);
    /// let runner = agent
    ///     .configure_runner(Runner::builder().app_name("adk-rust").agent(agent.clone()).session_service(sessions))
    ///     .audit_log(Arc::new(log))
    ///     .build()?;
    /// let harness = Harness::with_runner(agent, runner)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the runner's root agent is not `agent`.
    pub fn with_runner(agent: Arc<HarnessAgent>, runner: Runner) -> Result<Self> {
        if runner.root_agent_name() != adk_core::Agent::name(agent.as_ref()) {
            return Err(AdkError::config(
                "Harness::with_runner needs a runner whose root agent is the harness agent",
            ));
        }
        let sessions = runner.session_service().clone();
        let app_name = runner.app_name().to_string();
        #[cfg(feature = "memory")]
        attach_facts(&agent, &runner, &sessions, &app_name);
        Ok(Self {
            runner: Arc::new(runner),
            agent,
            sessions,
            app_name,
            run_store: None,
            goal_queue: DEFAULT_GOAL_QUEUE.to_string(),
        })
    }

    /// The harness agent.
    pub fn agent(&self) -> &Arc<HarnessAgent> {
        &self.agent
    }

    /// The application name.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// Opens `session` for `user`, creating it when it does not exist, or a new session.
    ///
    /// # Errors
    ///
    /// Returns the session service's error.
    pub async fn open_session(
        &self,
        user: &UserId,
        session: Option<&SessionId>,
    ) -> Result<SessionId> {
        if let Some(session) = session {
            match self
                .sessions
                .get(GetRequest {
                    app_name: self.app_name.clone(),
                    user_id: user.to_string(),
                    session_id: session.to_string(),
                    num_recent_events: Some(1),
                    after: None,
                })
                .await
            {
                Ok(_) => return Ok(session.clone()),
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error),
            }
        }
        let created = self
            .sessions
            .create(CreateRequest {
                app_name: self.app_name.clone(),
                user_id: user.to_string(),
                session_id: session.map(ToString::to_string),
                state: HashMap::new(),
            })
            .await?;
        Ok(SessionId::try_from(created.id().to_string())?)
    }

    /// Enqueues goals as durable runs in `store` (spec 3.1 §7.11): [`start_goal`] then
    /// writes the goal state and enqueues a run of target [`GOAL_TARGET`] for a
    /// `DurableWorker` serving [`goal_target`]; a held call parks the run until a decision
    /// wakes it. Long-term memory on a harness that requires authenticated identity reads
    /// the store too: a session that ran in an earlier process is distilled only when its
    /// latest run was authenticated (I42).
    ///
    /// [`start_goal`]: Self::start_goal
    /// [`goal_target`]: Self::goal_target
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runs: Arc<dyn adk_core::DurableRunStore> = Arc::new(adk_core::InMemoryRunStore::new());
    /// let harness = harness.with_run_store(runs.clone());
    /// let (target, runner) = harness.goal_target();
    /// let worker = adk_server::background::DurableWorker::new(runs).with_agent(target, runner).spawn();
    /// ```
    pub fn with_run_store(mut self, store: Arc<dyn adk_core::DurableRunStore>) -> Self {
        #[cfg(feature = "memory")]
        if let Ok(facts) = self.facts_runtime() {
            *facts.runs.lock().unwrap_or_else(|e| e.into_inner()) = Some(store.clone());
        }
        self.run_store = Some(store);
        self
    }

    /// Enqueues durable goals on `queue` instead of `default` (spec 3.1 §7.11), so a
    /// `DurableWorker` claiming from it runs goals only and the store's other runs stay
    /// with their own workers — for example [`GOAL_QUEUE`](crate::GOAL_QUEUE) beside a
    /// server's background runs on one store.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let harness = harness.with_run_store(runs.clone()).with_goal_queue(adk_harness::GOAL_QUEUE);
    /// let (target, runner) = harness.goal_target();
    /// let worker = DurableWorker::new(runs)
    ///     .with_agent(target, runner)
    ///     .with_config(DurableWorkerConfig::default().with_queue(harness.goal_queue()))
    ///     .spawn();
    /// ```
    pub fn with_goal_queue(mut self, queue: impl Into<String>) -> Self {
        self.goal_queue = queue.into();
        self
    }

    /// The queue durable goals are enqueued on (default `default`).
    pub fn goal_queue(&self) -> &str {
        &self.goal_queue
    }

    /// Answers, from the action ledger, the calls a stopped process left without a response,
    /// so a turn after a crash never runs a completed call again (spec 3.1 §7.12, I37).
    async fn recover(&self, user: &UserId, session: &SessionId) -> Result<()> {
        if self.runner.active_session_ids().iter().any(|active| active == session.as_ref()) {
            return Ok(());
        }
        let answered =
            self.runner.recover_unanswered_calls(user.as_ref(), session.as_ref()).await?;
        if answered > 0 {
            tracing::info!(session.id = %session, calls.answered = answered, "recovered calls a stopped run left unanswered");
        }
        Ok(())
    }

    /// Sends a user message and returns the turn's events.
    ///
    /// Held calls stranded by a run that ended before saving their result are settled
    /// first (see [`resume`](Self::resume)).
    ///
    /// # Errors
    ///
    /// Returns an error when the run cannot start, or the approval store's or the action
    /// ledger's error.
    pub async fn send(
        &self,
        user: &UserId,
        session: &SessionId,
        text: impl Into<String>,
    ) -> Result<EventStream> {
        self.turn(user, session, Content::new("user").with_text(text.into()), None).await
    }

    /// [`send`](Self::send) as the caller `context` authenticates (spec 3.3 §6): the run
    /// carries `context`, so the session's mode and standing rules apply as they do to a
    /// request a server admitted. `context` names `user`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::{RequestContext, SessionId, UserId};
    /// use adk_harness::Harness;
    ///
    /// # async fn example(harness: Harness, user: UserId, session: SessionId) -> adk_core::Result<()> {
    /// // The caller the server admitted for `user`.
    /// let context = RequestContext {
    ///     user_id: user.to_string(),
    ///     scopes: vec!["operator".into()],
    ///     metadata: Default::default(),
    /// };
    /// let events = harness.send_as(&user, &session, "create note.txt", context).await?;
    /// # let _ = events;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// As [`send`](Self::send); the run fails with `identity.subject_mismatch` when `context`
    /// names another user.
    pub async fn send_as(
        &self,
        user: &UserId,
        session: &SessionId,
        text: impl Into<String>,
        context: RequestContext,
    ) -> Result<EventStream> {
        self.turn(user, session, Content::new("user").with_text(text.into()), Some(context)).await
    }

    /// Runs an empty turn, which resumes held calls that now have a decision.
    ///
    /// A held call whose approval a run consumed but whose result that run never saved —
    /// the process stopped in between — has no decision and no pending entry, so no run
    /// would resume it. It is settled first (spec 3.1 §7.12, §9.2 I37): when the action
    /// ledger records it, it ran, and its record answers it as the agent answers a
    /// re-issued call, without running it again; otherwise it never began, and without a
    /// live confirmation handler it is held again for [`pending_approvals`](Self::pending_approvals).
    ///
    /// # Errors
    ///
    /// Returns an error when the run cannot start, or the approval store's or the action
    /// ledger's error.
    pub async fn resume(&self, user: &UserId, session: &SessionId) -> Result<EventStream> {
        self.turn(user, session, Content::new("user"), None).await
    }

    /// [`resume`](Self::resume) as the caller `context` authenticates (spec 3.3 §6), as
    /// [`send_as`](Self::send_as) sends.
    ///
    /// # Errors
    ///
    /// As [`resume`](Self::resume); the run fails with `identity.subject_mismatch` when
    /// `context` names another user.
    pub async fn resume_as(
        &self,
        user: &UserId,
        session: &SessionId,
        context: RequestContext,
    ) -> Result<EventStream> {
        self.turn(user, session, Content::new("user"), Some(context)).await
    }

    /// A turn of `content`, settling stranded held calls first, run as `context` when given.
    async fn turn(
        &self,
        user: &UserId,
        session: &SessionId,
        content: Content,
        context: Option<RequestContext>,
    ) -> Result<EventStream> {
        self.settle_stranded_calls(user, session).await?;
        self.recover(user, session).await?;
        let config = context.map(|context| RunConfig {
            request_context: Some(context),
            ..self.runner.run_config().clone()
        });
        self.runner.run_with_config(user.clone(), session.clone(), content, config).await
    }

    /// Answers from the action ledger, or holds again, the session's held calls that have
    /// neither a decision nor a pending entry (spec 3.1 §7.12).
    async fn settle_stranded_calls(&self, user: &UserId, session: &SessionId) -> Result<()> {
        // An active run may have taken a decision and not yet begun the call.
        if self.runner.active_session_ids().iter().any(|active| active == session.as_ref()) {
            return Ok(());
        }
        let runtime = self.agent.runtime();
        let store = runtime.approval_store();
        let scope =
            ApprovalScope::new(self.app_name.clone(), user.to_string(), session.to_string());
        let events = self.events(user, session).await?;
        let mut holders = Vec::<(String, String)>::new();
        for event in &events {
            if event.actions.tool_confirmation.is_some()
                && !holders.iter().any(|(a, b)| *a == event.author && *b == event.branch)
            {
                holders.push((event.author.clone(), event.branch.clone()));
            }
        }
        if holders.is_empty() {
            return Ok(());
        }
        let pending: Vec<String> =
            store.pending(&scope).await?.into_iter().map(|entry| entry.fingerprint).collect();
        let ledger = runtime.ledgers.for_run(self.runner.run_config().action_ledger.as_ref());
        for (author, branch) in holders {
            let mut answers = Vec::new();
            let mut answered = Vec::<String>::new();
            for call in held_tool_calls(&events, &author, &branch) {
                let Some(call_id) = call.id.as_deref() else {
                    continue;
                };
                let fingerprint = call.request.fingerprint();
                if pending.contains(&fingerprint)
                    || store.has_decision(&scope, &fingerprint).await?
                {
                    continue;
                }
                let key = crate::ledger::held_call_key(
                    &self.app_name,
                    user.as_ref(),
                    session.as_ref(),
                    call_id,
                    &fingerprint,
                );
                match ledger.get(&key).await? {
                    // A delegation holding several member calls is one call: one answer.
                    Some(_) if answered.contains(&call_id.to_string()) => {}
                    Some(record) => {
                        tracing::warn!(
                            tool.name = %call.name,
                            function_call.id = %call_id,
                            "held call ran without its result being saved; answering it from the action ledger"
                        );
                        answered.push(call_id.to_string());
                        answers.push(Part::FunctionResponse {
                            function_response: FunctionResponseData::new(
                                call.name.clone(),
                                crate::ledger::recorded_answer(&call.name, &record),
                            ),
                            id: call.id.clone(),
                            annotations: None,
                        });
                    }
                    None if runtime.confirmation_handler().is_none() => {
                        tracing::warn!(
                            tool.name = %call.name,
                            function_call.id = %call_id,
                            "held call lost its decision before it began; holding it again"
                        );
                        store.record_pending(&scope, &call.request).await?;
                    }
                    // A live handler asks the person when the next run resumes the call.
                    None => {}
                }
            }
            if answers.is_empty() {
                continue;
            }
            let mut event = Event::new(format!("harness-ledger-{}", uuid::Uuid::new_v4()));
            event.author = author;
            event.branch = branch;
            event.llm_response.content =
                Some(Content { role: "function".to_string(), parts: answers });
            self.sessions.append_event(session.as_ref(), event).await?;
        }
        Ok(())
    }

    /// The session's current mode.
    pub async fn mode(&self, user: &UserId, session: &SessionId) -> Result<HarnessMode> {
        let record = self
            .agent
            .runtime()
            .mode_store()
            .get(&self.app_name, user.as_ref(), session.as_ref())
            .await?;
        Ok(record.map_or(self.agent.runtime().policy().default_mode(), |record| record.mode))
    }

    fn audit(&self, user: &UserId, session: &SessionId) -> Option<HarnessAudit> {
        let trail = self.runner.audit_trail()?.clone();
        Some(HarnessAudit::new(
            trail,
            AuditActor::subject(SubjectRef::new(SubjectKind::Human, user.to_string())),
            session.to_string(),
        ))
    }

    /// Sets the session's mode (spec 3.1 §7.4).
    ///
    /// Only outside a run. A loosening first answers every held call in the session with a
    /// "cancelled by mode change" response and removes its pending entry and any decision
    /// for it, so no call a person has not seen under the looser mode runs on the next
    /// resume or on a later identical call (spec 3.1 §9.2 I14).
    ///
    /// # Errors
    ///
    /// Returns `harness.mode.run_active` while a run of the session is active, or the
    /// store's error.
    pub async fn set_mode(
        &self,
        user: &UserId,
        session: &SessionId,
        mode: HarnessMode,
    ) -> Result<()> {
        if self.runner.active_session_ids().iter().any(|active| active == session.as_ref()) {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Conflict,
                "harness.mode.run_active",
                "the session has an active run; set the mode when it ends",
            ));
        }
        let runtime = self.agent.runtime();
        let current = self.mode(user, session).await?;
        let audit = self.audit(user, session);
        let mut cancelled = Vec::new();
        let mut cancelled_fingerprints = Vec::new();
        if ModeChange::between(current, mode) == ModeChange::Loosen {
            let events = self.events(user, session).await?;
            let mut holders = Vec::<(String, String)>::new();
            for event in &events {
                if event.actions.tool_confirmation.is_some()
                    && !holders.iter().any(|(a, b)| *a == event.author && *b == event.branch)
                {
                    holders.push((event.author.clone(), event.branch.clone()));
                }
            }
            for (author, branch) in holders {
                let held = held_tool_calls(&events, &author, &branch);
                if held.is_empty() {
                    continue;
                }
                let mut event = Event::new(format!("harness-mode-{}", uuid::Uuid::new_v4()));
                event.author = author.clone();
                event.branch = branch.clone();
                // A delegation holding several member calls is one call with several
                // requests, and a call gets exactly one response.
                let mut answered = Vec::<String>::new();
                event.llm_response.content = Some(Content {
                    role: "function".to_string(),
                    parts: held
                        .iter()
                        .filter(|call| match &call.id {
                            Some(id) if answered.contains(id) => false,
                            Some(id) => {
                                answered.push(id.clone());
                                true
                            }
                            None => true,
                        })
                        .map(|call| Part::FunctionResponse {
                            function_response: FunctionResponseData::new(
                                call.name.clone(),
                                json!({
                                    "error": format!("Tool '{}' was not run: cancelled by mode change to {mode}", call.name),
                                    "status": CANCELLED_BY_MODE_CHANGE_STATUS,
                                }),
                            ),
                            id: call.id.clone(),
                            annotations: None,
                        })
                        .collect(),
                });
                cancelled.extend(answered);
                cancelled_fingerprints.extend(held.iter().map(|call| call.request.fingerprint()));
                self.sessions.append_event(session.as_ref(), event).await?;
            }
            let reason = format!("cancelled by mode change to {mode}");
            self.clear_inbox(user, session, &cancelled_fingerprints, &reason).await?;
            if let Some(audit) = &audit
                && !cancelled.is_empty()
            {
                audit
                    .record(
                        "harness.held.cancelled",
                        json!({ "callIds": cancelled, "reason": format!("mode changed from {current} to {mode}") }),
                    )
                    .await?;
            }
        }
        let record = write_mode_change(
            runtime.mode_store().as_ref(),
            runtime.policy().default_mode(),
            (&self.app_name, user.as_ref(), session.as_ref()),
            mode,
            ModeSource::User,
            None,
            audit.as_ref(),
            &cancelled,
        )
        .await?;
        let mut mirror = Event::new(format!("harness-mode-{}", uuid::Uuid::new_v4()));
        mirror.author = self.agent.info().name.clone();
        mirror.actions.state_delta.insert(
            MODE_STATE_KEY.to_string(),
            json!({ "mode": record.mode, "revision": record.revision, "setBy": record.set_by }),
        );
        self.sessions.append_event(session.as_ref(), mirror).await?;
        Ok(())
    }

    /// Removes the pending entries and any decisions of `fingerprints`, so no later call
    /// with the same fingerprint is authorized by an entry its cancelled call left (I14).
    async fn clear_inbox(
        &self,
        user: &UserId,
        session: &SessionId,
        fingerprints: &[String],
        reason: &str,
    ) -> Result<()> {
        if fingerprints.is_empty() {
            return Ok(());
        }
        let runtime = self.agent.runtime();
        let scope =
            ApprovalScope::new(self.app_name.clone(), user.to_string(), session.to_string());
        let store = runtime.approval_store();
        for fingerprint in fingerprints {
            store.decide(&scope, fingerprint, ToolApproval::deny().with_message(reason)).await?;
            store.take_approval(&scope, fingerprint).await?;
        }
        if let Some(service) = runtime.approval_service() {
            let mut query = adk_core::InboxQuery::default().with_app_name(self.app_name.clone());
            loop {
                let page = service.inbox(query.clone()).await?;
                for request in &page.items {
                    if request.scope == scope && fingerprints.contains(&request.fingerprint) {
                        service.cancel(&request.id, Some(reason.to_string())).await?;
                    }
                }
                match page.next_cursor {
                    Some(cursor) => query.cursor = Some(cursor),
                    None => break,
                }
            }
        }
        Ok(())
    }

    async fn events(&self, user: &UserId, session: &SessionId) -> Result<Vec<Event>> {
        Ok(self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await?
            .events()
            .all())
    }

    /// The session's todo list (spec 3.1 §7.6).
    pub async fn todos(&self, user: &UserId, session: &SessionId) -> Result<TodoList> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: Some(1),
                after: None,
            })
            .await?;
        Ok(session
            .state()
            .get(TODOS_STATE_KEY)
            .map(|value| TodoList::from_value(&value))
            .unwrap_or_default())
    }

    /// The spend of `user`, or of every user, per acting agent and vendor: the report
    /// behind the CLI's `/cost` and the web UI's `harness.spend` (spec 3.1 §7.13).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let report = harness.spend(Some(&user));
    /// println!("{}", report.render());
    /// ```
    #[cfg(feature = "observe")]
    pub fn spend(&self, user: Option<&UserId>) -> crate::observe::SpendReport {
        self.agent.runtime().spend(user.map(UserId::as_ref))
    }

    /// Calls of the session waiting for a decision.
    pub async fn pending_approvals(
        &self,
        user: &UserId,
        session: &SessionId,
    ) -> Result<Vec<PendingApproval>> {
        self.agent
            .runtime()
            .approval_store()
            .pending(&ApprovalScope::new(
                self.app_name.clone(),
                user.to_string(),
                session.to_string(),
            ))
            .await
    }

    /// Records a person's decision for the pending call with `fingerprint`; the next
    /// [`resume`](Self::resume) runs or refuses it (spec 3.1 §7.3). The call's open request
    /// in the approval service, if any, is withdrawn first, so it cannot decide the call
    /// again and no run is woken before the person's decision is recorded (spec 3.3 §7.3).
    ///
    /// A remembered rule must pin the call's exact declaration, and no rule is remembered
    /// for a non-waivable call.
    ///
    /// # Errors
    ///
    /// Returns `harness.decision.invalid` when no pending call has `fingerprint`, a rule
    /// cannot be remembered for the call, another decision for the call is being recorded in
    /// this process, or an approver, an expiry or another decision settled the call first —
    /// before its inbox request was withdrawn or while the person's decision was being
    /// recorded — or the store's error.
    pub async fn decide(
        &self,
        user: &UserId,
        session: &SessionId,
        fingerprint: &str,
        decision: HarnessDecision,
    ) -> Result<()> {
        let runtime = self.agent.runtime();
        let scope =
            ApprovalScope::new(self.app_name.clone(), user.to_string(), session.to_string());
        let _deciding = Deciding::claim((
            Arc::as_ptr(runtime.approval_store()).cast::<()>().addr(),
            scope.clone(),
            fingerprint.to_string(),
        ))?;
        let pending = runtime.approval_store().pending(&scope).await?;
        // A decision for a call that is not pending — decided, cancelled by a mode change, or
        // never held — would authorize a later call with the same fingerprint unasked (I14).
        let request = pending
            .iter()
            .find(|entry| entry.fingerprint == fingerprint)
            .map(|entry| entry.request.clone())
            .ok_or_else(|| {
                store_error(
                    "no pending call has this fingerprint: it was decided, cancelled by a mode \
                     change, or never held",
                )
            })?;
        let (approval, remember) = match decision {
            HarnessDecision::Approve => (ToolApproval::approve(), None),
            HarnessDecision::ApproveWithArgs(args) => {
                (ToolApproval::approve().with_edited_args(args), None)
            }
            HarnessDecision::ApproveAndRemember(rule) => {
                (ToolApproval::approve(), Some((rule, StandingDecision::Allow)))
            }
            HarnessDecision::Deny { message } => {
                let approval = ToolApproval::deny();
                (
                    match message {
                        Some(message) => approval.with_message(message),
                        None => approval,
                    },
                    None,
                )
            }
            HarnessDecision::DenyAndRemember(rule) => (
                ToolApproval::deny(),
                Some((rule, StandingDecision::Deny { reason: format!("denied by {user}") })),
            ),
        };
        // A rule that cannot be remembered refuses the decision before the inbox request is
        // withdrawn, so the call stays where every surface shows it (spec 3.3 §7.1).
        let remember = match remember {
            Some((suggestion, rule_decision)) => {
                let owner = RuleOwner {
                    app_name: self.app_name.clone(),
                    user_id: user.to_string(),
                    subject_kind: SubjectKind::Human,
                    session_id: session.to_string(),
                    created_by: SubjectRef::new(SubjectKind::Human, user.to_string()),
                };
                Some(self.agent.rule_for_held_call(&request, suggestion, rule_decision, &owner)?)
            }
            None => None,
        };
        // Withdrawn requests, reported to the kernel once the person's decision is recorded.
        #[cfg(feature = "kernel")]
        let mut withdrawn = Vec::new();
        // The inbox request is withdrawn, not cancelled: a cancel writes a denial and wakes
        // the run before the person's decision is recorded (spec 3.3 §7.3).
        if let Some(service) = runtime.approval_service() {
            let mut query = adk_core::InboxQuery::default()
                .with_session(&scope)
                .with_states(adk_core::ApprovalState::ALL);
            loop {
                let page = service.inbox(query.clone()).await?;
                let open = page
                    .items
                    .iter()
                    .filter(|item| item.fingerprint == fingerprint && item.is_open());
                for item in open {
                    let mut attempts = 0;
                    loop {
                        match service.withdraw(&item.id, Some(DECIDED_IN_THE_HARNESS.into())).await
                        {
                            Ok(_closed) => {
                                #[cfg(feature = "kernel")]
                                withdrawn.push(_closed);
                                break;
                            }
                            // A partial quorum or an escalation step moved it; still pending.
                            Err(error)
                                if error.code == "approval.version_conflict" && attempts < 8 =>
                            {
                                attempts += 1;
                            }
                            Err(error) if error.code == "approval.already_resolved" => {
                                return Err(store_error(
                                    "an approver decided this call in the approvals inbox first",
                                ));
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
                match page.next_cursor {
                    Some(cursor) => query = query.with_cursor(Some(cursor)),
                    None => break,
                }
            }
        }
        #[cfg(feature = "kernel")]
        let reported = approval.clone();
        let recorded = async move {
            // An approver's outcome, an expiry or another run's decision may have settled the
            // call while its requests were withdrawn; the person's decision must not outlive
            // it (spec 3.3 §7.3, §11 S-I2).
            if !runtime
                .approval_store()
                .pending(&scope)
                .await?
                .iter()
                .any(|entry| entry.fingerprint == fingerprint)
            {
                return Err(store_error(
                    "the call was decided another way while this decision was being recorded",
                ));
            }
            if let Some(rule) = remember {
                if let Some(audit) = self.audit(user, session) {
                    let fields = rule_audit_fields(&rule, request.function_call_id.as_deref());
                    audit.record("harness.rule.created", fields).await?;
                }
                runtime.standing_rules().grant(rule).await?;
            }
            runtime.approval_store().decide(&scope, fingerprint, approval).await
        }
        .await;
        #[cfg(feature = "kernel")]
        for closed in &withdrawn {
            let outcome = recorded.as_ref().ok().map(|()| &reported);
            runtime.kernel_approvals.decided_in_the_harness(closed, outcome).await;
        }
        recorded?;
        self.wake_parked(user, session, fingerprint).await
    }

    /// Wakes the session's durable runs parked on the call with `fingerprint`, as the
    /// approval service does, so a durable goal resumes once it is decided (spec 3.1 §7.11).
    async fn wake_parked(
        &self,
        user: &UserId,
        session: &SessionId,
        fingerprint: &str,
    ) -> Result<()> {
        let Some(runs) = &self.run_store else {
            return Ok(());
        };
        let parked = runs
            .list(
                adk_core::RunFilter::default()
                    .app_name(self.app_name.clone())
                    .user_id(user.to_string())
                    .session_id(session.to_string())
                    .states([adk_core::RunState::Waiting]),
            )
            .await?;
        for run in parked {
            if let Some(adk_core::RunCheckpointRef::AwaitingApproval { fingerprints, .. }) =
                &run.checkpoint
                && fingerprints.iter().any(|parked| parked == fingerprint)
            {
                runs.wake(&run.run_id).await?;
                tracing::info!(run.id = %run.run_id, "woke the durable run waiting for this decision");
            }
        }
        Ok(())
    }

    /// Records the workspace trust decision for the harness's workspace (spec 3.1 §7.7),
    /// bound to the instruction files the harness loaded: a later change to any of them —
    /// an edit, a new file, a removed one — leaves the workspace untrusted and records
    /// `harness.instructions.changed` until the next decision (spec 3.4 §10 P2).
    ///
    /// # Errors
    ///
    /// Returns the trust store's or the audit log's error.
    pub async fn trust_workspace(&self, user: &UserId, decision: TrustDecision) -> Result<()> {
        let runtime = self.agent.runtime();
        let digest = runtime.instructions().root_digest().to_string();
        let files = runtime.instructions().instructions_digest();
        if let Some(trail) = self.runner.audit_trail() {
            HarnessAudit::new(
                trail.clone(),
                AuditActor::subject(SubjectRef::new(SubjectKind::Human, user.to_string())),
                "",
            )
            .record(
                "harness.workspace.trust",
                json!({
                    "files": runtime.instructions().audit_files(decision == TrustDecision::Trusted),
                    "rootDigest": digest,
                    "instructionsDigest": files,
                    "decision": decision,
                    "by": user.to_string(),
                }),
            )
            .await?;
        }
        runtime.trust_store().set(
            WorkspaceTrust::new(digest, decision, user.to_string()).with_instructions_digest(files),
        )
    }

    /// Starts a goal for the session (spec 3.1 §7.11): the harness iterates until the
    /// goal's `until` command exits 0 or its iterations run out.
    ///
    /// Goals run only in Ask or Auto. The goal state is written to the goal store and
    /// mirrored to `harness:goal`. With a run store ([`with_run_store`](Self::with_run_store))
    /// the goal is enqueued as a durable run — `NewRun { target: "harness.goal",
    /// session_id, concurrency_key: session, idempotency_key: "goal:<session>:<digest>" }` —
    /// and the returned stream holds one event naming the run; otherwise the goal's first
    /// turn runs here and the stream is its events. While the goal runs, every turn of the
    /// session continues it.
    ///
    /// # Errors
    ///
    /// Returns `harness.goal.invalid` for an incomplete spec, `harness.goal.mode` outside
    /// Ask and Auto, `harness.goal.duplicate` when the same goal already ran in the session
    /// as a durable run, or the store's or runner's error.
    pub async fn start_goal(
        &self,
        user: &UserId,
        session: &SessionId,
        spec: GoalSpec,
    ) -> Result<EventStream> {
        let mode = self.mode(user, session).await?;
        let store = self.agent.runtime().goal_store().clone();
        let record = crate::goal::begin(
            store.as_ref(),
            mode,
            (&self.app_name, user.as_ref(), session.as_ref()),
            spec,
        )
        .await?;
        let mut mirror = Event::new(format!("harness-goal-{}", uuid::Uuid::new_v4()));
        mirror.author = self.agent.info().name.clone();
        mirror.actions.state_delta.insert(
            GOAL_STATE_KEY.to_string(),
            serde_json::to_value(&record.state).unwrap_or(Value::Null),
        );
        self.sessions.append_event(session.as_ref(), mirror).await?;
        tracing::info!(session.id = %session, goal.max_iterations = record.spec.max_iterations, "goal started");
        let Some(runs) = &self.run_store else {
            return self
                .runner
                .run(
                    user.clone(),
                    session.clone(),
                    Content::new("user").with_text(record.spec.prompt()),
                )
                .await;
        };
        let run = adk_core::NewRun::new(
            self.goal_queue.clone(),
            self.app_name.clone(),
            user.to_string(),
            self.goal_target_name(),
            json!({ "text": record.spec.prompt() }),
        )
        .with_session_id(session.to_string())
        .with_idempotency_key(format!("goal:{session}:{}", record.spec.digest()))
        .with_concurrency_key(session.to_string(), adk_core::OnConflict::Enqueue);
        let (run, note) = match runs.enqueue(run).await? {
            adk_core::Enqueued::Created(run) => (run, "enqueued"),
            adk_core::Enqueued::Existing(run) if !run.state.is_terminal() => {
                (run, "already enqueued")
            }
            adk_core::Enqueued::Existing(run) => {
                let mut cancelled = record.clone();
                cancelled.state.status = crate::goal::GoalStatus::Cancelled;
                store.set(&self.app_name, user.as_ref(), session.as_ref(), cancelled).await?;
                return Err(AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::Conflict,
                    "harness.goal.duplicate",
                    format!(
                        "this goal already ran in the session as durable run {}; change the goal or use another session",
                        run.run_id
                    ),
                ));
            }
            adk_core::Enqueued::Rejected { blocking_run_id } => {
                return Err(AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::Conflict,
                    "harness.goal.rejected",
                    format!("run {blocking_run_id} holds the session"),
                ));
            }
        };
        let mut event = Event::new(format!("harness-goal-{}", uuid::Uuid::new_v4()));
        event.author = self.agent.info().name.clone();
        event.llm_response.content = Some(
            Content::new("model")
                .with_text(format!("The goal runs as durable run {} ({note}).", run.run_id)),
        );
        event.provider_metadata.insert("harness.goal.run_id".to_string(), run.run_id.clone());
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }

    /// The session's goal (spec 3.1 §7.11), read from the goal store.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn goal(&self, user: &UserId, session: &SessionId) -> Result<Option<GoalState>> {
        Ok(self
            .agent
            .runtime()
            .goal_store()
            .get(&self.app_name, user.as_ref(), session.as_ref())
            .await?
            .map(|record| record.state))
    }

    /// The durable-run target and runner a `DurableWorker` runs goals with
    /// (`DurableWorker::with_agent`, spec 3.1 §7.11): [`GOAL_TARGET`] and this harness's
    /// runner, whose harness continues a session's running goal on every turn. A partition's
    /// harness runs goals as `harness.goal@<partition>` (spec 3.2 §7.8).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let (target, runner) = harness.goal_target();
    /// assert_eq!(target, "harness.goal");
    /// ```
    pub fn goal_target(&self) -> (String, Arc<Runner>) {
        (self.goal_target_name(), self.runner.clone())
    }

    /// [`GOAL_TARGET`], or `harness.goal@<partition>` for a partition's harness.
    fn goal_target_name(&self) -> String {
        #[cfg(feature = "kernel")]
        if let Some(partition) = self.agent.partition() {
            return crate::partition::partition_target(GOAL_TARGET, &partition.id);
        }
        GOAL_TARGET.to_string()
    }

    /// The session service the harness runner uses.
    #[cfg(feature = "kernel")]
    pub(crate) fn sessions(&self) -> &Arc<dyn SessionService> {
        &self.sessions
    }

    /// The durable run store goals are enqueued on, when one is set.
    #[cfg(feature = "kernel")]
    pub(crate) fn run_store(&self) -> Option<&Arc<dyn adk_core::DurableRunStore>> {
        self.run_store.as_ref()
    }

    /// An ambient goal (spec 3.1 §7.11, feature `ambient`): every event of `source` — a
    /// `CronTrigger`, `FileWatchTrigger` or `WebhookTrigger` — starts `spec` for `user` in
    /// `session`, or in a new session per trigger when `session` is `None`, under the same
    /// policy and budget as any goal. Start it with `AmbientAgent::start`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let nightly = harness.clone().ambient_goal(
    ///     Arc::new(adk_agent::CronTrigger::new("0 0 2 * * *")?),
    ///     UserId::new("ops")?,
    ///     None,
    ///     GoalSpec { goal: "make main green".into(), until: "cargo test".into(), max_iterations: 5, check_timeout: Duration::from_secs(900) },
    /// );
    /// ```
    #[cfg(feature = "ambient")]
    pub fn ambient_goal(
        self: Arc<Self>,
        source: Arc<dyn adk_agent::EventSource>,
        user: UserId,
        session: Option<SessionId>,
        spec: GoalSpec,
    ) -> adk_agent::AmbientAgent {
        let agent: Arc<dyn adk_core::Agent> = self.agent.clone();
        let handler: adk_agent::ambient::TriggerHandler = Arc::new(move |event, _agent| {
            let harness = self.clone();
            let (user, session, spec) = (user.clone(), session.clone(), spec.clone());
            Box::pin(async move {
                let session = harness.open_session(&user, session.as_ref()).await?;
                tracing::info!(trigger.source = %event.source, session.id = %session, "ambient goal triggered");
                harness.start_goal(&user, &session, spec).await
            })
        });
        adk_agent::AmbientAgent::new(agent, source).with_trigger_handler(handler)
    }

    /// Where standing rules are kept.
    pub fn standing_rules(&self) -> &Arc<dyn StandingRuleStore> {
        self.agent.runtime().standing_rules()
    }

    /// Interrupts the session's active run.
    pub fn interrupt(&self, session: &SessionId) -> bool {
        self.runner.interrupt(session.as_ref())
    }

    /// The runner.
    pub fn runner(&self) -> &Runner {
        &self.runner
    }

    /// Closes the runner and the agent's toolsets, stopping the container shell's session
    /// containers (spec 3.1 §7.9).
    ///
    /// Sessions with turns not yet distilled are distilled first (spec 3.1 §7.15.2); a
    /// distillation that fails is logged at `warn`. [`close_with_report`](Self::close_with_report)
    /// returns those failures instead.
    pub async fn close(&self) {
        self.close_with_report().await;
    }

    /// Closes the harness as [`close`](Self::close) does and reports what it could not
    /// finish: each session whose turns were not distilled into long-term memory, with
    /// the error (spec 3.1 §7.15.2).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let report = harness.close_with_report().await;
    /// for (session, error) in &report.distillation_failures {
    ///     eprintln!("long-term memory was not updated for session {session}: {error}");
    /// }
    /// ```
    pub async fn close_with_report(&self) -> CloseReport {
        #[cfg_attr(not(feature = "memory"), allow(unused_mut))]
        let mut report = CloseReport::default();
        #[cfg(feature = "memory")]
        if let Some(facts) = self.agent.runtime().toolset().facts() {
            report.distillation_failures = facts
                .close()
                .await
                .into_iter()
                .map(|((_, _, session), error)| (session, error))
                .collect();
        }
        // The runner closes the root agent: the coordinator's toolsets through a compiled
        // team root, the harness toolset included.
        self.runner.close().await;
        report
    }

    #[cfg(feature = "memory")]
    fn facts_runtime(&self) -> Result<&Arc<crate::facts::runtime::FactsRuntime>> {
        self.agent.runtime().toolset().facts().ok_or_else(|| {
            AdkError::config(
                "long-term memory is off for this harness; turn it on with \
                 HarnessAgentBuilder::long_term_memory",
            )
        })
    }

    /// The long-term facts of `user` in this harness's project, with the user's
    /// preferences, newest first and in every status (spec 3.1 §7.15.2).
    ///
    /// # Errors
    ///
    /// Returns a configuration error when long-term memory is off, or the store's error.
    #[cfg(feature = "memory")]
    pub async fn facts(&self, user: &UserId) -> Result<Vec<crate::facts::MemoryFact>> {
        let facts = self.facts_runtime()?;
        let scope = facts.scope(&self.app_name, user.as_ref());
        // A partition's harness lists its own scope only (spec 3.2 K-I15).
        let base = match facts.confined {
            true => crate::facts::FactFilter::exactly(&scope),
            false => crate::facts::FactFilter::recall(&scope),
        };
        let filter = crate::facts::FactFilter { live_only: false, ..base };
        facts.store.list(filter).await
    }

    /// Promotes `user`'s assumed fact `id` to `Known`, source `UserConfirmed`: a person's
    /// confirmation (spec 3.1 §7.15.2, §9.2 I41). Records `harness.memory.fact.promoted`.
    ///
    /// # Errors
    ///
    /// Returns `harness.memory.fact_not_found` for a fact that is not the user's in this
    /// project, `harness.memory.fact_not_live` for a superseded or forgotten one, or the
    /// store's or audit trail's error.
    #[cfg(feature = "memory")]
    pub async fn accept_fact(&self, user: &UserId, id: &str) -> Result<crate::facts::MemoryFact> {
        let facts = self.facts_runtime()?;
        let fact = facts.owned(&self.app_name, user.as_ref(), id).await?;
        if fact.status != crate::facts::FactStatus::Known {
            let by = SubjectRef::human(user.to_string());
            facts
                .change_status(
                    facts.audit_as(by.clone(), "").as_ref(),
                    &fact,
                    crate::facts::FactStatus::Known,
                    &by,
                )
                .await?;
        }
        facts.owned(&self.app_name, user.as_ref(), id).await
    }

    /// Forgets `user`'s fact `id`: it is marked `Forgotten` at once, dropped from the index,
    /// and never recalled from the next model call on (spec 3.1 §9.2 I44). Records
    /// `harness.memory.fact.forgotten`.
    ///
    /// # Errors
    ///
    /// As [`accept_fact`](Self::accept_fact).
    #[cfg(feature = "memory")]
    pub async fn forget_fact(&self, user: &UserId, id: &str) -> Result<()> {
        let facts = self.facts_runtime()?;
        let fact = facts.owned(&self.app_name, user.as_ref(), id).await?;
        let by = SubjectRef::human(user.to_string());
        facts
            .change_status(
                facts.audit_as(by.clone(), "").as_ref(),
                &fact,
                crate::facts::FactStatus::Forgotten,
                &by,
            )
            .await
    }

    /// Replaces `user`'s fact `id` with `text`, a person's correction: the new fact is
    /// `Known` with source `UserConfirmed`, and the old one `Superseded` by it, its history
    /// kept (spec 3.1 §7.15.2).
    ///
    /// # Errors
    ///
    /// As [`accept_fact`](Self::accept_fact), and `harness.memory.rejected` for a text that
    /// is empty, too long or holds a secret.
    #[cfg(feature = "memory")]
    pub async fn correct_fact(
        &self,
        user: &UserId,
        id: &str,
        text: impl Into<String>,
    ) -> Result<crate::facts::MemoryFact> {
        let facts = self.facts_runtime()?;
        let fact = facts.owned(&self.app_name, user.as_ref(), id).await?;
        facts.correct(&SubjectRef::human(user.to_string()), &fact, &text.into()).await
    }

    /// Distils those of `user`'s `sessions` the per-session distiller never reached, with
    /// verified-quote promotion off, so every fact it stores is `Assumed` (spec 3.2 §10 HK2,
    /// §9.2 K-I12): the public distillation entry a consolidation tick runs.
    ///
    /// The store's distillation cursor records how far each session was read, by this
    /// entry and by the per-session distiller, so no session is distilled twice. Skipped:
    /// a session read up to its last event, one this process will still distil, one active
    /// within `distill_idle` of `at`, and one in ReadOnly or Plan mode. On a harness that
    /// requires authenticated identity, a session that ran in an earlier process is
    /// distilled only when its latest run in the run store
    /// ([`with_run_store`](Self::with_run_store)) was authenticated (I42). `budget` bounds the
    /// model calls of all sessions together; without one, each session's run has the
    /// harness's budget or two model calls.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let report = harness.distill_sessions(&user, &["s1".into()], chrono::Utc::now(), None).await?;
    /// assert!(report.stored.len() <= 10);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error when long-term memory is off, or the store's error.
    #[cfg(feature = "memory")]
    pub async fn distill_sessions(
        &self,
        user: &UserId,
        sessions: &[String],
        at: chrono::DateTime<chrono::Utc>,
        budget: Option<adk_core::RunBudget>,
    ) -> Result<crate::facts::SessionDistillation> {
        self.facts_runtime()?
            .distill_sessions(&self.app_name, user.as_ref(), sessions, at, budget)
            .await
    }

    /// How far distillation has read each of `user`'s sessions: the session id and the time
    /// of the last event read, by the per-session distiller or
    /// [`distill_sessions`](Self::distill_sessions) (spec 3.2 §10 HK2). A session absent from
    /// the list was never distilled. A host compares it with each session's last event to
    /// find the sessions that ended without being distilled (spec 3.1 §7.33 fix 9).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let read: std::collections::HashMap<_, _> =
    ///     harness.distilled_sessions(&user).await?.into_iter().collect();
    /// let unsaved = read.get("s1").is_none_or(|upto| *upto < last_event_at);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error when long-term memory is off, or the store's error.
    #[cfg(feature = "memory")]
    pub async fn distilled_sessions(
        &self,
        user: &UserId,
    ) -> Result<Vec<(String, chrono::DateTime<chrono::Utc>)>> {
        self.facts_runtime()?.store.distilled_sessions(&self.app_name, user.as_ref()).await
    }

    /// Runs one consolidation tick over `user`'s facts in this harness's project and the
    /// user-wide ones (spec 3.2 §7.5, §10 HK2): the sessions in `request` the per-session
    /// distiller never reached are distilled as [`distill_sessions`](Self::distill_sessions)
    /// does; pass 1 collapses `Assumed` near-duplicates and expires stale `Assumed` facts
    /// without a model call; pass 2 merges similar `Assumed` facts on the distiller model.
    /// `Known` facts are never superseded, merged or expired (K-I22). When the passes would
    /// change more than `max_loss_fraction` of a scope's live facts, they apply nothing and
    /// the record says why. The record is kept in the fact store for
    /// [`undo_consolidation`](Self::undo_consolidation), and one
    /// `harness.memory.consolidated` audit record lists ids and counts, never text.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let record = harness
    ///     .consolidate_memory(&user, adk_harness::ConsolidationRequest::new().with_sessions(["s1"]))
    ///     .await?;
    /// println!("{} superseded, {} forgotten", record.superseded().count(), record.forgotten().count());
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error when long-term memory is off,
    /// `harness.memory.consolidation_unsupported` for a fact store that keeps no records,
    /// or the store's or audit trail's error.
    #[cfg(feature = "memory")]
    pub async fn consolidate_memory(
        &self,
        user: &UserId,
        request: crate::facts::ConsolidationRequest,
    ) -> Result<crate::facts::ConsolidationRecord> {
        self.facts_runtime()?.consolidate(&self.app_name, user.as_ref(), request).await
    }

    /// `user`'s consolidation records, newest first (spec 3.2 §7.5).
    ///
    /// # Errors
    ///
    /// Returns a configuration error when long-term memory is off, or the store's error.
    #[cfg(feature = "memory")]
    pub async fn consolidations(
        &self,
        user: &UserId,
    ) -> Result<Vec<crate::facts::ConsolidationRecord>> {
        self.facts_runtime()?.store.consolidations(&self.app_name, user.as_ref()).await
    }

    /// Reverts `user`'s consolidation tick `tick_id` (an id or a unique prefix) as
    /// [`undo_consolidation`](crate::undo_consolidation) does, and brings the index in line
    /// (spec 3.2 §7.5).
    ///
    /// # Errors
    ///
    /// As [`undo_consolidation`](crate::undo_consolidation), and a configuration error when
    /// long-term memory is off.
    #[cfg(feature = "memory")]
    pub async fn undo_consolidation(
        &self,
        user: &UserId,
        tick_id: &str,
    ) -> Result<crate::facts::ConsolidationUndo> {
        let facts = self.facts_runtime()?;
        let by = SubjectRef::human(user.to_string());
        let report = crate::facts::undo_consolidation(
            facts.store.as_ref(),
            &self.app_name,
            user.as_ref(),
            tick_id,
            &by,
        )
        .await?;
        for id in report.restored.iter().chain(&report.forgotten) {
            let Some(fact) = facts.store.get(id).await? else {
                continue;
            };
            let indexed = if fact.status.is_live() {
                facts.index.upsert(&fact).await
            } else {
                facts.forget_cached(id);
                facts.index.remove(&fact).await
            };
            if let Err(error) = indexed {
                tracing::warn!(error = %error, "could not update the fact index after an undo");
            }
        }
        Ok(report)
    }
}

/// Lets the agent's long-term memory distil the runner's sessions (spec 3.1 §7.15.2).
#[cfg(feature = "memory")]
fn attach_facts(
    agent: &HarnessAgent,
    runner: &Runner,
    sessions: &Arc<dyn SessionService>,
    app_name: &str,
) {
    if let Some(facts) = agent.runtime().toolset().facts() {
        facts.attach(
            sessions.clone(),
            app_name.to_string(),
            runner.governance().clone(),
            runner.audit_trail().cloned(),
        );
    }
}
