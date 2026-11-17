//! Agents served over MCP: each agent is an MCP tool whose call starts a durable run, followed
//! as an MCP task, with held tool calls asked as elicitations (feature `mcp`).
//!
//! [`McpAgentHost`] publishes agents through `adk-tool`'s [`McpServer`], so one server serves
//! them over Streamable HTTP ([`McpRouterBuilder`]) and stdio, with a Server Card. Every call
//! becomes a run in a [`DurableRunStore`], which a [`DurableWorker`](crate::background::DurableWorker)
//! — the host's own, or a kernel's — claims and runs:
//!
//! | MCP | Durable run |
//! |-----|-------------|
//! | `tools/call` naming an agent | A run of that agent for the caller, on a new session or the `sessionId` given |
//! | The call's answer, to a client that declared the tasks extension (SEP-2663) | A task whose id is the run id |
//! | `tasks/get` | The run as the store holds it; see [Task status](#task-status) |
//! | `tasks/update` | Answers to the elicitations of a run parked on held calls; see [Held calls](#held-calls) |
//! | `tasks/cancel` | [`DurableRunStore::request_cancel`] |
//! | The call's answer, to any other client | The run's answer, once it finishes; a run that parks asks through `input_required` (MCP `2026-07-28`) or `elicitation/create` (earlier revisions) |
//! | `notifications/progress` | The run's tool calls, tool results and text, while a call without a task waits; a task's `statusMessage` carries the latest |
//!
//! # Task status
//!
//! | Run | Task |
//! |-----|------|
//! | `Queued`, `Running` | `working`, with the run's latest step as `statusMessage` while this process runs it |
//! | `Waiting` on held calls still pending | `input_required`, one form elicitation per held call |
//! | `Waiting` otherwise (suspended, frozen, decided and about to resume) | `working` |
//! | `Completed` | `completed`, with the agent's answer as the result |
//! | `Failed` | `failed`; the run's error stays in the server's log |
//! | `Cancelled` | `cancelled` |
//!
//! The task lives as long as its run: a server restarted on the same stores answers for it, and
//! the worker that recovers the run continues it.
//!
//! # Held calls
//!
//! A run whose agent holds a tool call for approval parks
//! ([`RunCheckpointRef::AwaitingApproval`]). Each held call still pending in the approval store
//! is asked as a form elicitation keyed by a digest of the run, the call's fingerprint and its
//! hold, so an answer binds to the exact call it was asked for:
//!
//! | Answer | Decision |
//! |--------|----------|
//! | `accept`, unless `approve` is `false` | Approve |
//! | `accept` with `approve: false`, or `decline` | Reject, with the form's `reason` |
//! | `cancel` | Reject, as dismissed |
//!
//! The decision goes through the [approval service](McpAgentHostBuilder::approval_service) when
//! it holds an open request for the call — at the request's version, as the caller, under the
//! request's approval policy — and is otherwise recorded in the approval store under the call's
//! fingerprint. The run is then woken, and resumes the held call through the governed path. An
//! answer to a key the run no longer waits on — decided elsewhere, withdrawn, or held again — is
//! refused as stale, and nothing is decided.
//!
//! # Identity
//!
//! A run belongs to the caller that started it: its user is the [`McpCaller`]'s id, and a run an
//! authenticated caller starts runs as that authenticated user. Another caller's task id is
//! unknown. By default only authenticated callers may start runs; resolve them with
//! [`RequestContextCallers`] to use the server's own [`RequestContextExtractor`], or set a
//! [local caller](adk_tool::mcp::server::McpServerBuilder::local_caller) on a stdio server.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::{InMemoryApprovalStore, InMemoryRunStore};
//! use adk_server::background::DurableWorker;
//! use adk_server::mcp::{McpAgent, McpAgentHost};
//! use adk_tool::mcp::server::{McpCaller, McpRouterBuilder};
//!
//! # async fn serve(runner: Arc<adk_runner::Runner>) -> Result<(), Box<dyn std::error::Error>> {
//! let runs = Arc::new(InMemoryRunStore::new());
//! let approvals = Arc::new(InMemoryApprovalStore::new());
//! let host = McpAgentHost::builder("support", runs.clone())
//!     .agent(McpAgent::new("assistant", "Answers questions about orders"))
//!     .approval_store(approvals)
//!     .build()?;
//! let worker = DurableWorker::new(runs)
//!     .with_agent("assistant", runner)
//!     .with_hooks(host.worker_hooks())
//!     .spawn();
//! let server = host
//!     .server("com.example/support", "1.0.0", "The support team's assistant")
//!     .local_caller(McpCaller::authenticated("local:ana"))
//!     .build()?;
//! let router = McpRouterBuilder::new(server).build()?;
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
//! axum::serve(listener, router).await?;
//! worker.shutdown().await;
//! # Ok(())
//! # }
//! ```
//!
//! [`McpServer`]: adk_tool::mcp::server::McpServer
//! [`McpRouterBuilder`]: adk_tool::mcp::server::McpRouterBuilder
//! [`RequestContextExtractor`]: adk_core::request_context::RequestContextExtractor

mod caller;
mod live;

pub use caller::RequestContextCallers;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditTrail,
};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalDecision, ApprovalRequest, ApprovalRequestQuery, ApprovalScope,
    ApprovalService, ApprovalState, ApprovalStore, DurableRunStore, ErrorCategory, ErrorComponent,
    GovernanceControl, NewRun, OnConflict, PendingApproval, Result, RunCheckpointRef, RunRecord,
    RunState, Tool, ToolApproval, ToolContext, ToolEffect,
};
use adk_tool::mcp::rmcp::model::{
    CallToolResult, ContentBlock, DetailedTask, ElicitRequest, ElicitRequestParams, ElicitResult,
    ElicitationAction, InputRequest, InputRequests, InputResponses, JsonObject, Task, TaskPayload,
    TaskStatus,
};
use adk_tool::mcp::server::{
    McpCallExecutor, McpCaller, McpRequestPeer, McpServer, McpServerBuilder, McpTaskHost,
    McpToolCall, McpToolOutcome,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast};

use self::live::{Live, LiveHooks, Update};
use crate::background::WorkerHooks;

/// The prefix of the `requestState` that names the run an `input_required` result asked for.
const REQUEST_STATE_PREFIX: &str = "adk-run:";

/// Most characters of a held call's arguments an elicitation quotes.
const QUOTED_ARGS_CHARS: usize = 2_000;

/// One agent an [`McpAgentHost`] publishes as an MCP tool.
///
/// # Example
///
/// ```rust
/// use adk_server::mcp::McpAgent;
///
/// let agent = McpAgent::new("assistant", "Answers questions about orders")
///     .tool_name("ask_support")
///     .title("Support assistant");
/// assert_eq!(agent.target(), "assistant");
/// assert_eq!(agent.name(), "ask_support");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAgent {
    target: String,
    name: String,
    description: String,
    title: Option<String>,
}

impl McpAgent {
    /// The agent the durable worker runs as `target`, published as a tool of the same name with
    /// `description`.
    pub fn new(target: impl Into<String>, description: impl Into<String>) -> Self {
        let target = target.into();
        Self { name: target.clone(), target, description: description.into(), title: None }
    }

    /// Publishes the agent as the tool `name` instead of its target name.
    #[must_use]
    pub fn tool_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The tool's display title.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The worker target the agent's runs name.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The tool name the agent is published as.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Serves agents over MCP, with each call a durable run followed as an MCP task. See the
/// [module documentation](self).
#[derive(Clone)]
pub struct McpAgentHost {
    inner: Arc<Inner>,
}

struct Inner {
    app_name: String,
    queue: String,
    runs: Arc<dyn DurableRunStore>,
    agents: Vec<McpAgent>,
    approvals: Arc<dyn ApprovalStore>,
    approval_service: Option<Arc<dyn ApprovalService>>,
    governance: Option<GovernanceControl>,
    audit: Option<AuditTrail>,
    poll_interval: Duration,
    wait_limit: Duration,
    max_attempts: u32,
    allow_anonymous: bool,
    live: Arc<Live>,
    /// Serializes answers, so two answers to one held call decide it once.
    answering: Mutex<()>,
}

impl std::fmt::Debug for McpAgentHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpAgentHost")
            .field("app_name", &self.inner.app_name)
            .field("queue", &self.inner.queue)
            .field("agents", &self.inner.agents)
            .field("approval_service", &self.inner.approval_service.is_some())
            .finish_non_exhaustive()
    }
}

/// Builds an [`McpAgentHost`].
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// use adk_core::{InMemoryApprovalStore, InMemoryRunStore};
/// use adk_server::mcp::{McpAgent, McpAgentHost};
///
/// let host = McpAgentHost::builder("support", Arc::new(InMemoryRunStore::new()))
///     .agent(McpAgent::new("assistant", "Answers questions about orders"))
///     .approval_store(Arc::new(InMemoryApprovalStore::new()))
///     .queue("interactive")
///     .poll_interval(Duration::from_millis(500))
///     .build()?;
/// assert_eq!(host.agents()[0].name(), "assistant");
/// # Ok::<(), adk_core::AdkError>(())
/// ```
pub struct McpAgentHostBuilder {
    app_name: String,
    runs: Arc<dyn DurableRunStore>,
    agents: Vec<McpAgent>,
    queue: String,
    approvals: Option<Arc<dyn ApprovalStore>>,
    approval_service: Option<Arc<dyn ApprovalService>>,
    governance: Option<GovernanceControl>,
    audit: Option<AuditTrail>,
    poll_interval: Duration,
    wait_limit: Duration,
    max_attempts: u32,
    allow_anonymous: bool,
}

impl McpAgentHost {
    /// The default interval between reads of a run's state, and the poll interval tasks
    /// suggest.
    pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);
    /// The default longest wait of a call answered without a task.
    pub const DEFAULT_WAIT_LIMIT: Duration = Duration::from_secs(600);

    /// Starts a host of agents of the app `app_name` — the app name of the runners the worker
    /// runs them with — whose runs live in `runs`.
    pub fn builder(
        app_name: impl Into<String>,
        runs: Arc<dyn DurableRunStore>,
    ) -> McpAgentHostBuilder {
        McpAgentHostBuilder {
            app_name: app_name.into(),
            runs,
            agents: Vec::new(),
            queue: "default".to_string(),
            approvals: None,
            approval_service: None,
            governance: None,
            audit: None,
            poll_interval: Self::DEFAULT_POLL_INTERVAL,
            wait_limit: Self::DEFAULT_WAIT_LIMIT,
            max_attempts: NewRun::DEFAULT_MAX_ATTEMPTS,
            allow_anonymous: false,
        }
    }

    /// The published agents.
    pub fn agents(&self) -> &[McpAgent] {
        &self.inner.agents
    }

    /// The hooks to install on the worker that runs the agents, with
    /// [`DurableWorker::with_hooks`](crate::background::DurableWorker::with_hooks), so calls
    /// hear of the runs' progress and settle at once instead of at the next poll.
    pub fn worker_hooks(&self) -> Arc<dyn WorkerHooks> {
        Arc::new(LiveHooks {
            live: Arc::clone(&self.inner.live),
            app_name: self.inner.app_name.clone(),
            targets: self.inner.agents.iter().map(|agent| agent.target.clone()).collect(),
        })
    }

    /// One tool per agent, as an [`McpServer`] publishes it; calls reach the host as the
    /// server's executor.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.inner
            .agents
            .iter()
            .map(|agent| Arc::new(AgentTool(agent.clone())) as Arc<dyn Tool>)
            .collect()
    }

    /// An [`McpServer`] builder named `name` at `version` with `description` (see
    /// [`McpServer::builder`]) that publishes the agents, runs their calls through this host,
    /// follows their tasks, and bounds no call by time: a call without a task waits up to the
    /// host's [wait limit](McpAgentHostBuilder::wait_limit).
    pub fn server(
        &self,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> McpServerBuilder {
        McpServer::builder(name, version)
            .description(description)
            .tools(self.tools())
            .executor(Arc::new(self.clone()))
            .task_host(Arc::new(self.clone()))
            .call_timeout(None)
    }

    /// The run `task_id` when `caller` started it through this host.
    async fn owned(&self, task_id: &str, caller: &McpCaller) -> Result<RunRecord> {
        let inner = &self.inner;
        let record = inner.runs.get(task_id).await?.filter(|record| {
            record.app_name == inner.app_name
                && record.queue == inner.queue
                && record.user_id == caller.id()
                && record.authenticated == caller.is_authenticated()
                && inner.agents.iter().any(|agent| agent.target == record.target)
        });
        record.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                "mcp.agent.unknown_task",
                format!("no task '{}' of yours exists on this server", quoted(task_id)),
            )
        })
    }

    /// The held calls `record` waits on that are still pending, with their elicitation keys.
    async fn outstanding(&self, record: &RunRecord) -> Result<Vec<Held>> {
        let Some(RunCheckpointRef::AwaitingApproval { session_id, fingerprints, .. }) =
            &record.checkpoint
        else {
            return Ok(Vec::new());
        };
        if record.state != RunState::Waiting || record.wake_pending {
            return Ok(Vec::new());
        }
        let scope = ApprovalScope::new(&record.app_name, &record.user_id, session_id);
        let pending: Vec<PendingApproval> = self
            .inner
            .approvals
            .pending(&scope)
            .await?
            .into_iter()
            .filter(|entry| fingerprints.contains(&entry.fingerprint))
            .collect();
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        let open = match &self.inner.approval_service {
            Some(_) => self.open_requests(&scope).await?,
            None => Vec::new(),
        };
        Ok(pending
            .into_iter()
            .map(|entry| {
                let approval =
                    open.iter().find(|request| request.fingerprint == entry.fingerprint).cloned();
                let key = held_key(&record.run_id, &entry, approval.as_ref());
                Held { key, scope: scope.clone(), entry, approval }
            })
            .collect())
    }

    /// The pending approval requests of `scope`.
    async fn open_requests(&self, scope: &ApprovalScope) -> Result<Vec<ApprovalRequest>> {
        let mut query = ApprovalRequestQuery::default()
            .with_session(scope)
            .with_states([ApprovalState::Pending]);
        let mut open = Vec::new();
        loop {
            let batch = match self.inner.approvals.list_requests(&query).await {
                Ok(batch) => batch,
                // A store that keeps no requests has none open.
                Err(error) if error.category == ErrorCategory::Unsupported => return Ok(open),
                Err(error) => return Err(error),
            };
            let full = batch.len() >= query.limit;
            let last = batch.last().map(ApprovalRequest::cursor);
            open.extend(batch);
            if !full {
                return Ok(open);
            }
            query = query.with_after(last);
        }
    }

    /// Records `responses` as the decisions of the held calls `record` waits on, then wakes it.
    async fn answer(
        &self,
        record: &RunRecord,
        caller: &McpCaller,
        responses: &InputResponses,
    ) -> Result<()> {
        if responses.is_empty() {
            return Err(invalid("mcp.agent.no_answers", "the update answers no input request"));
        }
        let _answering = self.inner.answering.lock().await;
        let held = self.outstanding(record).await?;
        let mut decisions = Vec::with_capacity(responses.len());
        for (key, answer) in responses {
            let Some(call) = held.iter().find(|call| &call.key == key) else {
                return Err(AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::Conflict,
                    "mcp.agent.stale_answer",
                    format!(
                        "input request '{}' is not outstanding: the call it asked about was \
                         decided, withdrawn or held again; read the task and answer its \
                         current requests",
                        quoted(key)
                    ),
                ));
            };
            decisions.push((call, decision_of(key, answer)?));
        }
        let approver = SubjectRef::human(caller.id());
        for (call, (approve, reason)) in decisions {
            match (&self.inner.approval_service, &call.approval) {
                (Some(service), Some(request)) => {
                    let decision = if approve {
                        ApprovalDecision::approve(approver.clone())
                    } else {
                        ApprovalDecision::reject(approver.clone(), reason.clone())
                    };
                    service.decide(&request.id, request.version, decision).await?;
                }
                _ => {
                    if let Some(trail) = &self.inner.audit {
                        let decision =
                            if approve { AuditDecision::Approved } else { AuditDecision::Rejected };
                        let draft = AuditRecordDraft::new(
                            AuditRecordKind::Approval,
                            &call.entry.request.tool_name,
                        )
                        .actor(AuditActor::subject(approver.clone()))
                        .args_digest(trail.digest(&call.entry.request.args))
                        .decision(decision)
                        .ext("source", json!("mcp_elicitation"))
                        .ext("runId", json!(record.run_id))
                        .ext("sessionId", json!(call.scope.session_id));
                        trail.record(draft).await?;
                    }
                    let approval = if approve {
                        ToolApproval::approve()
                    } else {
                        ToolApproval::deny().with_message(reason.clone())
                    };
                    self.inner
                        .approvals
                        .decide(&call.scope, &call.entry.fingerprint, approval)
                        .await?;
                }
            }
            tracing::info!(
                run.id = %record.run_id,
                caller.id = caller.id(),
                tool.name = %call.entry.request.tool_name,
                approved = approve,
                "mcp caller decided a held call"
            );
        }
        self.inner.runs.wake(&record.run_id).await
    }

    /// Waits until run `run_id` finishes or parks on held calls, or `limit` passes, reporting
    /// its progress to `peer`.
    async fn settle(
        &self,
        run_id: &str,
        peer: Option<&McpRequestPeer>,
        limit: Duration,
    ) -> Result<(RunRecord, Vec<Held>)> {
        let deadline = tokio::time::Instant::now() + limit;
        let mut updates = self.inner.live.subscribe(run_id);
        let settled = loop {
            let record = self.inner.runs.get(run_id).await?.ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::NotFound,
                    "mcp.agent.run_pruned",
                    format!("run '{run_id}' is no longer in the run store"),
                )
            })?;
            if record.state.is_terminal() {
                break (record, Vec::new());
            }
            let held = self.outstanding(&record).await?;
            if !held.is_empty() || tokio::time::Instant::now() >= deadline {
                break (record, held);
            }
            let poll = tokio::time::sleep(self.inner.poll_interval);
            tokio::pin!(poll);
            loop {
                tokio::select! {
                    update = updates.recv() => match update {
                        Ok(Update::Progress(line)) => {
                            if let Some(peer) = peer {
                                peer.progress(line).await;
                            }
                        }
                        Ok(Update::Settled) | Err(broadcast::error::RecvError::Lagged(_)) => break,
                        Err(broadcast::error::RecvError::Closed) => {
                            updates = self.inner.live.subscribe(run_id);
                        }
                    },
                    () = &mut poll => break,
                    () = tokio::time::sleep_until(deadline) => break,
                }
            }
        };
        drop(updates);
        self.inner.live.release(run_id);
        Ok(settled)
    }

    /// The task `record` is, with the `held` calls it waits on.
    fn task(&self, record: &RunRecord, held: &[Held]) -> DetailedTask {
        let poll_ms = u64::try_from(self.inner.poll_interval.as_millis()).unwrap_or(u64::MAX);
        let base = Task::new(
            &record.run_id,
            TaskStatus::Working,
            record.created_at.to_rfc3339(),
            record.updated_at.to_rfc3339(),
        )
        .with_poll_interval_ms(poll_ms.max(1));
        let live = || self.inner.live.status(&record.run_id);
        let (payload, message) = match record.state {
            RunState::Completed => {
                let result = completed(record);
                let text = result_text(&result);
                (TaskPayload::Completed { result: call_result(&result) }, Some(text))
            }
            RunState::Failed => {
                let mut error = JsonObject::new();
                error.insert("code".to_string(), json!(-32603));
                error.insert("message".to_string(), json!("the agent run failed"));
                error.insert("data".to_string(), json!({ "runId": record.run_id }));
                (TaskPayload::Failed { error }, Some("the agent run failed".to_string()))
            }
            RunState::Cancelled => (TaskPayload::Cancelled, Some("cancelled".to_string())),
            RunState::Waiting if !held.is_empty() => {
                let tools: Vec<&str> =
                    held.iter().map(|call| call.entry.request.tool_name.as_str()).collect();
                let message = format!("waiting for approval to run {}", tools.join(", "));
                let agent = self.agent_name(&record.target);
                let input_requests: InputRequests =
                    held.iter().map(|call| (call.key.clone(), elicitation(call, agent))).collect();
                (TaskPayload::InputRequired { input_requests }, Some(message))
            }
            RunState::Waiting => (TaskPayload::Working, Some("paused".to_string())),
            RunState::Queued => (TaskPayload::Working, Some("queued".to_string())),
            _ => (TaskPayload::Working, Some(live().unwrap_or_else(|| "running".to_string()))),
        };
        let task = match message {
            Some(message) => base.with_status_message(message),
            None => base,
        };
        DetailedTask::new(task, payload)
    }

    /// The tool name `target` is published as.
    fn agent_name<'a>(&'a self, target: &'a str) -> &'a str {
        self.inner
            .agents
            .iter()
            .find(|agent| agent.target == target)
            .map_or(target, |agent| agent.name.as_str())
    }

    /// Starts a run for `call`.
    async fn start(&self, call: &McpToolCall) -> Result<RunRecord> {
        let inner = &self.inner;
        let agent =
            inner.agents.iter().find(|agent| agent.name == call.tool.name()).ok_or_else(|| {
                invalid("mcp.agent.unknown", "the tool names no agent of this host")
            })?;
        let message = call.arguments.get("message").and_then(Value::as_str).unwrap_or_default();
        if message.trim().is_empty() {
            return Err(invalid("mcp.agent.message", "`message` must be a non-empty string"));
        }
        let session_id = match call.arguments.get("sessionId").and_then(Value::as_str) {
            Some(session) => {
                adk_core::identity::SessionId::try_from(session).map_err(|error| {
                    invalid("mcp.agent.session_id", format!("`sessionId` is not usable: {error}"))
                })?;
                session.to_string()
            }
            None => format!("mcp-{}", uuid::Uuid::new_v4()),
        };
        let mut run = NewRun::new(
            &inner.queue,
            &inner.app_name,
            call.caller.id(),
            &agent.target,
            json!({ "text": message }),
        )
        .with_session_id(&session_id)
        .with_concurrency_key(format!("mcp-session:{session_id}"), OnConflict::Enqueue)
        .with_max_attempts(inner.max_attempts);
        if call.caller.is_authenticated() {
            run = run.with_authenticated_user();
        }
        let enqueued = inner.runs.enqueue(run).await?;
        let record = enqueued.record().cloned().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Conflict,
                "mcp.agent.rejected",
                "the run store refused the run",
            )
        })?;
        tracing::info!(
            run.id = %record.run_id,
            caller.id = call.caller.id(),
            agent = %agent.target,
            session.id = %session_id,
            task = call.accepts_task,
            "mcp call started an agent run"
        );
        Ok(record)
    }

    /// The answer to a call that waits for its run: the result, the questions it waits on, or
    /// a refusal.
    async fn wait_for(&self, call: &McpToolCall, run_id: &str) -> Result<McpToolOutcome> {
        let mut guard =
            CancelOnDrop { runs: Some(Arc::clone(&self.inner.runs)), run_id: run_id.to_string() };
        loop {
            let (record, held) =
                self.settle(run_id, call.peer.as_ref(), self.inner.wait_limit).await?;
            if record.state.is_terminal() {
                guard.disarm();
                return Ok(outcome_of(&record));
            }
            if held.is_empty() {
                guard.disarm();
                return Err(AdkError::timeout(
                    ErrorComponent::Server,
                    "mcp.agent.wait_limit",
                    format!(
                        "run '{run_id}' is still working after {} s; call with a client that \
                         takes MCP tasks to follow it",
                        self.inner.wait_limit.as_secs()
                    ),
                ));
            }
            let agent = self.agent_name(&record.target);
            if call.accepts_input_required {
                guard.disarm();
                let input_requests =
                    held.iter().map(|held| (held.key.clone(), elicitation(held, agent))).collect();
                return Ok(McpToolOutcome::InputRequired {
                    input_requests,
                    request_state: Some(format!("{REQUEST_STATE_PREFIX}{run_id}")),
                });
            }
            let Some(peer) = call.peer.as_ref().filter(|peer| peer.can_elicit()) else {
                guard.disarm();
                return Err(AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::Unsupported,
                    "mcp.agent.needs_input",
                    format!(
                        "run '{run_id}' waits for approval of a held call, and this client can \
                         be asked neither through a task, input_required nor elicitation; \
                         decide it in the approval inbox"
                    ),
                ));
            };
            let mut responses = InputResponses::new();
            for held in &held {
                let InputRequest::Elicitation(request) = elicitation(held, agent) else {
                    continue;
                };
                let answer = peer.elicit(request.params).await?;
                responses
                    .insert(held.key.clone(), serde_json::to_value(answer).unwrap_or(Value::Null));
            }
            self.answer(&record, &call.caller, &responses).await?;
        }
    }
}

impl McpAgentHostBuilder {
    /// Publishes `agent`.
    #[must_use]
    pub fn agent(mut self, agent: McpAgent) -> Self {
        self.agents.push(agent);
        self
    }

    /// Enqueues runs on `queue`, which the worker running the agents claims from; `default`
    /// by default.
    #[must_use]
    pub fn queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = queue.into();
        self
    }

    /// The approval store of the runners that run the agents, where their held calls wait;
    /// required.
    #[must_use]
    pub fn approval_store(mut self, store: Arc<dyn ApprovalStore>) -> Self {
        self.approvals = Some(store);
        self
    }

    /// Decides held calls the service holds an open request for through it, under the
    /// request's approval policy, with the caller as the approver.
    #[must_use]
    pub fn approval_service(mut self, service: Arc<dyn ApprovalService>) -> Self {
        self.approval_service = Some(service);
        self
    }

    /// Refuses new calls while `control` refuses work. The worker's own control stops the
    /// runs already started.
    #[must_use]
    pub fn governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Records each decision an answer makes in the approval store in `trail`, before it takes
    /// effect; a decision whose record cannot be written under
    /// [`AuditFailureMode::Block`](adk_core::audit_log::AuditFailureMode::Block) is refused.
    /// The approval service records the decisions it takes through its own trail.
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// How often a waiting call reads its run, and the poll interval tasks suggest;
    /// [`McpAgentHost::DEFAULT_POLL_INTERVAL`] by default.
    #[must_use]
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// The longest a call answered without a task waits for its run;
    /// [`McpAgentHost::DEFAULT_WAIT_LIMIT`] by default. The run goes on after the wait.
    #[must_use]
    pub fn wait_limit(mut self, limit: Duration) -> Self {
        self.wait_limit = limit;
        self
    }

    /// The most attempts each run may take, counting recoveries;
    /// [`NewRun::DEFAULT_MAX_ATTEMPTS`] by default.
    #[must_use]
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Lets unauthenticated callers start runs, which then all belong to the one user
    /// `anonymous` and see each other's tasks; off by default.
    #[must_use]
    pub fn allow_anonymous(mut self, allow: bool) -> Self {
        self.allow_anonymous = allow;
        self
    }

    /// Builds the host.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidInput` error when no agent is published, two agents share a tool
    /// name, no approval store is set, or the poll interval is zero.
    pub fn build(self) -> Result<McpAgentHost> {
        if self.agents.is_empty() {
            return Err(invalid("mcp.agent.none", "publish at least one agent with `agent(..)`"));
        }
        for (index, agent) in self.agents.iter().enumerate() {
            if self.agents[..index].iter().any(|other| other.name == agent.name) {
                return Err(invalid(
                    "mcp.agent.duplicate",
                    format!(
                        "two agents are published as '{}'; MCP tool names are unique",
                        agent.name
                    ),
                ));
            }
        }
        let approvals = self.approvals.ok_or_else(|| {
            invalid(
                "mcp.agent.approval_store",
                "set the runners' approval store with `approval_store(..)`, so held calls can \
                 be asked and decided",
            )
        })?;
        if self.poll_interval.is_zero() {
            return Err(invalid("mcp.agent.poll_interval", "the poll interval must be above zero"));
        }
        Ok(McpAgentHost {
            inner: Arc::new(Inner {
                app_name: self.app_name,
                queue: self.queue,
                runs: self.runs,
                agents: self.agents,
                approvals,
                approval_service: self.approval_service,
                governance: self.governance,
                audit: self.audit,
                poll_interval: self.poll_interval,
                wait_limit: self.wait_limit,
                max_attempts: self.max_attempts,
                allow_anonymous: self.allow_anonymous,
                live: Arc::default(),
                answering: Mutex::new(()),
            }),
        })
    }
}

#[async_trait]
impl McpCallExecutor for McpAgentHost {
    async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome> {
        if !call.caller.is_authenticated() && !self.inner.allow_anonymous {
            return Err(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Forbidden,
                "mcp.agent.unauthenticated",
                "serving an agent needs an authenticated caller: resolve callers on the router, \
                 or name the local caller of a stdio server",
            ));
        }
        if let Some(control) = &self.inner.governance {
            control.check()?;
        }
        // A retried call carries the answers to the questions its run asked.
        if let Some(state) = &call.request_state {
            let run_id = state.strip_prefix(REQUEST_STATE_PREFIX).ok_or_else(|| {
                invalid("mcp.agent.request_state", "the requestState names no run of this server")
            })?;
            let record = self.owned(run_id, &call.caller).await?;
            if let Some(responses) =
                call.input_responses.as_ref().filter(|responses| !responses.is_empty())
            {
                self.answer(&record, &call.caller, responses).await?;
            }
            return self.wait_for(&call, &record.run_id).await;
        }
        let record = self.start(&call).await?;
        if call.accepts_task {
            return Ok(McpToolOutcome::Task(self.task(&record, &[]).task));
        }
        self.wait_for(&call, &record.run_id).await
    }

    fn is_visible(&self, _tool: &dyn Tool, caller: &McpCaller) -> bool {
        caller.is_authenticated() || self.inner.allow_anonymous
    }
}

#[async_trait]
impl McpTaskHost for McpAgentHost {
    async fn get(&self, task_id: &str, caller: &McpCaller) -> Result<DetailedTask> {
        let record = self.owned(task_id, caller).await?;
        let held = self.outstanding(&record).await?;
        Ok(self.task(&record, &held))
    }

    async fn update(
        &self,
        task_id: &str,
        caller: &McpCaller,
        responses: InputResponses,
    ) -> Result<()> {
        let record = self.owned(task_id, caller).await?;
        self.answer(&record, caller, &responses).await
    }

    async fn cancel(&self, task_id: &str, caller: &McpCaller) -> Result<()> {
        let record = self.owned(task_id, caller).await?;
        if record.state.is_terminal() {
            return Err(invalid(
                "mcp.agent.task_finished",
                format!("task '{}' already finished", quoted(task_id)),
            ));
        }
        tracing::info!(run.id = %record.run_id, caller.id = caller.id(), "mcp caller cancelled an agent run");
        self.inner.runs.request_cancel(&record.run_id).await
    }
}

/// A held call a run waits on, with the key its elicitation is asked under.
#[derive(Debug, Clone)]
struct Held {
    key: String,
    scope: ApprovalScope,
    entry: PendingApproval,
    /// The approval service's open request for the call, if any.
    approval: Option<ApprovalRequest>,
}

/// The key of a held call's elicitation: a digest of the run, the call's fingerprint, when it
/// was held, and the approval request's version, so it changes whenever the question does.
fn held_key(run_id: &str, entry: &PendingApproval, approval: Option<&ApprovalRequest>) -> String {
    let digest = adk_core::json_digest(&json!([
        run_id,
        entry.fingerprint,
        entry.requested_at.to_rfc3339(),
        approval.map(|request| (request.id.as_str(), request.version)),
    ]));
    let hex = digest.rsplit(':').next().unwrap_or(&digest);
    format!("approval-{}", &hex[..hex.len().min(24)])
}

/// The form elicitation that asks the caller about `held`, made by `agent`.
fn elicitation(held: &Held, agent: &str) -> InputRequest {
    let request = &held.entry.request;
    let (summary, args) = match &held.approval {
        Some(approval) => (Some(approval.summary.as_str()), &approval.args_redacted),
        None => (None, &request.args),
    };
    let args = args.to_string();
    let mut quoted_args: String = args.chars().take(QUOTED_ARGS_CHARS).collect();
    if args.chars().count() > QUOTED_ARGS_CHARS {
        quoted_args.push('…');
    }
    let mut message = format!(
        "The agent `{agent}` asks to run `{}` with {quoted_args}. Approve this call?",
        request.tool_name
    );
    if let Some(summary) = summary.filter(|summary| !summary.is_empty()) {
        message.push_str(&format!("\n\n{summary}"));
    }
    let schema = json!({
        "type": "object",
        "properties": {
            "approve": {
                "type": "boolean",
                "title": "Approve",
                "description": "Run the call as shown",
                "default": true
            },
            "reason": {
                "type": "string",
                "title": "Reason",
                "description": "Why not, told to the agent when you decline"
            }
        }
    });
    let requested_schema = serde_json::from_value(schema)
        .expect("the approval form schema is a valid elicitation schema");
    InputRequest::Elicitation(ElicitRequest::new(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        requested_schema,
    }))
}

/// The decision an elicitation `answer` to `key` gives: whether to approve, and the reason.
fn decision_of(key: &str, answer: &Value) -> Result<(bool, String)> {
    let result: ElicitResult = serde_json::from_value(answer.clone()).map_err(|error| {
        invalid(
            "mcp.agent.answer",
            format!("the answer to '{}' is not an elicitation result: {error}", quoted(key)),
        )
    })?;
    let field = |name: &str| result.content.as_ref().and_then(|content| content.get(name)).cloned();
    let reason = field("reason").and_then(|reason| reason.as_str().map(str::to_string));
    Ok(match result.action {
        ElicitationAction::Accept if field("approve") != Some(Value::Bool(false)) => {
            (true, String::new())
        }
        ElicitationAction::Accept | ElicitationAction::Decline => {
            (false, reason.unwrap_or_else(|| "the caller declined the call".to_string()))
        }
        // `cancel`, and any action a later revision adds, decides nothing in the call's favour.
        _ => (false, "the caller dismissed the request".to_string()),
    })
}

/// The answer of a completed run: its text, session and run id.
fn completed(record: &RunRecord) -> Value {
    let result = record.result.as_ref();
    let text =
        result.and_then(|result| result.get("text")).and_then(Value::as_str).unwrap_or_default();
    let session = result
        .and_then(|result| result.get("sessionId"))
        .and_then(Value::as_str)
        .or(record.session_id.as_deref())
        .unwrap_or_default();
    json!({ "text": text, "sessionId": session, "runId": record.run_id })
}

fn result_text(result: &Value) -> String {
    result["text"].as_str().unwrap_or_default().to_string()
}

/// `result` as the `CallToolResult` a completed task carries.
fn call_result(result: &Value) -> JsonObject {
    let mut call = CallToolResult::success(vec![ContentBlock::text(result_text(result))]);
    call.structured_content = Some(result.clone());
    match serde_json::to_value(call) {
        Ok(Value::Object(object)) => object,
        _ => JsonObject::new(),
    }
}

/// How a call that waited for `record`, now finished, ends.
fn outcome_of(record: &RunRecord) -> McpToolOutcome {
    match record.state {
        RunState::Completed => McpToolOutcome::Completed(completed(record)),
        RunState::Cancelled => McpToolOutcome::Failed(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Cancelled,
            "mcp.agent.cancelled",
            "the agent run was cancelled",
        )),
        _ => {
            tracing::warn!(run.id = %record.run_id, error = ?record.error, "agent run served over mcp failed");
            McpToolOutcome::Failed(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Internal,
                "mcp.agent.failed",
                "the agent run failed",
            ))
        }
    }
}

/// Requests cancellation of a run whose waiting call was dropped, unless disarmed.
struct CancelOnDrop {
    runs: Option<Arc<dyn DurableRunStore>>,
    run_id: String,
}

impl CancelOnDrop {
    fn disarm(&mut self) {
        self.runs = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let Some(runs) = self.runs.take() else { return };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        let run_id = std::mem::take(&mut self.run_id);
        runtime.spawn(async move {
            match runs.request_cancel(&run_id).await {
                Ok(()) => tracing::info!(run.id = %run_id, "cancelled the run of a dropped mcp call"),
                Err(error) => tracing::warn!(run.id = %run_id, error = %error, "could not cancel the run of a dropped mcp call"),
            }
        });
    }
}

/// The tool an agent is published as. Calls reach the host as the server's executor and never
/// this tool's `execute`.
struct AgentTool(McpAgent);

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        &self.0.name
    }

    fn description(&self) -> &str {
        &self.0.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "What to ask the agent"
                },
                "sessionId": {
                    "type": "string",
                    "description": "Continue the conversation of an earlier answer's sessionId; omit to start a new one"
                }
            },
            "required": ["message"],
            "additionalProperties": false
        }))
    }

    fn response_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "sessionId": { "type": "string" },
                "runId": { "type": "string" }
            },
            "required": ["text", "sessionId", "runId"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn is_long_running(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Unsupported,
            "mcp.agent.direct_call",
            "an agent published over MCP runs through its McpAgentHost, not as a local tool",
        ))
    }
}

fn invalid(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Server, ErrorCategory::InvalidInput, code, message)
}

/// `text` cut to 128 characters, for quoting a caller's input back.
fn quoted(text: &str) -> String {
    let mut quoted: String = text.chars().take(128).collect();
    if text.chars().count() > 128 {
        quoted.push('…');
    }
    quoted
}

#[cfg(test)]
mod tests;
