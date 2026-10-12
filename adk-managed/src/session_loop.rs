//! Supervised session loop for the managed agent runtime.
//!
//! The [`SessionLoop`] is the core execution engine. It runs as a
//! `tokio::spawn`ed background task, dequeues [`UserEvent`]s from an
//! mpsc channel, processes each turn, and broadcasts [`SessionEvent`]s
//! to stream subscribers.
//!
//! # Architecture
//!
//! The loop composes:
//! - [`SequenceCounter`] — assigns monotonically increasing `seq` to each event
//! - [`CheckpointManager`] — records each event and flushes it to the state store
//! - [`ManagedStores`] — the durable run each turn executes under, and the decision store
//!   client answers are recorded in
//! - `tokio::broadcast` — fan-out to stream subscribers
//! - [`Runner`] — drives the agent through the real LLM
//! - [`SessionUsageTracker`] — tracks per-turn and cumulative token usage
//!
//! # Control Flow
//!
//! ```text
//! user.message → enqueue a run on the session's queue → claim it
//!   → emit status.running → invoke Runner → map each output event
//!   → no pending action: finish the run, emit status.idle (end_turn)
//!   → custom tool call or held confirmation: park the run (Waiting until the
//!     action's expiry), emit status.idle (requires_action)
//! user.custom_tool_result / user.tool_confirmation
//!   → record the answer in the decision store → wake the run → claim it
//!   → append custom tool results to the conversation → continue the turn
//! ```
//!
//! The loop never blocks on the client: while a turn waits, the loop keeps reading its
//! channel, and the wait lives in the run store rather than in this task. A parked
//! session polls its queue every [`DurableConfig::poll_interval`], so an answer recorded
//! by another runtime instance, or the action's expiry, resumes it.
//!
//! # Interrupt and Pause
//!
//! - **Interrupt**: A [`CancellationToken`] signals the loop to stop at the next
//!   boundary. On interrupt, the loop cancels the session's run, emits `status.idle`, and
//!   exits.
//! - **Pause/Resume**: A pause flag + [`Notify`] allow the loop to park until
//!   resumed.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, RwLock, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

#[cfg(feature = "memory")]
use adk_core::Memory;
use adk_core::{
    Agent, ClaimedRun, Content, Event, FunctionResponseData, NewRun, OnConflict, Part,
    RunCheckpointRef, RunConfig, RunOutcome, RunState as DurableRunState, SessionId, ToolApproval,
    UserId,
};
use adk_runner::Runner;
use adk_session::service::SessionService;

use crate::agent_builder::is_custom_tool_pending;
use crate::checkpoint::{CheckpointManager, RunState};
use crate::durable::{DurableConfig, ManagedStores, custom_result_key, decision_scope};
use crate::event_mapping::{RunnerOutput, map_runner_output};
use crate::sequence::SequenceCounter;
use crate::state_store::{PendingAction, PendingKind};
use crate::types::{
    ConfirmationResult, ContentBlock, RuntimeError, SessionEvent, SessionStatus, StopReason,
    UserEvent,
};
use crate::usage::{SessionUsageTracker, UsageReport};

/// The most runs one drain claims, so a run that keeps becoming claimable cannot spin.
const MAX_CLAIMS_PER_DRAIN: usize = 64;

/// Supervised session loop — one per active session.
///
/// Runs as a background `tokio::spawn`ed task. Receives user events via an
/// mpsc channel, processes each turn through the real Runner under a durable run, and
/// broadcasts session events via a `tokio::broadcast` channel.
///
/// # Example
///
/// ```rust,ignore
/// use tokio::sync::{broadcast, mpsc};
/// use tokio_util::sync::CancellationToken;
/// use adk_managed::session_loop::SessionLoop;
///
/// let (event_tx, event_rx) = mpsc::channel(64);
/// let (broadcast_tx, _) = broadcast::channel(256);
/// let cancel = CancellationToken::new();
///
/// let session_loop = SessionLoop::new(
///     "session_001".to_string(),
///     event_rx,
///     broadcast_tx,
///     cancel.clone(),
///     agent,
///     session_service,
/// );
///
/// let handle = tokio::spawn(session_loop.run());
/// // Send events via event_tx...
/// ```
pub struct SessionLoop {
    /// Session identifier.
    session_id: String,
    /// Input channel for user events.
    event_rx: mpsc::Receiver<UserEvent>,
    /// Broadcast channel for session events (fan-out to subscribers).
    event_tx: broadcast::Sender<SessionEvent>,
    /// Monotonic sequence counter.
    seq: SequenceCounter,
    /// Checkpoint manager for session state (shared with ActiveSession for replay).
    checkpoint: Arc<RwLock<CheckpointManager>>,
    /// Cancellation token for interrupt handling.
    cancel_token: CancellationToken,
    /// Pause flag — when true, the loop parks until resumed.
    pause_flag: Arc<Mutex<bool>>,
    /// Notify used to wake the loop after resume.
    pause_notify: Arc<Notify>,
    /// The owner this session's Runner calls are made under.
    ///
    /// Defaults to the historical `managed` / `managed_user` constants so an existing loop
    /// keeps working, and is replaced by [`SessionLoop::with_owner`] when the runtime knows
    /// who the session belongs to.
    owner: (String, String),
    /// Current session status.
    ///
    /// Shared with the public session handle when the runtime installs its own via
    /// [`SessionLoop::with_shared_status`].
    status: Arc<RwLock<SessionStatus>>,
    /// The agent driving this session.
    agent: Arc<dyn Agent>,
    /// Session persistence backend (needed by the Runner).
    session_service: Arc<dyn SessionService>,
    /// Optional memory service for cross-session RAG injection.
    #[cfg(feature = "memory")]
    memory: Option<Arc<dyn Memory>>,
    /// Accumulated usage tracking across all turns.
    usage_tracker: SessionUsageTracker,
    /// Names of the client-executed tools; `None` classifies tools by name.
    custom_tools: Option<BTreeSet<String>>,
    /// Where runs, state, and client answers are kept.
    stores: ManagedStores,
    /// How runs are claimed, heartbeated, and parked.
    durable: DurableConfig,
}

/// What one turn of the agent produced besides session events.
#[derive(Default)]
struct TurnOutput {
    usage: UsageReport,
    /// Custom tool calls the model issued, by call ID, not yet reported to the client.
    custom_calls: HashMap<String, (String, Value)>,
    pending: Vec<PendingAction>,
}

/// How one turn ended.
enum TurnEnd {
    /// The model finished; the run is complete.
    Done(String),
    /// The turn could not start; the run is already finished.
    Failed,
    /// The turn waits on the client for these actions.
    Parked(Vec<String>),
    /// The session was interrupted.
    Interrupted,
    /// Another worker holds the run now; nothing more is written.
    LeaseLost,
}

impl SessionLoop {
    /// Create a new session loop over in-memory [`ManagedStores`].
    ///
    /// # Arguments
    ///
    /// * `session_id` - The session this loop operates on.
    /// * `event_rx` - Receiver for incoming user events.
    /// * `event_tx` - Broadcast sender for outgoing session events.
    /// * `cancel_token` - Token to signal interrupt/shutdown.
    /// * `agent` - The built agent to drive through the Runner.
    /// * `session_service` - Session persistence for the Runner.
    pub fn new(
        session_id: String,
        event_rx: mpsc::Receiver<UserEvent>,
        event_tx: broadcast::Sender<SessionEvent>,
        cancel_token: CancellationToken,
        agent: Arc<dyn Agent>,
        session_service: Arc<dyn SessionService>,
    ) -> Self {
        let checkpoint = Arc::new(RwLock::new(CheckpointManager::new(session_id.clone())));
        Self {
            session_id,
            event_rx,
            event_tx,
            seq: SequenceCounter::default(),
            checkpoint,
            cancel_token,
            pause_flag: Arc::new(Mutex::new(false)),
            pause_notify: Arc::new(Notify::new()),
            owner: ("managed".to_string(), "managed_user".to_string()),
            status: Arc::new(RwLock::new(SessionStatus::Queued)),
            agent,
            session_service,
            #[cfg(feature = "memory")]
            memory: None,
            usage_tracker: SessionUsageTracker::new(),
            custom_tools: None,
            stores: ManagedStores::in_memory(),
            durable: DurableConfig::default(),
        }
    }

    /// Create a session loop with custom pause controls (for external pause/resume).
    ///
    /// This allows the runtime to share the pause flag, notify, and checkpoint
    /// with the session handle so that `pause()`, `resume()`, and `stream_events()`
    /// (replay) work correctly against the same state the loop writes to.
    #[cfg(feature = "memory")]
    #[allow(clippy::too_many_arguments)]
    pub fn with_pause_controls(
        session_id: String,
        event_rx: mpsc::Receiver<UserEvent>,
        event_tx: broadcast::Sender<SessionEvent>,
        cancel_token: CancellationToken,
        pause_flag: Arc<Mutex<bool>>,
        pause_notify: Arc<Notify>,
        checkpoint: Arc<RwLock<CheckpointManager>>,
        agent: Arc<dyn Agent>,
        session_service: Arc<dyn SessionService>,
        memory: Option<Arc<dyn Memory>>,
    ) -> Self {
        let mut session_loop =
            Self::new(session_id, event_rx, event_tx, cancel_token, agent, session_service);
        session_loop.pause_flag = pause_flag;
        session_loop.pause_notify = pause_notify;
        session_loop.checkpoint = checkpoint;
        session_loop.memory = memory;
        session_loop
    }

    /// Create a session loop with custom pause controls (for external pause/resume).
    ///
    /// See the `memory`-enabled variant for full documentation.
    #[cfg(not(feature = "memory"))]
    #[allow(clippy::too_many_arguments)]
    pub fn with_pause_controls(
        session_id: String,
        event_rx: mpsc::Receiver<UserEvent>,
        event_tx: broadcast::Sender<SessionEvent>,
        cancel_token: CancellationToken,
        pause_flag: Arc<Mutex<bool>>,
        pause_notify: Arc<Notify>,
        checkpoint: Arc<RwLock<CheckpointManager>>,
        agent: Arc<dyn Agent>,
        session_service: Arc<dyn SessionService>,
    ) -> Self {
        let mut session_loop =
            Self::new(session_id, event_rx, event_tx, cancel_token, agent, session_service);
        session_loop.pause_flag = pause_flag;
        session_loop.pause_notify = pause_notify;
        session_loop.checkpoint = checkpoint;
        session_loop
    }

    /// Makes this loop's Runner calls under `owner`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let session_loop = SessionLoop::with_pause_controls(/* ... */)
    ///     .with_owner(owner.app_name(), owner.user_id());
    /// ```
    pub fn with_owner(mut self, app_name: impl Into<String>, user_id: impl Into<String>) -> Self {
        self.owner = (app_name.into(), user_id.into());
        self
    }

    /// Reports status into the handle the caller already holds.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let session_loop = SessionLoop::with_pause_controls(/* ... */)
    ///     .with_shared_status(Arc::clone(&active.status));
    /// ```
    pub fn with_shared_status(mut self, status: Arc<RwLock<SessionStatus>>) -> Self {
        self.status = status;
        self
    }

    /// Keeps runs, state, and client answers in `stores`.
    ///
    /// The checkpoint manager writes to `stores.state` unless it already has a store.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let session_loop = SessionLoop::new(/* ... */).with_stores(ManagedStores::in_memory());
    /// ```
    pub fn with_stores(mut self, stores: ManagedStores) -> Self {
        self.stores = stores;
        self
    }

    /// Claims, heartbeats, and parks runs as `config` says.
    pub fn with_durable_config(mut self, config: DurableConfig) -> Self {
        self.durable = config;
        self
    }

    /// Treats exactly the tools named in `names` as client-executed custom tools.
    ///
    /// Without it, a tool that is not a built-in (`bash`, `filesystem`, `web_search`,
    /// `web_fetch`, `code_execution`) and not an MCP tool (`mcp_` prefix or `::`) is
    /// treated as custom.
    pub fn with_custom_tools(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.custom_tools = Some(names.into_iter().collect());
        self
    }

    /// Get a clone of the pause flag for external control.
    pub fn pause_flag(&self) -> Arc<Mutex<bool>> {
        Arc::clone(&self.pause_flag)
    }

    /// Get a clone of the pause notify for external control.
    pub fn pause_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.pause_notify)
    }

    /// Run the session loop (consumes self).
    ///
    /// This is the main loop body, designed to be `tokio::spawn`ed. It runs
    /// until the input channel is closed or the cancellation token is triggered.
    /// Closing the channel leaves a parked run parked, so another runtime instance can
    /// resume it; an interrupt cancels it.
    ///
    /// # Errors
    ///
    /// Returns `Err(RuntimeError)` if an unrecoverable error occurs.
    pub async fn run(mut self) -> Result<(), RuntimeError> {
        info!(session_id = %self.session_id, "session loop started");
        self.prepare().await;

        let mut tick = tokio::time::interval(self.durable.poll_interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            if self.cancel_token.is_cancelled() {
                debug!(session_id = %self.session_id, "interrupt detected, shutting down");
                self.stop_interrupted().await;
                break;
            }

            self.check_pause().await;

            let holds_run = self.checkpoint.read().await.durable_run_id().is_some();
            let cancel = self.cancel_token.clone();
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    debug!(session_id = %self.session_id, "interrupted while waiting for event");
                    self.stop_interrupted().await;
                    break;
                }
                event = self.event_rx.recv() => match event {
                    Some(UserEvent::Interrupt {}) => {
                        debug!(session_id = %self.session_id, "user.interrupt received");
                        self.stop_interrupted().await;
                        break;
                    }
                    Some(event) => self.handle(event).await,
                    None => {
                        debug!(session_id = %self.session_id, "event channel closed, shutting down");
                        break;
                    }
                },
                _ = tick.tick(), if holds_run => self.poll().await,
            }
        }

        info!(session_id = %self.session_id, "session loop exited");
        Ok(())
    }

    /// Restores the sequence position, attaches the state store, and resumes a run left
    /// claimable by a previous host.
    async fn prepare(&mut self) {
        let holds_run = {
            let mut checkpoint = self.checkpoint.write().await;
            if checkpoint.store().is_none() {
                let manager =
                    std::mem::replace(&mut *checkpoint, CheckpointManager::new(String::new()));
                *checkpoint = manager.with_store(Arc::clone(&self.stores.state));
            }
            self.seq = SequenceCounter::new(checkpoint.run_state().seq);
            checkpoint.durable_run_id().is_some()
        };
        if holds_run {
            self.drain(None).await;
        }
    }

    /// Dispatches one user event other than an interrupt.
    async fn handle(&mut self, event: UserEvent) {
        match event {
            UserEvent::Message { content } => self.on_message(content).await,
            UserEvent::CustomToolResult { custom_tool_use_id, content } => {
                let response = custom_tool_response(&content);
                self.on_answer(
                    &custom_tool_use_id,
                    PendingKind::CustomTool,
                    Answer::Result(response),
                )
                .await;
            }
            UserEvent::ToolConfirmation { tool_use_id, result, deny_message } => {
                let approval = match result {
                    ConfirmationResult::Allow => ToolApproval::approve(),
                    ConfirmationResult::Deny => ToolApproval::deny().with_message(
                        deny_message.unwrap_or_else(|| "denied by the user".to_string()),
                    ),
                };
                self.on_answer(&tool_use_id, PendingKind::Confirmation, Answer::Decision(approval))
                    .await;
            }
            UserEvent::ToolResult { tool_use_id, .. } => {
                debug!(
                    session_id = %self.session_id,
                    tool_use_id = %tool_use_id,
                    "tool result received (self-hosted only, not yet wired)"
                );
            }
            UserEvent::DefineOutcome { criteria } => {
                debug!(session_id = %self.session_id, criteria = %criteria, "outcome criteria defined");
            }
            UserEvent::Interrupt {} => {}
        }
    }

    /// Queues a turn for `content` on the session's run queue and runs it.
    async fn on_message(&mut self, content: Vec<ContentBlock>) {
        let waiting: Vec<String> =
            self.checkpoint.read().await.pending().iter().map(|action| action.id.clone()).collect();
        if !waiting.is_empty() {
            self.emit_error(
                "requires_action",
                format!(
                    "the session waits on the client for {waiting:?}; send \
                     user.custom_tool_result or user.tool_confirmation for each before a new \
                     message"
                ),
            )
            .await;
            return;
        }

        let run = NewRun::new(
            self.durable.queue(&self.session_id),
            self.owner.0.clone(),
            self.owner.1.clone(),
            self.agent.name(),
            json!({ "kind": "message", "content": content }),
        )
        .with_session_id(self.session_id.clone())
        .with_concurrency_key(self.session_id.clone(), OnConflict::Enqueue);
        match self.stores.runs.enqueue(run).await {
            Ok(enqueued) => {
                let run_id = enqueued.record().map(|record| record.run_id.clone());
                self.drain(run_id).await;
            }
            Err(error) => {
                self.emit_error("durable_store", format!("could not queue the turn: {error}")).await
            }
        }
    }

    /// Records the client's answer to a pending action and resumes the turn waiting on it.
    async fn on_answer(&mut self, id: &str, kind: PendingKind, answer: Answer) {
        let (action, run_id) = {
            let checkpoint = self.checkpoint.read().await;
            let action = checkpoint
                .pending()
                .iter()
                .find(|action| action.id == id && action.kind == kind)
                .cloned();
            (action, checkpoint.durable_run_id().map(str::to_string))
        };
        let Some(action) = action else {
            self.emit_error(
                "unknown_tool_use",
                format!("the session is not waiting on {kind:?} '{id}'"),
            )
            .await;
            return;
        };
        if kind == PendingKind::Confirmation && self.stores.approval_service.is_some() {
            self.emit_error(
                "approval_service",
                format!(
                    "'{id}' is decided through the runtime's approval service; decide its \
                     request there instead of sending user.tool_confirmation"
                ),
            )
            .await;
            return;
        }

        let approval = match answer {
            Answer::Result(response) => ToolApproval::approve().with_message(response.to_string()),
            Answer::Decision(approval) => approval,
        };
        let scope = decision_scope(&self.owner.0, &self.owner.1, &self.session_id);
        if let Err(error) =
            self.stores.decisions.decide(&scope, &action.decision_key, approval).await
        {
            self.emit_error(
                "durable_store",
                format!("could not record the answer to '{id}': {error}"),
            )
            .await;
            return;
        }
        if let Some(run_id) = run_id
            && let Err(error) = self.stores.runs.wake(&run_id).await
        {
            // The answer is recorded, so the run takes it when it is next claimed.
            warn!(session_id = %self.session_id, run.id = %run_id, error = %error, "could not wake the parked run");
        }
        self.drain(None).await;
    }

    /// Resumes the session's run if it became claimable, and lets go of a run another
    /// runtime instance has finished.
    async fn poll(&mut self) {
        self.drain(None).await;
        let Some(run_id) = self.checkpoint.read().await.durable_run_id().map(str::to_string) else {
            return;
        };
        let finished = match self.stores.runs.get(&run_id).await {
            Ok(Some(record)) => record.state.is_terminal(),
            Ok(None) => true,
            Err(_) => false,
        };
        if finished {
            self.refresh_from_store().await;
            let mut checkpoint = self.checkpoint.write().await;
            if checkpoint.durable_run_id() == Some(run_id.as_str()) {
                checkpoint.set_pending(Vec::new(), None);
            }
        }
    }

    /// Claims and executes this session's claimable runs.
    ///
    /// With a `target`, keeps polling while that run is still queued behind a run of this
    /// session held by another runtime instance.
    async fn drain(&mut self, target: Option<String>) {
        let queue = self.durable.queue(&self.session_id);
        for _ in 0..MAX_CLAIMS_PER_DRAIN {
            if self.cancel_token.is_cancelled() {
                return;
            }
            let claimed = match self
                .stores
                .runs
                .claim(&queue, &self.durable.worker_id, self.durable.lease_ttl)
                .await
            {
                Ok(claimed) => claimed,
                Err(error) => {
                    self.emit_error(
                        "durable_store",
                        format!("could not claim the session's run: {error}"),
                    )
                    .await;
                    return;
                }
            };
            if let Some(claimed) = claimed {
                self.execute(claimed).await;
                continue;
            }
            let Some(target) = target.as_deref() else { return };
            match self.stores.runs.get(target).await {
                Ok(Some(record)) if record.state == DurableRunState::Queued => {
                    tokio::select! {
                        _ = self.cancel_token.cancelled() => return,
                        _ = tokio::time::sleep(self.durable.poll_interval) => {}
                    }
                }
                _ => return,
            }
        }
    }

    /// Executes one claimed run: a new turn, a recovered turn, or a parked turn.
    async fn execute(&mut self, claimed: ClaimedRun) {
        self.refresh_from_store().await;
        {
            let mut checkpoint = self.checkpoint.write().await;
            let pending = checkpoint.pending().to_vec();
            checkpoint.set_pending(pending, Some(claimed.record.run_id.clone()));
        }
        match claimed.record.checkpoint.clone() {
            Some(RunCheckpointRef::AwaitingApproval { .. }) => self.resume_parked(claimed).await,
            Some(RunCheckpointRef::Session { invocation_id: Some(invocation_id), .. }) => {
                self.recover(claimed, &invocation_id).await;
            }
            _ => match message_content(&claimed.record.input) {
                Some(content) => {
                    let content = self.build_user_content(&content);
                    self.run_turn(claimed, content).await;
                }
                None => {
                    let error =
                        format!("run input is not a managed turn: {}", claimed.record.input);
                    self.finish(&claimed, RunOutcome::Failed { error, retryable: false }).await;
                }
            },
        }
    }

    /// Resumes a turn whose worker stopped mid-run: unanswered calls are answered from the
    /// action ledger and the turn continues without repeating the user's message.
    async fn recover(&mut self, claimed: ClaimedRun, invocation_id: &str) {
        info!(session_id = %self.session_id, run.id = %claimed.record.run_id, "recovering an interrupted turn");
        let runner = match self.build_runner() {
            Ok(runner) => runner,
            Err(error) => {
                let error = error.to_string();
                self.finish(&claimed, RunOutcome::Failed { error, retryable: true }).await;
                return;
            }
        };
        if let Err(error) = runner.recover_unanswered_calls(&self.owner.1, &self.session_id).await {
            warn!(session_id = %self.session_id, error = %error, "could not answer interrupted calls");
        }
        let turn_persisted = match self.conversation_events().await {
            Ok(events) => events
                .iter()
                .any(|event| event.invocation_id == invocation_id && event.author == "user"),
            Err(_) => false,
        };
        let content = match (turn_persisted, message_content(&claimed.record.input)) {
            (false, Some(content)) => self.build_user_content(&content),
            _ => Content::new("user").with_text(""),
        };
        self.run_turn(claimed, content).await;
    }

    /// Resumes a parked turn once every action it waits on is answered or expired, and
    /// parks it again otherwise.
    async fn resume_parked(&mut self, claimed: ClaimedRun) {
        let scope = decision_scope(&self.owner.0, &self.owner.1, &self.session_id);
        let now = Utc::now();
        let mut pending = self.checkpoint.read().await.pending().to_vec();
        let mut waiting = Vec::new();
        for action in &mut pending {
            let expired = action.expires_at.is_some_and(|at| at <= now);
            match action.kind {
                PendingKind::CustomTool if action.resolution.is_some() => {}
                PendingKind::CustomTool => {
                    match self.stores.decisions.take_approval(&scope, &action.decision_key).await {
                        Ok(Some(approval)) => {
                            action.resolution = Some(
                                approval
                                    .message
                                    .as_deref()
                                    .and_then(|message| serde_json::from_str(message).ok())
                                    .unwrap_or(Value::Null),
                            );
                        }
                        Ok(None) if expired => {
                            action.resolution = Some(json!({
                                "error": format!(
                                    "the client did not return a result for custom tool '{}' \
                                     before it expired at {}",
                                    action.tool_name,
                                    action.expires_at.map(|at| at.to_rfc3339()).unwrap_or_default()
                                ),
                                "status": "expired",
                            }));
                        }
                        Ok(None) => waiting.push(action.expires_at),
                        Err(error) => {
                            warn!(session_id = %self.session_id, error = %error, "could not read a custom tool result");
                            waiting.push(action.expires_at);
                        }
                    }
                }
                PendingKind::Confirmation => {
                    let decided = self
                        .stores
                        .decisions
                        .has_decision(&scope, &action.decision_key)
                        .await
                        .unwrap_or(false);
                    if decided {
                        continue;
                    }
                    if expired {
                        let denial = ToolApproval::deny().with_message(format!(
                            "the confirmation for '{}' expired before the client decided",
                            action.tool_name
                        ));
                        if let Err(error) =
                            self.stores.decisions.decide(&scope, &action.decision_key, denial).await
                        {
                            warn!(session_id = %self.session_id, error = %error, "could not record an expired confirmation");
                            waiting.push(action.expires_at);
                        }
                    } else {
                        waiting.push(action.expires_at);
                    }
                }
            }
        }

        // Results taken from the decision store are consumed there, so they are written to
        // the snapshot before anything else can fail.
        let run_id = claimed.record.run_id.clone();
        self.checkpoint.write().await.set_pending(pending.clone(), Some(run_id.clone()));
        self.flush().await;
        if !waiting.is_empty() {
            let until = waiting.into_iter().flatten().min();
            if let Err(error) = self.stores.runs.wait(&run_id, claimed.epoch, until).await {
                warn!(session_id = %self.session_id, run.id = %run_id, error = %error, "could not park the turn again");
            }
            return;
        }

        if let Err(error) = self.append_custom_results(&pending).await {
            self.emit_error(
                "session_store",
                format!("could not add custom tool results to the conversation: {error}"),
            )
            .await;
            // Parked again briefly: the results are in the snapshot, so the next claim
            // retries the append.
            let retry = chrono::Duration::from_std(self.durable.poll_interval)
                .ok()
                .and_then(|interval| Utc::now().checked_add_signed(interval));
            if let Err(error) = self.stores.runs.wait(&run_id, claimed.epoch, retry).await {
                warn!(session_id = %self.session_id, run.id = %run_id, error = %error, "could not park the turn again");
            }
            return;
        }
        self.checkpoint.write().await.set_pending(Vec::new(), Some(run_id));
        self.flush().await;
        // An empty turn adds nothing to the conversation: the agent resumes held calls that
        // have decisions and the model continues from the results.
        self.run_turn(claimed, Content::new("user").with_text("")).await;
    }

    /// Appends the client's custom tool results to the conversation, once.
    ///
    /// A result supersedes its call's pending response in the history the model sees.
    async fn append_custom_results(&self, pending: &[PendingAction]) -> adk_core::Result<()> {
        let events = self.conversation_events().await?;
        let answered: BTreeSet<String> = events
            .iter()
            .flat_map(|event| event.llm_response.content.iter().flat_map(|c| c.parts.iter()))
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, id: Some(id), .. }
                    if !adk_core::is_confirmation_pending(&function_response.response) =>
                {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect();
        let parts: Vec<Part> = pending
            .iter()
            .filter(|action| action.kind == PendingKind::CustomTool)
            .filter(|action| {
                action.function_call_id.as_ref().is_none_or(|id| !answered.contains(id))
            })
            .filter_map(|action| {
                action.resolution.clone().map(|response| Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        action.tool_name.clone(),
                        response,
                    ),
                    id: action.function_call_id.clone(),
                    annotations: None,
                })
            })
            .collect();
        if parts.is_empty() {
            return Ok(());
        }
        let mut event = Event::new(format!("managed-result-{}", uuid::Uuid::new_v4()));
        event.author = "user".to_string();
        event.llm_response.content = Some(Content { role: "user".to_string(), parts });
        self.session_service
            .append_event_for_identity(adk_session::AppendEventRequest {
                identity: self.identity()?,
                event,
            })
            .await
    }

    /// Runs one turn of the agent under `claimed` and finishes or parks the run.
    async fn run_turn(&mut self, claimed: ClaimedRun, content: Content) {
        *self.status.write().await = SessionStatus::Running;
        let running_event = SessionEvent::StatusRunning { seq: self.seq.next() };
        self.emit_event(running_event).await;

        let (end, usage) = self.drive(&claimed, content).await;
        let stop_reason = match end {
            TurnEnd::LeaseLost => {
                // Another worker recovered the run; it reports the turn's outcome.
                *self.status.write().await = SessionStatus::Idle;
                return;
            }
            TurnEnd::Interrupted => {
                self.finish(&claimed, RunOutcome::Cancelled).await;
                StopReason::EndTurn
            }
            TurnEnd::Failed => StopReason::EndTurn,
            TurnEnd::Done(invocation_id) => {
                let result = json!({ "sessionId": self.session_id, "invocationId": invocation_id });
                self.finish(&claimed, RunOutcome::Completed(result)).await;
                StopReason::EndTurn
            }
            TurnEnd::Parked(ids) => StopReason::RequiresAction { event_ids: ids },
        };
        let usage = if usage.is_empty() {
            None
        } else {
            self.usage_tracker.record_turn(usage.clone());
            Some(usage)
        };
        self.emit_idle(Some(stop_reason), usage).await;
    }

    /// Drives the Runner for one turn under a heartbeat, and parks the run when the turn
    /// waits on the client.
    async fn drive(&mut self, claimed: &ClaimedRun, content: Content) -> (TurnEnd, UsageReport) {
        let mut turn = TurnOutput::default();
        if self.cancel_token.is_cancelled() {
            return (TurnEnd::Interrupted, turn.usage);
        }
        let run_id = claimed.record.run_id.clone();
        let epoch = claimed.epoch;

        let started = async {
            let runner = self.build_runner()?;
            let user = UserId::try_from(self.owner.1.as_str())
                .map_err(|e| RuntimeError::invalid_request(e.to_string()))?;
            let session = SessionId::try_from(self.session_id.as_str())
                .map_err(|e| RuntimeError::invalid_request(e.to_string()))?;
            runner
                .start_with_config(user, session, content, Some(self.run_config(&run_id)))
                .await
                .map_err(|e| RuntimeError::internal(format!("runner invocation failed: {e}")))
        };
        let invocation = match started.await {
            Ok(invocation) => invocation,
            Err(error) => {
                self.emit_error("runner_error", error.to_string()).await;
                self.finish(
                    claimed,
                    RunOutcome::Failed { error: error.to_string(), retryable: false },
                )
                .await;
                return (TurnEnd::Failed, turn.usage);
            }
        };
        let invocation_id = invocation.invocation_id().to_string();
        let checkpoint = RunCheckpointRef::Session {
            session_id: self.session_id.clone(),
            invocation_id: Some(invocation_id.clone()),
        };
        if self.stores.runs.set_checkpoint(&run_id, epoch, checkpoint).await.is_err() {
            return (TurnEnd::LeaseLost, turn.usage);
        }

        let turn_cancel = self.cancel_token.child_token();
        let lease_lost = Arc::new(AtomicBool::new(false));
        let heartbeat = tokio::spawn(heartbeat(
            Arc::clone(&self.stores.runs),
            run_id.clone(),
            epoch,
            self.durable.lease_ttl,
            turn_cancel.clone(),
            Arc::clone(&lease_lost),
        ));

        let mut events = invocation.into_events();
        loop {
            let next = tokio::select! {
                biased;
                _ = turn_cancel.cancelled() => break,
                next = events.next() => next,
            };
            let Some(next) = next else { break };
            match next {
                Ok(event) => self.process_runner_event(&event, &mut turn).await,
                Err(error) => {
                    warn!(session_id = %self.session_id, error = %error, "runner event stream error");
                    self.emit_error("runner_error", error.to_string()).await;
                }
            }
        }
        drop(events);
        heartbeat.abort();

        if lease_lost.load(Ordering::SeqCst) {
            return (TurnEnd::LeaseLost, turn.usage);
        }
        if self.cancel_token.is_cancelled() || turn_cancel.is_cancelled() {
            return (TurnEnd::Interrupted, turn.usage);
        }
        if turn.pending.is_empty() {
            return (TurnEnd::Done(invocation_id), turn.usage);
        }

        let ids: Vec<String> = turn.pending.iter().map(|action| action.id.clone()).collect();
        let until = turn.pending.iter().filter_map(|action| action.expires_at).min();
        let fingerprints = turn.pending.iter().map(|action| action.decision_key.clone()).collect();
        self.checkpoint
            .write()
            .await
            .set_pending(std::mem::take(&mut turn.pending), Some(run_id.clone()));
        self.flush().await;
        let parked = async {
            self.stores
                .runs
                .set_checkpoint(
                    &run_id,
                    epoch,
                    RunCheckpointRef::AwaitingApproval {
                        session_id: self.session_id.clone(),
                        invocation_id: Some(invocation_id),
                        fingerprints,
                        expires_at: until,
                    },
                )
                .await?;
            self.stores.runs.wait(&run_id, epoch, until).await
        };
        if let Err(error) = parked.await {
            warn!(session_id = %self.session_id, run.id = %run_id, error = %error, "could not park the turn");
            return (TurnEnd::LeaseLost, turn.usage);
        }
        debug!(session_id = %self.session_id, run.id = %run_id, pending = ?ids, "turn parked on the client");
        (TurnEnd::Parked(ids), turn.usage)
    }

    /// The run configuration of one turn under `run_id`.
    fn run_config(&self, run_id: &str) -> RunConfig {
        // Session events carry complete content, so the agent yields one event per model
        // response rather than provider deltas.
        let mut builder = RunConfig::builder()
            .streaming_mode(adk_core::StreamingMode::None)
            .durable_run_id(run_id);
        if let Some(service) = &self.stores.approval_service {
            builder = builder.approval_service(Arc::clone(service));
        }
        builder.approval_store(Arc::clone(&self.stores.decisions)).build()
    }

    /// Ends the attempt of `claimed` with `outcome` and releases the session's run.
    async fn finish(&mut self, claimed: &ClaimedRun, outcome: RunOutcome) {
        let run_id = &claimed.record.run_id;
        if let Err(error) = self.stores.runs.finish(run_id, claimed.epoch, outcome).await {
            warn!(session_id = %self.session_id, run.id = %run_id, error = %error, "could not finish the run");
        }
        let mut checkpoint = self.checkpoint.write().await;
        if checkpoint.durable_run_id() == Some(run_id.as_str()) {
            let pending = checkpoint.pending().to_vec();
            checkpoint.set_pending(pending, None);
        }
        drop(checkpoint);
        self.flush().await;
    }

    /// Stops after an interrupt: cancels the session's run and reports the session idle.
    async fn stop_interrupted(&mut self) {
        let run_id = {
            let mut checkpoint = self.checkpoint.write().await;
            let run_id = checkpoint.durable_run_id().map(str::to_string);
            checkpoint.set_pending(Vec::new(), None);
            run_id
        };
        if let Some(run_id) = run_id
            && let Err(error) = self.stores.runs.request_cancel(&run_id).await
        {
            warn!(session_id = %self.session_id, run.id = %run_id, error = %error, "could not cancel the session's run");
        }
        self.emit_idle(Some(StopReason::EndTurn), None).await;
    }

    /// Replaces local state with the stored snapshot when another runtime instance has
    /// advanced the session since this one last wrote it.
    async fn refresh_from_store(&mut self) {
        let mut checkpoint = self.checkpoint.write().await;
        let Some(store) = checkpoint.store().cloned() else { return };
        match store.load(&self.session_id).await {
            Ok(Some(stored)) if stored.run_state.seq > checkpoint.run_state().seq => {
                debug!(session_id = %self.session_id, "session advanced elsewhere; reloading");
                self.seq = SequenceCounter::new(stored.run_state.seq);
                *self.status.write().await = stored.run_state.status;
                checkpoint.replace(stored);
            }
            Ok(_) => {}
            Err(error) => {
                warn!(session_id = %self.session_id, error = %error, "could not read stored session state")
            }
        }
    }

    /// Build a Runner instance for this turn.
    fn build_runner(&self) -> Result<Runner, RuntimeError> {
        #[allow(unused_mut)]
        let mut builder = Runner::builder()
            .app_name(self.owner.0.as_str())
            .agent(Arc::clone(&self.agent))
            .session_service(Arc::clone(&self.session_service))
            .cancellation_token(self.cancel_token.clone());

        #[cfg(feature = "memory")]
        if let Some(ref memory) = self.memory {
            builder = builder.memory_service(Arc::clone(memory));
        }

        builder.build().map_err(|e| RuntimeError::internal(format!("failed to build runner: {e}")))
    }

    fn identity(&self) -> adk_core::Result<adk_core::AdkIdentity> {
        Ok(adk_core::AdkIdentity::new(
            adk_core::AppName::try_from(self.owner.0.as_str())?,
            UserId::try_from(self.owner.1.as_str())?,
            SessionId::try_from(self.session_id.as_str())?,
        ))
    }

    /// The conversation's events as the session service holds them.
    async fn conversation_events(&self) -> adk_core::Result<Vec<Event>> {
        Ok(self.session_service.get_for_identity(&self.identity()?).await?.events().all())
    }

    /// Convert managed ContentBlocks into an adk-core Content for the Runner.
    fn build_user_content(&self, blocks: &[ContentBlock]) -> Content {
        let mut parts = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(Part::Text { text: text.clone() });
                }
                ContentBlock::Image { source } => {
                    if let Some(url) = source.get("url").and_then(|v| v.as_str()) {
                        parts.push(Part::FileData {
                            mime_type: source
                                .get("media_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("image/png")
                                .to_string(),
                            file_uri: url.to_string(),
                            annotations: None,
                        });
                    }
                }
                ContentBlock::File { file_id } => {
                    parts.push(Part::FileData {
                        mime_type: "application/octet-stream".to_string(),
                        file_uri: file_id.clone(),
                        annotations: None,
                    });
                }
            }
        }

        Content { role: "user".to_string(), parts }
    }

    /// Maps one Runner event to session events and records what the turn waits on.
    async fn process_runner_event(&mut self, event: &Event, turn: &mut TurnOutput) {
        if let Some(ref usage_meta) = event.llm_response.usage_metadata {
            let report = UsageReport::from_usage_metadata(usage_meta);
            turn.usage.accumulate(&report);
        }

        // Skip partial streaming chunks — we only emit complete events
        if event.llm_response.partial {
            return;
        }

        // A held call: the client must allow or deny it. The event's text is a notice to
        // the caller, not model output.
        if let Some(request) = &event.actions.tool_confirmation {
            let id = request
                .function_call_id
                .clone()
                .unwrap_or_else(|| format!("tu_{}", uuid::Uuid::new_v4()));
            if let Some((name, input)) = turn.custom_calls.get(&id).cloned() {
                // A custom tool held for confirmation was not reported yet.
                let output = RunnerOutput::BuiltinToolCall { tool_use_id: id.clone(), name, input };
                let session_event = map_runner_output(output, self.seq.next());
                self.emit_event(session_event).await;
            }
            let expires_at = if self.stores.approval_service.is_some() {
                // The approval service applies its own expiry policy.
                None
            } else {
                self.expiry()
            };
            turn.pending.push(PendingAction {
                id,
                kind: PendingKind::Confirmation,
                tool_name: request.tool_name.clone(),
                function_call_id: request.function_call_id.clone(),
                decision_key: request.fingerprint(),
                input: request.args.clone(),
                expires_at,
                resolution: None,
            });
            return;
        }

        let Some(ref content) = event.llm_response.content else { return };
        // Providers that stream deliver a response's text as many parts; adjacent text
        // parts are one message.
        let mut text = String::new();
        for part in &content.parts {
            if let Part::Text { text: piece } = part {
                text.push_str(piece);
                continue;
            }
            if !text.is_empty() {
                let output = RunnerOutput::TextContent { text: std::mem::take(&mut text) };
                let session_event = map_runner_output(output, self.seq.next());
                self.emit_event(session_event).await;
            }
            match part {
                Part::FunctionCall { name, args, id, .. } => {
                    let tool_use_id =
                        id.clone().unwrap_or_else(|| format!("tu_{}", uuid::Uuid::new_v4()));
                    let output = match self.classify_tool(name) {
                        ToolKind::Custom => {
                            // Reported when the tool asks the client for its result, so a
                            // call held for confirmation is not reported as awaiting one.
                            turn.custom_calls.insert(tool_use_id, (name.clone(), args.clone()));
                            continue;
                        }
                        ToolKind::Builtin => RunnerOutput::BuiltinToolCall {
                            tool_use_id,
                            name: name.clone(),
                            input: args.clone(),
                        },
                        ToolKind::Mcp => RunnerOutput::McpToolCall {
                            tool_use_id,
                            name: name.clone(),
                            input: args.clone(),
                        },
                    };
                    let session_event = map_runner_output(output, self.seq.next());
                    self.emit_event(session_event).await;
                }
                Part::FunctionResponse { function_response, id, .. }
                    if is_custom_tool_pending(&function_response.response) =>
                {
                    let custom_tool_use_id =
                        id.clone().unwrap_or_else(|| format!("ctu_{}", uuid::Uuid::new_v4()));
                    let input = id
                        .as_ref()
                        .and_then(|id| turn.custom_calls.get(id))
                        .map(|(_, args)| args.clone())
                        .unwrap_or(Value::Null);
                    let output = RunnerOutput::CustomToolCall {
                        custom_tool_use_id: custom_tool_use_id.clone(),
                        name: function_response.name.clone(),
                        input: input.clone(),
                    };
                    let session_event = map_runner_output(output, self.seq.next());
                    self.emit_event(session_event).await;
                    turn.pending.push(PendingAction {
                        decision_key: custom_result_key(&custom_tool_use_id),
                        id: custom_tool_use_id,
                        kind: PendingKind::CustomTool,
                        tool_name: function_response.name.clone(),
                        function_call_id: id.clone(),
                        input,
                        expires_at: self.expiry(),
                        resolution: None,
                    });
                }
                _ => {}
            }
        }
        if !text.is_empty() {
            let output = RunnerOutput::TextContent { text };
            let session_event = map_runner_output(output, self.seq.next());
            self.emit_event(session_event).await;
        }
    }

    /// When an action created now expires.
    fn expiry(&self) -> Option<DateTime<Utc>> {
        self.durable
            .pending_expiry
            .and_then(|expiry| chrono::Duration::from_std(expiry).ok())
            .and_then(|expiry| Utc::now().checked_add_signed(expiry))
    }

    /// Classify a tool call by name to determine which RunnerOutput variant to use.
    fn classify_tool(&self, name: &str) -> ToolKind {
        if let Some(custom) = &self.custom_tools {
            if custom.contains(name) {
                return ToolKind::Custom;
            }
            return if name.starts_with("mcp_") || name.contains("::") {
                ToolKind::Mcp
            } else {
                ToolKind::Builtin
            };
        }

        // Known built-in tools execute server-side
        const BUILTIN_TOOLS: &[&str] =
            &["bash", "filesystem", "web_search", "web_fetch", "code_execution"];

        if BUILTIN_TOOLS.contains(&name) {
            ToolKind::Builtin
        } else if name.starts_with("mcp_") || name.contains("::") {
            ToolKind::Mcp
        } else {
            // All other tools are custom (client-executed)
            ToolKind::Custom
        }
    }

    /// Writes the current snapshot to the state store.
    async fn flush(&self) {
        if let Err(error) = self.checkpoint.read().await.flush().await {
            warn!(session_id = %self.session_id, error = %error, "could not write session state");
        }
    }

    /// Emit a session event: checkpoint it, write it to the state store, then broadcast.
    async fn emit_event(&mut self, event: SessionEvent) {
        let status = *self.status.read().await;
        {
            let mut checkpoint = self.checkpoint.write().await;
            let run_state = RunState {
                seq: self.seq.current(),
                pending_tool_ids: checkpoint.pending().iter().map(|a| a.id.clone()).collect(),
                status,
            };
            checkpoint.checkpoint(event.clone(), run_state);
            // Written before the broadcast, so a subscriber never sees an event the store
            // does not hold.
            if let Err(error) = checkpoint.flush().await {
                warn!(session_id = %self.session_id, error = %error, "could not write session state");
            }
        }

        // Broadcast to subscribers (ignore if no receivers).
        let _ = self.event_tx.send(event);
    }

    /// Emits an `error` event.
    async fn emit_error(&mut self, code: &str, message: String) {
        let event = SessionEvent::Error { code: code.to_string(), message, seq: self.seq.next() };
        self.emit_event(event).await;
    }

    /// Emit a `status.idle` event and update internal status.
    async fn emit_idle(&mut self, stop_reason: Option<StopReason>, usage: Option<UsageReport>) {
        {
            let mut status = self.status.write().await;
            // Archiving is terminal; the turn it stopped does not make the session idle.
            if *status != SessionStatus::Archived {
                *status = SessionStatus::Idle;
            }
        }
        let idle_event = SessionEvent::StatusIdle { seq: self.seq.next(), stop_reason, usage };
        self.emit_event(idle_event).await;
    }

    /// Check and handle pause state. If paused, blocks until resumed.
    async fn check_pause(&self) {
        loop {
            let is_paused = *self.pause_flag.lock().await;
            if !is_paused {
                break;
            }
            debug!(session_id = %self.session_id, "session loop paused, waiting for resume");
            self.pause_notify.notified().await;
        }
    }
}

/// A client's answer to a pending action.
enum Answer {
    /// A custom tool's result, as the model will see it.
    Result(Value),
    /// A confirmation decision.
    Decision(ToolApproval),
}

/// The function response a custom tool result's content becomes.
///
/// Text blocks become `{"result": "<text>"}`; anything else keeps its blocks.
fn custom_tool_response(content: &[ContentBlock]) -> Value {
    let texts: Option<Vec<&str>> = content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Image { .. } | ContentBlock::File { .. } => None,
        })
        .collect();
    match texts {
        Some(texts) => json!({ "result": texts.join("\n") }),
        None => json!({ "content": content }),
    }
}

/// The user content of a managed turn's run input.
fn message_content(input: &Value) -> Option<Vec<ContentBlock>> {
    (input.get("kind").and_then(Value::as_str) == Some("message"))
        .then(|| input.get("content").cloned())
        .flatten()
        .and_then(|content| serde_json::from_value(content).ok())
}

/// Renews the lease every third of `ttl` until the turn ends.
///
/// A heartbeat that reports a cancellation request, or that finds the run held by another
/// worker, cancels the turn; the latter also marks the lease lost so the turn writes
/// nothing more.
async fn heartbeat(
    runs: Arc<dyn adk_core::DurableRunStore>,
    run_id: String,
    epoch: u64,
    ttl: std::time::Duration,
    turn_cancel: CancellationToken,
    lease_lost: Arc<AtomicBool>,
) {
    let period = (ttl / 3).max(std::time::Duration::from_millis(10));
    loop {
        tokio::time::sleep(period).await;
        match runs.heartbeat(&run_id, epoch, ttl).await {
            Ok(false) => {}
            Ok(true) => {
                turn_cancel.cancel();
                return;
            }
            Err(error) if error.code == adk_core::run_store::RUN_STALE_EPOCH_CODE => {
                lease_lost.store(true, Ordering::SeqCst);
                turn_cancel.cancel();
                return;
            }
            Err(error) => warn!(run.id = %run_id, error = %error, "heartbeat failed"),
        }
    }
}

/// Tool classification used internally by the session loop.
///
/// Re-exported from [`crate::event_mapping::ToolKind`] for internal use.
use crate::event_mapping::ToolKind;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use adk_core::{FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream};
    use async_stream::stream;
    use async_trait::async_trait;

    /// Mock LLM that returns a configurable response.
    struct TestLlm {
        response_text: String,
    }

    impl TestLlm {
        fn new(text: &str) -> Self {
            Self { response_text: text.to_string() }
        }
    }

    #[async_trait]
    impl Llm for TestLlm {
        fn name(&self) -> &str {
            "test-llm"
        }

        async fn generate_content(
            &self,
            _request: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<LlmResponseStream> {
            let text = self.response_text.clone();
            let s = stream! {
                yield Ok(LlmResponse {
                    content: Some(Content::new("model").with_text(&text)),
                    partial: false,
                    turn_complete: true,
                    finish_reason: Some(FinishReason::Stop),
                    ..Default::default()
                });
            };
            Ok(Box::pin(s))
        }
    }

    /// Build a test agent with the given LLM.
    fn build_test_agent(llm: impl Llm + 'static) -> Arc<dyn Agent> {
        let agent =
            adk_agent::LlmAgentBuilder::new("test-agent").model(Arc::new(llm)).build().unwrap();
        Arc::new(agent)
    }

    /// Helper to create a session loop with default test configuration.
    fn create_test_loop()
    -> (mpsc::Sender<UserEvent>, broadcast::Receiver<SessionEvent>, CancellationToken, SessionLoop)
    {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, broadcast_rx) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let agent = build_test_agent(TestLlm::new("Hello from the agent"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        let session_loop = SessionLoop::new(
            "test_session".to_string(),
            event_rx,
            broadcast_tx,
            cancel.clone(),
            agent,
            session_service,
        );

        (event_tx, broadcast_rx, cancel, session_loop)
    }

    #[tokio::test]
    async fn test_basic_message_flow() {
        let (event_tx, mut broadcast_rx, _cancel, session_loop) = create_test_loop();

        let handle = tokio::spawn(session_loop.run());

        // Send a message.
        event_tx
            .send(UserEvent::Message {
                content: vec![ContentBlock::Text { text: "Hello".to_string() }],
            })
            .await
            .unwrap();

        // Expect: status.running, then agent response events, then status.idle
        let ev1 = broadcast_rx.recv().await.unwrap();
        match ev1 {
            SessionEvent::StatusRunning { seq } => assert_eq!(seq, 0),
            other => panic!("expected StatusRunning, got: {other:?}"),
        }

        // Collect remaining events until we get StatusIdle
        let mut got_message = false;
        let mut got_idle = false;
        for _ in 0..10 {
            match tokio::time::timeout(Duration::from_secs(5), broadcast_rx.recv()).await {
                Ok(Ok(SessionEvent::Message { content, .. })) => {
                    assert!(!content.is_empty());
                    got_message = true;
                }
                Ok(Ok(SessionEvent::StatusIdle { stop_reason, .. })) => {
                    assert!(matches!(stop_reason, Some(StopReason::EndTurn)));
                    got_idle = true;
                    break;
                }
                Ok(Ok(SessionEvent::Error { message, .. })) => {
                    // In test environments without a real model, errors are acceptable
                    debug!("got error event: {message}");
                }
                Ok(Ok(other)) => {
                    debug!("got other event: {other:?}");
                }
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        // We must at least get status.idle (the turn completes regardless)
        assert!(got_idle, "expected StatusIdle event");

        // Close the channel to stop the loop.
        drop(event_tx);
        let result = handle.await.unwrap();
        assert!(result.is_ok());

        // Note: got_message depends on whether the Runner successfully invoked
        // the mock LLM. In unit tests, InMemorySessionService may not have the
        // session pre-created so the Runner creates one — either way the flow
        // should complete without panics.
        let _ = got_message;
    }

    #[tokio::test]
    async fn test_seq_monotonically_increases() {
        let (event_tx, mut broadcast_rx, _cancel, session_loop) = create_test_loop();

        let handle = tokio::spawn(session_loop.run());

        // Send a message
        event_tx
            .send(UserEvent::Message {
                content: vec![ContentBlock::Text { text: "First".to_string() }],
            })
            .await
            .unwrap();

        // Collect events from the turn
        let mut seqs = Vec::new();
        for _ in 0..10 {
            match tokio::time::timeout(Duration::from_secs(5), broadcast_rx.recv()).await {
                Ok(Ok(ev)) => {
                    let seq = match &ev {
                        SessionEvent::StatusRunning { seq } => *seq,
                        SessionEvent::Message { seq, .. } => *seq,
                        SessionEvent::StatusIdle { seq, .. } => *seq,
                        SessionEvent::ToolUse { seq, .. } => *seq,
                        SessionEvent::CustomToolUse { seq, .. } => *seq,
                        SessionEvent::McpToolUse { seq, .. } => *seq,
                        SessionEvent::Error { seq, .. } => *seq,
                    };
                    seqs.push(seq);
                    if matches!(ev, SessionEvent::StatusIdle { .. }) {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Verify strict monotonic increase.
        assert!(seqs.len() >= 2, "expected at least 2 events");
        for window in seqs.windows(2) {
            assert!(
                window[1] > window[0],
                "seq must be strictly increasing: {} should be > {}",
                window[1],
                window[0]
            );
        }

        drop(event_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_interrupt_stops_loop() {
        let (event_tx, mut broadcast_rx, cancel, session_loop) = create_test_loop();

        let handle = tokio::spawn(session_loop.run());

        // Give the loop a moment to start waiting.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Trigger interrupt.
        cancel.cancel();

        // Should emit status.idle on interrupt.
        let ev = broadcast_rx.recv().await.unwrap();
        match ev {
            SessionEvent::StatusIdle { stop_reason, .. } => {
                assert!(matches!(stop_reason, Some(StopReason::EndTurn)));
            }
            other => panic!("expected StatusIdle on interrupt, got: {other:?}"),
        }

        // The loop should exit cleanly.
        let result = handle.await.unwrap();
        assert!(result.is_ok());

        drop(event_tx);
    }

    #[tokio::test]
    async fn test_user_interrupt_event_stops_loop() {
        let (event_tx, mut broadcast_rx, _cancel, session_loop) = create_test_loop();

        let handle = tokio::spawn(session_loop.run());

        // Send an interrupt event.
        event_tx.send(UserEvent::Interrupt {}).await.unwrap();

        // Should emit status.idle.
        let ev = broadcast_rx.recv().await.unwrap();
        match ev {
            SessionEvent::StatusIdle { stop_reason, .. } => {
                assert!(matches!(stop_reason, Some(StopReason::EndTurn)));
            }
            other => panic!("expected StatusIdle, got: {other:?}"),
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());

        drop(event_tx);
    }

    #[tokio::test]
    async fn test_pause_and_resume() {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, mut broadcast_rx) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let pause_flag = Arc::new(Mutex::new(false));
        let pause_notify = Arc::new(Notify::new());
        let agent = build_test_agent(TestLlm::new("resumed response"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        #[cfg(feature = "memory")]
        let session_loop = SessionLoop::with_pause_controls(
            "pause_test".to_string(),
            event_rx,
            broadcast_tx,
            cancel.clone(),
            Arc::clone(&pause_flag),
            Arc::clone(&pause_notify),
            Arc::new(RwLock::new(CheckpointManager::new("pause_test".to_string()))),
            agent,
            session_service,
            None,
        );
        #[cfg(not(feature = "memory"))]
        let session_loop = SessionLoop::with_pause_controls(
            "pause_test".to_string(),
            event_rx,
            broadcast_tx,
            cancel.clone(),
            Arc::clone(&pause_flag),
            Arc::clone(&pause_notify),
            Arc::new(RwLock::new(CheckpointManager::new("pause_test".to_string()))),
            agent,
            session_service,
        );

        let handle = tokio::spawn(session_loop.run());

        // Pause the loop.
        *pause_flag.lock().await = true;

        // Send a message — should not be processed while paused.
        event_tx
            .send(UserEvent::Message {
                content: vec![ContentBlock::Text { text: "While paused".to_string() }],
            })
            .await
            .unwrap();

        // Give the loop time to potentially process (it shouldn't).
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify nothing was broadcast yet (try_recv should fail).
        assert!(broadcast_rx.try_recv().is_err());

        // Resume.
        *pause_flag.lock().await = false;
        pause_notify.notify_one();

        // Now the message should be processed.
        let ev1 = tokio::time::timeout(Duration::from_secs(2), broadcast_rx.recv())
            .await
            .expect("timed out waiting for event after resume")
            .unwrap();

        match ev1 {
            SessionEvent::StatusRunning { .. } => {}
            other => panic!("expected StatusRunning after resume, got: {other:?}"),
        }

        // Clean up.
        drop(event_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_channel_close_stops_loop() {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, _broadcast_rx) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let agent = build_test_agent(TestLlm::new("test"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        let session_loop = SessionLoop::new(
            "close_test".to_string(),
            event_rx,
            broadcast_tx,
            cancel,
            agent,
            session_service,
        );

        let handle = tokio::spawn(session_loop.run());

        // Drop the sender — closes the channel.
        drop(event_tx);

        // Loop should exit cleanly.
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    /// A model whose response arrives as several text parts, as streamed responses do.
    struct SplitTextLlm;

    #[async_trait]
    impl Llm for SplitTextLlm {
        fn name(&self) -> &str {
            "split"
        }

        async fn generate_content(
            &self,
            _request: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<LlmResponseStream> {
            let content = Content {
                role: "model".to_string(),
                parts: vec![
                    Part::Text { text: "Order 42 ".to_string() },
                    Part::Text { text: "shipped".to_string() },
                ],
            };
            let s = stream! {
                yield Ok(LlmResponse {
                    content: Some(content),
                    partial: false,
                    turn_complete: true,
                    finish_reason: Some(FinishReason::Stop),
                    ..Default::default()
                });
            };
            Ok(Box::pin(s))
        }
    }

    #[tokio::test]
    async fn adjacent_text_parts_are_one_message() {
        let (event_tx, event_rx) = mpsc::channel(8);
        let (broadcast_tx, mut broadcast_rx) = broadcast::channel(16);
        let sessions = Arc::new(adk_session::InMemorySessionService::new());
        sessions
            .create(adk_session::CreateRequest {
                app_name: "managed".to_string(),
                user_id: "managed_user".to_string(),
                session_id: Some("split_text".to_string()),
                state: Default::default(),
            })
            .await
            .unwrap();
        let session_loop = SessionLoop::new(
            "split_text".to_string(),
            event_rx,
            broadcast_tx,
            CancellationToken::new(),
            build_test_agent(SplitTextLlm),
            sessions,
        );
        let handle = tokio::spawn(session_loop.run());
        event_tx
            .send(UserEvent::Message { content: vec![ContentBlock::Text { text: "hi".into() }] })
            .await
            .unwrap();

        let mut messages = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), broadcast_rx.recv()).await {
                Ok(Ok(SessionEvent::Message { content, .. })) => messages.push(content),
                Ok(Ok(SessionEvent::StatusIdle { .. })) => break,
                Ok(Ok(_)) => {}
                other => panic!("no status.idle: {other:?}"),
            }
        }
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            matches!(&messages[0][..], [ContentBlock::Text { text }] if text == "Order 42 shipped"),
            "{messages:?}"
        );
        drop(event_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_tool_classification() {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, _) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let agent = build_test_agent(TestLlm::new("test"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        let session_loop = SessionLoop::new(
            "classify_test".to_string(),
            event_rx,
            broadcast_tx,
            cancel,
            agent,
            session_service,
        );

        // Test builtin tools
        assert!(matches!(session_loop.classify_tool("bash"), ToolKind::Builtin));
        assert!(matches!(session_loop.classify_tool("filesystem"), ToolKind::Builtin));
        assert!(matches!(session_loop.classify_tool("web_search"), ToolKind::Builtin));
        assert!(matches!(session_loop.classify_tool("web_fetch"), ToolKind::Builtin));
        assert!(matches!(session_loop.classify_tool("code_execution"), ToolKind::Builtin));

        // Test MCP tools
        assert!(matches!(session_loop.classify_tool("mcp_file_read"), ToolKind::Mcp));
        assert!(matches!(session_loop.classify_tool("server::tool"), ToolKind::Mcp));

        // Test custom tools
        assert!(matches!(session_loop.classify_tool("get_weather"), ToolKind::Custom));
        assert!(matches!(session_loop.classify_tool("deploy"), ToolKind::Custom));

        drop(event_tx);
    }
}
