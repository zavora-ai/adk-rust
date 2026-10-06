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
use std::sync::Arc;

use a2a_protocol_types::artifact::{Artifact, ArtifactId};
use a2a_protocol_types::events::{StreamResponse, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a_protocol_types::task::{Task, TaskState};
use a2a_protocol_types::{AgentCard, Message, TaskPushNotificationConfig};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::RwLock;

use super::card::CachedAgentCard;
use super::convert::internal_task_to_wire;
use super::error::A2aError;
use super::executor::V1Executor;
use super::push::PushNotificationSender;
use super::stream::wrap_artifact_event;
use super::task_store::{ListTasksParams, TaskStore, TaskStoreEntry};
use crate::auth_bridge::RequestContext;

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
    /// Returns an error if the task store fails.
    pub async fn tasks_list(&self, params: ListTasksParams) -> Result<Vec<Task>, A2aError> {
        self.for_caller(None).tasks_list(params).await
    }

    /// Snapshots an ownerless task; see [`CallerScope::tasks_subscribe`].
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
    async fn start_agent(
        runner_config: &Arc<adk_runner::RunnerConfig>,
        caller: Option<&RequestContext>,
        context_id: &str,
        msg: &Message,
    ) -> Result<adk_core::EventStream, A2aError> {
        use adk_core::{SessionId, UserId};
        use adk_session::{CreateRequest, GetRequest};

        let app_name = &runner_config.app_name;
        // The context ID is chosen by the client, so it only names the session user when
        // no authentication is configured.
        let user_id =
            caller.map_or_else(|| format!("a2a-{context_id}"), |context| context.user_id.clone());
        let session_id = context_id.to_string();

        // Ensure session exists
        let session_service = &runner_config.session_service;
        let get_result = session_service
            .get(GetRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await;

        if get_result.is_err() {
            session_service
                .create(CreateRequest {
                    app_name: app_name.clone(),
                    user_id: user_id.clone(),
                    session_id: Some(session_id.clone()),
                    state: std::collections::HashMap::new(),
                })
                .await
                .map_err(|e| A2aError::Internal { message: format!("session create: {e}") })?;
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
        if let Some(ref cancellation_token) = runner_config.cancellation_token {
            runner_builder = runner_builder.cancellation_token(cancellation_token.clone());
        }
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
    /// Returns [`A2aError::ExtendedAgentCardNotConfigured`] if no card is set.
    pub async fn agent_card_extended(&self) -> Result<AgentCard, A2aError> {
        let cached = self.agent_card.read().await;
        Ok(cached.card.clone())
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

    /// Sends a message, creating a task and processing it through the executor.
    ///
    /// When a runner config is present, invokes the agent through the ADK
    /// Runner for real LLM generation. The LLM response is recorded as an
    /// artifact on the task. Without a runner, performs state transitions only.
    ///
    /// A message whose `contextId` matches one of this caller's `INPUT_REQUIRED`
    /// tasks resumes that task; another caller's task with the same `contextId` is
    /// never resumed.
    ///
    /// # Errors
    ///
    /// Returns an error if task creation, state transitions, or store
    /// operations fail.
    pub async fn message_send(&self, msg: Message) -> Result<Task, A2aError> {
        validate_message(&msg)?;
        let handler = self.handler;

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

        // Multi-turn resume: check if contextId matches one of this caller's INPUT_REQUIRED tasks
        if let Some(ref ctx_id) = msg.context_id
            && let Some(existing) = handler.task_store.find_task_by_context(&ctx_id.0).await?
            && existing.status.state == TaskState::InputRequired
            && existing.owner() == self.owner()
        {
            // Resume the existing task
            let task_id = existing.id.clone();
            let context_id = existing.context_id.clone();

            // Transition from INPUT_REQUIRED to Working
            handler
                .executor
                .transition_state(&task_id, &context_id, TaskState::Working, None)
                .await?;

            // Append the new message to history
            handler.task_store.add_history_message(&task_id, msg.clone()).await?;

            // Run the agent if a runner config is available
            if let Some(runner_config) = &handler.runner_config {
                match self.run_agent(runner_config, &task_id, &context_id, &msg).await {
                    Ok(()) => {}
                    Err(e) => {
                        let _ =
                            handler.executor.fail_task(&task_id, &context_id, &e.to_string()).await;
                        let entry = handler.task_store.get_task(&task_id).await?;
                        return internal_task_to_wire(&entry);
                    }
                }
            }

            // Transition to COMPLETED
            handler
                .executor
                .transition_state(&task_id, &context_id, TaskState::Completed, None)
                .await?;

            // Record idempotency mapping
            handler.idempotency_map.write().await.insert(idempotency_key, task_id.clone());

            let entry = handler.task_store.get_task(&task_id).await?;
            return internal_task_to_wire(&entry);
        }
        // If task is in a terminal state or other non-INPUT_REQUIRED state,
        // fall through to create a new task (existing behavior)

        let task_id = uuid::Uuid::new_v4().to_string();
        let context_id = msg
            .context_id
            .as_ref()
            .map(|c| c.0.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Create task in SUBMITTED state
        handler.executor.create_task_for(&task_id, &context_id, self.owner()).await?;

        // Add the incoming message to history
        handler.task_store.add_history_message(&task_id, msg.clone()).await?;

        // Transition to WORKING
        handler.executor.transition_state(&task_id, &context_id, TaskState::Working, None).await?;

        // Run the agent if a runner config is available
        if let Some(runner_config) = &handler.runner_config {
            match self.run_agent(runner_config, &task_id, &context_id, &msg).await {
                Ok(()) => {}
                Err(e) => {
                    // Transition to FAILED on error
                    let _ = handler.executor.fail_task(&task_id, &context_id, &e.to_string()).await;
                    let entry = handler.task_store.get_task(&task_id).await?;
                    return internal_task_to_wire(&entry);
                }
            }
        }

        // Transition to COMPLETED
        handler
            .executor
            .transition_state(&task_id, &context_id, TaskState::Completed, None)
            .await?;

        // Record idempotency mapping
        handler.idempotency_map.write().await.insert(idempotency_key, task_id.clone());

        // Retrieve and return the final task
        let entry = handler.task_store.get_task(&task_id).await?;
        internal_task_to_wire(&entry)
    }

    /// Runs the agent to completion and records its output as a single artifact.
    async fn run_agent(
        &self,
        runner_config: &Arc<adk_runner::RunnerConfig>,
        task_id: &str,
        context_id: &str,
        msg: &Message,
    ) -> Result<(), A2aError> {
        let mut event_stream =
            RequestHandler::start_agent(runner_config, self.caller.as_ref(), context_id, msg)
                .await?;

        let mut response_text = String::new();
        while let Some(result) = event_stream.next().await {
            match result {
                Ok(event) => {
                    if let Some(content) = &event.llm_response.content {
                        for part in &content.parts {
                            if let Some(text) = part.text() {
                                response_text.push_str(text);
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(A2aError::Internal { message: format!("agent error: {e}") });
                }
            }
        }

        if !response_text.is_empty() {
            let artifact = Artifact::new(
                ArtifactId::new(uuid::Uuid::new_v4().to_string()),
                vec![a2a_protocol_types::Part::text(&response_text)],
            );
            self.handler.executor.record_artifact(task_id, context_id, artifact).await?;
        }

        Ok(())
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
    /// | stream ends | `TaskStatusUpdateEvent` — `Completed` |
    /// | stream errors | `TaskStatusUpdateEvent` — `Failed` |
    ///
    /// Chunks of one response share an artifact ID so a client can reassemble them, which is the
    /// same contract adk-python and adk-go implement over their A2A SDKs.
    ///
    /// With no runner configured the task is created and completed without agent output, which
    /// keeps discovery-only deployments working.
    ///
    /// # Errors
    ///
    /// Returns an error if task creation fails. Failures once streaming has begun are reported
    /// as a `Failed` status event, since the response has already started.
    pub async fn message_stream(
        &self,
        msg: Message,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
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

        let task_id = uuid::Uuid::new_v4().to_string();
        let context_id = msg
            .context_id
            .as_ref()
            .map(|c| c.0.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Create task
        handler.executor.create_task_for(&task_id, &context_id, self.owner()).await?;

        // Keep a copy for the agent: `add_history_message` takes ownership.
        let msg_for_agent = msg.clone();

        // Add the incoming message to history
        handler.task_store.add_history_message(&task_id, msg).await?;

        // Record idempotency mapping
        handler.idempotency_map.write().await.insert(idempotency_key, task_id.clone());

        // Get the task entry for the first SSE event
        let task_entry = handler.task_store.get_task(&task_id).await?;
        let first_task = internal_task_to_wire(&task_entry)?;

        let executor = handler.executor.clone();
        let tid = task_id.clone();
        let cid = context_id.clone();
        let runner_config = handler.runner_config.clone();
        let caller = self.caller.clone();
        let stream_msg = msg_for_agent;

        let stream = async_stream::stream! {
            yield Ok(StreamResponse::Task(first_task));

            match executor.transition_state(&tid, &cid, TaskState::Working, None).await {
                Ok(event) => yield Ok(StreamResponse::StatusUpdate(event)),
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }

            if let Some(config) = runner_config {
                let started =
                    RequestHandler::start_agent(&config, caller.as_ref(), &cid, &stream_msg).await;
                let mut event_stream = match started {
                    Ok(stream) => stream,
                    Err(e) => {
                        // The agent never started, so nothing partial was emitted. Report the
                        // task as failed rather than completing a task that did no work.
                        if let Ok(event) = executor
                            .transition_state(&tid, &cid, TaskState::Failed, None)
                            .await
                        {
                            yield Ok(StreamResponse::StatusUpdate(event));
                        }
                        yield Err(e);
                        return;
                    }
                };

                // One artifact ID for the whole response so a client can join the chunks.
                let artifact_id = uuid::Uuid::new_v4().to_string();
                let mut chunks_sent = false;
                let mut full_text = String::new();

                while let Some(result) = event_stream.next().await {
                    match result {
                        Ok(event) => {
                            let Some(content) = &event.llm_response.content else { continue };
                            let text: String =
                                content.parts.iter().filter_map(|p| p.text()).collect();
                            if text.is_empty() {
                                continue;
                            }
                            full_text.push_str(&text);

                            let partial = event.llm_response.partial;
                            let artifact = Artifact::new(
                                ArtifactId::new(artifact_id.clone()),
                                vec![a2a_protocol_types::Part::text(&text)],
                            );
                            yield Ok(wrap_artifact_event(TaskArtifactUpdateEvent {
                                task_id: a2a_protocol_types::TaskId::new(tid.clone()),
                                context_id: a2a_protocol_types::ContextId::new(cid.clone()),
                                artifact,
                                append: Some(chunks_sent),
                                last_chunk: Some(!partial),
                                metadata: None,
                            }));
                            chunks_sent = true;
                        }
                        Err(e) => {
                            if let Ok(event) = executor
                                .transition_state(&tid, &cid, TaskState::Failed, None)
                                .await
                            {
                                yield Ok(StreamResponse::StatusUpdate(event));
                            }
                            yield Err(A2aError::Internal {
                                message: format!("agent error: {e}"),
                            });
                            return;
                        }
                    }
                }

                // Persist the joined text so `tasks/get` returns the same artifact a
                // non-streaming caller would have received.
                if !full_text.is_empty() {
                    let artifact = Artifact::new(
                        ArtifactId::new(artifact_id),
                        vec![a2a_protocol_types::Part::text(&full_text)],
                    );
                    if let Err(e) = executor.record_artifact(&tid, &cid, artifact).await {
                        yield Err(e);
                        return;
                    }
                }
            }

            match executor.transition_state(&tid, &cid, TaskState::Completed, None).await {
                Ok(event) => yield Ok(StreamResponse::StatusUpdate(event)),
                Err(e) => yield Err(e),
            }
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

    /// Cancels one of this caller's tasks by transitioning it to CANCELED state.
    ///
    /// Validates that the task is not already in a terminal state before
    /// canceling.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or belongs to
    /// another caller, or [`A2aError::TaskNotCancelable`] if the task is in a
    /// terminal state.
    pub async fn tasks_cancel(&self, task_id: &str) -> Result<Task, A2aError> {
        validate_id(task_id, "taskId")?;
        let entry = self.owned_task(task_id).await?;

        // Check if task is in a terminal state
        if is_terminal_state(entry.status.state) {
            return Err(A2aError::TaskNotCancelable {
                task_id: task_id.to_string(),
                current_state: format!("{:?}", entry.status.state),
            });
        }

        // Transition to CANCELED via the executor (validates state machine)
        self.handler
            .executor
            .transition_state(task_id, &entry.context_id, TaskState::Canceled, None)
            .await?;

        // Return the updated task
        let updated = self.handler.task_store.get_task(task_id).await?;
        internal_task_to_wire(&updated)
    }

    /// Lists this caller's tasks matching the given parameters.
    ///
    /// Supports filtering by context_id, state, and pagination via page_size.
    /// `page_size` applies after the owner filter.
    ///
    /// # Errors
    ///
    /// Returns an error if the task store fails.
    pub async fn tasks_list(&self, params: ListTasksParams) -> Result<Vec<Task>, A2aError> {
        let page_size = params.page_size;
        let unpaged = ListTasksParams { page_size: None, ..params };
        let mut entries: Vec<TaskStoreEntry> = self
            .handler
            .task_store
            .list_tasks(unpaged)
            .await?
            .into_iter()
            .filter(|entry| entry.owner() == self.owner())
            .collect();
        if let Some(page_size) = page_size {
            entries.truncate(page_size as usize);
        }
        entries.iter().map(internal_task_to_wire).collect()
    }

    /// Returns the current state of one of this caller's tasks as a short stream,
    /// then closes.
    ///
    /// > **Important:** this is a point-in-time snapshot, not a live subscription. It emits the
    /// > task and its current status and then ends; it does not deliver subsequent updates. A
    /// > client that needs live updates should use `message/stream`, which streams the agent's
    /// > events as they are produced.
    ///
    /// A real re-attach would require a per-task event queue that outlives the request, which
    /// the A2A SDKs in adk-python and adk-go provide and this hand-rolled server does not yet.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or belongs to
    /// another caller, or [`A2aError::TaskNotCancelable`] if the task already
    /// reached a terminal state.
    pub async fn tasks_subscribe(
        &self,
        task_id: &str,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        let entry = self.owned_task(task_id).await?;

        // For terminal tasks, return an error — can't subscribe to completed tasks
        if is_terminal_state(entry.status.state) {
            return Err(A2aError::TaskNotCancelable {
                task_id: task_id.to_string(),
                current_state: format!("{:?}", entry.status.state),
            });
        }

        let task = internal_task_to_wire(&entry)?;
        let status_event = TaskStatusUpdateEvent {
            task_id: a2a_protocol_types::TaskId::new(task_id),
            context_id: a2a_protocol_types::ContextId::new(&entry.context_id),
            status: entry.status.clone(),
            metadata: None,
        };

        let stream = futures::stream::iter(vec![
            Ok(StreamResponse::Task(task)),
            Ok(StreamResponse::StatusUpdate(status_event)),
        ]);
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
        // Verify task exists
        let mut entry = self.owned_task(task_id).await?;

        // Assign a server-generated config ID if not present
        if config.id.is_none() {
            config.id = Some(uuid::Uuid::new_v4().to_string());
        }
        config.task_id = task_id.to_string();

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
    async fn stream_failure_does_not_report_a_completed_task() {
        let (handler, _store) = handler_with_agent(Arc::new(FailingAgent));

        let mut stream = handler.message_stream(make_test_message()).await.expect("stream starts");
        let mut states = Vec::new();
        let mut saw_error = false;
        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamResponse::StatusUpdate(e)) => states.push(e.status.state),
                Ok(_) => {}
                Err(_) => saw_error = true,
            }
        }

        assert!(saw_error, "the agent error must surface to the caller");
        assert!(
            states.contains(&TaskState::Failed),
            "terminal state must be Failed, got {states:?}"
        );
        assert!(
            !states.contains(&TaskState::Completed),
            "a failed run must never report Completed: {states:?}"
        );
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
        assert_eq!(tasks.len(), 2);
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
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].context_id.0, "ctx-a");
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
        assert_eq!(tasks.len(), 2);
    }

    #[tokio::test]
    async fn tasks_list_empty() {
        let handler = make_handler();
        let tasks = handler.tasks_list(ListTasksParams::default()).await.unwrap();
        assert!(tasks.is_empty());
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
            assert!(scope.tasks_list(ListTasksParams::default()).await.unwrap().is_empty());
        }

        let bob = handler.for_caller(caller("bob"));
        assert_eq!(bob.tasks_get("bob-task", None).await.unwrap().status.state, TaskState::Working);
        assert_eq!(bob.tasks_list(ListTasksParams::default()).await.unwrap().len(), 1);
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
