//! `Harness`: a harness agent with its own `Runner` (spec 3.1 §7.3, feature `runner`).

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::audit_log::AuditActor;
use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    AdkError, ApprovalScope, Content, ErrorCategory, ErrorComponent, Event, EventStream,
    FunctionResponseData, Part, PendingApproval, Result, SessionId, ToolApproval, UserId,
    held_tool_calls,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, SessionService};
use serde_json::{Value, json};

use crate::agent::HarnessAgent;
use crate::approvals::{
    RuleOwner, RuleSuggestion, StandingDecision, StandingRule, StandingRuleStore, rule_audit_fields,
};
use crate::audit::HarnessAudit;
use crate::goal::{GOAL_STATE_KEY, GOAL_TARGET, GoalSpec, GoalState};
use crate::instructions::{TrustDecision, WorkspaceTrust};
use crate::mode::{HarnessMode, MODE_STATE_KEY, ModeChange, ModeSource, write_mode_change};
use crate::policy::{TableRow, ToolKind};
use crate::tools::{TODOS_STATE_KEY, TodoList};

/// Status of the response that cancels a held call on a mode loosening (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::CANCELLED_BY_MODE_CHANGE_STATUS, "cancelled_by_mode_change");
/// ```
pub const CANCELLED_BY_MODE_CHANGE_STATUS: &str = "cancelled_by_mode_change";

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
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Harness")
            .field("app_name", &self.app_name)
            .field("agent", &self.agent)
            .finish()
    }
}

fn store_error(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::InvalidInput,
        "harness.decision.invalid",
        message,
    )
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
        let runner = agent.configure_runner(builder).build()?;
        Ok(Self { runner: Arc::new(runner), agent, sessions, app_name, run_store: None })
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
        Ok(Self { runner: Arc::new(runner), agent, sessions, app_name, run_store: None })
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
    /// wakes it.
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
        self.run_store = Some(store);
        self
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
        self.settle_stranded_calls(user, session).await?;
        self.recover(user, session).await?;
        self.runner
            .run(user.clone(), session.clone(), Content::new("user").with_text(text.into()))
            .await
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
        self.settle_stranded_calls(user, session).await?;
        self.recover(user, session).await?;
        self.runner.run(user.clone(), session.clone(), Content::new("user")).await
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
                    Some(record) => {
                        tracing::warn!(
                            tool.name = %call.name,
                            function_call.id = %call_id,
                            "held call ran without its result being saved; answering it from the action ledger"
                        );
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
                event.llm_response.content = Some(Content {
                    role: "function".to_string(),
                    parts: held
                        .iter()
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
                cancelled.extend(held.iter().filter_map(|call| call.id.clone()));
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
    /// [`resume`](Self::resume) runs or refuses it (spec 3.1 §7.3).
    ///
    /// A remembered rule must pin the call's exact declaration, and no rule is remembered
    /// for a non-waivable call.
    ///
    /// # Errors
    ///
    /// Returns `harness.decision.invalid` when no pending call has `fingerprint` or a rule
    /// cannot be remembered for the call, or the store's error.
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
        if let Some((suggestion, rule_decision)) = remember {
            let origin = request
                .origin
                .as_ref()
                .ok_or_else(|| store_error("the pending call records no tool origin"))?;
            let kind = runtime.policy().registry().classify(
                &self.agent.info().name,
                &request.tool_name,
                Some(origin),
                &request.args,
                adk_core::ToolEffect::NonIdempotent,
                runtime.policy().isolation_status().isolation(),
                false,
            );
            let protected = kind == ToolKind::Edit
                && request.args.get("path").and_then(Value::as_str).is_none_or(|path| {
                    crate::approvals::matcher::is_protected_target(runtime.workspace().root(), path)
                });
            if !kind.rule_eligible() || TableRow::of(&kind, protected).is_non_waivable() {
                return Err(store_error("no standing rule applies to this call; decide it once"));
            }
            if !suggestion.origin.admits(origin)
                || !matches!(suggestion.origin, crate::approvals::OriginPin::Exact { .. })
                || suggestion.tool != request.tool_name
            {
                return Err(store_error(
                    "a remembered rule must pin this call's tool and declaration",
                ));
            }
            if rule_decision == StandingDecision::Allow
                && !StandingRule::suggestions(&request).contains(&suggestion)
            {
                return Err(store_error(
                    "only a suggested rule can be remembered as an allow rule",
                ));
            }
            let owner = RuleOwner {
                app_name: self.app_name.clone(),
                user_id: user.to_string(),
                subject_kind: SubjectKind::Human,
                session_id: session.to_string(),
                created_by: SubjectRef::new(SubjectKind::Human, user.to_string()),
            };
            let rule = suggestion.into_rule(&owner, rule_decision, Some(fingerprint.to_string()));
            if let Some(audit) = self.audit(user, session) {
                audit.record("harness.rule.created", rule_audit_fields(&rule, &request)).await?;
            }
            runtime.standing_rules().grant(rule).await?;
        }
        runtime.approval_store().decide(&scope, fingerprint, approval).await?;
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

    /// Records the workspace trust decision for the harness's workspace (spec 3.1 §7.7).
    ///
    /// # Errors
    ///
    /// Returns the trust store's or the audit log's error.
    pub async fn trust_workspace(&self, user: &UserId, decision: TrustDecision) -> Result<()> {
        let runtime = self.agent.runtime();
        let digest = runtime.instructions().root_digest().to_string();
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
                    "decision": decision,
                    "by": user.to_string(),
                }),
            )
            .await?;
        }
        runtime.trust_store().set(WorkspaceTrust::new(digest, decision, user.to_string()))
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
            "default",
            self.app_name.clone(),
            user.to_string(),
            GOAL_TARGET,
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
    /// runner, whose harness continues a session's running goal on every turn.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let (target, runner) = harness.goal_target();
    /// assert_eq!(target, "harness.goal");
    /// ```
    pub fn goal_target(&self) -> (String, Arc<Runner>) {
        (GOAL_TARGET.to_string(), self.runner.clone())
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

    /// Closes the runner and the agent's toolsets.
    pub async fn close(&self) {
        self.runner.close().await;
    }
}
