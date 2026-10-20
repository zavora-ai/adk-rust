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
use crate::goal::{GoalSpec, GoalState};
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
        Ok(Self { runner: Arc::new(runner), agent, sessions, app_name })
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
        Ok(Self { runner: Arc::new(runner), agent, sessions, app_name })
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

    /// Sends a user message and returns the turn's events.
    ///
    /// # Errors
    ///
    /// Returns an error when the run cannot start.
    pub async fn send(
        &self,
        user: &UserId,
        session: &SessionId,
        text: impl Into<String>,
    ) -> Result<EventStream> {
        self.runner
            .run(user.clone(), session.clone(), Content::new("user").with_text(text.into()))
            .await
    }

    /// Runs an empty turn, which resumes held calls that now have a decision.
    ///
    /// # Errors
    ///
    /// Returns an error when the run cannot start.
    pub async fn resume(&self, user: &UserId, session: &SessionId) -> Result<EventStream> {
        self.runner.run(user.clone(), session.clone(), Content::new("user")).await
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
    /// "cancelled by mode change" response, so no call a person has not seen under the
    /// looser mode runs on the next resume (spec 3.1 §9.2 I14).
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
                self.sessions.append_event(session.as_ref(), event).await?;
            }
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
    /// Returns `harness.decision.invalid` when a rule cannot be remembered for the call,
    /// or the store's error.
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
        let request = pending
            .iter()
            .find(|entry| entry.fingerprint == fingerprint)
            .map(|entry| entry.request.clone());
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
            let request =
                request.ok_or_else(|| store_error("no pending call has this fingerprint"))?;
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
        runtime.approval_store().decide(&scope, fingerprint, approval).await
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

    /// Starts a durable goal (spec 3.1 §7.11; not yet implemented).
    ///
    /// # Errors
    ///
    /// Always returns `not yet implemented: spec 3.1 §7.11`.
    pub async fn start_goal(
        &self,
        _user: &UserId,
        _session: &SessionId,
        _spec: GoalSpec,
    ) -> Result<EventStream> {
        Err(AdkError::config("not yet implemented: spec 3.1 §7.11 (goal engine)"))
    }

    /// The session's goal; `None` until the goal engine of spec 3.1 §7.11 lands.
    pub async fn goal(&self, _user: &UserId, _session: &SessionId) -> Result<Option<GoalState>> {
        Ok(None)
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
