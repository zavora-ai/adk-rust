//! The in-process source (spec 3.3 §6): a kernel, a harness or a runner in the same process
//! as the surfaces, with the session store and the approval service they write.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{
    ApprovalDecision, ApprovalRequest, ApprovalService, ApprovalState, Content, ErrorCategory,
    Event, GovernanceControl, InboxQuery, NewRun, RequestContext, RunCheckpointRef, RunState,
    SessionId, UserId,
};
use adk_harness::{
    ArgSpec, Harness, HarnessMode, RuleOwner, RulePredicate, RuleSuggestion, StandingDecision,
    StandingRule, SuggestedScope,
};
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
    agents: Option<adk_kernel::agents::AgentsHandle>,
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
    agents: Option<adk_kernel::agents::AgentsHandle>,
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

    /// The daily spending limit in micro-USD, over the one in force in the kernel's area of
    /// this source's turns.
    #[must_use]
    pub fn daily_limit(mut self, micro_usd: u64) -> Self {
        self.daily_limit = Some(micro_usd);
        self
    }

    /// The agent table of a kernel this source does not run turns on — a kernel built after
    /// the source, such as the one `adk-rust up` serves the terminal's commands for — so
    /// "Always allow" on an added agent's card makes a rule for that agent alone (spec 3.3
    /// S-I19). A [`kernel`](Self::kernel) brings its own.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::agents::AgentsHandle;
    /// use adk_session::InMemorySessionService;
    ///
    /// let source = adk_surface::LocalSource::builder("adk-rust")
    ///     .sessions(std::sync::Arc::new(InMemorySessionService::new()))
    ///     .agents(AgentsHandle::new())
    ///     .build()
    ///     .unwrap();
    /// # let _ = source;
    /// ```
    #[must_use]
    pub fn agents(mut self, agents: adk_kernel::agents::AgentsHandle) -> Self {
        self.agents = Some(agents);
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
            agents: self.agents,
        })
    }
}

impl LocalSource {
    /// A builder for the sessions of `app_name`.
    pub fn builder(app_name: impl Into<String>) -> LocalSourceBuilder {
        LocalSourceBuilder { app: app_name.into(), ..LocalSourceBuilder::default() }
    }

    /// The kernel's agent table: the kernel's own, else the handle's while its kernel lives.
    fn agent_table(&self) -> Option<adk_kernel::agents::Agents> {
        match &self.kernel {
            Some((kernel, _)) => Some(kernel.agents().clone()),
            None => self.agents.as_ref().and_then(adk_kernel::agents::AgentsHandle::get),
        }
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

    /// The classes the kernel's mounted drivers declare `tool` of; a name two drivers share
    /// has both (spec 3.2 K-I9).
    fn driver_classes(&self, tool: &str) -> Vec<adk_kernel::drivers::EffectClass> {
        let Some(drivers) = self.kernel.as_ref().and_then(|(kernel, _)| kernel.drivers()) else {
            return Vec::new();
        };
        drivers
            .mounted()
            .iter()
            .filter_map(|id| drivers.record(id))
            .map(|record| record.manifest.class_of(tool))
            .collect()
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

    /// Starts a turn of the session through the harness or the runner and runs it in the
    /// background; `text` empty resumes the session's decided calls. With `caller`, the turn
    /// carries the caller's identity, so the harness applies the session's mode and standing
    /// rules as it does to a request a server admitted (spec 3.3 §6).
    ///
    /// Returns once the turn is registered as running, so [`busy`](Source::busy) sees it and
    /// input queued behind it waits for it (S-I4).
    async fn run_turn(
        &self,
        user: &str,
        session: &str,
        text: &str,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let (user_id, session_id) = Self::ids(user, session)?;
        let text = text.to_string();
        let context =
            caller.filter(|caller| caller.user_id() == user).map(|caller| RequestContext {
                user_id: caller.user_id().to_string(),
                scopes: caller.groups.clone(),
                metadata: Default::default(),
            });
        let stream = if let Some(harness) = &self.harness {
            match (text.is_empty(), context) {
                (true, None) => harness.resume(&user_id, &session_id).await,
                (true, Some(context)) => harness.resume_as(&user_id, &session_id, context).await,
                (false, None) => harness.send(&user_id, &session_id, text).await,
                (false, Some(context)) => {
                    harness.send_as(&user_id, &session_id, text, context).await
                }
            }
        } else if let Some(runner) = &self.runner {
            let content = if text.is_empty() {
                Content::new("user")
            } else {
                Content::new("user").with_text(text)
            };
            let config = context.map(|context| adk_core::RunConfig {
                request_context: Some(context),
                ..runner.run_config().clone()
            });
            runner.run_with_config(user_id, session_id.clone(), content, config).await
        } else {
            return Err(SurfaceError::unsupported(
                "sending a message without a kernel, harness or runner",
            ));
        };
        let stream = stream?;
        let session = session_id.to_string();
        tokio::spawn(async move { drain(stream, &session).await });
        Ok(())
    }

    /// The harness's pending call for `request`, with the standing rule "Always allow" makes
    /// and the words that say what it covers.
    async fn always_allow_rule(
        &self,
        caller: Option<&Caller>,
        request: &ApprovalRequest,
    ) -> Option<(String, StandingRule, adk_core::ToolConfirmationRequest)> {
        let harness = self.harness.as_ref()?;
        let runtime = harness.agent().runtime();
        let pending = runtime.approval_store().pending(&request.scope).await.ok()?;
        let held = pending.into_iter().find(|entry| entry.fingerprint == request.fingerprint)?;
        // A call a person asked for through a drawn form is never made a standing rule
        // (spec 3.3 §7.4 rule 5, wave 4 decision 12).
        if held
            .request
            .function_call_id
            .as_deref()
            .is_some_and(adk_core::RequestedCall::is_requested_id)
        {
            return None;
        }
        // A file write: every file write in its folder (spec 3.3 §8.2). A file change no
        // folder rule covers — a delete or a move — is asked about every time.
        let suggestion = match harness.agent().folder_suggestion(&held.request) {
            Some(folder) => folder,
            None => StandingRule::suggestions(&held.request).into_iter().find(|suggestion| {
                !suggestion
                    .predicates
                    .iter()
                    .any(|predicate| matches!(predicate, RulePredicate::PathWithin { .. }))
            })?,
        };
        let label = always_allow_label(&suggestion);
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
        let mut rule = harness
            .agent()
            .rule_for_held_call(&held.request, suggestion, StandingDecision::Allow, &owner)
            .ok()?;
        // "Always allow" on an added agent's card makes a rule for that agent alone (spec 3.3
        // S-I19). When the agent table cannot be read, no rule is made: one for every agent
        // would let the others do what the person allowed this one.
        if let (Some(table), Some(acting)) = (self.agent_table(), &request.acting) {
            match table.get(&crate::project::agent_name(request)).await {
                Ok(Some(_)) => rule.agent = Some(acting.agent.id.as_str().to_string()),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(approval.id = %request.id, error = %error, "could not tell whose rule \"Always allow\" makes; none is offered");
                    return None;
                }
            }
        }
        Some((label, rule, held.request))
    }
}

impl LocalSource {
    /// The kernel target of `agent`: the source's own for the primary agent, else the added
    /// agent's id.
    fn target_of(&self, agent: &str) -> Option<String> {
        let (_, own) = self.kernel.as_ref()?;
        Some(if agent == adk_harness::PRIMARY_AGENT_STAMP || agent == own || agent == self.app {
            own.clone()
        } else {
            agent.to_string()
        })
    }

    /// The target a message to `session` goes to (spec 3.3 §5.7 "Threads"): the agent its
    /// stamp names, before the first run the agent of its latest run, else the primary agent.
    async fn target_for(&self, user: &str, session: &str) -> Result<String> {
        let own = self.kernel.as_ref().map(|(_, target)| target.clone()).unwrap_or_default();
        Ok(match self.agent_of_session(user, session).await? {
            Some(agent) => self.target_of(&agent).unwrap_or(own),
            None => own,
        })
    }
}

/// The primary agent's name on screen, as the suggesting agent of an added one.
const PRIMARY_NAME: &str = "ADK-Rust";

/// The host's offset from UTC now: dates on screen are the host's.
fn local_offset() -> chrono::FixedOffset {
    *chrono::Local::now().offset()
}

impl LocalSource {
    /// Applies `action` with the digest its preview showed, through the kernel's agent table.
    async fn apply_agent(
        &self,
        caller: &Caller,
        action: adk_kernel::agents::AgentAction,
        digest: &str,
    ) -> Result<crate::agents::AgentDetail> {
        let (kernel, _) =
            self.kernel.as_ref().ok_or_else(|| SurfaceError::unsupported("agents of your own"))?;
        match crate::agents::local::apply(
            kernel.agents(),
            caller,
            action,
            digest,
            PRIMARY_NAME,
            local_offset(),
        )
        .await?
        {
            crate::agents::local::Applied::Agent(detail) => Ok(*detail),
            crate::agents::local::Applied::Removed(_) => {
                Err(SurfaceError::Invalid("the agent was removed".into()))
            }
        }
    }
}

/// "Always allow …" for the rule `suggestion` makes, saying all it covers: the command
/// prefix, the file writes in the folder or the whole workspace, the site, or the tool with
/// any arguments, in every session or this one, and that commands running project scripts
/// allow whatever they do (spec 3.3 §8.2, spec 3.1 §7.5).
fn always_allow_label(suggestion: &RuleSuggestion) -> String {
    let (label, covers) = match suggestion.predicates.first() {
        Some(RulePredicate::ShellPrefix { tokens, .. }) => {
            (words::ALWAYS_ALLOW_FOR_THIS_COMMAND, format!("{} …", tokens.join(" ")))
        }
        Some(RulePredicate::WritesWithin { dir }) if dir == "." => (
            words::ALWAYS_ALLOW_FOR_THIS_FOLDER,
            "file writes anywhere in the workspace".to_string(),
        ),
        Some(RulePredicate::WritesWithin { dir }) => {
            (words::ALWAYS_ALLOW_FOR_THIS_FOLDER, format!("file writes anywhere in {dir}/"))
        }
        Some(RulePredicate::PathWithin { dir, .. }) if dir == "." => {
            (words::ALWAYS_ALLOW_FOR_THIS_FOLDER, "anywhere in the workspace".to_string())
        }
        Some(RulePredicate::PathWithin { dir, .. }) => {
            (words::ALWAYS_ALLOW_FOR_THIS_FOLDER, format!("anywhere in {dir}/"))
        }
        Some(RulePredicate::Arg(ArgSpec::DomainIn { domains, .. })) => {
            (words::ALWAYS_ALLOW_FOR_THIS_SITE, format!("any page on {}", domains.join(", ")))
        }
        Some(RulePredicate::Arg(ArgSpec::Equals { value, .. })) => (
            words::ALWAYS_ALLOW_FOR_THIS_SITE,
            format!("{} on {}", suggestion.tool, value.as_str().unwrap_or_default()),
        ),
        // Any other condition on the arguments, and none: the tool, with any arguments.
        Some(RulePredicate::Arg(_)) | None => {
            (words::ALWAYS_ALLOW_FOR_THIS_PERSON, format!("{} with any arguments", suggestion.tool))
        }
    };
    words::always_allow_covering(
        label,
        &covers,
        suggestion.scope == SuggestedScope::User,
        suggestion.runs_scripts,
    )
}

/// Runs a turn's stream to its end, so its events reach the session store.
async fn drain(mut stream: adk_core::EventStream, session: &str) {
    while let Some(event) = stream.next().await {
        if let Err(error) = event {
            tracing::warn!(session.id = %session, error = %error, "a turn a surface started failed");
            break;
        }
    }
}

/// A standing rule granted once the decision it was made for approved the call, through the
/// harness, so `harness.rule.created` is recorded before the rule is stored (spec 3.1 §7.5,
/// §9.3).
struct GrantRule {
    harness: Arc<Harness>,
    rule: StandingRule,
    /// The held call the rule was made from.
    from: adk_core::ToolConfirmationRequest,
    /// The surface it was made on, such as `web`.
    surface: String,
}

#[async_trait]
impl RuleCommit for GrantRule {
    async fn commit(self: Box<Self>) -> Result<()> {
        self.harness.agent().grant_rule(self.rule, &self.surface, Some(&self.from)).await?;
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
        self.resume_decided(request, None).await
    }

    async fn resolved_by(&self, caller: &Caller, request: &ApprovalRequest) -> Result<()> {
        // A suggested agent is added as the surface that approved it, so its record names
        // where (spec 3.3 §11).
        if request.tool_name == adk_kernel::agents::AGENT_CREATE_TOOL_NAME {
            if let Some(table) = self.agent_table() {
                table.answered(&request.id, &crate::agents::agent_caller(caller)).await;
            }
            return Ok(());
        }
        self.resume_decided(request, Some(caller)).await
    }

    async fn always_allow_offer(&self, request: &ApprovalRequest) -> Option<AlwaysAllowOffer> {
        self.always_allow_rule(None, request).await.map(|(label, ..)| AlwaysAllowOffer { label })
    }

    async fn prepare_always_allow(
        &self,
        caller: &Caller,
        request: &ApprovalRequest,
    ) -> Result<Box<dyn RuleCommit>> {
        let harness =
            self.harness.clone().ok_or_else(|| SurfaceError::unsupported("always allow"))?;
        let (_, rule, from) =
            self.always_allow_rule(Some(caller), request).await.ok_or_else(|| {
                SurfaceError::Invalid("no standing rule applies to this call; allow it once".into())
            })?;
        let surface = serde_json::to_value(caller.surface)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "surface".to_string());
        Ok(Box::new(GrantRule { harness, rule, from, surface }))
    }

    async fn agents(&self, user: &str) -> Result<Vec<AgentState>> {
        if let Some((kernel, _)) = &self.kernel {
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
            // A removed agent leaves the roster; its past runs stay in its threads. One
            // listing carries every agent's state (spec 3.3 §5.7).
            let gone: Vec<String> = kernel
                .agents()
                .list()
                .await?
                .into_iter()
                .filter(|view| {
                    matches!(
                        view.agent.state,
                        adk_kernel::agents::AgentRecordState::Removing
                            | adk_kernel::agents::AgentRecordState::Removed
                    )
                })
                .map(|view| view.agent.id)
                .collect();
            latest.retain(|name, _| !gone.contains(name));
            return Ok(latest.into_values().collect());
        }
        let name = match (&self.harness, &self.runner) {
            (Some(harness), _) => harness.agent().info().name.clone(),
            (None, Some(_)) => self.app.clone(),
            (None, None) => return Ok(Vec::new()),
        };
        let active = self.active_sessions(user);
        let waiting = match &self.approvals {
            Some(approvals) => approvals
                .inbox(
                    InboxQuery::default()
                        .with_app_name(self.app.clone())
                        .with_user_id(user)
                        .with_states(vec![ApprovalState::Pending])
                        .with_limit(1),
                )
                .await?
                .items
                .into_iter()
                .next(),
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

    /// The kernel target new messages run on, else the harness's agent (spec 3.3 §5.5).
    fn primary_agent(&self) -> Option<String> {
        match (&self.kernel, &self.harness) {
            (Some((_, target)), _) => Some(target.clone()),
            (None, Some(harness)) => Some(harness.agent().info().name.clone()),
            (None, None) => None,
        }
    }

    /// A tool the kernel's mounted drivers declare `publish_public` or `financial`, the
    /// classes nothing may allow without a person (spec 3.2 K-I9), as the kernel's own decide
    /// route and chat rate it.
    fn rates_high(&self, request: &ApprovalRequest) -> bool {
        self.driver_classes(&request.tool_name)
            .into_iter()
            .any(adk_kernel::drivers::EffectClass::is_never_auto_allowed)
    }

    /// A tool the kernel's mounted drivers declare `financial` (spec 3.2 K-I9).
    fn moves_money(&self, request: &ApprovalRequest) -> bool {
        self.driver_classes(&request.tool_name)
            .contains(&adk_kernel::drivers::EffectClass::Financial)
    }

    async fn busy(&self, user: &str, session: &str) -> Result<bool> {
        if self.kernel.is_some() {
            return Ok(self.processes(user).await?.iter().any(|process| {
                process.run.session_id.as_deref() == Some(session)
                    && matches!(process.run.state, RunState::Running | RunState::Queued)
            }));
        }
        Ok(self.active_sessions(user).iter().any(|active| active == session))
    }

    async fn waiting_on_decision(&self, user: &str, session: &str) -> Result<bool> {
        if self.kernel.is_some() {
            return Ok(self.processes(user).await?.iter().any(|process| {
                process.run.session_id.as_deref() == Some(session)
                    && process.run.state == RunState::Waiting
                    && matches!(
                        process.run.checkpoint,
                        Some(RunCheckpointRef::AwaitingApproval { .. })
                    )
            }));
        }
        let Some(approvals) = &self.approvals else {
            return Ok(false);
        };
        // Every open request of the caller's session, whoever may answer it; only whether
        // one exists leaves this source.
        let page = approvals
            .inbox(
                InboxQuery::default()
                    .with_app_name(self.app.clone())
                    .with_user_id(user)
                    .with_session_id(session)
                    .with_states(vec![ApprovalState::Pending])
                    .with_limit(1),
            )
            .await?;
        Ok(!page.items.is_empty())
    }

    async fn start_turn(&self, caller: &Caller, session: &str, text: &str) -> Result<()> {
        let user = caller.user_id();
        self.ensure_session(user, session).await?;
        if let Some((kernel, _)) = &self.kernel {
            // A thread belongs to one agent: its stamp, then its latest run (spec 3.3 §5.7).
            let target = self.target_for(user, session).await?;
            let run = NewRun::new(
                QueueClass::Interactive.as_str(),
                self.app.clone(),
                user,
                target,
                serde_json::json!({ "text": text }),
            )
            .with_session_id(session)
            .with_authenticated_user();
            kernel.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive)).await?;
            return Ok(());
        }
        self.run_turn(user, session, text, Some(caller)).await
    }

    async fn request_call(
        &self,
        caller: &Caller,
        session: &str,
        text: &str,
        call: adk_core::RequestedCall,
    ) -> Result<()> {
        let user = caller.user_id();
        self.ensure_session(user, session).await?;
        if let Some((kernel, _)) = &self.kernel {
            // Held as the drawing agent inside its own target — the target of the session's
            // latest run, an area's or an added agent's — parked durably and woken by the
            // decision on any surface (spec 3.3 §7.4 rule 5, wave 4 decision 13).
            let latest = self
                .processes(user)
                .await?
                .into_iter()
                .find(|process| process.run.session_id.as_deref() == Some(session))
                .map(|process| process.run.target);
            let target = match latest {
                Some(target) => target,
                None => self.target_for(user, session).await?,
            };
            let run = NewRun::new(
                QueueClass::Interactive.as_str(),
                self.app.clone(),
                user,
                target,
                serde_json::json!({ "text": text, "requestedCall": call }),
            )
            .with_session_id(session)
            .with_authenticated_user();
            kernel.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive)).await?;
            return Ok(());
        }
        let Some(harness) = &self.harness else {
            return Err(SurfaceError::unsupported(
                "sending a form an agent drew without a kernel or harness",
            ));
        };
        let (user_id, session_id) = Self::ids(user, session)?;
        let context = RequestContext {
            user_id: user.to_string(),
            scopes: caller.groups.clone(),
            metadata: Default::default(),
        };
        let stream = harness.send_requested(&user_id, &session_id, text, call, context).await?;
        let session = session.to_string();
        tokio::spawn(async move { drain(stream, &session).await });
        Ok(())
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

    // The area this source's turns run in, the primary agent's: with several areas, its spend
    // today against its own limit in force (spec 3.3 §10 K3, §13 Q7).
    async fn spend(&self, user: &str) -> Result<Option<SpendToday>> {
        let Some((kernel, target)) = &self.kernel else {
            return Ok(self.daily_limit.map(|limit| SpendToday { spent: 0, limit: Some(limit) }));
        };
        let area = kernel.partition_of_target(target).cloned().unwrap_or_default();
        let spent = self
            .processes(user)
            .await?
            .iter()
            .filter(|process| process.meta.partition == area)
            .map(|process| process.view.spend_today)
            .sum();
        let limit = match self.daily_limit {
            Some(limit) => Some(limit),
            None => kernel.area_settings(&area).await?.daily_spending_limit,
        };
        Ok(Some(SpendToday { spent, limit }))
    }

    fn quiet_hours(&self) -> Option<QuietHours> {
        self.kernel.as_ref().and_then(|(kernel, _)| kernel.config().quiet_hours)
    }

    fn keeps_ledger(&self) -> bool {
        self.kernel.as_ref().is_some_and(|(kernel, _)| kernel.ledger().is_some())
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

    fn keeps_agents(&self) -> bool {
        self.kernel.is_some()
    }

    async fn agent_definitions(&self, user: &str) -> Result<Vec<crate::agents::AgentDetail>> {
        let Some((kernel, _)) = &self.kernel else {
            return Ok(Vec::new());
        };
        crate::agents::local::definitions(kernel.agents(), user, PRIMARY_NAME, local_offset()).await
    }

    async fn agent(&self, user: &str, id: &str) -> Result<Option<crate::agents::AgentDetail>> {
        let Some((kernel, _)) = &self.kernel else {
            return Ok(None);
        };
        match crate::agents::local::agent(kernel.agents(), id, user, PRIMARY_NAME, local_offset())
            .await
        {
            Ok(detail) => Ok(Some(detail)),
            Err(SurfaceError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn agent_choices(&self) -> Result<crate::agents::AgentChoices> {
        let (kernel, _) =
            self.kernel.as_ref().ok_or_else(|| SurfaceError::unsupported("agents of your own"))?;
        crate::agents::local::choices(kernel.agents(), kernel.partitions()).await
    }

    async fn preview_agent(
        &self,
        caller: &Caller,
        request: crate::agents::AgentPreviewRequest,
    ) -> Result<crate::agents::AgentPreview> {
        let (kernel, _) =
            self.kernel.as_ref().ok_or_else(|| SurfaceError::unsupported("agents of your own"))?;
        crate::agents::local::preview(kernel.agents(), caller, request).await
    }

    async fn add_agent(
        &self,
        caller: &Caller,
        draft: crate::agents::AgentDraft,
        digest: &str,
        replaces: Option<String>,
    ) -> Result<crate::agents::AgentDetail> {
        let action = adk_kernel::agents::AgentAction::Create {
            definition: draft.into_definition(),
            replaces,
        };
        self.apply_agent(caller, action, digest).await
    }

    async fn change_agent(
        &self,
        caller: &Caller,
        agent: &str,
        version: u64,
        draft: crate::agents::AgentDraft,
        digest: &str,
    ) -> Result<crate::agents::AgentDetail> {
        let action = adk_kernel::agents::AgentAction::Change {
            id: agent.to_string(),
            expected_version: version,
            definition: draft.into_definition(),
        };
        self.apply_agent(caller, action, digest).await
    }

    async fn pause_agent(
        &self,
        caller: &Caller,
        agent: &str,
    ) -> Result<crate::agents::AgentDetail> {
        let action = adk_kernel::agents::AgentAction::Pause { id: agent.to_string() };
        let digest = action.digest();
        self.apply_agent(caller, action, &digest).await
    }

    async fn resume_agent(
        &self,
        caller: &Caller,
        agent: &str,
    ) -> Result<crate::agents::AgentDetail> {
        let action = adk_kernel::agents::AgentAction::Resume { id: agent.to_string() };
        let digest = action.digest();
        self.apply_agent(caller, action, &digest).await
    }

    async fn remove_agent(
        &self,
        caller: &Caller,
        agent: &str,
        version: u64,
        digest: &str,
    ) -> Result<crate::agents::EndedWork> {
        let (kernel, _) =
            self.kernel.as_ref().ok_or_else(|| SurfaceError::unsupported("removing an agent"))?;
        let action = adk_kernel::agents::AgentAction::Remove {
            id: agent.to_string(),
            expected_version: version,
        };
        match crate::agents::local::apply(
            kernel.agents(),
            caller,
            action,
            digest,
            PRIMARY_NAME,
            local_offset(),
        )
        .await?
        {
            crate::agents::local::Applied::Removed(ended) => Ok(ended),
            crate::agents::local::Applied::Agent(_) => Ok(crate::agents::EndedWork::default()),
        }
    }

    async fn copy_rule_to_agent(&self, caller: &Caller, agent: &str, rule: &str) -> Result<String> {
        let harness =
            self.harness.as_ref().ok_or_else(|| SurfaceError::unsupported("copying a rule"))?;
        let (kernel, _) =
            self.kernel.as_ref().ok_or_else(|| SurfaceError::unsupported("copying a rule"))?;
        // Only an agent the person added and still has: never a suggestion, whose id may be
        // given again, nor a removed one.
        let added =
            kernel.agents().get(agent).await?.is_some_and(|view| view.agent.state.is_hosted());
        if !added {
            return Err(SurfaceError::not_found(format!("agent '{agent}'")));
        }
        let store = harness.agent().runtime().standing_rules().clone();
        let original = store
            .list(&self.app, caller.user_id())
            .await?
            .into_iter()
            .find(|candidate| candidate.id == rule)
            .ok_or_else(|| SurfaceError::not_found(format!("rule '{rule}'")))?;
        let pinned = adk_core::AgentPrincipal::derived(&self.app, agent).id.as_str().to_string();
        let copy = adk_harness::copy_rule_to(&original, &pinned, caller.subject.clone())?;
        let label = copy.label.clone();
        // Through the harness, which records `harness.rule.created` before the rule is
        // stored (spec 3.1 I39, spec 3.3 §11).
        let surface = serde_json::to_value(caller.surface)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "surface".to_string());
        harness.agent().grant_rule(copy, &surface, None).await?;
        Ok(label)
    }

    async fn agent_threads(&self, user: &str, agent: &str) -> Result<Vec<(String, DateTime<Utc>)>> {
        let mut threads: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        for process in self.processes(user).await? {
            if process.run.target != agent {
                continue;
            }
            if let Some(session) = process.run.session_id.clone() {
                let at = threads.entry(session).or_insert(process.run.updated_at);
                *at = (*at).max(process.run.updated_at);
            }
        }
        Ok(threads.into_iter().collect())
    }

    async fn agent_of_session(&self, user: &str, session: &str) -> Result<Option<String>> {
        if self.kernel.is_none() {
            return Ok(None);
        }
        let stamped = self
            .sessions
            .get(GetRequest {
                app_name: self.app.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: Some(1),
                after: None,
            })
            .await
            .ok()
            .and_then(|found| found.state().get(adk_harness::AGENT_STATE_KEY))
            .and_then(|stamp| stamp.as_str().map(str::to_string));
        if stamped.is_some() {
            return Ok(stamped);
        }
        Ok(self
            .processes(user)
            .await?
            .into_iter()
            .find(|process| process.run.session_id.as_deref() == Some(session))
            .map(|process| process.run.target))
    }

    async fn start_turn_with(
        &self,
        caller: &Caller,
        agent: &str,
        session: &str,
        text: &str,
    ) -> Result<()> {
        let Some(target) = self.target_of(agent) else {
            return self.start_turn(caller, session, text).await;
        };
        let (kernel, _) =
            self.kernel.as_ref().ok_or_else(|| SurfaceError::unsupported("sending a message"))?;
        let user = caller.user_id();
        self.ensure_session(user, session).await?;
        let run = NewRun::new(
            QueueClass::Interactive.as_str(),
            self.app.clone(),
            user,
            target,
            serde_json::json!({ "text": text }),
        )
        .with_session_id(session)
        .with_authenticated_user();
        kernel.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive)).await?;
        Ok(())
    }
}

impl LocalSource {
    /// Resumes the session of a decided `request` (spec 3.3 §7.3): a durable run parked on the
    /// call is woken by the approval service; a call held in a plain run resumes with an
    /// empty turn, as `Harness::resume` does, as the person who answered when it is theirs.
    async fn resume_decided(
        &self,
        request: &ApprovalRequest,
        caller: Option<&Caller>,
    ) -> Result<()> {
        if request.run_id.is_some() && self.kernel.is_some() {
            return Ok(());
        }
        if self.harness.is_none() && self.runner.is_none() {
            return Ok(());
        }
        self.run_turn(&request.scope.user_id, &request.scope.session_id, "", caller).await
    }

    /// The sessions of `user` in this application with a run in flight, oldest first.
    fn active_sessions(&self, user: &str) -> Vec<String> {
        let runs = match (&self.harness, &self.runner) {
            (Some(harness), _) => harness.runner().active_runs(),
            (None, Some(runner)) => runner.active_runs(),
            (None, None) => Vec::new(),
        };
        let mut sessions: Vec<String> = runs
            .into_iter()
            .filter(|run| run.user_id.as_ref() == user && run.app_name.as_ref() == self.app)
            .map(|run| run.session_id.as_ref().to_string())
            .collect();
        sessions.sort();
        sessions.dedup();
        sessions
    }
}
