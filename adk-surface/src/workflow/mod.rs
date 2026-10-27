//! The workflow observer and debugger (spec 3.3 §6.1 `workflow`, §12 developer track): a
//! graph agent's runs, steps and forks, for the web home's Runs tab.
//!
//! [`Workflows`] reads what adk-graph records in every checkpoint — the run, each step's nodes
//! with their status, duration and writes, a fork's lineage — and the effect log of governed
//! nodes. Reading changes nothing: rewinding to a step loads one checkpoint by id (S-I1).
//!
//! | What | Where |
//! |---|---|
//! | A graph's nodes, edges and channels | [`Workflows::graphs`] |
//! | A session's latest run: status, node states, steps, lineage, forks | [`Workflows::run`] |
//! | Every run of a session, from records only | [`Workflows::runs`] |
//! | One step: its nodes, "What changed", what a fork from it would do | [`Workflows::step`] |
//! | A fork from a completed step, with edits, into a new session of the caller | [`Workflows::fork`] |
//! | Card hints for a governed node's decision | [`CardHints`] for [`Workflows`] |
//! | High risk for a node that spends money | [`Workflows::approval_router`] |
//!
//! A decision a graph waits on is the inbox entry, drawn as every other card and answered with
//! the engine's `decide` command (S-I1, S-I2): the observer only links to it. Its card says
//! where it came from — "From the invoice-run workflow · Open in Runs" — never offers
//! **Always allow**, and marks money for a node that spends it.
//!
//! Every value shown passes secret redaction, the host's hidden keys
//! ([`Workflows::hide_keys`]) and a size cap; values are never logged. Developer panels may
//! use technical words (spec 3.3 §8); the cards stay plain.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use adk_core::{ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, StoreApprovalService};
//! use adk_surface::workflow::Workflows;
//!
//! # fn example(graph: Arc<adk_graph::agent::GraphAgent>, runner: Arc<adk_runner::Runner>) {
//! let workflows = Workflows::new("adk-rust").graph(graph).hide_keys(["apiKey"]);
//! // Calls of a node that spends money are high risk on every surface.
//! let approvals = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
//!     workflows.approval_router(ApprovalPolicy::new([ApproverSelector::group("owner")])),
//! );
//! let workflows = workflows.runner(runner);
//! # let _ = (approvals, workflows);
//! # }
//! ```

mod project;
mod values;
mod view;
pub mod words;

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{
    AdkError, Agent, ApprovalCall, ApprovalRequest, ApprovalRisk, ApprovalRoute, ApprovalRouter,
    ApprovalState, Content, ErrorCategory, ErrorComponent, RequestContext, RunConfig, SessionId,
    UserId,
};
use adk_graph::agent::{GraphAgent, session_thread_id};
use adk_graph::checkpoint::Checkpointer;
use adk_graph::node::{ExecutionConfig, NodeContext};
use adk_graph::record::{NodeRecord, NodeStatus};
use adk_graph::shape::{GraphShape, NodeShape};
use adk_graph::state::Checkpoint;
use adk_graph::time_travel::{ForkError, ForkSpec};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, ListRequest, SessionService};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, Utc};
use futures::StreamExt;
use serde_json::json;

use crate::engine::SurfaceEngine;
use crate::error::{Result, SurfaceError};
use crate::hints::{CardHint, CardHints};
use crate::model::{CardDetail, Reversibility};
use crate::origin::{Caller, SurfaceKind};
use project::{Context, Found, Saved, StepInputs};
use values::Display;

pub use view::{
    Change, ChannelValue, EdgeView, EffectView, ForkOutcome, ForkPoint, ForkRequest, ForkSummary,
    GraphView, GraphsResponse, NodeDecision, NodeDetail, NodeRun, NodeState, NodeView, RunLineage,
    RunStatus, RunStep, RunSummary, RunsResponse, StateChange, StepDetail, StepState, WorkflowRun,
};

/// The audit action of a fork made on the web home (spec 3.3 §11, §12).
pub const SURFACE_AUDIT_WORKFLOW_FORK: &str = "surface.workflow.fork";

/// The largest value one fork edit may set, in bytes of compact JSON.
pub const FORK_VALUE_LIMIT: usize = 64 * 1024;

/// The most sessions of a person read to find a run's forks.
const FORK_SCAN_LIMIT: usize = 500;

/// The workflow observer over a host's graph agents (spec 3.3 §12). Cheap to clone.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::workflow::Workflows;
///
/// # fn example(graph: Arc<adk_graph::agent::GraphAgent>, runner: Arc<adk_runner::Runner>) {
/// let workflows = Workflows::new("adk-rust").graph(graph).runner(runner);
/// assert_eq!(workflows.graphs().len(), 1);
/// # }
/// ```
#[derive(Clone)]
pub struct Workflows {
    inner: Arc<Inner>,
}

#[derive(Clone)]
struct Inner {
    app: String,
    graphs: Vec<Arc<GraphAgent>>,
    runner: Option<Arc<Runner>>,
    sessions: Option<Arc<dyn SessionService>>,
    display: Display,
    hidden: Vec<String>,
    offset: FixedOffset,
}

impl std::fmt::Debug for Workflows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workflows")
            .field("app", &self.inner.app)
            .field(
                "graphs",
                &self.inner.graphs.iter().map(|graph| graph.name().to_string()).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Workflows {
    /// An observer for the graph agents a runner of application `app` runs (spec 3.3 §12).
    pub fn new(app: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                app: app.into(),
                graphs: Vec::new(),
                runner: None,
                sessions: None,
                display: Display::default(),
                hidden: Vec::new(),
                offset: *chrono::Local::now().offset(),
            }),
        }
    }

    fn change(mut self, change: impl FnOnce(&mut Inner)) -> Self {
        change(Arc::make_mut(&mut self.inner));
        self
    }

    /// Observes `graph`, a graph agent with a checkpointer that the host's runner runs.
    #[must_use]
    pub fn graph(self, graph: Arc<GraphAgent>) -> Self {
        self.change(|inner| inner.graphs.push(graph))
    }

    /// Starts a fork's run through `runner`, the runner whose root agent is the graph.
    #[must_use]
    pub fn runner(self, runner: Arc<Runner>) -> Self {
        self.change(|inner| inner.runner = Some(runner))
    }

    /// Reads and creates sessions through `sessions`; the runner's when unset.
    #[must_use]
    pub fn sessions(self, sessions: Arc<dyn SessionService>) -> Self {
        self.change(|inner| inner.sessions = Some(sessions))
    }

    /// Shows the values of these state keys, at any depth, as `[hidden]`.
    #[must_use]
    pub fn hide_keys(self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.change(|inner| {
            inner.hidden.extend(keys.into_iter().map(Into::into));
            inner.display = Display::new(inner.hidden.iter().cloned());
        })
    }

    /// Shows times at `offset` east of UTC instead of the host's time zone.
    #[must_use]
    pub fn utc_offset(self, offset: FixedOffset) -> Self {
        self.change(|inner| inner.offset = offset)
    }

    /// The application whose sessions this observer reads.
    pub fn app_name(&self) -> &str {
        &self.inner.app
    }

    /// Every graph agent observed, with its nodes, edges and channels.
    pub fn graphs(&self) -> Vec<GraphView> {
        self.inner
            .graphs
            .iter()
            .map(|agent| {
                project::graph_view(
                    agent.name(),
                    agent.description(),
                    &agent.graph().shape(),
                    agent.graph().state_channels(),
                )
            })
            .collect()
    }

    fn session_service(&self) -> Result<Arc<dyn SessionService>> {
        if let Some(sessions) = &self.inner.sessions {
            return Ok(Arc::clone(sessions));
        }
        match &self.inner.runner {
            Some(runner) => Ok(Arc::clone(runner.session_service())),
            None => Err(SurfaceError::unsupported("the workflow observer without sessions")),
        }
    }

    /// Refuses a session that is not the caller's (S-I8): it answers as not found.
    async fn owned(&self, user: &str, session: &str) -> Result<()> {
        let got = self
            .session_service()?
            .get(GetRequest {
                app_name: self.inner.app.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: Some(0),
                after: None,
            })
            .await;
        match got {
            Ok(_) => Ok(()),
            Err(error) if error.category == ErrorCategory::NotFound => {
                Err(SurfaceError::not_found(format!("session '{session}'")))
            }
            Err(error) => Err(SurfaceError::Source(error)),
        }
    }

    fn thread(&self, user: &str, session: &str) -> String {
        session_thread_id(&self.inner.app, user, session)
    }

    /// The graph `name` names, else the one with checkpoints on `thread`, else the only one.
    async fn graph_for(&self, thread: &str, name: Option<&str>) -> Result<&Arc<GraphAgent>> {
        if let Some(name) = name {
            return self
                .inner
                .graphs
                .iter()
                .find(|agent| agent.name() == name)
                .ok_or_else(|| SurfaceError::not_found(format!("workflow '{name}'")));
        }
        for agent in &self.inner.graphs {
            if checkpointer(agent)?.load(thread).await.map_err(source)?.is_some() {
                return Ok(agent);
            }
        }
        self.inner.graphs.first().ok_or_else(|| SurfaceError::not_found("a workflow"))
    }

    /// The session's checkpoints, oldest first, with their records read.
    async fn saved(agent: &GraphAgent, thread: &str) -> Result<Vec<Saved>> {
        Ok(checkpointer(agent)?
            .list(thread)
            .await
            .map_err(source)?
            .into_iter()
            .map(Saved::new)
            .collect())
    }

    /// Every run of `session`, from the checkpoints' records only (spec 3.3 §12).
    ///
    /// # Errors
    ///
    /// Returns `surface.not_found` for a session that is not the caller's (S-I8), or for a
    /// workflow `graph` does not name; the checkpointer's error otherwise.
    pub async fn runs(
        &self,
        caller: &Caller,
        session: &str,
        graph: Option<&str>,
    ) -> Result<RunsResponse> {
        let user = caller.user_id();
        self.owned(user, session).await?;
        let thread = self.thread(user, session);
        let agent = self.graph_for(&thread, graph).await?;
        let saved = Self::saved(agent, &thread).await?;
        let shape = agent.graph().shape();
        let channels = agent.graph().state_channels();
        let context = self.context(session, &thread, agent.name(), &shape, &channels);
        let found = Found::default();
        Ok(RunsResponse {
            session_id: session.to_string(),
            graph: agent.name().to_string(),
            thread_id: thread.clone(),
            runs: project::summaries(&context, &saved),
            lineage: project::lineage(&context, &saved, &found),
            removed_before: project::removed_before(&saved),
        })
    }

    fn context<'a>(
        &'a self,
        session: &'a str,
        thread: &'a str,
        graph: &'a str,
        shape: &'a GraphShape,
        channels: &'a [String],
    ) -> Context<'a> {
        Context {
            session,
            thread,
            graph,
            shape,
            channels,
            offset: self.inner.offset,
            display: &self.inner.display,
        }
    }

    /// The latest run of `session`, as the Runs tab shows it (spec 3.3 §12): each node's
    /// state, the steps, the lineage and forks, and the decision each waiting node waits on,
    /// matched in the inbox by origin and node.
    ///
    /// # Errors
    ///
    /// Returns `surface.not_found` for a session that is not the caller's (S-I8), or for a
    /// workflow `graph` does not name; the checkpointer's or the source's error otherwise.
    pub async fn run(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        graph: Option<&str>,
    ) -> Result<WorkflowRun> {
        let user = caller.user_id();
        self.owned(user, session).await?;
        let thread = self.thread(user, session);
        let agent = self.graph_for(&thread, graph).await?;
        let checkpoints = checkpointer(agent)?;
        let saved = Self::saved(agent, &thread).await?;
        let shape = agent.graph().shape();
        let channels = agent.graph().state_channels();

        let mut found = Found {
            waiting: self.waiting(engine, caller, session, agent.name()).await?,
            stopped: engine.source().stopped().await.unwrap_or(false),
            forks: self.forks(checkpoints.as_ref(), user, session, &thread).await,
            ..Found::default()
        };
        let newest = saved.last().map(|saved| &saved.checkpoint);
        if let Some(lineage) = newest.and_then(|newest| newest.lineage().ok().flatten()) {
            let (started, inherited) = parent_facts(checkpoints.as_ref(), &lineage).await;
            found.parent_run_started = started;
            found.inherited = inherited;
            found.parent_session = session_of_thread(&self.inner.app, user, &lineage.parent);
        }
        if newest.is_some_and(Checkpoint::is_paused) && found.waiting.is_empty() {
            found.question = self.question(user, session).await;
        }
        let context = self.context(session, &thread, agent.name(), &shape, &channels);
        Ok(project::run_view(&context, &saved, found))
    }

    /// One step of `session`, from the checkpoint `checkpoint` (spec 3.3 §12): its nodes,
    /// "What changed" since the checkpoint before it, its state, and what a fork from it would
    /// do. Reading it changes nothing.
    ///
    /// # Errors
    ///
    /// Returns `surface.not_found` for a session that is not the caller's, or a checkpoint that
    /// is not of the session's thread (S-I8); the checkpointer's error otherwise.
    pub async fn step(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        checkpoint: &str,
    ) -> Result<StepDetail> {
        let user = caller.user_id();
        self.owned(user, session).await?;
        let thread = self.thread(user, session);
        let agent = self.graph_for(&thread, None).await?;
        let checkpoints = checkpointer(agent)?;
        let target = checkpoints
            .load_by_id(checkpoint)
            .await
            .map_err(source)?
            .filter(|found| found.thread_id == thread)
            .ok_or_else(|| SurfaceError::not_found(format!("checkpoint '{checkpoint}'")))?;
        let previous = checkpoints
            .list_page(&thread, Some(target.version), 1)
            .await
            .map_err(source)?
            .into_iter()
            .next();
        let later: Vec<Saved> = checkpoints
            .list(&thread)
            .await
            .map_err(source)?
            .into_iter()
            .filter(|later| later.version > target.version)
            .map(Saved::new)
            .collect();
        let root = target.lineage().ok().flatten().map_or_else(|| thread.clone(), |l| l.root);
        let effects =
            adk_graph::governed::effect_log(checkpoints.as_ref(), &root).await.map_err(source)?;
        let shape = agent.graph().shape();
        let channels = agent.graph().state_channels();

        // Each governed node's call, at this checkpoint's state, in short.
        let context_state =
            NodeContext::new(target.state.clone(), ExecutionConfig::new(&thread), target.step);
        let calls: BTreeMap<String, String> = shape
            .nodes
            .iter()
            .filter(|node| node.governed.is_some())
            .filter_map(|node| {
                let args = agent.graph().node(&node.name)?.call_args(&context_state)?;
                Some((node.name.clone(), project::call_summary(&args)?))
            })
            .collect();

        let saved = Saved::new(target);
        let decisions = self.decisions(engine, caller, session, agent.name(), &saved).await?;
        let context = self.context(session, &thread, agent.name(), &shape, &channels);
        Ok(project::step_detail(
            &context,
            &saved,
            StepInputs {
                previous: previous.as_ref(),
                later: &later,
                effects: &effects,
                decisions,
                calls,
            },
        ))
    }

    /// The newest checkpoint version of `session`'s thread, which the Runs tab's feed watches;
    /// `None` before the first checkpoint.
    ///
    /// # Errors
    ///
    /// Returns `surface.not_found` for a session that is not the caller's (S-I8).
    pub async fn version(&self, caller: &Caller, session: &str) -> Result<Option<u64>> {
        let user = caller.user_id();
        self.owned(user, session).await?;
        let thread = self.thread(user, session);
        let agent = self.graph_for(&thread, None).await?;
        Ok(checkpointer(agent)?.load(&thread).await.map_err(source)?.map(|newest| newest.version))
    }

    /// Forks `session` at the completed checkpoint `request.from` into a new session of the
    /// caller, with `request.set` written over whole channels, and starts it when
    /// `request.start` is set (spec 3.3 §12).
    ///
    /// | Refused | With |
    /// |---|---|
    /// | A paired device, or any surface but the web home | `403 surface.not_here` |
    /// | A session that is not the caller's, or a checkpoint not of its thread | `404 surface.not_found` (S-I8) |
    /// | A decision of the session waits | `409 workflow.decision_waiting`: "Answer the waiting decision first" |
    /// | A pause, a step with nothing after it, an edit of a key the graph does not declare, a value over [`FORK_VALUE_LIMIT`] | `400` with the fork's `fork.*` code |
    ///
    /// The original thread is never written; a governed call the fork makes unchanged is
    /// answered from its record. While Stop all is on the fork is made and not started.
    /// The fork is recorded as [`SURFACE_AUDIT_WORKFLOW_FORK`] with the session, the
    /// checkpoint, the fork's session, the edited keys and the caller's surface and origin.
    ///
    /// # Errors
    ///
    /// As the table says, and the stores' errors.
    pub async fn fork(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        request: ForkRequest,
    ) -> Result<ForkOutcome> {
        if caller.device.is_some() || caller.surface != SurfaceKind::Web {
            return Err(SurfaceError::NotHere(
                "a fork is made on the web home's Runs tab, not from a paired device or another \
                 surface"
                    .into(),
            ));
        }
        let user = caller.user_id();
        self.owned(user, session).await?;
        if engine.source().waiting_on_decision(user, session).await?
            || !engine.session_decisions(caller, session).await?.is_empty()
        {
            return Err(SurfaceError::Source(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Conflict,
                "workflow.decision_waiting",
                words::ANSWER_THE_WAITING_DECISION_FIRST,
            )));
        }
        if let Some((key, _)) =
            request.set.iter().find(|(_, value)| value.to_string().len() > FORK_VALUE_LIMIT)
        {
            return Err(refused(
                "fork.too_large",
                format!("'{key}' is too large; a fork sets at most {FORK_VALUE_LIMIT} bytes a key"),
            ));
        }
        let thread = self.thread(user, session);
        let agent = self.graph_for(&thread, None).await?;
        let checkpoints = checkpointer(agent)?;
        let fork_session = format!("s-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let fork_thread = self.thread(user, &fork_session);
        let spec = ForkSpec {
            from: request.from.clone(),
            new_thread_id: fork_thread.clone(),
            set: request.set,
        };
        let handle = agent.graph().time_travel(&thread).map_err(source)?;
        let fork = handle.fork(spec).await.map_err(fork_refused)?;
        let created = self
            .session_service()?
            .create(CreateRequest {
                app_name: self.inner.app.clone(),
                user_id: user.to_string(),
                session_id: Some(fork_session.clone()),
                state: Default::default(),
            })
            .await;
        if let Err(error) = created {
            // A fork without its session is unreachable; it is removed.
            if let Err(cleanup) = checkpoints.delete(&fork_thread).await {
                tracing::warn!(error = %cleanup, "a fork whose session failed could not be removed");
            }
            return Err(SurfaceError::Source(error));
        }
        let edited: Vec<String> =
            fork.lineage().ok().flatten().map(|lineage| lineage.edited).unwrap_or_default();
        engine
            .record(
                caller,
                SURFACE_AUDIT_WORKFLOW_FORK,
                json!({
                    "sessionId": session,
                    "from": request.from,
                    "forkSessionId": fork_session,
                    "edited": edited,
                }),
            )
            .await?;
        tracing::info!(
            session.id = %session,
            fork.session_id = %fork_session,
            fork.edited = edited.len(),
            "forked a workflow run"
        );
        let started = request.start && !engine.source().stopped().await.unwrap_or(false);
        if started {
            self.start_turn(caller, &fork_session).await?;
        }
        Ok(ForkOutcome {
            session_id: fork_session,
            thread_id: fork_thread,
            from: request.from,
            edited,
            started,
            text: if started || !request.start { words::FORKED } else { words::FORKED_STOPPED }
                .to_string(),
        })
    }

    /// Starts a fork made and not started, such as one made while everything was stopped.
    ///
    /// # Errors
    ///
    /// Returns `403 surface.not_here` from a paired device or another surface,
    /// `404 surface.not_found` for a session that is not the caller's, `409 workflow.stopped`
    /// while everything is stopped, and `400 workflow.started` for a run that already started.
    pub async fn start(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
    ) -> Result<ForkOutcome> {
        if caller.device.is_some() || caller.surface != SurfaceKind::Web {
            return Err(SurfaceError::NotHere(
                "a fork is started on the web home's Runs tab".into(),
            ));
        }
        let user = caller.user_id();
        self.owned(user, session).await?;
        let thread = self.thread(user, session);
        let agent = self.graph_for(&thread, None).await?;
        let newest = checkpointer(agent)?.load(&thread).await.map_err(source)?;
        // A fork not started is its first checkpoint alone: a lineage and no run.
        let lineage = match newest {
            Some(newest) if newest.run_record().ok().flatten().is_none() => {
                newest.lineage().ok().flatten()
            }
            _ => None,
        }
        .ok_or_else(|| refused("workflow.started", "this run already started"))?;
        if engine.source().stopped().await.unwrap_or(false) {
            return Err(SurfaceError::Source(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Conflict,
                "workflow.stopped",
                words::FORKED_STOPPED,
            )));
        }
        self.start_turn(caller, session).await?;
        Ok(ForkOutcome {
            session_id: session.to_string(),
            thread_id: thread,
            from: lineage.parent_checkpoint,
            edited: lineage.edited,
            started: true,
            text: words::FORKED.to_string(),
        })
    }

    /// Runs a turn of `session` with no message, as the caller: the graph continues from its
    /// newest checkpoint.
    async fn start_turn(&self, caller: &Caller, session: &str) -> Result<()> {
        let runner = self
            .inner
            .runner
            .as_ref()
            .ok_or_else(|| SurfaceError::unsupported("starting a fork without a runner"))?;
        let user =
            UserId::new(caller.user_id()).map_err(|e| SurfaceError::Invalid(e.to_string()))?;
        let id = SessionId::new(session).map_err(|e| SurfaceError::Invalid(e.to_string()))?;
        let context = RequestContext {
            user_id: caller.user_id().to_string(),
            scopes: caller.groups.clone(),
            metadata: Default::default(),
        };
        let config = RunConfig { request_context: Some(context), ..runner.run_config().clone() };
        let mut stream =
            runner.run_with_config(user, id, Content::new("user"), Some(config)).await?;
        let session = session.to_string();
        tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                if let Err(error) = event {
                    tracing::warn!(session.id = %session, error = %error, "a fork's run failed");
                    break;
                }
            }
        });
        Ok(())
    }

    /// The pending decision each node of `graph` waits on in `session`, by node.
    async fn waiting(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        graph: &str,
    ) -> Result<BTreeMap<String, String>> {
        let origin = origin_of(graph);
        let mut requests: Vec<ApprovalRequest> = engine
            .source()
            .requests(caller, Some(session), true)
            .await?
            .into_iter()
            .filter(|request| {
                request.state == ApprovalState::Pending
                    && request.scope.session_id == session
                    && request.origin.as_ref().is_some_and(|known| known.source == origin)
            })
            .collect();
        requests.sort_by_key(|request| request.created_at);
        Ok(requests.into_iter().map(|request| (request.tool_name, request.id)).collect())
    }

    /// The decision each governed node of a step waited on, and what became of it.
    async fn decisions(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        graph: &str,
        saved: &Saved,
    ) -> Result<BTreeMap<String, NodeDecision>> {
        let governed: Vec<&NodeRecord> = saved
            .steps
            .iter()
            .flat_map(|step| &step.nodes)
            .filter(|node| node.effect_key.is_some() && node.status != NodeStatus::Recorded)
            .collect();
        if governed.is_empty() {
            return Ok(BTreeMap::new());
        }
        let origin = origin_of(graph);
        let requests: Vec<ApprovalRequest> = engine
            .source()
            .requests(caller, Some(session), false)
            .await?
            .into_iter()
            .filter(|request| {
                request.scope.session_id == session
                    && request.origin.as_ref().is_some_and(|known| known.source == origin)
            })
            .collect();
        let mut decisions = BTreeMap::new();
        for node in governed {
            let ended = node.started_at
                + chrono::Duration::milliseconds(i64::try_from(node.duration_ms).unwrap_or(0))
                + chrono::Duration::seconds(5);
            let Some(request) = requests
                .iter()
                .filter(|request| request.tool_name == node.node && request.created_at <= ended)
                .max_by_key(|request| request.created_at)
            else {
                continue;
            };
            let waiting = request.state == ApprovalState::Pending;
            let at =
                request.created_at.with_timezone(&self.inner.offset).format("%H:%M").to_string();
            let text = if waiting {
                words::asked_at(&at)
            } else {
                match engine.decision(caller, &request.id).await {
                    Ok(card) => card.decided.map_or(card.status_text, |decided| decided.text),
                    Err(_) => words::asked_at(&at),
                }
            };
            decisions
                .insert(node.node.clone(), NodeDecision { id: request.id.clone(), waiting, text });
        }
        Ok(decisions)
    }

    /// The forks of `thread`: the caller's other sessions whose lineage names it, newest first.
    async fn forks(
        &self,
        checkpoints: &dyn Checkpointer,
        user: &str,
        session: &str,
        thread: &str,
    ) -> Vec<ForkSummary> {
        let Ok(service) = self.session_service() else { return Vec::new() };
        let listed = service
            .list(ListRequest {
                app_name: self.inner.app.clone(),
                user_id: user.to_string(),
                limit: Some(FORK_SCAN_LIMIT),
                offset: None,
            })
            .await
            .unwrap_or_default();
        let mut forks = Vec::new();
        for other in listed {
            if other.id() == session {
                continue;
            }
            let Ok(Some(newest)) = checkpoints.load(&self.thread(user, other.id())).await else {
                continue;
            };
            let Some(lineage) = newest.lineage().ok().flatten().filter(|l| l.parent == thread)
            else {
                continue;
            };
            let at =
                newest.run_record().ok().flatten().map_or(newest.created_at, |run| run.started_at);
            forks.push(ForkSummary {
                session_id: other.id().to_string(),
                at,
                at_text: at.with_timezone(&self.inner.offset).format("%H:%M").to_string(),
                parent_step: lineage.parent_step,
                text: words::fork_summary(lineage.parent_step, &lineage.edited),
                edited: lineage.edited,
            });
        }
        forks.sort_by_key(|fork| std::cmp::Reverse(fork.at));
        forks
    }

    /// The question a paused node asked, from the session's latest graph interrupt event.
    async fn question(&self, user: &str, session: &str) -> Option<String> {
        let found = self
            .session_service()
            .ok()?
            .get(GetRequest {
                app_name: self.inner.app.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: Some(20),
                after: None,
            })
            .await
            .ok()?;
        found.events().all().iter().rev().find_map(|event| {
            let payload = adk_graph::interrupt::GraphInterruptPayload::from_event(event)?;
            (payload.kind == "dynamic").then_some(payload.message).flatten()
        })
    }

    /// The governed node `request` is for, when its origin is a graph observed here.
    fn governed_node(
        &self,
        request_origin: Option<&str>,
        node: &str,
    ) -> Option<(String, NodeShape)> {
        let origin = request_origin?;
        let agent = self.inner.graphs.iter().find(|agent| origin == origin_of(agent.name()))?;
        let shape = agent.graph().shape();
        let node =
            shape.nodes.into_iter().find(|shape| shape.name == node && shape.governed.is_some())?;
        Some((agent.name().to_string(), node))
    }

    /// An approval router that keeps `fallback`'s route and raises the risk of a call from a
    /// governed node that spends money to high, so chat, voice and the kernel's own routes
    /// treat it as money (spec 3.3 §7.2, §7.4 rule 4).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ApprovalPolicy, ApproverSelector};
    /// use adk_surface::workflow::Workflows;
    ///
    /// let router = Workflows::new("adk-rust")
    ///     .approval_router(ApprovalPolicy::new([ApproverSelector::group("owner")]));
    /// # let _ = router;
    /// ```
    pub fn approval_router(&self, fallback: impl ApprovalRouter + 'static) -> WorkflowRouter {
        WorkflowRouter { workflows: self.clone(), fallback: Arc::new(fallback) }
    }
}

/// The router [`Workflows::approval_router`] builds.
pub struct WorkflowRouter {
    workflows: Workflows,
    fallback: Arc<dyn ApprovalRouter>,
}

impl std::fmt::Debug for WorkflowRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowRouter").field("workflows", &self.workflows).finish_non_exhaustive()
    }
}

impl ApprovalRouter for WorkflowRouter {
    fn route(&self, call: &ApprovalCall) -> Option<ApprovalRoute> {
        let mut route = self.fallback.route(call)?;
        let origin = call.request.origin.as_ref().map(|origin| origin.source.as_str());
        let money = self
            .workflows
            .governed_node(origin, &call.request.tool_name)
            .is_some_and(|(_, node)| node.governed.is_some_and(|governed| governed.spends_money));
        if money {
            route.risk = route.risk.max(ApprovalRisk::High);
        }
        Some(route)
    }
}

#[async_trait]
impl CardHints for Workflows {
    /// A governed node's card (spec 3.3 §7.1, §12): never **Always allow**, money for a node
    /// that spends it, "Can't be undone" for a non-idempotent one, and "From the invoice-run
    /// workflow · Open in Runs".
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
        let origin = request.origin.as_ref().map(|origin| origin.source.as_str());
        let (graph, node) = self.governed_node(origin, &request.tool_name)?;
        let mut hint = CardHint::new().without_always_allow().with_detail(
            CardDetail::new(words::from_the_workflow(&graph)).with_link(
                words::OPEN_IN_RUNS,
                format!("/ui/developer/runs/{}", path_segment(&request.scope.session_id)),
            ),
        );
        if node.governed.is_some_and(|governed| governed.spends_money) {
            hint = hint.spends_money();
        }
        if project::effect_of(&node) == Some(adk_core::ToolEffect::NonIdempotent) {
            hint = hint.with_reversibility(Reversibility::CantBeUndone);
        }
        Some(hint)
    }
}

/// The origin a graph agent's governed calls carry.
fn origin_of(graph: &str) -> String {
    format!("graph:{graph}")
}

fn checkpointer(agent: &GraphAgent) -> Result<Arc<dyn Checkpointer>> {
    agent.graph().checkpointer().cloned().ok_or_else(|| {
        SurfaceError::unsupported(format!(
            "observing '{}', a graph without a checkpointer",
            agent.name()
        ))
    })
}

fn source(error: adk_graph::GraphError) -> SurfaceError {
    SurfaceError::Source(error.into())
}

fn refused(code: &'static str, message: impl Into<String>) -> SurfaceError {
    SurfaceError::Source(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::InvalidInput,
        code,
        message.into(),
    ))
}

/// A fork's refusal, in the words the fork dialog shows.
fn fork_refused(error: ForkError) -> SurfaceError {
    match error {
        ForkError::UnknownCheckpoint { id } | ForkError::OtherThread { id, .. } => {
            SurfaceError::not_found(format!("checkpoint '{id}'"))
        }
        ForkError::Paused { .. } => refused("fork.paused", words::FORK_FROM_THE_COMPLETED_STEP),
        ForkError::NothingToRun { .. } => {
            refused("fork.nothing_to_run", words::NOTHING_RUNS_AFTER_THE_LAST_STEP)
        }
        ForkError::UndeclaredChannel { key } => {
            refused("fork.undeclared_channel", format!("Not a channel of this graph: {key}"))
        }
        ForkError::UnknownKey { key } => {
            refused("fork.unknown_key", format!("Not a key of this checkpoint: {key}"))
        }
        ForkError::Checkpointer(error) => source(error),
        other => refused(other.code(), other.to_string()),
    }
}

/// When the run a fork came from started, and the nodes it finished up to the fork.
async fn parent_facts(
    checkpoints: &dyn Checkpointer,
    lineage: &adk_graph::record::Lineage,
) -> (Option<DateTime<Utc>>, Vec<NodeRecord>) {
    let Ok(parent) = checkpoints.list(&lineage.parent).await else { return (None, Vec::new()) };
    let saved: Vec<Saved> = parent.into_iter().map(Saved::new).collect();
    let Some(at) =
        saved.iter().position(|saved| saved.checkpoint.checkpoint_id == lineage.parent_checkpoint)
    else {
        return (None, Vec::new());
    };
    let Some(run) = saved[at].run.clone() else { return (None, Vec::new()) };
    let mut done: BTreeMap<String, NodeRecord> = BTreeMap::new();
    for entry in saved[..=at].iter().filter(|entry| entry.run.as_ref() == Some(&run)) {
        for step in entry.steps.iter().filter(|step| step.step <= lineage.parent_step) {
            for node in &step.nodes {
                if matches!(node.status, NodeStatus::Done | NodeStatus::Recorded) {
                    done.insert(node.node.clone(), node.clone());
                }
            }
        }
    }
    (Some(run.started_at), done.into_values().collect())
}

/// The session of `thread`, when it is a session of `user` in `app`.
fn session_of_thread(app: &str, user: &str, thread: &str) -> Option<String> {
    let prefix = session_thread_id(app, user, "");
    let encoded = thread.strip_prefix(&prefix)?;
    let decoded = encoded.replace("%3A", ":").replace("%25", "%");
    (session_thread_id(app, user, &decoded) == thread).then_some(decoded)
}

/// `value` as one path segment: every byte but the unreserved ones percent-encoded.
fn path_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_names_its_session_only_for_its_own_user() {
        let thread = session_thread_id("adk-rust", "ana", "s:1%");
        assert_eq!(session_of_thread("adk-rust", "ana", &thread), Some("s:1%".to_string()));
        assert_eq!(session_of_thread("adk-rust", "bob", &thread), None);
    }

    #[test]
    fn a_session_is_one_path_segment() {
        assert_eq!(path_segment("s-1/2 3"), "s-1%2F2%203");
    }
}
