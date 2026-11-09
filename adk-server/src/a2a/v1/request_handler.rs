//! A2A v1.0.0 request handler — shared dispatch layer.
//!
//! The [`RequestHandler`] maps operation names to executor/store calls and is
//! used by both the JSON-RPC and REST transport handlers. It owns references
//! to the [`V1Executor`], [`TaskStore`], [`PushNotificationSender`], and
//! [`CachedAgentCard`].
//!
//! When a [`RunnerConfig`] is provided, `message_send` and `message_stream`
//! invoke the agent through the ADK Runner for real LLM generation. Without
//! a runner config, they fall back to stub behavior (state transitions only).
//!
//! Operations run on behalf of a caller ([`RequestHandler::for_caller`]). An
//! authenticated caller owns the session and every task it creates; tasks owned
//! by anyone else are reported as not found.
//!
//! [`RunnerConfig`]: adk_runner::RunnerConfig

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use a2a_protocol_types::artifact::{Artifact, ArtifactId};
use a2a_protocol_types::events::{StreamResponse, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a_protocol_types::params::SendMessageConfiguration;
use a2a_protocol_types::responses::TaskListResponse;
use a2a_protocol_types::task::{ContextId, Task, TaskId, TaskState};
use a2a_protocol_types::{AgentCard, Message, TaskPushNotificationConfig};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::card::CachedAgentCard;
use super::convert::internal_task_to_wire;
use super::error::A2aError;
use super::executor::V1Executor;
use super::pending::PendingCalls;
use super::push::PushNotificationSender;
use super::stream::wrap_artifact_event;
use super::task_store::{ListTasksParams, TaskStore, TaskStoreEntry};
use super::text::ResponseText;
use crate::a2a::processor::{Awaiting, awaiting};
use crate::auth_bridge::RequestContext;

/// The state a finished run leaves its task in: waiting on the caller when an event
/// stopped the run for input or credentials, completed otherwise.
fn finished_state(pending: Option<Awaiting>) -> TaskState {
    match pending {
        Some(Awaiting::Auth) => TaskState::AuthRequired,
        Some(Awaiting::Input) => TaskState::InputRequired,
        None => TaskState::Completed,
    }
}

/// Validates an ID string (messageId or taskId).
fn validate_id(id: &str, field_name: &str) -> Result<(), A2aError> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(A2aError::InvalidParams {
            message: format!("{field_name} must not be empty or whitespace-only"),
        });
    }
    if id.len() > 256 {
        return Err(A2aError::InvalidParams {
            message: format!("{field_name} exceeds 256 character limit ({} chars)", id.len()),
        });
    }
    Ok(())
}

/// Validates a message for well-formedness before processing.
fn validate_message(msg: &Message) -> Result<(), A2aError> {
    if msg.parts.is_empty() {
        return Err(A2aError::InvalidParams {
            message: "message must contain at least one part".to_string(),
        });
    }
    validate_id(&msg.id.0, "messageId")?;
    if let Some(ref metadata) = msg.metadata {
        let size = serde_json::to_vec(metadata).map(|v| v.len()).unwrap_or(0);
        if size > 65_536 {
            return Err(A2aError::InvalidParams {
                message: format!("metadata exceeds 64 KB limit ({size} bytes)"),
            });
        }
    }
    Ok(())
}

/// Idempotency key: the owner scopes a `messageId`, so one caller's ID cannot
/// return another caller's task.
type IdempotencyKey = (Option<String>, String);

/// Shared dispatch layer for A2A v1.0.0 operations.
///
/// Maps operation names to executor/store calls. Used by both the JSON-RPC
/// handler and the REST handler.
///
/// When constructed with a [`adk_runner::RunnerConfig`] via [`RequestHandler::with_runner`],
/// `message_send` and `message_stream` invoke the agent through the ADK Runner
/// for real LLM generation. Without a runner config, they perform state
/// transitions only (useful for protocol-level testing).
///
/// The operation methods on `RequestHandler` act for an unauthenticated caller.
/// Use [`for_caller`](Self::for_caller) to act for an authenticated one; the
/// bundled transports do this with the principal from the request extensions.
pub struct RequestHandler {
    executor: Arc<V1Executor>,
    task_store: Arc<dyn TaskStore>,
    #[allow(dead_code)] // Used by push notification delivery in task 7.3
    push_sender: Arc<dyn PushNotificationSender>,
    agent_card: Arc<RwLock<CachedAgentCard>>,
    runner_config: Option<Arc<adk_runner::RunnerConfig>>,
    /// (owner, messageId) → taskId mapping for idempotent request handling.
    idempotency_map: RwLock<HashMap<IdempotencyKey, String>>,
    /// The agent runs in progress, so `tasks_cancel` can stop them.
    running: Arc<RunningTasks>,
}

impl RequestHandler {
    /// Creates a new request handler without a runner (stub mode).
    pub fn new(
        executor: Arc<V1Executor>,
        task_store: Arc<dyn TaskStore>,
        push_sender: Arc<dyn PushNotificationSender>,
        agent_card: Arc<RwLock<CachedAgentCard>>,
    ) -> Self {
        Self {
            executor,
            task_store,
            push_sender,
            agent_card,
            runner_config: None,
            idempotency_map: RwLock::new(HashMap::new()),
            running: Arc::default(),
        }
    }

    /// Creates a new request handler with a runner for real LLM invocation.
    pub fn with_runner(
        executor: Arc<V1Executor>,
        task_store: Arc<dyn TaskStore>,
        push_sender: Arc<dyn PushNotificationSender>,
        agent_card: Arc<RwLock<CachedAgentCard>>,
        runner_config: Arc<adk_runner::RunnerConfig>,
    ) -> Self {
        Self {
            executor,
            task_store,
            push_sender,
            agent_card,
            runner_config: Some(runner_config),
            idempotency_map: RwLock::new(HashMap::new()),
            running: Arc::default(),
        }
    }

    /// Returns a view of this handler that acts on behalf of `caller`.
    ///
    /// With `Some(caller)`, the agent session belongs to `caller.user_id`, every
    /// created task is owned by that user, and operations on tasks owned by anyone
    /// else fail with [`A2aError::TaskNotFound`]. With `None` — no authentication
    /// configured — the session user is `a2a-{contextId}` and only ownerless tasks
    /// are visible.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let task = handler.for_caller(Some(request_context)).message_send(msg).await?;
    /// ```
    pub fn for_caller(&self, caller: Option<RequestContext>) -> CallerScope<'_> {
        CallerScope { handler: self, caller }
    }

    /// Sends a message as an unauthenticated caller; see [`CallerScope::message_send`].
    ///
    /// # Errors
    ///
    /// See [`CallerScope::message_send`].
    pub async fn message_send(&self, msg: Message) -> Result<Task, A2aError> {
        self.for_caller(None).message_send(msg).await
    }

    /// Sends a message with the request's configuration as an unauthenticated caller; see
    /// [`CallerScope::message_send_with`].
    ///
    /// # Errors
    ///
    /// See [`CallerScope::message_send_with`].
    pub async fn message_send_with(
        &self,
        msg: Message,
        configuration: Option<&SendMessageConfiguration>,
    ) -> Result<Task, A2aError> {
        self.for_caller(None).message_send_with(msg, configuration).await
    }

    /// Sends a streaming message as an unauthenticated caller; see
    /// [`CallerScope::message_stream`].
    ///
    /// # Errors
    ///
    /// See [`CallerScope::message_stream`].
    pub async fn message_stream(
        &self,
        msg: Message,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        self.for_caller(None).message_stream(msg).await
    }

    /// Retrieves an ownerless task; see [`CallerScope::tasks_get`].
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or has an owner.
    pub async fn tasks_get(
        &self,
        task_id: &str,
        history_len: Option<u32>,
    ) -> Result<Task, A2aError> {
        self.for_caller(None).tasks_get(task_id, history_len).await
    }

    /// Cancels an ownerless task; see [`CallerScope::tasks_cancel`].
    ///
    /// # Errors
    ///
    /// See [`CallerScope::tasks_cancel`].
    pub async fn tasks_cancel(&self, task_id: &str) -> Result<Task, A2aError> {
        self.for_caller(None).tasks_cancel(task_id).await
    }

    /// Lists ownerless tasks; see [`CallerScope::tasks_list`].
    ///
    /// # Errors
    ///
    /// See [`CallerScope::tasks_list`].
    pub async fn tasks_list(&self, params: ListTasksParams) -> Result<TaskListResponse, A2aError> {
        self.for_caller(None).tasks_list(params).await
    }

    /// Subscribes to an ownerless task; see [`CallerScope::tasks_subscribe`].
    ///
    /// # Errors
    ///
    /// See [`CallerScope::tasks_subscribe`].
    pub async fn tasks_subscribe(
        &self,
        task_id: &str,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        self.for_caller(None).tasks_subscribe(task_id).await
    }

    /// Creates a push notification configuration on an ownerless task; see
    /// [`CallerScope::push_config_create`].
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or has an owner.
    pub async fn push_config_create(
        &self,
        task_id: &str,
        config: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2aError> {
        self.for_caller(None).push_config_create(task_id, config).await
    }

    /// Retrieves a push notification configuration from an ownerless task; see
    /// [`CallerScope::push_config_get`].
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task or config does not exist.
    pub async fn push_config_get(
        &self,
        task_id: &str,
        config_id: &str,
    ) -> Result<TaskPushNotificationConfig, A2aError> {
        self.for_caller(None).push_config_get(task_id, config_id).await
    }

    /// Lists the push notification configurations of an ownerless task; see
    /// [`CallerScope::push_config_list`].
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or has an owner.
    pub async fn push_config_list(
        &self,
        task_id: &str,
    ) -> Result<Vec<TaskPushNotificationConfig>, A2aError> {
        self.for_caller(None).push_config_list(task_id).await
    }

    /// Deletes a push notification configuration from an ownerless task; see
    /// [`CallerScope::push_config_delete`].
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task or config does not exist.
    pub async fn push_config_delete(&self, task_id: &str, config_id: &str) -> Result<(), A2aError> {
        self.for_caller(None).push_config_delete(task_id, config_id).await
    }

    /// Builds the session, converts the inbound message, and starts the agent.
    ///
    /// Both the buffered (`message/send`) and streaming (`message/stream`) paths use this, so
    /// they cannot drift: streaming yields from the same stream the buffered path drains.
    /// Cancelling `cancellation` ends the run, including a model call in flight.
    async fn start_agent(
        runner_config: &Arc<adk_runner::RunnerConfig>,
        caller: Option<&RequestContext>,
        context_id: &str,
        msg: &Message,
        cancellation: CancellationToken,
    ) -> Result<adk_core::EventStream, A2aError> {
        use adk_core::{SessionId, UserId};
        use adk_session::{CreateRequest, GetRequest};

        let app_name = &runner_config.app_name;
        // The context ID is chosen by the client, so it only names the session user when
        // no authentication is configured.
        let user_id = session_user_id(caller, context_id);
        let session_id = context_id.to_string();

        // Only a missing session is created: a read that fails for another reason must not
        // replace the stored conversation. A create that loses a race finds the session.
        let session_service = &runner_config.session_service;
        let existing = session_service
            .get(GetRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await;
        match existing {
            Ok(_) => {}
            Err(error) if error.is_not_found() => {
                let created = session_service
                    .create(CreateRequest {
                        app_name: app_name.clone(),
                        user_id: user_id.clone(),
                        session_id: Some(session_id.clone()),
                        state: std::collections::HashMap::new(),
                    })
                    .await;
                if let Err(error) = created
                    && !error.is_conflict()
                {
                    return Err(A2aError::Internal { message: format!("session create: {error}") });
                }
            }
            Err(error) => {
                return Err(A2aError::Internal { message: format!("session lookup: {error}") });
            }
        }

        // Convert v1 message parts to ADK Content
        let mut adk_parts = Vec::new();
        for part in &msg.parts {
            let adk_part = super::convert::wire_part_to_adk(part)?;
            adk_parts.push(adk_part);
        }
        let content = adk_core::Content { role: "user".to_string(), parts: adk_parts };

        // Create runner and execute
        let mut runner_builder = adk_runner::Runner::builder()
            .app_name(runner_config.app_name.clone())
            .agent(runner_config.agent.clone())
            .session_service(runner_config.session_service.clone());
        if let Some(control) = &runner_config.governance {
            runner_builder = runner_builder.governance(control.clone());
        }
        if let Some(policy) = &runner_config.tool_policy {
            runner_builder = runner_builder.tool_policy(policy.clone());
        }
        if let Some(ref artifact_service) = runner_config.artifact_service {
            runner_builder = runner_builder.artifact_service(artifact_service.clone());
        }
        if let Some(ref memory_service) = runner_config.memory_service {
            runner_builder = runner_builder.memory_service(memory_service.clone());
        }
        if let Some(ref plugin_manager) = runner_config.plugin_manager {
            runner_builder = runner_builder.plugin_manager(plugin_manager.clone());
        }
        if let Some(ref run_config) = runner_config.run_config {
            runner_builder = runner_builder.run_config(run_config.clone());
        }
        if let Some(ref compaction_config) = runner_config.compaction_config {
            runner_builder = runner_builder.compaction_config(compaction_config.clone());
        }
        if let Some(ref context_cache_config) = runner_config.context_cache_config {
            runner_builder = runner_builder.context_cache_config(context_cache_config.clone());
        }
        if let Some(ref cache_capable) = runner_config.cache_capable {
            runner_builder = runner_builder.cache_capable(cache_capable.clone());
        }
        if let Some(ref intra_compaction_config) = runner_config.intra_compaction_config {
            runner_builder =
                runner_builder.intra_compaction_config(intra_compaction_config.clone());
        }
        if let Some(ref intra_compaction_summarizer) = runner_config.intra_compaction_summarizer {
            runner_builder =
                runner_builder.intra_compaction_summarizer(intra_compaction_summarizer.clone());
        }
        // The caller's own context wins over a context baked into the shared config.
        if let Some(request_context) =
            caller.cloned().or_else(|| runner_config.request_context.clone())
        {
            runner_builder = runner_builder.request_context(request_context);
        }
        // A child of the configuration's own token (see `RunningTasks::start`), so that token
        // still stops the run.
        runner_builder = runner_builder.cancellation_token(cancellation);
        let runner = runner_builder
            .build()
            .map_err(|e| A2aError::Internal { message: format!("runner create: {e}") })?;

        runner
            .run(
                UserId::new(&user_id).map_err(|e| A2aError::Internal { message: e.to_string() })?,
                SessionId::new(&session_id)
                    .map_err(|e| A2aError::Internal { message: e.to_string() })?,
                content,
            )
            .await
            .map_err(|e| A2aError::Internal { message: format!("runner run: {e}") })
    }

    /// Returns the extended agent card.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::ExtendedAgentCardNotConfigured`] when the agent card says the
    /// agent offers none (`capabilities.extendedAgentCard: false`).
    pub async fn agent_card_extended(&self) -> Result<AgentCard, A2aError> {
        let cached = self.agent_card.read().await;
        if cached.card.capabilities.extended_agent_card == Some(false) {
            return Err(A2aError::ExtendedAgentCardNotConfigured);
        }
        Ok(cached.card.clone())
    }

    /// Refuses `operation` when the agent card says the agent does not offer it.
    ///
    /// A capability the card leaves unstated is not checked, so a hand-assembled card that
    /// states none keeps serving everything.
    /// [`A2aV1Server`](super::server::A2aV1Server) states every capability.
    async fn require(&self, operation: Capability) -> Result<(), A2aError> {
        let capabilities = self.agent_card.read().await.card.capabilities.clone();
        match operation {
            Capability::Streaming(method) if capabilities.streaming == Some(false) => {
                Err(A2aError::UnsupportedOperation {
                    method: format!("{method}: the agent card does not declare streaming"),
                })
            }
            Capability::PushNotifications if capabilities.push_notifications == Some(false) => {
                Err(A2aError::PushNotificationNotSupported)
            }
            _ => Ok(()),
        }
    }

    /// Returns a reference to the underlying executor.
    pub fn executor(&self) -> &Arc<V1Executor> {
        &self.executor
    }

    /// Returns a reference to the underlying task store.
    pub fn task_store(&self) -> &Arc<dyn TaskStore> {
        &self.task_store
    }
}

/// A [`RequestHandler`] acting on behalf of one caller.
///
/// Created by [`RequestHandler::for_caller`]. Every operation is scoped to the
/// caller: tasks it creates are owned by the caller's `user_id`, and a task owned
/// by anyone else is reported as [`A2aError::TaskNotFound`] — the same answer as
/// for a task that does not exist, so task IDs cannot be probed.
pub struct CallerScope<'a> {
    handler: &'a RequestHandler,
    caller: Option<RequestContext>,
}

impl CallerScope<'_> {
    /// The authenticated user this scope acts for, or `None` without authentication.
    pub fn owner(&self) -> Option<&str> {
        self.caller.as_ref().map(|context| context.user_id.as_str())
    }

    /// Loads a task, treating one owned by another caller as missing.
    async fn owned_task(&self, task_id: &str) -> Result<TaskStoreEntry, A2aError> {
        let entry = self.handler.task_store.get_task(task_id).await?;
        if entry.owner() == self.owner() {
            Ok(entry)
        } else {
            Err(A2aError::TaskNotFound { task_id: task_id.to_string() })
        }
    }

    fn idempotency_key(&self, message_id: &str) -> IdempotencyKey {
        (self.owner().map(str::to_string), message_id.to_string())
    }

    /// Sends a message and waits for the task to finish or to stop for the caller.
    ///
    /// The same as [`message_send_with`](Self::message_send_with) without a configuration.
    ///
    /// # Errors
    ///
    /// See [`message_send_with`](Self::message_send_with).
    pub async fn message_send(&self, msg: Message) -> Result<Task, A2aError> {
        self.message_send_with(msg, None).await
    }

    /// Sends a message, creating a task or continuing one, and processes it through the
    /// executor.
    ///
    /// When a runner config is present, invokes the agent through the ADK
    /// Runner for real LLM generation. The LLM response is recorded as an
    /// artifact on the task. Without a runner, performs state transitions only.
    ///
    /// The message continues a task (A2A §3.4):
    /// - a message with a `taskId` continues that task, which must be one of this
    ///   caller's and must be waiting on the caller (`INPUT_REQUIRED` or
    ///   `AUTH_REQUIRED`);
    /// - a message without one, whose `contextId` matches one of this caller's
    ///   `INPUT_REQUIRED` tasks, continues that task. Another caller's task with the
    ///   same `contextId` is never continued.
    ///
    /// A task that stops for the caller names what it waits for in its status message:
    /// each pending function call and its interim result, as data parts tagged with
    /// [`ADK_TYPE_METADATA_KEY`](super::convert::ADK_TYPE_METADATA_KEY). The caller
    /// answers with a `function_response` data part for the call's `id`.
    ///
    /// With `returnImmediately` set in `configuration`, the call returns as soon as the
    /// task is `WORKING`, and the agent runs on in the background (A2A §3.2.2). Otherwise
    /// it returns once the task finishes or stops for the caller. Either way,
    /// [`tasks_cancel`](Self::tasks_cancel) stops the run.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] for a `taskId` that names no task of this
    /// caller's, [`A2aError::InvalidParams`] when the message's `contextId` is not that
    /// task's, [`A2aError::UnsupportedOperation`] when that task has ended or is still
    /// running, and an error if task creation, state transitions, or store operations
    /// fail. A failed run is not an error: the task is returned `FAILED`, with the error
    /// as its status message.
    pub async fn message_send_with(
        &self,
        msg: Message,
        configuration: Option<&SendMessageConfiguration>,
    ) -> Result<Task, A2aError> {
        validate_message(&msg)?;
        let handler = self.handler;
        if configuration.is_some_and(|c| c.task_push_notification_config.is_some()) {
            handler.require(Capability::PushNotifications).await?;
        }

        // Idempotency check
        let idempotency_key = self.idempotency_key(&msg.id.0);
        {
            let map = handler.idempotency_map.read().await;
            if let Some(existing_task_id) = map.get(&idempotency_key) {
                // Try to return the existing task
                match self.tasks_get(existing_task_id, None).await {
                    Ok(task) => return Ok(task),
                    Err(A2aError::TaskNotFound { .. }) => {
                        // Stale entry — will be removed below and processed as new
                    }
                    Err(e) => return Err(e),
                }
                // If we get here, the entry was stale — remove it
                drop(map);
                handler.idempotency_map.write().await.remove(&idempotency_key);
            }
        }

        let (task_id, context_id) = self.prepare_task(&msg).await?;
        handler.executor.transition_state(&task_id, &context_id, TaskState::Working, None).await?;

        if let Some(runner_config) = &handler.runner_config {
            let guard = handler.running.start(&task_id, runner_config.cancellation_token.as_ref());
            let run = RunContext {
                executor: handler.executor.clone(),
                runner_config: runner_config.clone(),
                caller: self.caller.clone(),
            };
            let drive = drive(run, task_id.clone(), context_id.clone(), msg, guard);
            if configuration.and_then(|c| c.return_immediately).unwrap_or(false) {
                let background_task_id = task_id.clone();
                tokio::spawn(async move {
                    if let Err(error) = drive.await {
                        tracing::warn!(
                            task_id = %background_task_id,
                            %error,
                            "could not record the end of a background A2A run"
                        );
                    }
                });
            } else {
                drive.await?;
            }
        } else {
            handler
                .executor
                .transition_state(&task_id, &context_id, TaskState::Completed, None)
                .await?;
        }

        // Record idempotency mapping
        handler.idempotency_map.write().await.insert(idempotency_key, task_id.clone());

        let entry = handler.task_store.get_task(&task_id).await?;
        internal_task_to_wire(&entry)
    }

    /// The task `msg` continues, if any (see [`message_send_with`](Self::message_send_with)).
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] for a `taskId` that names no task of this
    /// caller's, [`A2aError::InvalidParams`] when the message's `contextId` is not that
    /// task's, and [`A2aError::UnsupportedOperation`] when that task has ended or is still
    /// running.
    async fn task_to_continue(&self, msg: &Message) -> Result<Option<TaskStoreEntry>, A2aError> {
        if let Some(task_id) = &msg.task_id {
            validate_id(&task_id.0, "taskId")?;
            let entry = self.owned_task(&task_id.0).await?;
            if let Some(context_id) = &msg.context_id
                && context_id.0 != entry.context_id
            {
                return Err(A2aError::InvalidParams {
                    message: format!(
                        "contextId {} is not the context of task {}",
                        context_id.0, entry.id
                    ),
                });
            }
            return match entry.status.state {
                TaskState::InputRequired | TaskState::AuthRequired => Ok(Some(entry)),
                state if is_terminal_state(state) => Err(A2aError::UnsupportedOperation {
                    method: format!("a message to task {} in terminal state {state:?}", entry.id),
                }),
                state => Err(A2aError::UnsupportedOperation {
                    method: format!("a message to task {} while it is {state:?}", entry.id),
                }),
            };
        }

        if let Some(ctx_id) = &msg.context_id
            && let Some(existing) = self.handler.task_store.find_task_by_context(&ctx_id.0).await?
            && existing.status.state == TaskState::InputRequired
            && existing.owner() == self.owner()
        {
            return Ok(Some(existing));
        }
        Ok(None)
    }

    /// Finds or creates the task `msg` runs on and adds `msg` to its history. Returns the
    /// task's ID and context ID. The task keeps its state: a new task is `SUBMITTED`.
    ///
    /// # Errors
    ///
    /// See [`task_to_continue`](Self::task_to_continue); also returns an error if task
    /// creation or the store fails.
    async fn prepare_task(&self, msg: &Message) -> Result<(String, String), A2aError> {
        let handler = self.handler;
        let (task_id, context_id) = match self.task_to_continue(msg).await? {
            Some(existing) => (existing.id, existing.context_id),
            None => {
                let task_id = uuid::Uuid::new_v4().to_string();
                let context_id = msg
                    .context_id
                    .as_ref()
                    .map(|c| c.0.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let metadata = handler
                    .runner_config
                    .as_ref()
                    .map(|config| adk_task_metadata(config, self.caller.as_ref(), &context_id))
                    .unwrap_or_default();
                handler
                    .executor
                    .create_task_with_metadata(&task_id, &context_id, self.owner(), metadata)
                    .await?;
                (task_id, context_id)
            }
        };
        handler.task_store.add_history_message(&task_id, msg.clone()).await?;
        Ok((task_id, context_id))
    }

    /// Sends a streaming message, returning a stream of SSE events.
    ///
    /// Drives the agent and translates its events as they arrive:
    ///
    /// | Agent event | A2A event |
    /// |-------------|-----------|
    /// | first, before any output | `Task`, then `TaskStatusUpdateEvent` — `Working` |
    /// | content with `partial = true` | `TaskArtifactUpdateEvent` — `append`, not last chunk |
    /// | content with `partial = false` | `TaskArtifactUpdateEvent` — final chunk |
    /// | stream ends after a held or long-running call | `TaskStatusUpdateEvent` — `InputRequired`, with the pending calls as its message |
    /// | stream ends after a credential request | `TaskStatusUpdateEvent` — `AuthRequired` |
    /// | stream ends | `TaskStatusUpdateEvent` — `Completed` |
    /// | stream errors | `TaskStatusUpdateEvent` — `Failed`, with the error as its message |
    /// | [`tasks_cancel`](Self::tasks_cancel) stops the run | `TaskStatusUpdateEvent` — `Canceled` |
    ///
    /// Chunks of one response share an artifact ID so a client can reassemble them, which is the
    /// same contract adk-python and adk-go implement over their A2A SDKs.
    ///
    /// The message continues a task as in [`message_send_with`](Self::message_send_with); the
    /// stream then starts with that task.
    ///
    /// With no runner configured the task is created and completed without agent output, which
    /// keeps discovery-only deployments working.
    ///
    /// # Errors
    ///
    /// Returns an error if the message cannot continue the task it names (see
    /// [`message_send_with`](Self::message_send_with)) or task creation fails. Failures
    /// once streaming has begun are reported
    /// as a `Failed` status event whose message carries the error text, since the response has
    /// already started. That status is the last item: the stream ends at a terminal state
    /// (A2A §3.1.2), so no error item follows it. An `Err` item appears only when the task could
    /// not be moved to a terminal state, and it is then the last item.
    pub async fn message_stream(
        &self,
        msg: Message,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        self.handler.require(Capability::Streaming("SendStreamingMessage")).await?;
        validate_message(&msg)?;
        let handler = self.handler;

        // Idempotency check — return existing task as single-element stream
        let idempotency_key = self.idempotency_key(&msg.id.0);
        {
            let map = handler.idempotency_map.read().await;
            if let Some(existing_task_id) = map.get(&idempotency_key) {
                match self.owned_task(existing_task_id).await {
                    Ok(entry) => {
                        let task = internal_task_to_wire(&entry)?;
                        let stream =
                            futures::stream::once(async move { Ok(StreamResponse::Task(task)) });
                        return Ok(stream.boxed());
                    }
                    Err(A2aError::TaskNotFound { .. }) => {
                        // Stale entry — remove and process as new
                        drop(map);
                        handler.idempotency_map.write().await.remove(&idempotency_key);
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        let (task_id, context_id) = self.prepare_task(&msg).await?;

        // Record idempotency mapping
        handler.idempotency_map.write().await.insert(idempotency_key, task_id.clone());

        // Get the task entry for the first SSE event
        let task_entry = handler.task_store.get_task(&task_id).await?;
        let first_task = internal_task_to_wire(&task_entry)?;
        // Each update carries the task's metadata, as the executor's do.
        let update_metadata = first_task.metadata.clone();

        let executor = handler.executor.clone();
        let tid = task_id.clone();
        let cid = context_id.clone();
        let runner_config = handler.runner_config.clone();
        let caller = self.caller.clone();
        let stream_msg = msg;
        // Registered before the stream is returned, so a cancel that races the first poll
        // still stops the run. The stream owns it: the run stays cancelable until it ends.
        let guard = runner_config
            .as_ref()
            .map(|config| handler.running.start(&task_id, config.cancellation_token.as_ref()));

        let stream = async_stream::stream! {
            let guard = guard;
            yield Ok(StreamResponse::Task(first_task));
            let mut awaiting_caller = None;
            let mut pending = PendingCalls::default();

            match executor.transition_state(&tid, &cid, TaskState::Working, None).await {
                Ok(event) => yield Ok(StreamResponse::StatusUpdate(event)),
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }

            if let (Some(config), Some(guard)) = (runner_config, guard.as_ref()) {
                let cancellation = guard.token().clone();
                let started = RequestHandler::start_agent(
                    &config,
                    caller.as_ref(),
                    &cid,
                    &stream_msg,
                    cancellation.clone(),
                )
                .await;
                let mut event_stream = match started {
                    Ok(stream) => stream,
                    Err(e) => {
                        // The agent never started, so nothing partial was emitted. Report the
                        // task as failed rather than completing a task that did no work.
                        yield fail_stream(&executor, &tid, &cid, e).await;
                        return;
                    }
                };

                // One artifact ID for the whole response so a client can join the chunks.
                let artifact_id = uuid::Uuid::new_v4().to_string();
                let mut chunks_sent = false;
                let mut full_text = ResponseText::default();
                let mut media = Vec::new();

                while let Some(result) = event_stream.next().await {
                    match result {
                        Ok(event) => {
                            awaiting_caller = awaiting_caller.max(awaiting(&event));
                            pending.observe(&event);
                            media.extend(media_parts(&event));
                            let Some((text, append)) = full_text.push(&event) else { continue };

                            let partial = event.llm_response.partial;
                            let artifact = Artifact::new(
                                ArtifactId::new(artifact_id.clone()),
                                vec![a2a_protocol_types::Part::text(&text)],
                            );
                            let update = wrap_artifact_event(TaskArtifactUpdateEvent {
                                task_id: a2a_protocol_types::TaskId::new(tid.clone()),
                                context_id: a2a_protocol_types::ContextId::new(cid.clone()),
                                artifact,
                                append: Some(chunks_sent && append),
                                last_chunk: Some(!partial),
                                metadata: update_metadata.clone(),
                            });
                            executor.publish(&tid, update.clone());
                            yield Ok(update);
                            chunks_sent = true;
                        }
                        Err(e) => {
                            let error = A2aError::Internal { message: format!("agent error: {e}") };
                            yield fail_stream(&executor, &tid, &cid, error).await;
                            return;
                        }
                    }
                }

                // A cancelled run ends without its partial output, on the `Canceled` status.
                if cancellation.is_cancelled() {
                    yield settle(&executor, &tid, &cid, TaskState::Canceled, Vec::new())
                        .await
                        .map(StreamResponse::StatusUpdate);
                    return;
                }

                // Persist the joined text so `tasks/get` returns the same artifact a
                // non-streaming caller would have received.
                let full_text = full_text.text();
                if !full_text.is_empty() {
                    let artifact = Artifact::new(
                        ArtifactId::new(artifact_id),
                        vec![a2a_protocol_types::Part::text(&full_text)],
                    );
                    if let Err(e) = executor.persist_artifact(&tid, &cid, artifact).await {
                        yield fail_stream(&executor, &tid, &cid, e).await;
                        return;
                    }
                }

                // Files and inline data, as one more artifact.
                if !media.is_empty() {
                    let artifact =
                        Artifact::new(ArtifactId::new(uuid::Uuid::new_v4().to_string()), media);
                    match executor.record_artifact(&tid, &cid, artifact).await {
                        Ok(mut event) => {
                            event.last_chunk = Some(true);
                            yield Ok(wrap_artifact_event(event));
                        }
                        Err(e) => {
                            yield fail_stream(&executor, &tid, &cid, e).await;
                            return;
                        }
                    }
                }
            }

            let state = finished_state(awaiting_caller);
            match settle(&executor, &tid, &cid, state, pending.into_parts()).await {
                Ok(event) => yield Ok(StreamResponse::StatusUpdate(event)),
                Err(e) => yield Err(e),
            }
            drop(guard);
        };

        Ok(stream.boxed())
    }

    /// Retrieves one of this caller's tasks by ID.
    ///
    /// Optionally limits the number of history messages returned.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or belongs to
    /// another caller.
    pub async fn tasks_get(
        &self,
        task_id: &str,
        history_len: Option<u32>,
    ) -> Result<Task, A2aError> {
        validate_id(task_id, "taskId")?;
        let mut entry = self.owned_task(task_id).await?;

        // Truncate history if requested
        if let Some(len) = history_len {
            let len = len as usize;
            if entry.history.len() > len {
                let start = entry.history.len() - len;
                entry.history = entry.history[start..].to_vec();
            }
        }

        internal_task_to_wire(&entry)
    }

    /// Cancels one of this caller's tasks: stops its agent run, if one is in progress,
    /// and moves it to `CANCELED`. The run ends without recording its partial output.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or belongs to
    /// another caller, or [`A2aError::TaskNotCancelable`] if the task is in a
    /// terminal state, including one its run reached while it was being cancelled.
    pub async fn tasks_cancel(&self, task_id: &str) -> Result<Task, A2aError> {
        validate_id(task_id, "taskId")?;
        let entry = self.owned_task(task_id).await?;
        let not_cancelable = |state: TaskState| A2aError::TaskNotCancelable {
            task_id: task_id.to_string(),
            current_state: format!("{state:?}"),
        };

        if is_terminal_state(entry.status.state) {
            return Err(not_cancelable(entry.status.state));
        }

        // Stop the run first. A run that ends in the meantime has already moved the task to
        // its final state, and the transition below then reports it as not cancelable.
        self.handler.running.cancel(task_id);
        match self
            .handler
            .executor
            .transition_state(task_id, &entry.context_id, TaskState::Canceled, None)
            .await
        {
            Ok(_) => {}
            // The cancelled run may have recorded the cancellation first.
            Err(A2aError::InvalidStateTransition { .. }) => {
                let current = self.handler.task_store.get_task(task_id).await?;
                if current.status.state != TaskState::Canceled {
                    return Err(not_cancelable(current.status.state));
                }
            }
            Err(error) => return Err(error),
        }

        let updated = self.handler.task_store.get_task(task_id).await?;
        internal_task_to_wire(&updated)
    }

    /// Lists this caller's tasks, one page at a time (A2A §3.1.4).
    ///
    /// - **Filters:** `context_id`, `state`, and `status_timestamp_after` (RFC 3339; tasks
    ///   updated at or after it).
    /// - **Order:** most recently updated first.
    /// - **Page size:** `page_size` tasks per page, 50 when unset, between 1 and 100.
    ///   `next_page_token` continues the list, and is empty on the last page.
    /// - **Task contents:** each task keeps its last `history_length` messages (all when
    ///   unset). It keeps its artifacts only with `include_artifacts`.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::InvalidParams`] for a page size out of range, a malformed
    /// timestamp or page token, and an error if the task store fails.
    pub async fn tasks_list(&self, params: ListTasksParams) -> Result<TaskListResponse, A2aError> {
        let page_size = match params.page_size {
            None => DEFAULT_PAGE_SIZE,
            Some(size @ 1..=MAX_PAGE_SIZE) => size,
            Some(size) => {
                return Err(A2aError::InvalidParams {
                    message: format!("pageSize must be between 1 and {MAX_PAGE_SIZE}, got {size}"),
                });
            }
        };
        let updated_after = params
            .status_timestamp_after
            .as_deref()
            .map(|timestamp| {
                chrono::DateTime::parse_from_rfc3339(timestamp)
                    .map(|time| time.with_timezone(&chrono::Utc))
                    .map_err(|e| A2aError::InvalidParams {
                        message: format!("statusTimestampAfter {timestamp:?}: {e}"),
                    })
            })
            .transpose()?;
        let after = params
            .page_token
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(PageCursor::decode)
            .transpose()?;
        let history_length = params.history_length;
        let include_artifacts = params.include_artifacts.unwrap_or(false);

        let unpaged = ListTasksParams { page_size: None, page_token: None, ..params };
        let mut entries: Vec<TaskStoreEntry> = self
            .handler
            .task_store
            .list_tasks(unpaged)
            .await?
            .into_iter()
            .filter(|entry| entry.owner() == self.owner())
            .filter(|entry| updated_after.is_none_or(|after| entry.updated_at >= after))
            .collect();
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.id.cmp(&b.id)));

        let total_size = entries.len();
        let start = after.map_or(0, |cursor| {
            entries.iter().position(|entry| cursor.precedes(entry)).unwrap_or(entries.len())
        });
        let end = (start + page_size as usize).min(entries.len());
        let next_page_token = if end < entries.len() {
            PageCursor::after(&entries[end - 1]).encode()
        } else {
            String::new()
        };

        let tasks = entries[start..end]
            .iter()
            .cloned()
            .map(|mut entry| {
                if let Some(len) = history_length {
                    let keep = entry.history.len().saturating_sub(len as usize);
                    entry.history.drain(..keep);
                }
                if !include_artifacts {
                    entry.artifacts.clear();
                }
                internal_task_to_wire(&entry)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TaskListResponse {
            tasks,
            next_page_token,
            page_size,
            total_size: u32::try_from(total_size).unwrap_or(u32::MAX),
        })
    }

    /// Subscribes to one of this caller's tasks (A2A §3.1.6): the stream starts with the
    /// task as it is now, then carries its updates as they happen, whichever request runs
    /// it. A send with `returnImmediately`, a stream another client opened, or a resumed
    /// task all qualify.
    ///
    /// The stream ends when the task reaches a terminal state or stops for its caller
    /// (`INPUT_REQUIRED`, `AUTH_REQUIRED`). When no run of the task is in progress, nothing
    /// will change it until a message continues it, so the stream holds the task and its
    /// status and ends.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or belongs to
    /// another caller, or [`A2aError::UnsupportedOperation`] if the task already reached a
    /// terminal state.
    pub async fn tasks_subscribe(
        &self,
        task_id: &str,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        self.handler.require(Capability::Streaming("SubscribeToTask")).await?;
        let entry = self.owned_task(task_id).await?;
        if is_terminal_state(entry.status.state) {
            return Err(A2aError::UnsupportedOperation {
                method: format!(
                    "subscribing to task {task_id} in terminal state {:?}",
                    entry.status.state
                ),
            });
        }

        if !self.handler.running.contains(task_id) {
            let task = internal_task_to_wire(&entry)?;
            let status_event = TaskStatusUpdateEvent {
                task_id: TaskId::new(task_id),
                context_id: ContextId::new(&entry.context_id),
                status: entry.status.clone(),
                metadata: None,
            };
            let stream = futures::stream::iter(vec![
                Ok(StreamResponse::Task(task)),
                Ok(StreamResponse::StatusUpdate(status_event)),
            ]);
            return Ok(stream.boxed());
        }

        // Subscribe before reading the task, so no update falls between the snapshot and the
        // first live one. One that arrives twice only repeats a state the client has.
        let mut updates = self.handler.executor.subscribe(task_id);
        let snapshot = self.owned_task(task_id).await?;
        let task = internal_task_to_wire(&snapshot)?;
        let settled = ends_subscription(snapshot.status.state);

        let stream = async_stream::stream! {
            yield Ok(StreamResponse::Task(task));
            if settled {
                return;
            }
            loop {
                match updates.recv().await {
                    Ok(update) => {
                        let ends = matches!(
                            &update,
                            StreamResponse::StatusUpdate(event)
                                if ends_subscription(event.status.state)
                        );
                        yield Ok(update);
                        if ends {
                            return;
                        }
                    }
                    // A subscriber that fell behind misses updates, not the end.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        };
        Ok(stream.boxed())
    }

    /// Creates a push notification configuration for one of this caller's tasks.
    ///
    /// Assigns a server-generated config ID and stores the config on the task.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or belongs to
    /// another caller.
    pub async fn push_config_create(
        &self,
        task_id: &str,
        mut config: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2aError> {
        self.handler.require(Capability::PushNotifications).await?;
        // Verify task exists
        let mut entry = self.owned_task(task_id).await?;

        // Assign a server-generated config ID if not present
        if config.id.is_none() {
            config.id = Some(uuid::Uuid::new_v4().to_string());
        }
        config.task_id = Some(task_id.to_string());

        // Add to the task's push configs
        entry.push_configs.push(config.clone());
        entry.updated_at = chrono::Utc::now();

        // Re-persist the task with updated push configs
        // (We delete and re-create since TaskStore doesn't have an update_push_configs method)
        self.handler.task_store.delete_task(task_id).await?;
        self.handler.task_store.create_task(entry).await?;

        Ok(config)
    }

    /// Retrieves a push notification configuration by task ID and config ID.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task or config does not exist, or
    /// the task belongs to another caller.
    pub async fn push_config_get(
        &self,
        task_id: &str,
        config_id: &str,
    ) -> Result<TaskPushNotificationConfig, A2aError> {
        self.handler.require(Capability::PushNotifications).await?;
        let entry = self.owned_task(task_id).await?;

        entry.push_configs.iter().find(|c| c.id.as_deref() == Some(config_id)).cloned().ok_or_else(
            || A2aError::TaskNotFound {
                task_id: format!("push config {config_id} on task {task_id}"),
            },
        )
    }

    /// Lists all push notification configurations for one of this caller's tasks.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or belongs to
    /// another caller.
    pub async fn push_config_list(
        &self,
        task_id: &str,
    ) -> Result<Vec<TaskPushNotificationConfig>, A2aError> {
        self.handler.require(Capability::PushNotifications).await?;
        let entry = self.owned_task(task_id).await?;
        Ok(entry.push_configs)
    }

    /// Deletes a push notification configuration.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task or config does not exist, or
    /// the task belongs to another caller.
    pub async fn push_config_delete(&self, task_id: &str, config_id: &str) -> Result<(), A2aError> {
        self.handler.require(Capability::PushNotifications).await?;
        let mut entry = self.owned_task(task_id).await?;

        let original_len = entry.push_configs.len();
        entry.push_configs.retain(|c| c.id.as_deref() != Some(config_id));

        if entry.push_configs.len() == original_len {
            return Err(A2aError::TaskNotFound {
                task_id: format!("push config {config_id} on task {task_id}"),
            });
        }

        entry.updated_at = chrono::Utc::now();

        // Re-persist
        self.handler.task_store.delete_task(task_id).await?;
        self.handler.task_store.create_task(entry).await?;

        Ok(())
    }
}

/// The files and inline data a finished model turn produced, as A2A parts.
fn media_parts(event: &adk_core::Event) -> Vec<a2a_protocol_types::Part> {
    if event.llm_response.partial {
        return Vec::new();
    }
    event
        .content()
        .into_iter()
        .filter(|content| content.role == "model")
        .flat_map(|content| &content.parts)
        .filter_map(super::convert::media_part_to_wire)
        .collect()
}

/// The ADK user whose session runs a task in `context_id`: the authenticated caller, or
/// `a2a-{context_id}` without authentication.
fn session_user_id(caller: Option<&RequestContext>, context_id: &str) -> String {
    caller.map_or_else(|| format!("a2a-{context_id}"), |context| context.user_id.clone())
}

/// The metadata of a task the agent of `runner_config` runs in `context_id`. It names the
/// ADK app, user and session (`adk_app_name`, `adk_user_id`, `adk_session_id`), and marks
/// the task with the ADK extension, as adk-python and adk-go do.
fn adk_task_metadata(
    runner_config: &adk_runner::RunnerConfig,
    caller: Option<&RequestContext>,
    context_id: &str,
) -> HashMap<String, serde_json::Value> {
    use super::convert::{
        ADK_APP_NAME_METADATA_KEY, ADK_EXTENSION_URI, ADK_SESSION_ID_METADATA_KEY,
        ADK_USER_ID_METADATA_KEY,
    };
    HashMap::from([
        (ADK_APP_NAME_METADATA_KEY.to_string(), runner_config.app_name.clone().into()),
        (ADK_USER_ID_METADATA_KEY.to_string(), session_user_id(caller, context_id).into()),
        (ADK_SESSION_ID_METADATA_KEY.to_string(), context_id.into()),
        (ADK_EXTENSION_URI.to_string(), true.into()),
    ])
}

/// An optional A2A operation the agent card can say the agent does not offer.
enum Capability {
    /// `SendStreamingMessage` or `SubscribeToTask`, named for the error.
    Streaming(&'static str),
    /// Push notification configuration.
    PushNotifications,
}

/// `ListTasks` page size when the request sets none (A2A §3.1.4).
const DEFAULT_PAGE_SIZE: u32 = 50;

/// The largest `ListTasks` page size a request may ask for (A2A §3.1.4).
const MAX_PAGE_SIZE: u32 = 100;

/// Where a `ListTasks` page ended: the last task listed, in the list's order (most recently
/// updated first, then by ID). Its page token is opaque to the client.
struct PageCursor {
    updated_at: chrono::DateTime<chrono::Utc>,
    id: String,
}

impl PageCursor {
    fn after(entry: &TaskStoreEntry) -> Self {
        Self { updated_at: entry.updated_at, id: entry.id.clone() }
    }

    /// Whether `entry` comes after this cursor in the list's order.
    fn precedes(&self, entry: &TaskStoreEntry) -> bool {
        entry.updated_at < self.updated_at
            || (entry.updated_at == self.updated_at && entry.id > self.id)
    }

    fn encode(&self) -> String {
        use base64::Engine as _;
        let raw = format!("{}|{}", self.updated_at.to_rfc3339(), self.id);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    }

    fn decode(token: &str) -> Result<Self, A2aError> {
        use base64::Engine as _;
        let invalid =
            || A2aError::InvalidParams { message: format!("invalid pageToken {token:?}") };
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(invalid)?;
        let (time, id) = raw.split_once('|').ok_or_else(invalid)?;
        let updated_at = chrono::DateTime::parse_from_rfc3339(time)
            .map_err(|_| invalid())?
            .with_timezone(&chrono::Utc);
        Ok(Self { updated_at, id: id.to_string() })
    }
}

/// The agent runs in progress, by task ID, so cancelling a task stops its run.
#[derive(Default)]
struct RunningTasks {
    next_run: AtomicU64,
    runs: Mutex<HashMap<String, (u64, CancellationToken)>>,
}

impl RunningTasks {
    /// Registers a run of `task_id`. Its token is a child of `parent`, the runner
    /// configuration's own token, which therefore still stops it. The run stays registered
    /// until the returned guard is dropped.
    fn start(self: &Arc<Self>, task_id: &str, parent: Option<&CancellationToken>) -> RunGuard {
        let token = parent.map_or_else(CancellationToken::new, CancellationToken::child_token);
        let run = self.next_run.fetch_add(1, Ordering::Relaxed);
        self.runs().insert(task_id.to_string(), (run, token.clone()));
        RunGuard { running: Arc::clone(self), task_id: task_id.to_string(), run, token }
    }

    /// Whether a run of `task_id` is in progress.
    fn contains(&self, task_id: &str) -> bool {
        self.runs().contains_key(task_id)
    }

    /// Cancels the run of `task_id`, if one is in progress.
    fn cancel(&self, task_id: &str) {
        if let Some((_, token)) = self.runs().get(task_id) {
            token.cancel();
        }
    }

    fn runs(&self) -> MutexGuard<'_, HashMap<String, (u64, CancellationToken)>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A registered run. Dropping it unregisters the run, unless a later run of the same task,
/// one that resumed it, has taken its place.
struct RunGuard {
    running: Arc<RunningTasks>,
    task_id: String,
    run: u64,
    token: CancellationToken,
}

impl RunGuard {
    /// The token that stops this run.
    fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let mut runs = self.running.runs();
        if runs.get(&self.task_id).is_some_and(|(run, _)| *run == self.run) {
            runs.remove(&self.task_id);
        }
    }
}

/// What a run needs, owned, so a run started with `returnImmediately` can outlive the
/// request that started it.
struct RunContext {
    executor: Arc<V1Executor>,
    runner_config: Arc<adk_runner::RunnerConfig>,
    caller: Option<RequestContext>,
}

/// What a finished run produced.
struct Finished {
    /// The response text, recorded as one artifact.
    text: String,
    /// Files and inline data the agent produced, recorded as a second artifact.
    media: Vec<a2a_protocol_types::Part>,
    /// The state the run leaves the task in.
    state: TaskState,
    /// The calls the task waits on, as tagged data parts.
    pending: Vec<a2a_protocol_types::Part>,
}

/// Runs the agent on a `Working` task and leaves the task in the state the run ends in:
/// - its text becomes one artifact;
/// - a run that stops for the caller leaves the task waiting, with the pending calls as
///   its status message;
/// - a failed run fails the task, with the error as its status message;
/// - a cancelled run leaves the task `Canceled`, without its partial output.
///
/// `guard` keeps the run cancelable until it ends.
///
/// # Errors
///
/// Returns an error only when the task store fails: a failed run is recorded on the task.
async fn drive(
    run: RunContext,
    task_id: String,
    context_id: String,
    msg: Message,
    guard: RunGuard,
) -> Result<(), A2aError> {
    let cancellation = guard.token().clone();
    let outcome = run_agent(&run, &context_id, &msg, cancellation.clone()).await;
    if cancellation.is_cancelled() {
        settle(&run.executor, &task_id, &context_id, TaskState::Canceled, Vec::new()).await?;
        return Ok(());
    }
    match outcome {
        Ok(finished) => {
            if !finished.text.is_empty() {
                let artifact = Artifact::new(
                    ArtifactId::new(uuid::Uuid::new_v4().to_string()),
                    vec![a2a_protocol_types::Part::text(&finished.text)],
                );
                run.executor.record_artifact(&task_id, &context_id, artifact).await?;
            }
            if !finished.media.is_empty() {
                let artifact = Artifact::new(
                    ArtifactId::new(uuid::Uuid::new_v4().to_string()),
                    finished.media,
                );
                run.executor.record_artifact(&task_id, &context_id, artifact).await?;
            }
            settle(&run.executor, &task_id, &context_id, finished.state, finished.pending).await?;
        }
        Err(error) => {
            let text = vec![super::convert::error_message_part(error.to_string())];
            settle(&run.executor, &task_id, &context_id, TaskState::Failed, text).await?;
        }
    }
    drop(guard);
    Ok(())
}

/// Runs the agent to the end of its event stream and returns what it produced.
///
/// # Errors
///
/// Returns an error if the agent cannot start or its stream fails.
async fn run_agent(
    run: &RunContext,
    context_id: &str,
    msg: &Message,
    cancellation: CancellationToken,
) -> Result<Finished, A2aError> {
    let mut event_stream = RequestHandler::start_agent(
        &run.runner_config,
        run.caller.as_ref(),
        context_id,
        msg,
        cancellation,
    )
    .await?;

    let mut response_text = ResponseText::default();
    let mut media = Vec::new();
    let mut awaiting_caller = None;
    let mut pending = PendingCalls::default();
    while let Some(result) = event_stream.next().await {
        let event =
            result.map_err(|e| A2aError::Internal { message: format!("agent error: {e}") })?;
        awaiting_caller = awaiting_caller.max(awaiting(&event));
        pending.observe(&event);
        response_text.push(&event);
        media.extend(media_parts(&event));
    }

    Ok(Finished {
        text: response_text.text(),
        media,
        state: finished_state(awaiting_caller),
        pending: pending.into_parts(),
    })
}

/// Moves a task to `state` with `parts` as its status message, unless the task already
/// ended another way. A run that ends while it is being cancelled must not fail on the
/// `Canceled` state the cancel recorded; the returned event then carries the state the task
/// ended in.
///
/// # Errors
///
/// Returns an error if the task store fails, or the transition is not allowed from a
/// state that is not terminal.
async fn settle(
    executor: &V1Executor,
    task_id: &str,
    context_id: &str,
    state: TaskState,
    parts: Vec<a2a_protocol_types::Part>,
) -> Result<TaskStatusUpdateEvent, A2aError> {
    match executor.transition_state_with_parts(task_id, context_id, state, parts).await {
        Err(error @ A2aError::InvalidStateTransition { .. }) => {
            let current = executor.task_store().get_task(task_id).await?;
            if is_terminal_state(current.status.state) {
                Ok(TaskStatusUpdateEvent {
                    task_id: TaskId::new(task_id),
                    context_id: ContextId::new(context_id),
                    status: current.status,
                    metadata: None,
                })
            } else {
                Err(error)
            }
        }
        other => other,
    }
}

/// Ends a stream that failed with `error`: moves the task to `Failed` with the error text as
/// the status message and returns that status event, which must be the stream's last item.
///
/// This matches the non-streaming path, which fails a task the same way. When the task cannot be
/// moved to `Failed`, no terminal status was sent, so `error` itself is returned to end the
/// stream instead.
async fn fail_stream(
    executor: &V1Executor,
    task_id: &str,
    context_id: &str,
    error: A2aError,
) -> Result<StreamResponse, A2aError> {
    match executor.fail_task(task_id, context_id, &error.to_string()).await {
        Ok(event) => Ok(StreamResponse::StatusUpdate(event)),
        Err(fail_error) => {
            tracing::warn!(task_id, error = %fail_error, "could not mark the failed stream's task as failed");
            Err(error)
        }
    }
}

/// Whether a task in `state` has nothing more for a subscriber until a message continues
/// it: it ended, or it waits on its caller.
fn ends_subscription(state: TaskState) -> bool {
    is_terminal_state(state) || matches!(state, TaskState::InputRequired | TaskState::AuthRequired)
}

/// Returns `true` if the given task state is terminal.
fn is_terminal_state(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected
    )
}

#[cfg(test)]
mod tests {
    use super::super::push::NoOpPushNotificationSender;
    use super::super::task_store::InMemoryTaskStore;
    use super::*;
    use a2a_protocol_types::{
        AgentCapabilities, AgentCard, AgentInterface, AgentSkill, MessageId, MessageRole, Part,
        TaskPushNotificationConfig,
    };

    // ── message/stream must carry real agent output ────────────────────────
    //
    // The handler used to create a task and transition Working -> Completed without invoking the
    // Runner, so a client saw a task reporting success that produced nothing. Both reference ADK
    // implementations drive the runner and translate its events; these assert that contract.

    /// Reads the text out of an A2A part, or `None` for non-text content.
    fn part_text(part: &a2a_protocol_types::Part) -> Option<&str> {
        match &part.content {
            a2a_protocol_types::PartContent::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }

    /// Emits `chunks` as partial events then a final one, the shape a streaming model produces.
    struct ChunkingAgent {
        chunks: Vec<String>,
    }

    struct SnapshotAgent {
        prefix: String,
        snapshot: String,
    }

    #[async_trait::async_trait]
    impl adk_core::Agent for SnapshotAgent {
        fn name(&self) -> &str {
            "snapshot_agent"
        }

        fn description(&self) -> &str {
            "emits an authoritative response after text deltas"
        }

        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            let mut first = adk_core::Event::new("invocation");
            first.set_content(adk_core::Content::new("model").with_text(&self.prefix));
            let mut delta = adk_core::Event::new("invocation");
            delta.llm_response.partial = true;
            delta.set_content(adk_core::Content::new("model").with_text("Hello"));
            let mut terminal = delta.clone();
            terminal.llm_response.partial = false;
            terminal.llm_response.provider_metadata =
                Some(serde_json::json!({"content_complete": true}));
            terminal.set_content(adk_core::Content::new("model").with_text(&self.snapshot));
            Ok(Box::pin(futures::stream::iter(vec![Ok(first), Ok(delta), Ok(terminal)])))
        }
    }

    #[async_trait::async_trait]
    impl adk_core::Agent for ChunkingAgent {
        fn name(&self) -> &str {
            "chunking_agent"
        }

        fn description(&self) -> &str {
            "emits partial text chunks"
        }

        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            let chunks = self.chunks.clone();
            let last = chunks.len().saturating_sub(1);
            let events: Vec<adk_core::Result<adk_core::Event>> = chunks
                .into_iter()
                .enumerate()
                .map(|(i, text)| {
                    let mut event = adk_core::Event::new("chunking_agent");
                    event.llm_response.content =
                        Some(adk_core::Content::new("model").with_text(&text));
                    event.llm_response.partial = i < last;
                    Ok(event)
                })
                .collect();
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// Fails immediately, to check the terminal state is not reported as success.
    struct FailingAgent;

    #[async_trait::async_trait]
    impl adk_core::Agent for FailingAgent {
        fn name(&self) -> &str {
            "failing_agent"
        }

        fn description(&self) -> &str {
            "fails immediately"
        }

        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            Ok(Box::pin(futures::stream::iter(vec![Err(adk_core::AdkError::model(
                "upstream exploded",
            ))])))
        }
    }

    fn handler_with_agent(agent: Arc<dyn adk_core::Agent>) -> (RequestHandler, Arc<dyn TaskStore>) {
        let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
        let executor = Arc::new(V1Executor::new(store.clone()));
        let cached = Arc::new(RwLock::new(CachedAgentCard::new(make_test_agent_card())));
        let runner_config = Arc::new(
            adk_runner::Runner::builder()
                .app_name("a2a-stream-test")
                .agent(agent)
                .session_service(Arc::new(adk_session::InMemorySessionService::new()))
                .build_config(),
        );
        let handler = RequestHandler::with_runner(
            executor,
            store.clone(),
            Arc::new(NoOpPushNotificationSender),
            cached,
            runner_config,
        );
        (handler, store)
    }

    #[tokio::test]
    async fn stream_delivers_agent_text_as_artifact_chunks() {
        let (handler, _store) = handler_with_agent(Arc::new(ChunkingAgent {
            chunks: vec!["Hel".into(), "lo ".into(), "world".into()],
        }));

        let mut stream = handler.message_stream(make_test_message()).await.expect("stream starts");
        let mut artifacts = Vec::new();
        let mut states = Vec::new();
        while let Some(item) = stream.next().await {
            match item.expect("no stream error") {
                StreamResponse::ArtifactUpdate(e) => artifacts.push(e),
                StreamResponse::StatusUpdate(e) => states.push(e.status.state),
                _ => {}
            }
        }

        assert_eq!(artifacts.len(), 3, "one artifact event per agent chunk");
        let joined: String =
            artifacts.iter().flat_map(|e| e.artifact.parts.iter().filter_map(part_text)).collect();
        assert_eq!(joined, "Hello world", "the agent's text must reach the client");
        assert_eq!(states, vec![TaskState::Working, TaskState::Completed]);
    }

    #[tokio::test]
    async fn stream_chunks_share_one_artifact_id_and_mark_the_last() {
        let (handler, _store) =
            handler_with_agent(Arc::new(ChunkingAgent { chunks: vec!["a".into(), "b".into()] }));

        let mut stream = handler.message_stream(make_test_message()).await.expect("stream starts");
        let mut artifacts = Vec::new();
        while let Some(item) = stream.next().await {
            if let StreamResponse::ArtifactUpdate(e) = item.expect("no error") {
                artifacts.push(e);
            }
        }

        assert_eq!(artifacts.len(), 2);
        assert_eq!(
            artifacts[0].artifact.id, artifacts[1].artifact.id,
            "a client joins chunks by artifact ID, so they must match"
        );
        assert_eq!(artifacts[0].append, Some(false), "the first chunk starts the artifact");
        assert_eq!(artifacts[1].append, Some(true), "later chunks append");
        assert_eq!(artifacts[0].last_chunk, Some(false));
        assert_eq!(artifacts[1].last_chunk, Some(true), "the final chunk must be marked");
    }

    #[tokio::test]
    async fn snapshots_replace_streamed_deltas_and_persist_the_same_text() {
        for prefix in ["", "Earlier. "] {
            for snapshot in ["Hello world", "Corrected", ""] {
                let (handler, store) = handler_with_agent(Arc::new(SnapshotAgent {
                    prefix: prefix.into(),
                    snapshot: snapshot.into(),
                }));
                let mut stream = handler.message_stream(make_test_message()).await.unwrap();
                let mut task_id = None;
                let mut received = String::new();
                let mut last_update = None;
                while let Some(item) = stream.next().await {
                    match item.unwrap() {
                        StreamResponse::Task(task) => task_id = Some(task.id.0),
                        StreamResponse::ArtifactUpdate(update) => {
                            if update.append != Some(true) {
                                received.clear();
                            }
                            received.extend(update.artifact.parts.iter().filter_map(part_text));
                            last_update = Some(update);
                        }
                        _ => {}
                    }
                }
                let expected = format!("{prefix}{snapshot}");
                assert_eq!(received, expected);
                let update = last_update.unwrap();
                assert_eq!(
                    (update.append, update.last_chunk),
                    (Some(snapshot.starts_with("Hello")), Some(true))
                );
                let task = store.get_task(&task_id.unwrap()).await.unwrap();
                let stored: String = task
                    .artifacts
                    .iter()
                    .flat_map(|artifact| artifact.parts.iter().filter_map(part_text))
                    .collect();
                assert_eq!(stored, expected);
            }
        }
    }

    #[tokio::test]
    async fn buffered_send_replaces_deltas_with_the_snapshot() {
        let (handler, _) = handler_with_agent(Arc::new(SnapshotAgent {
            prefix: "Earlier. ".into(),
            snapshot: "Hello world".into(),
        }));
        let task = handler.message_send(make_test_message()).await.unwrap();
        let text: String = task
            .artifacts
            .unwrap()
            .iter()
            .flat_map(|artifact| artifact.parts.iter().filter_map(part_text))
            .collect();
        assert_eq!(text, "Earlier. Hello world");
    }

    /// The text of a status message's parts.
    fn status_text(status: &a2a_protocol_types::TaskStatus) -> String {
        status.message.iter().flat_map(|m| m.parts.iter().filter_map(part_text)).collect()
    }

    #[tokio::test]
    async fn stream_failure_ends_on_a_failed_status_that_carries_the_error() {
        let (handler, store) = handler_with_agent(Arc::new(FailingAgent));

        let stream = handler.message_stream(make_test_message()).await.expect("stream starts");
        let items: Vec<_> = stream.collect().await;

        assert!(
            items.iter().all(Result::is_ok),
            "the error rides on the terminal status, not on a separate error item: {items:?}"
        );
        let states: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                Ok(StreamResponse::StatusUpdate(e)) => Some(e.status.state),
                _ => None,
            })
            .collect();
        assert!(
            !states.contains(&TaskState::Completed),
            "a failed run must never report Completed: {states:?}"
        );

        // A2A §3.1.2: the stream closes at the terminal state, so the Failed status is last.
        let Some(Ok(StreamResponse::StatusUpdate(last))) = items.last() else {
            panic!("the stream must end on a status update: {items:?}");
        };
        assert_eq!(last.status.state, TaskState::Failed);
        let text = status_text(&last.status);
        assert!(text.contains("upstream exploded"), "the status message names the error: {text}");

        // The stored task says the same, and so does a non-streaming send that fails.
        let stored = store.get_task(&last.task_id.0).await.unwrap();
        assert_eq!(status_text(&stored.status), text);
        let (handler, _) = handler_with_agent(Arc::new(FailingAgent));
        let sent = handler.message_send(make_test_message()).await.unwrap();
        assert_eq!(sent.status.state, TaskState::Failed);
        assert_eq!(status_text(&sent.status), text);
    }

    #[tokio::test]
    async fn streamed_text_is_persisted_for_tasks_get() {
        let (handler, store) = handler_with_agent(Arc::new(ChunkingAgent {
            chunks: vec!["one ".into(), "two".into()],
        }));

        let mut stream = handler.message_stream(make_test_message()).await.expect("stream starts");
        let mut task_id = None;
        while let Some(item) = stream.next().await {
            if let Ok(StreamResponse::Task(t)) = item {
                task_id = Some(t.id.0.clone());
            }
        }

        let entry = store.get_task(&task_id.expect("task event")).await.expect("task exists");
        let text: String =
            entry.artifacts.iter().flat_map(|a| a.parts.iter().filter_map(part_text)).collect();
        assert_eq!(text, "one two", "a later tasks/get must see what was streamed");
    }

    // ── Waiting on the caller, continuing a task, cancel and returnImmediately ──

    /// Asks for approval with a long-running call, the way a human-in-the-loop tool does,
    /// and answers once the caller sends the call's result.
    struct ApprovalAgent;

    #[async_trait::async_trait]
    impl adk_core::Agent for ApprovalAgent {
        fn name(&self) -> &str {
            "approval_agent"
        }

        fn description(&self) -> &str {
            "waits for an approval"
        }

        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }

        async fn run(
            &self,
            ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            let approved = ctx.user_content().parts.iter().any(|part| {
                matches!(
                    part,
                    adk_core::Part::FunctionResponse { function_response, id: Some(id), .. }
                        if id == "call-approve"
                            && function_response.response["status"] == "approved"
                )
            });
            if approved {
                let mut done = adk_core::Event::new("inv");
                done.set_content(adk_core::Content::new("model").with_text("Approved."));
                return Ok(Box::pin(futures::stream::iter(vec![Ok(done)])));
            }

            let mut call = adk_core::Event::new("inv");
            call.set_content(adk_core::Content {
                role: "model".to_string(),
                parts: vec![adk_core::Part::FunctionCall {
                    name: "request_approval".to_string(),
                    args: serde_json::json!({"amount": 500}),
                    id: Some("call-approve".to_string()),
                    thought_signature: None,
                }],
            });
            call.long_running_tool_ids = vec!["call-approve".to_string()];
            let mut interim = adk_core::Event::new("inv");
            interim.set_content(adk_core::Content {
                role: "user".to_string(),
                parts: vec![adk_core::Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new(
                        "request_approval",
                        serde_json::json!({"status": "pending"}),
                    ),
                    id: Some("call-approve".to_string()),
                    annotations: None,
                }],
            });
            let mut text = adk_core::Event::new("inv");
            text.set_content(adk_core::Content::new("model").with_text("Approval pending."));
            Ok(Box::pin(futures::stream::iter(vec![Ok(call), Ok(interim), Ok(text)])))
        }
    }

    /// The caller's approval, a `function_response` data part tagged the way adk-python and
    /// adk-go tag it, sent for `task_id`.
    fn approval_for(task_id: &TaskId, context_id: Option<&ContextId>) -> Message {
        let part: Part = serde_json::from_value(serde_json::json!({
            "data": {"id": "call-approve", "name": "request_approval", "response": {"status": "approved"}},
            "metadata": {"adk_type": "function_response"}
        }))
        .unwrap();
        Message {
            id: MessageId::new("msg-2"),
            role: MessageRole::User,
            parts: vec![part],
            task_id: Some(task_id.clone()),
            context_id: context_id.cloned(),
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        }
    }

    fn artifact_texts(task: &Task) -> Vec<String> {
        task.artifacts
            .iter()
            .flatten()
            .flat_map(|artifact| artifact.parts.iter().filter_map(part_text).map(str::to_string))
            .collect()
    }

    #[tokio::test]
    async fn a_waiting_task_names_the_pending_call_and_the_answer_continues_it() {
        let (handler, _) = handler_with_agent(Arc::new(ApprovalAgent));

        let waiting = handler.message_send(make_test_message()).await.unwrap();
        assert_eq!(waiting.status.state, TaskState::InputRequired);
        let parts: Vec<_> = waiting
            .status
            .message
            .as_ref()
            .expect("the status names the pending call")
            .parts
            .iter()
            .map(|part| serde_json::to_value(part).unwrap())
            .collect();
        assert_eq!(
            parts,
            vec![
                serde_json::json!({
                    "data": {"id": "call-approve", "name": "request_approval", "args": {"amount": 500}},
                    "metadata": {"adk_type": "function_call", "adk_is_long_running": true}
                }),
                serde_json::json!({
                    "data": {"id": "call-approve", "name": "request_approval", "response": {"status": "pending"}},
                    "metadata": {"adk_type": "function_response"}
                }),
            ]
        );

        let answer = approval_for(&waiting.id, Some(&waiting.context_id));
        let done = handler.message_send(answer).await.unwrap();
        assert_eq!(done.id, waiting.id, "the answer continues the same task");
        assert_eq!(done.status.state, TaskState::Completed);
        assert!(artifact_texts(&done).contains(&"Approved.".to_string()), "{done:?}");
    }

    #[tokio::test]
    async fn a_stream_names_the_pending_call_and_a_task_id_alone_continues_the_task() {
        let (handler, _) = handler_with_agent(Arc::new(ApprovalAgent));

        let items: Vec<_> =
            handler.message_stream(make_test_message()).await.unwrap().collect().await;
        let Some(Ok(StreamResponse::StatusUpdate(waiting))) = items.last() else {
            panic!("the stream ends on a status: {items:?}");
        };
        assert_eq!(waiting.status.state, TaskState::InputRequired);
        assert_eq!(waiting.status.message.as_ref().map(|m| m.parts.len()), Some(2));

        // A2A §3.4: with only a taskId, the context is the task's.
        let answer = approval_for(&waiting.task_id, None);
        let items: Vec<_> = handler.message_stream(answer).await.unwrap().collect().await;
        let Some(Ok(StreamResponse::Task(first))) = items.first() else {
            panic!("the stream starts with the task: {items:?}");
        };
        assert_eq!(first.id, waiting.task_id, "the answer continues the same task");
        let Some(Ok(StreamResponse::StatusUpdate(last))) = items.last() else {
            panic!("the stream ends on a status: {items:?}");
        };
        assert_eq!(last.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn a_message_naming_a_task_it_cannot_continue_is_refused() {
        let (handler, _) = handler_with_agent(Arc::new(ApprovalAgent));

        let mut unknown = make_test_message();
        unknown.task_id = Some(TaskId::new("no-such-task"));
        assert!(matches!(handler.message_send(unknown).await, Err(A2aError::TaskNotFound { .. })));

        let waiting = handler.message_send(make_test_message()).await.unwrap();
        let elsewhere = approval_for(&waiting.id, Some(&ContextId::new("another-context")));
        assert!(matches!(
            handler.message_send(elsewhere).await,
            Err(A2aError::InvalidParams { .. })
        ));

        let done = handler.message_send(approval_for(&waiting.id, None)).await.unwrap();
        assert_eq!(done.status.state, TaskState::Completed);
        let mut after_the_end = approval_for(&waiting.id, None);
        after_the_end.id = MessageId::new("msg-3");
        assert!(matches!(
            handler.message_send(after_the_end).await,
            Err(A2aError::UnsupportedOperation { .. })
        ));
    }

    /// Answers after `delay` and records that it got that far.
    struct SlowAgent {
        delay: std::time::Duration,
        answered: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl adk_core::Agent for SlowAgent {
        fn name(&self) -> &str {
            "slow_agent"
        }

        fn description(&self) -> &str {
            "answers late"
        }

        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            let delay = self.delay;
            let answered = self.answered.clone();
            Ok(Box::pin(async_stream::stream! {
                tokio::time::sleep(delay).await;
                answered.store(true, Ordering::SeqCst);
                let mut event = adk_core::Event::new("inv");
                event.set_content(adk_core::Content::new("model").with_text("Too late."));
                yield Ok(event);
            }))
        }
    }

    fn slow_handler(
        delay_ms: u64,
    ) -> (RequestHandler, Arc<dyn TaskStore>, Arc<std::sync::atomic::AtomicBool>) {
        let answered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (handler, store) = handler_with_agent(Arc::new(SlowAgent {
            delay: std::time::Duration::from_millis(delay_ms),
            answered: answered.clone(),
        }));
        (handler, store, answered)
    }

    fn return_immediately() -> SendMessageConfiguration {
        serde_json::from_value(serde_json::json!({"returnImmediately": true})).unwrap()
    }

    #[tokio::test]
    async fn return_immediately_answers_while_the_run_goes_on_in_the_background() {
        let (handler, store, _) = slow_handler(50);

        let started = handler
            .message_send_with(make_test_message(), Some(&return_immediately()))
            .await
            .unwrap();
        assert_eq!(started.status.state, TaskState::Working, "the send does not wait for the run");

        let mut state = TaskState::Working;
        for _ in 0..100 {
            state = store.get_task(&started.id.0).await.unwrap().status.state;
            if state != TaskState::Working {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(state, TaskState::Completed);
        let done = handler.tasks_get(&started.id.0, None).await.unwrap();
        assert_eq!(artifact_texts(&done), vec!["Too late.".to_string()]);
    }

    #[tokio::test]
    async fn cancel_stops_the_run_and_a_second_cancel_is_refused() {
        let (handler, store, answered) = slow_handler(300);

        let started = handler
            .message_send_with(make_test_message(), Some(&return_immediately()))
            .await
            .unwrap();
        let canceled = handler.tasks_cancel(&started.id.0).await.unwrap();
        assert_eq!(canceled.status.state, TaskState::Canceled);
        assert!(matches!(
            handler.tasks_cancel(&started.id.0).await,
            Err(A2aError::TaskNotCancelable { .. })
        ));

        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(
            !answered.load(Ordering::SeqCst),
            "cancelling stops the agent, not only the task's state"
        );
        let stored = store.get_task(&started.id.0).await.unwrap();
        assert_eq!(stored.status.state, TaskState::Canceled);
        assert!(stored.artifacts.is_empty(), "a cancelled run records no output");
    }

    #[tokio::test]
    async fn cancelling_a_streamed_task_ends_the_stream_on_canceled() {
        let (handler, _, answered) = slow_handler(5_000);

        let mut stream = handler.message_stream(make_test_message()).await.unwrap();
        let Some(Ok(StreamResponse::Task(task))) = stream.next().await else {
            panic!("the stream starts with the task");
        };
        let Some(Ok(StreamResponse::StatusUpdate(working))) = stream.next().await else {
            panic!("then reports it working");
        };
        assert_eq!(working.status.state, TaskState::Working);

        let canceled = handler.tasks_cancel(&task.id.0).await.unwrap();
        assert_eq!(canceled.status.state, TaskState::Canceled);
        let rest: Vec<_> =
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.collect::<Vec<_>>())
                .await
                .expect("a cancelled stream ends");
        let Some(Ok(StreamResponse::StatusUpdate(last))) = rest.last() else {
            panic!("the stream ends on a status: {rest:?}");
        };
        assert_eq!(last.status.state, TaskState::Canceled);
        assert!(!answered.load(Ordering::SeqCst));
    }

    // ── Media in artifacts ──────────────────────────────────────────────────

    /// Answers with a caption and a PNG.
    struct ImageAgent;

    #[async_trait::async_trait]
    impl adk_core::Agent for ImageAgent {
        fn name(&self) -> &str {
            "image_agent"
        }

        fn description(&self) -> &str {
            "draws"
        }

        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            let mut event = adk_core::Event::new("inv");
            event.set_content(
                adk_core::Content::new("model")
                    .with_text("A square.")
                    .with_inline_data("image/png", vec![0x89, b'P', b'N', b'G']),
            );
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
        }
    }

    fn image_parts(artifacts: &[Artifact]) -> Vec<&a2a_protocol_types::Part> {
        artifacts
            .iter()
            .flat_map(|artifact| &artifact.parts)
            .filter(|part| part.media_type.as_deref() == Some("image/png"))
            .collect()
    }

    #[tokio::test]
    async fn files_and_inline_data_reach_the_client_beside_the_text() {
        let (handler, store) = handler_with_agent(Arc::new(ImageAgent));
        let sent = handler.message_send(make_test_message()).await.unwrap();
        let artifacts = sent.artifacts.clone().unwrap_or_default();
        assert_eq!(artifact_texts(&sent), vec!["A square.".to_string()]);
        assert_eq!(image_parts(&artifacts).len(), 1, "{artifacts:?}");

        let (handler, _) = handler_with_agent(Arc::new(ImageAgent));
        let updates: Vec<_> = handler
            .message_stream(make_test_message())
            .await
            .unwrap()
            .map(|item| item.unwrap())
            .collect()
            .await;
        let streamed: Vec<Artifact> = updates
            .iter()
            .filter_map(|update| match update {
                StreamResponse::ArtifactUpdate(event) => Some(event.artifact.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(image_parts(&streamed).len(), 1, "the stream carries the image too");
        let stored = store.get_task(&sent.id.0).await.unwrap();
        assert_eq!(image_parts(&stored.artifacts).len(), 1);
    }

    // ── ADK metadata ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn tasks_and_their_updates_name_the_adk_app_user_and_session() {
        use super::super::convert::{ADK_ERROR_MESSAGE_METADATA_KEY, ADK_EXTENSION_URI};

        let (handler, _) =
            handler_with_agent(Arc::new(ChunkingAgent { chunks: vec!["hi".into()] }));
        let mut stream = handler.message_stream(make_test_message()).await.unwrap();
        let Some(Ok(StreamResponse::Task(task))) = stream.next().await else {
            panic!("the stream starts with the task");
        };
        let context = task.context_id.0.clone();
        let metadata = task.metadata.clone().expect("the task carries ADK metadata");
        assert_eq!(metadata["adk_app_name"], "a2a-stream-test");
        assert_eq!(metadata["adk_session_id"], context.as_str());
        assert_eq!(metadata["adk_user_id"], format!("a2a-{context}"));
        assert_eq!(metadata[ADK_EXTENSION_URI], true);
        for update in stream.collect::<Vec<_>>().await {
            let metadata = match update.unwrap() {
                StreamResponse::StatusUpdate(event) => event.metadata,
                StreamResponse::ArtifactUpdate(event) => event.metadata,
                _ => continue,
            };
            assert_eq!(
                metadata.expect("every update carries it")["adk_session_id"],
                context.as_str()
            );
        }

        // A failed run marks its error text.
        let (handler, _) = handler_with_agent(Arc::new(FailingAgent));
        let failed = handler.message_send(make_test_message()).await.unwrap();
        let error_part = &failed.status.message.expect("the error").parts[0];
        assert_eq!(error_part.metadata.as_ref().unwrap()[ADK_ERROR_MESSAGE_METADATA_KEY], true);
    }

    // ── Capabilities the card states ────────────────────────────────────────

    #[tokio::test]
    async fn operations_the_card_says_are_off_are_refused() {
        let store = Arc::new(InMemoryTaskStore::new());
        let mut card = make_test_agent_card();
        card.capabilities = AgentCapabilities::none()
            .with_streaming(false)
            .with_push_notifications(false)
            .with_extended_agent_card(false);
        let handler = RequestHandler::new(
            Arc::new(V1Executor::new(store.clone())),
            store,
            Arc::new(NoOpPushNotificationSender),
            Arc::new(RwLock::new(CachedAgentCard::new(card))),
        );

        let task = handler.message_send(make_test_message()).await.unwrap();
        assert!(matches!(
            handler.message_stream(make_test_message()).await,
            Err(A2aError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            handler.tasks_subscribe(&task.id.0).await,
            Err(A2aError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            handler.push_config_list(&task.id.0).await,
            Err(A2aError::PushNotificationNotSupported)
        ));
        let with_push: SendMessageConfiguration = serde_json::from_value(serde_json::json!({
            "taskPushNotificationConfig": {"url": "https://hooks.example.com/a2a"}
        }))
        .unwrap();
        let mut msg = make_test_message();
        msg.id = MessageId::new("msg-push");
        assert!(matches!(
            handler.message_send_with(msg, Some(&with_push)).await,
            Err(A2aError::PushNotificationNotSupported)
        ));
        assert!(matches!(
            handler.agent_card_extended().await,
            Err(A2aError::ExtendedAgentCardNotConfigured)
        ));
    }

    // ── ListTasks pages ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_tasks_pages_through_every_task_once_newest_first() {
        let handler = make_handler();
        for i in 0..5 {
            let mut msg = make_test_message();
            msg.id = MessageId::new(format!("msg-{i}"));
            handler.message_send(msg).await.unwrap();
        }

        let mut seen = Vec::new();
        let mut token = None;
        loop {
            let page = handler
                .tasks_list(ListTasksParams {
                    page_size: Some(2),
                    page_token: token.clone(),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(page.total_size, 5);
            assert_eq!(page.page_size, 2);
            assert!(page.tasks.len() <= 2);
            assert!(
                page.tasks.iter().all(|task| task.artifacts.is_none()),
                "no artifacts unless asked"
            );
            seen.extend(page.tasks.into_iter().map(|task| task.id.0));
            if page.next_page_token.is_empty() {
                break;
            }
            token = Some(page.next_page_token);
        }
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!((seen.len(), unique.len()), (5, 5), "every task once: {seen:?}");

        for bad in [
            ListTasksParams { page_size: Some(0), ..Default::default() },
            ListTasksParams { page_size: Some(101), ..Default::default() },
            ListTasksParams { page_token: Some("not a token".into()), ..Default::default() },
            ListTasksParams {
                status_timestamp_after: Some("yesterday".into()),
                ..Default::default()
            },
        ] {
            assert!(matches!(handler.tasks_list(bad).await, Err(A2aError::InvalidParams { .. })));
        }

        // Tasks updated before the cutoff are left out.
        let later = (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339();
        let none = handler
            .tasks_list(ListTasksParams {
                status_timestamp_after: Some(later),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(none.total_size, 0);
    }

    // ── SubscribeToTask follows a running task ──────────────────────────────

    /// Collects a subscription, which must end within two seconds.
    async fn collect_subscription(
        stream: BoxStream<'static, Result<StreamResponse, A2aError>>,
    ) -> Vec<StreamResponse> {
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.collect::<Vec<_>>())
            .await
            .expect("the subscription ends")
            .into_iter()
            .map(|item| item.expect("no stream error"))
            .collect()
    }

    fn last_state(updates: &[StreamResponse]) -> Option<TaskState> {
        updates.iter().rev().find_map(|update| match update {
            StreamResponse::StatusUpdate(event) => Some(event.status.state),
            _ => None,
        })
    }

    fn streamed_text(updates: &[StreamResponse]) -> String {
        updates
            .iter()
            .filter_map(|update| match update {
                StreamResponse::ArtifactUpdate(event) => Some(&event.artifact),
                _ => None,
            })
            .flat_map(|artifact| artifact.parts.iter().filter_map(part_text))
            .collect()
    }

    #[tokio::test]
    async fn a_subscriber_follows_a_background_run_to_its_end() {
        let (handler, _, _) = slow_handler(200);
        let started = handler
            .message_send_with(make_test_message(), Some(&return_immediately()))
            .await
            .unwrap();

        let updates =
            collect_subscription(handler.tasks_subscribe(&started.id.0).await.unwrap()).await;
        let Some(StreamResponse::Task(first)) = updates.first() else {
            panic!("the subscription starts with the task: {updates:?}");
        };
        assert_eq!(first.status.state, TaskState::Working);
        assert_eq!(streamed_text(&updates), "Too late.", "the run's output reaches the subscriber");
        assert_eq!(last_state(&updates), Some(TaskState::Completed));
    }

    #[tokio::test]
    async fn a_subscriber_follows_a_task_another_client_streams() {
        let (handler, _, _) = slow_handler(200);
        let mut stream = handler.message_stream(make_test_message()).await.unwrap();
        let Some(Ok(StreamResponse::Task(task))) = stream.next().await else {
            panic!("the stream starts with the task");
        };
        let streaming = tokio::spawn(stream.collect::<Vec<_>>());

        let updates =
            collect_subscription(handler.tasks_subscribe(&task.id.0).await.unwrap()).await;
        assert_eq!(streamed_text(&updates), "Too late.");
        assert_eq!(last_state(&updates), Some(TaskState::Completed));
        streaming.await.unwrap();
    }

    #[tokio::test]
    async fn a_task_waiting_on_its_caller_is_a_snapshot_and_a_finished_one_is_refused() {
        let (handler, _) = handler_with_agent(Arc::new(ApprovalAgent));
        let waiting = handler.message_send(make_test_message()).await.unwrap();

        let updates =
            collect_subscription(handler.tasks_subscribe(&waiting.id.0).await.unwrap()).await;
        assert!(matches!(updates.first(), Some(StreamResponse::Task(_))));
        assert_eq!(last_state(&updates), Some(TaskState::InputRequired));

        handler.message_send(approval_for(&waiting.id, None)).await.unwrap();
        assert!(matches!(
            handler.tasks_subscribe(&waiting.id.0).await,
            Err(A2aError::UnsupportedOperation { .. })
        ));
    }

    fn make_handler() -> RequestHandler {
        let store = Arc::new(InMemoryTaskStore::new());
        let executor = Arc::new(V1Executor::new(store.clone()));
        let push_sender = Arc::new(NoOpPushNotificationSender);
        let card = make_test_agent_card();
        let cached = Arc::new(RwLock::new(CachedAgentCard::new(card)));
        RequestHandler::new(executor, store, push_sender, cached)
    }

    fn make_test_agent_card() -> AgentCard {
        AgentCard {
            name: "test-agent".to_string(),
            url: Some("http://localhost:8080".to_string()),
            description: "A test agent".to_string(),
            version: "1.0.0".to_string(),
            supported_interfaces: vec![AgentInterface {
                url: "http://localhost:8080/a2a".to_string(),
                protocol_binding: "JSONRPC".to_string(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            }],
            default_input_modes: vec!["text/plain".to_string()],
            default_output_modes: vec!["text/plain".to_string()],
            skills: vec![AgentSkill {
                id: "echo".to_string(),
                name: "Echo".to_string(),
                description: "Echoes input".to_string(),
                tags: vec![],
                examples: None,
                input_modes: None,
                output_modes: None,
                security_requirements: None,
            }],
            capabilities: AgentCapabilities::none(),
            provider: None,
            icon_url: None,
            documentation_url: None,
            security_schemes: None,
            security_requirements: None,
            signatures: None,
        }
    }

    fn make_test_message() -> Message {
        Message {
            id: MessageId::new("msg-1"),
            role: MessageRole::User,
            parts: vec![Part::text("hello")],
            task_id: None,
            context_id: None,
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        }
    }

    /// Fails the next `get` with a transient error when armed.
    struct FlakySessions {
        inner: adk_session::InMemorySessionService,
        fail_next_get: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl adk_session::SessionService for FlakySessions {
        async fn create(
            &self,
            req: adk_session::CreateRequest,
        ) -> adk_core::Result<Box<dyn adk_session::Session>> {
            self.inner.create(req).await
        }

        async fn get(
            &self,
            req: adk_session::GetRequest,
        ) -> adk_core::Result<Box<dyn adk_session::Session>> {
            if self.fail_next_get.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(adk_core::AdkError::unavailable(
                    adk_core::ErrorComponent::Session,
                    "session.backend.timeout",
                    "the session backend timed out",
                ));
            }
            self.inner.get(req).await
        }

        async fn list(
            &self,
            req: adk_session::ListRequest,
        ) -> adk_core::Result<Vec<Box<dyn adk_session::Session>>> {
            self.inner.list(req).await
        }

        async fn delete(&self, req: adk_session::DeleteRequest) -> adk_core::Result<()> {
            self.inner.delete(req).await
        }

        async fn append_event(
            &self,
            session_id: &str,
            event: adk_core::Event,
        ) -> adk_core::Result<()> {
            self.inner.append_event(session_id, event).await
        }

        async fn append_event_for_identity(
            &self,
            req: adk_session::AppendEventRequest,
        ) -> adk_core::Result<()> {
            self.inner.append_event_for_identity(req).await
        }
    }

    /// A transient failure reading the context's session fails the message and leaves the
    /// stored session as it was, instead of creating it again over its history.
    #[tokio::test]
    async fn message_send_keeps_the_session_when_its_lookup_fails() {
        use adk_session::SessionService;

        let sessions = Arc::new(FlakySessions {
            inner: adk_session::InMemorySessionService::new(),
            fail_next_get: std::sync::atomic::AtomicBool::new(false),
        });
        let identity = adk_core::AdkIdentity::new(
            "a2a-stream-test".try_into().unwrap(),
            "a2a-ctx-kept".try_into().unwrap(),
            "ctx-kept".try_into().unwrap(),
        );
        sessions
            .create(adk_session::CreateRequest {
                app_name: "a2a-stream-test".to_string(),
                user_id: "a2a-ctx-kept".to_string(),
                session_id: Some("ctx-kept".to_string()),
                state: std::collections::HashMap::from([(
                    "plan".to_string(),
                    serde_json::json!("gold"),
                )]),
            })
            .await
            .unwrap();
        sessions
            .append_event_for_identity(adk_session::AppendEventRequest {
                identity: identity.clone(),
                event: adk_core::Event::new("earlier"),
            })
            .await
            .unwrap();

        let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
        let handler = RequestHandler::with_runner(
            Arc::new(V1Executor::new(store.clone())),
            store,
            Arc::new(NoOpPushNotificationSender),
            Arc::new(RwLock::new(CachedAgentCard::new(make_test_agent_card()))),
            Arc::new(
                adk_runner::Runner::builder()
                    .app_name("a2a-stream-test")
                    .agent(Arc::new(ChunkingAgent { chunks: vec!["answer".into()] }))
                    .session_service(sessions.clone())
                    .build_config(),
            ),
        );
        let mut message = make_test_message();
        message.context_id = Some(a2a_protocol_types::ContextId::new("ctx-kept"));

        sessions.fail_next_get.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(task) = handler.message_send(message).await {
            assert_ne!(task.status.state, TaskState::Completed);
        }

        let stored = sessions.inner.get_for_identity(&identity).await.unwrap();
        assert_eq!(
            (stored.events().len(), stored.state().get("plan")),
            (1, Some(serde_json::json!("gold")))
        );
    }

    // ── message_send ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn message_send_creates_and_completes_task() {
        let handler = make_handler();
        let msg = make_test_message();

        let task = handler.message_send(msg).await.unwrap();

        assert_eq!(task.status.state, TaskState::Completed);
        assert!(!task.id.0.is_empty());
        assert!(!task.context_id.0.is_empty());
        // History should contain the sent message
        assert!(task.history.is_some());
        assert_eq!(task.history.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn message_send_uses_provided_context_id() {
        let handler = make_handler();
        let mut msg = make_test_message();
        msg.context_id = Some(a2a_protocol_types::ContextId::new("my-ctx"));

        let task = handler.message_send(msg).await.unwrap();
        assert_eq!(task.context_id.0, "my-ctx");
    }

    // ── tasks_get ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn tasks_get_returns_task() {
        let handler = make_handler();
        let msg = make_test_message();
        let task = handler.message_send(msg).await.unwrap();

        let retrieved = handler.tasks_get(&task.id.0, None).await.unwrap();
        assert_eq!(retrieved.id, task.id);
        assert_eq!(retrieved.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn tasks_get_truncates_history() {
        let handler = make_handler();

        // Create a task and add multiple history messages
        let msg = make_test_message();
        let task = handler.message_send(msg).await.unwrap();

        // Add more history
        let msg2 = Message {
            id: MessageId::new("msg-2"),
            role: MessageRole::Agent,
            parts: vec![Part::text("response")],
            task_id: None,
            context_id: None,
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        };
        handler.task_store.add_history_message(&task.id.0, msg2).await.unwrap();

        // Get with history_len=1 should only return the last message
        let retrieved = handler.tasks_get(&task.id.0, Some(1)).await.unwrap();
        assert_eq!(retrieved.history.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn tasks_get_not_found() {
        let handler = make_handler();
        let err = handler.tasks_get("nonexistent", None).await.unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    // ── tasks_cancel ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn tasks_cancel_cancels_working_task() {
        let handler = make_handler();

        // Create a task in WORKING state
        let task_id = "cancel-test";
        let ctx_id = "ctx-cancel";
        handler.executor.create_task(task_id, ctx_id).await.unwrap();
        handler.executor.transition_state(task_id, ctx_id, TaskState::Working, None).await.unwrap();

        let task = handler.tasks_cancel(task_id).await.unwrap();
        assert_eq!(task.status.state, TaskState::Canceled);
    }

    #[tokio::test]
    async fn tasks_cancel_rejects_terminal_task() {
        let handler = make_handler();
        let msg = make_test_message();
        let task = handler.message_send(msg).await.unwrap();

        // Task is COMPLETED (terminal) — cancel should fail
        let err = handler.tasks_cancel(&task.id.0).await.unwrap_err();
        assert!(matches!(err, A2aError::TaskNotCancelable { .. }));
    }

    #[tokio::test]
    async fn tasks_cancel_not_found() {
        let handler = make_handler();
        let err = handler.tasks_cancel("nonexistent").await.unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    // ── tasks_list ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn tasks_list_returns_all_tasks() {
        let handler = make_handler();

        handler.message_send(make_test_message()).await.unwrap();
        let mut msg2 = make_test_message();
        msg2.id = MessageId::new("msg-list-2");
        handler.message_send(msg2).await.unwrap();

        let tasks = handler.tasks_list(ListTasksParams::default()).await.unwrap();
        assert_eq!(tasks.tasks.len(), 2);
    }

    #[tokio::test]
    async fn tasks_list_filters_by_context_id() {
        let handler = make_handler();

        let mut msg1 = make_test_message();
        msg1.context_id = Some(a2a_protocol_types::ContextId::new("ctx-a"));
        handler.message_send(msg1).await.unwrap();

        let mut msg2 = make_test_message();
        msg2.id = MessageId::new("msg-ctx-b");
        msg2.context_id = Some(a2a_protocol_types::ContextId::new("ctx-b"));
        handler.message_send(msg2).await.unwrap();

        let tasks = handler
            .tasks_list(ListTasksParams {
                context_id: Some("ctx-a".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(tasks.tasks.len(), 1);
        assert_eq!(tasks.tasks[0].context_id.0, "ctx-a");
    }

    #[tokio::test]
    async fn tasks_list_with_page_size() {
        let handler = make_handler();

        handler.message_send(make_test_message()).await.unwrap();
        let mut msg2 = make_test_message();
        msg2.id = MessageId::new("msg-page-2");
        handler.message_send(msg2).await.unwrap();
        let mut msg3 = make_test_message();
        msg3.id = MessageId::new("msg-page-3");
        handler.message_send(msg3).await.unwrap();

        let tasks = handler
            .tasks_list(ListTasksParams { page_size: Some(2), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(tasks.tasks.len(), 2);
    }

    #[tokio::test]
    async fn tasks_list_empty() {
        let handler = make_handler();
        let tasks = handler.tasks_list(ListTasksParams::default()).await.unwrap();
        assert!(tasks.tasks.is_empty());
    }

    // ── push_config_create / get / list / delete ─────────────────────────

    #[tokio::test]
    async fn push_config_lifecycle() {
        let handler = make_handler();

        // Create a task first
        let msg = make_test_message();
        let task = handler.message_send(msg).await.unwrap();
        let task_id = &task.id.0;

        // Create push config
        let config = TaskPushNotificationConfig::new(task_id, "https://example.com/webhook");
        let created = handler.push_config_create(task_id, config).await.unwrap();
        assert!(created.id.is_some());
        assert_eq!(created.url, "https://example.com/webhook");
        let config_id = created.id.clone().unwrap();

        // Get push config
        let retrieved = handler.push_config_get(task_id, &config_id).await.unwrap();
        assert_eq!(retrieved.url, "https://example.com/webhook");

        // List push configs
        let configs = handler.push_config_list(task_id).await.unwrap();
        assert_eq!(configs.len(), 1);

        // Delete push config
        handler.push_config_delete(task_id, &config_id).await.unwrap();

        // Verify deleted
        let configs = handler.push_config_list(task_id).await.unwrap();
        assert!(configs.is_empty());
    }

    #[tokio::test]
    async fn push_config_get_not_found() {
        let handler = make_handler();
        let msg = make_test_message();
        let task = handler.message_send(msg).await.unwrap();

        let err = handler.push_config_get(&task.id.0, "nonexistent").await.unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn push_config_delete_not_found() {
        let handler = make_handler();
        let msg = make_test_message();
        let task = handler.message_send(msg).await.unwrap();

        let err = handler.push_config_delete(&task.id.0, "nonexistent").await.unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn push_config_create_task_not_found() {
        let handler = make_handler();
        let config = TaskPushNotificationConfig::new("nonexistent", "https://example.com/hook");
        let err = handler.push_config_create("nonexistent", config).await.unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    // ── agent_card_extended ──────────────────────────────────────────────

    #[tokio::test]
    async fn agent_card_extended_returns_card() {
        let handler = make_handler();
        let card = handler.agent_card_extended().await.unwrap();
        assert_eq!(card.name, "test-agent");
        assert_eq!(card.version, "1.0.0");
        assert_eq!(card.supported_interfaces.len(), 1);
    }

    // ── message_stream ───────────────────────────────────────────────────

    #[tokio::test]
    async fn message_stream_yields_events() {
        use a2a_protocol_types::events::StreamResponse;

        let handler = make_handler();
        let mut msg = make_test_message();
        msg.id = MessageId::new("msg-stream-test");

        let mut stream = handler.message_stream(msg).await.unwrap();

        // First event should be a Task object
        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(first, StreamResponse::Task(_)), "first event should be Task");

        // Should yield WORKING event
        let event1 = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            event1,
            StreamResponse::StatusUpdate(ref e) if e.status.state == TaskState::Working
        ));

        // Should yield COMPLETED event
        let event2 = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            event2,
            StreamResponse::StatusUpdate(ref e) if e.status.state == TaskState::Completed
        ));

        // Stream should end
        assert!(stream.next().await.is_none());
    }

    // ── input validation ─────────────────────────────────────────────────

    #[tokio::test]
    async fn message_send_rejects_empty_parts() {
        let handler = make_handler();
        let mut msg = make_test_message();
        msg.parts = vec![];
        let err = handler.message_send(msg).await.unwrap_err();
        assert!(matches!(err, A2aError::InvalidParams { .. }));
        assert!(err.to_string().contains("at least one part"));
    }

    #[tokio::test]
    async fn message_send_rejects_empty_message_id() {
        let handler = make_handler();
        let mut msg = make_test_message();
        msg.id = MessageId::new("");
        let err = handler.message_send(msg).await.unwrap_err();
        assert!(matches!(err, A2aError::InvalidParams { .. }));
        assert!(err.to_string().contains("messageId"));
    }

    #[tokio::test]
    async fn message_send_rejects_whitespace_message_id() {
        let handler = make_handler();
        let mut msg = make_test_message();
        msg.id = MessageId::new("   ");
        let err = handler.message_send(msg).await.unwrap_err();
        assert!(matches!(err, A2aError::InvalidParams { .. }));
    }

    #[tokio::test]
    async fn message_send_rejects_long_message_id() {
        let handler = make_handler();
        let mut msg = make_test_message();
        msg.id = MessageId::new("x".repeat(257));
        let err = handler.message_send(msg).await.unwrap_err();
        assert!(matches!(err, A2aError::InvalidParams { .. }));
        assert!(err.to_string().contains("256"));
    }

    #[tokio::test]
    async fn tasks_get_rejects_empty_task_id() {
        let handler = make_handler();
        let err = handler.tasks_get("", None).await.unwrap_err();
        assert!(matches!(err, A2aError::InvalidParams { .. }));
        assert!(err.to_string().contains("taskId"));
    }

    #[tokio::test]
    async fn tasks_get_rejects_long_task_id() {
        let handler = make_handler();
        let long_id = "x".repeat(257);
        let err = handler.tasks_get(&long_id, None).await.unwrap_err();
        assert!(matches!(err, A2aError::InvalidParams { .. }));
    }

    #[tokio::test]
    async fn tasks_cancel_rejects_empty_task_id() {
        let handler = make_handler();
        let err = handler.tasks_cancel("").await.unwrap_err();
        assert!(matches!(err, A2aError::InvalidParams { .. }));
    }

    #[tokio::test]
    async fn message_send_rejects_oversized_metadata() {
        let handler = make_handler();
        let mut msg = make_test_message();
        // Create metadata > 64KB
        let big_value = "x".repeat(70_000);
        msg.metadata = Some(serde_json::json!({"big": big_value}));
        let err = handler.message_send(msg).await.unwrap_err();
        assert!(matches!(err, A2aError::InvalidParams { .. }));
        assert!(err.to_string().contains("64 KB"));
    }

    // ── idempotency ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn message_send_idempotent_same_message_id() {
        let handler = make_handler();
        let msg1 = make_test_message();
        let msg2 = make_test_message(); // same messageId "msg-1"

        let task1 = handler.message_send(msg1).await.unwrap();
        let task2 = handler.message_send(msg2).await.unwrap();

        assert_eq!(task1.id, task2.id, "same messageId should return same task");
    }

    #[tokio::test]
    async fn message_send_different_message_id_creates_new_task() {
        let handler = make_handler();
        let msg1 = make_test_message();
        let mut msg2 = make_test_message();
        msg2.id = MessageId::new("msg-2");

        let task1 = handler.message_send(msg1).await.unwrap();
        let task2 = handler.message_send(msg2).await.unwrap();

        assert_ne!(task1.id, task2.id, "different messageId should create different tasks");
    }

    #[tokio::test]
    async fn message_stream_idempotent_returns_existing() {
        use a2a_protocol_types::events::StreamResponse;

        let handler = make_handler();
        let msg1 = make_test_message();

        // First call creates the task via message_send
        let task = handler.message_send(msg1).await.unwrap();

        // Second call via message_stream with same messageId should return existing
        let msg2 = make_test_message(); // same messageId "msg-1"
        let mut stream = handler.message_stream(msg2).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        match first {
            StreamResponse::Task(t) => assert_eq!(t.id.0, task.id.0),
            other => panic!("expected Task variant, got {other:?}"),
        }
    }

    // ── multi-turn resume ────────────────────────────────────────────────

    #[tokio::test]
    async fn message_send_resumes_input_required_task() {
        let handler = make_handler();

        // Create a task and transition it to INPUT_REQUIRED
        let task_id = "resume-test";
        let ctx_id = "ctx-resume";
        handler.executor.create_task(task_id, ctx_id).await.unwrap();
        handler.executor.transition_state(task_id, ctx_id, TaskState::Working, None).await.unwrap();
        handler
            .executor
            .transition_state(task_id, ctx_id, TaskState::InputRequired, None)
            .await
            .unwrap();

        // Send a follow-up message with the same contextId
        let mut msg = make_test_message();
        msg.id = MessageId::new("msg-resume");
        msg.context_id = Some(a2a_protocol_types::ContextId::new(ctx_id));

        let task = handler.message_send(msg).await.unwrap();

        // Should resume the existing task (same task ID)
        assert_eq!(task.id.0, task_id);
        assert_eq!(task.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn message_send_creates_new_task_for_terminal_context() {
        let handler = make_handler();

        // Create a completed task
        let mut msg1 = make_test_message();
        msg1.id = MessageId::new("msg-terminal-1");
        msg1.context_id = Some(a2a_protocol_types::ContextId::new("ctx-terminal"));
        let task1 = handler.message_send(msg1).await.unwrap();
        assert_eq!(task1.status.state, TaskState::Completed);

        // Send another message with the same contextId — should create a new task
        let mut msg2 = make_test_message();
        msg2.id = MessageId::new("msg-terminal-2");
        msg2.context_id = Some(a2a_protocol_types::ContextId::new("ctx-terminal"));
        let task2 = handler.message_send(msg2).await.unwrap();

        assert_ne!(task1.id, task2.id, "terminal context should create new task");
    }

    #[tokio::test]
    async fn message_send_creates_new_task_without_context_id() {
        let handler = make_handler();
        let mut msg = make_test_message();
        msg.id = MessageId::new("msg-no-ctx");
        msg.context_id = None;

        let task = handler.message_send(msg).await.unwrap();
        assert_eq!(task.status.state, TaskState::Completed);
    }

    // ── Caller scoping ─────────────────────────────────────────────────────
    //
    // The session user was `a2a-{contextId}` and every operation looked tasks up by ID
    // alone, so an authenticated caller could reach another caller's session through its
    // contextId and read, cancel, or reconfigure its tasks through their IDs.

    fn caller(user_id: &str) -> Option<RequestContext> {
        Some(RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }

    fn message_in(message_id: &str, context_id: &str) -> Message {
        let mut msg = make_test_message();
        msg.id = MessageId::new(message_id);
        msg.context_id = Some(a2a_protocol_types::ContextId::new(context_id));
        msg
    }

    fn is_not_found<T>(result: Result<T, A2aError>) -> bool {
        matches!(result, Err(A2aError::TaskNotFound { .. }))
    }

    #[tokio::test]
    async fn another_callers_task_is_reported_as_missing() {
        let handler = make_handler();
        handler.executor.create_task_for("bob-task", "bob-ctx", Some("bob")).await.unwrap();
        handler
            .executor
            .transition_state("bob-task", "bob-ctx", TaskState::Working, None)
            .await
            .unwrap();
        let hook = TaskPushNotificationConfig::new("bob-task", "https://example.com/hook");

        for scope in [handler.for_caller(caller("alice")), handler.for_caller(None)] {
            assert!(is_not_found(scope.tasks_get("bob-task", None).await));
            assert!(is_not_found(scope.tasks_cancel("bob-task").await));
            assert!(is_not_found(scope.tasks_subscribe("bob-task").await));
            assert!(is_not_found(scope.push_config_create("bob-task", hook.clone()).await));
            assert!(is_not_found(scope.push_config_list("bob-task").await));
            assert!(scope.tasks_list(ListTasksParams::default()).await.unwrap().tasks.is_empty());
        }

        let bob = handler.for_caller(caller("bob"));
        assert_eq!(bob.tasks_get("bob-task", None).await.unwrap().status.state, TaskState::Working);
        assert_eq!(bob.tasks_list(ListTasksParams::default()).await.unwrap().tasks.len(), 1);
        assert_eq!(bob.tasks_cancel("bob-task").await.unwrap().status.state, TaskState::Canceled);
    }

    #[tokio::test]
    async fn a_reused_message_id_does_not_return_another_callers_task() {
        let handler = make_handler();
        let bobs = handler
            .for_caller(caller("bob"))
            .message_send(message_in("shared-msg", "bob-ctx"))
            .await
            .unwrap();

        let alices = handler
            .for_caller(caller("alice"))
            .message_send(message_in("shared-msg", "alice-ctx"))
            .await
            .unwrap();
        assert_ne!(alices.id, bobs.id);
        assert_eq!(alices.context_id.0, "alice-ctx");

        let bob_retry = handler
            .for_caller(caller("bob"))
            .message_send(message_in("shared-msg", "bob-ctx"))
            .await
            .unwrap();
        assert_eq!(bob_retry.id, bobs.id, "bob's own retry still deduplicates");
    }

    #[tokio::test]
    async fn another_callers_input_required_task_is_not_resumed() {
        let handler = make_handler();
        handler.executor.create_task_for("bob-task", "shared-ctx", Some("bob")).await.unwrap();
        for state in [TaskState::Working, TaskState::InputRequired] {
            handler.executor.transition_state("bob-task", "shared-ctx", state, None).await.unwrap();
        }

        let alices = handler
            .for_caller(caller("alice"))
            .message_send(message_in("alice-msg", "shared-ctx"))
            .await
            .unwrap();
        assert_ne!(alices.id.0, "bob-task");

        let bobs = handler.for_caller(caller("bob")).tasks_get("bob-task", None).await.unwrap();
        assert_eq!(bobs.status.state, TaskState::InputRequired);
        assert_eq!(bobs.history, None, "alice's message must not join bob's task");
    }

    #[tokio::test]
    async fn the_session_belongs_to_the_authenticated_caller() {
        let sessions = Arc::new(adk_session::InMemorySessionService::new());
        let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
        let runner_config = Arc::new(
            adk_runner::Runner::builder()
                .app_name("a2a-identity")
                .agent(Arc::new(ChunkingAgent { chunks: vec!["ok".into()] }))
                .session_service(sessions.clone())
                .build_config(),
        );
        let handler = RequestHandler::with_runner(
            Arc::new(V1Executor::new(store.clone())),
            store,
            Arc::new(NoOpPushNotificationSender),
            Arc::new(RwLock::new(CachedAgentCard::new(make_test_agent_card()))),
            runner_config,
        );

        handler.for_caller(caller("alice")).message_send(message_in("m1", "ctx-1")).await.unwrap();

        use adk_session::SessionService;
        let session_for = |user_id: &str| {
            sessions.get(adk_session::GetRequest {
                app_name: "a2a-identity".to_string(),
                user_id: user_id.to_string(),
                session_id: "ctx-1".to_string(),
                num_recent_events: None,
                after: None,
            })
        };
        assert!(session_for("alice").await.is_ok());
        assert!(
            session_for("a2a-ctx-1").await.is_err(),
            "the client-chosen context ID must not name the session user"
        );
    }

    #[tokio::test]
    async fn the_owner_is_not_sent_on_the_wire() {
        let handler = make_handler();
        let alice = handler.for_caller(caller("alice"));

        let task = alice.message_send(message_in("m-send", "ctx")).await.unwrap();
        assert_eq!(task.metadata, None);
        let entry = handler.task_store.get_task(&task.id.0).await.unwrap();
        assert_eq!(entry.owner(), Some("alice"));

        let events: Vec<_> =
            alice.message_stream(message_in("m-stream", "ctx")).await.unwrap().collect().await;
        for event in events {
            match event.unwrap() {
                StreamResponse::Task(task) => assert_eq!(task.metadata, None),
                StreamResponse::StatusUpdate(update) => assert_eq!(update.metadata, None),
                StreamResponse::ArtifactUpdate(update) => assert_eq!(update.metadata, None),
                StreamResponse::Message(message) => assert_eq!(message.metadata, None),
                // `StreamResponse` is `#[non_exhaustive]`; nothing else carries task metadata.
                _ => {}
            }
        }
    }
}
