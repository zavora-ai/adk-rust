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

use std::collections::HashMap;
use std::sync::Arc;

use a2a_protocol_types::artifact::{Artifact, ArtifactId};
use a2a_protocol_types::events::{StreamResponse, TaskStatusUpdateEvent};
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
use super::task_store::{ListTasksParams, TaskStore};

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

/// Shared dispatch layer for A2A v1.0.0 operations.
///
/// Maps operation names to executor/store calls. Used by both the JSON-RPC
/// handler and the REST handler.
///
/// When constructed with a [`adk_runner::RunnerConfig`] via [`RequestHandler::with_runner`],
/// `message_send` and `message_stream` invoke the agent through the ADK Runner
/// for real LLM generation. Without a runner config, they perform state
/// transitions only (useful for protocol-level testing).
pub struct RequestHandler {
    executor: Arc<V1Executor>,
    task_store: Arc<dyn TaskStore>,
    #[allow(dead_code)] // Used by push notification delivery in task 7.3
    push_sender: Arc<dyn PushNotificationSender>,
    agent_card: Arc<RwLock<CachedAgentCard>>,
    runner_config: Option<Arc<adk_runner::RunnerConfig>>,
    /// messageId → taskId mapping for idempotent request handling.
    idempotency_map: RwLock<HashMap<String, String>>,
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

    /// Sends a message, creating a task and processing it through the executor.
    ///
    /// When a runner config is present, invokes the agent through the ADK
    /// Runner for real LLM generation. The LLM response is recorded as an
    /// artifact on the task. Without a runner, performs state transitions only.
    ///
    /// # Errors
    ///
    /// Returns an error if task creation, state transitions, or store
    /// operations fail.
    pub async fn message_send(&self, msg: Message) -> Result<Task, A2aError> {
        validate_message(&msg)?;

        // Idempotency check
        let message_id = msg.id.0.clone();
        {
            let map = self.idempotency_map.read().await;
            if let Some(existing_task_id) = map.get(&message_id) {
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
                self.idempotency_map.write().await.remove(&message_id);
            }
        }

        // Multi-turn resume: check if contextId matches an existing INPUT_REQUIRED task
        if let Some(ref ctx_id) = msg.context_id {
            if let Some(existing) = self.task_store.find_task_by_context(&ctx_id.0).await? {
                if existing.status.state == TaskState::InputRequired {
                    // Resume the existing task
                    let task_id = existing.id.clone();
                    let context_id = existing.context_id.clone();

                    // Transition from INPUT_REQUIRED to Working
                    self.executor
                        .transition_state(&task_id, &context_id, TaskState::Working, None)
                        .await?;

                    // Append the new message to history
                    self.task_store.add_history_message(&task_id, msg.clone()).await?;

                    // Run the agent if a runner config is available
                    if let Some(runner_config) = &self.runner_config {
                        match self.run_agent(runner_config, &task_id, &context_id, &msg).await {
                            Ok(()) => {}
                            Err(e) => {
                                let _ = self
                                    .executor
                                    .fail_task(&task_id, &context_id, &e.to_string())
                                    .await;
                                let entry = self.task_store.get_task(&task_id).await?;
                                return internal_task_to_wire(&entry);
                            }
                        }
                    }

                    // Transition to COMPLETED
                    self.executor
                        .transition_state(&task_id, &context_id, TaskState::Completed, None)
                        .await?;

                    // Record idempotency mapping
                    self.idempotency_map.write().await.insert(message_id, task_id.clone());

                    let entry = self.task_store.get_task(&task_id).await?;
                    return internal_task_to_wire(&entry);
                }
                // If task is in a terminal state or other non-INPUT_REQUIRED state,
                // fall through to create a new task (existing behavior)
            }
        }

        let task_id = uuid::Uuid::new_v4().to_string();
        let context_id = msg
            .context_id
            .as_ref()
            .map(|c| c.0.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Create task in SUBMITTED state
        self.executor.create_task(&task_id, &context_id).await?;

        // Add the incoming message to history
        self.task_store.add_history_message(&task_id, msg.clone()).await?;

        // Transition to WORKING
        self.executor.transition_state(&task_id, &context_id, TaskState::Working, None).await?;

        // Run the agent if a runner config is available
        if let Some(runner_config) = &self.runner_config {
            match self.run_agent(runner_config, &task_id, &context_id, &msg).await {
                Ok(()) => {}
                Err(e) => {
                    // Transition to FAILED on error
                    let _ = self.executor.fail_task(&task_id, &context_id, &e.to_string()).await;
                    let entry = self.task_store.get_task(&task_id).await?;
                    return internal_task_to_wire(&entry);
                }
            }
        }

        // Transition to COMPLETED
        self.executor.transition_state(&task_id, &context_id, TaskState::Completed, None).await?;

        // Record idempotency mapping
        self.idempotency_map.write().await.insert(message_id, task_id.clone());

        // Retrieve and return the final task
        let entry = self.task_store.get_task(&task_id).await?;
        internal_task_to_wire(&entry)
    }

    /// Runs the agent through the ADK Runner and records the response as an artifact.
    async fn run_agent(
        &self,
        runner_config: &Arc<adk_runner::RunnerConfig>,
        task_id: &str,
        context_id: &str,
        msg: &Message,
    ) -> Result<(), A2aError> {
        use adk_core::{SessionId, UserId};
        use adk_session::{CreateRequest, GetRequest};

        let app_name = &runner_config.app_name;
        let user_id = format!("a2a-{context_id}");
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
        let runner = adk_runner::Runner::new(adk_runner::RunnerConfig {
            app_name: runner_config.app_name.clone(),
            agent: runner_config.agent.clone(),
            session_service: runner_config.session_service.clone(),
            artifact_service: runner_config.artifact_service.clone(),
            memory_service: runner_config.memory_service.clone(),
            plugin_manager: runner_config.plugin_manager.clone(),
            run_config: runner_config.run_config.clone(),
            compaction_config: runner_config.compaction_config.clone(),
            context_cache_config: runner_config.context_cache_config.clone(),
            cache_capable: runner_config.cache_capable.clone(),
            request_context: runner_config.request_context.clone(),
            cancellation_token: runner_config.cancellation_token.clone(),
        })
        .map_err(|e| A2aError::Internal { message: format!("runner create: {e}") })?;

        let mut event_stream = runner
            .run(
                UserId::new(&user_id).map_err(|e| A2aError::Internal { message: e.to_string() })?,
                SessionId::new(&session_id)
                    .map_err(|e| A2aError::Internal { message: e.to_string() })?,
                content,
            )
            .await
            .map_err(|e| A2aError::Internal { message: format!("runner run: {e}") })?;

        // Collect LLM response text from events
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

        // Record the response as an artifact if we got any text
        if !response_text.is_empty() {
            let artifact = Artifact::new(
                ArtifactId::new(uuid::Uuid::new_v4().to_string()),
                vec![a2a_protocol_types::Part::text(&response_text)],
            );
            self.executor.record_artifact(task_id, &context_id, artifact).await?;
        }

        Ok(())
    }

    /// Sends a streaming message, returning a stream of SSE events.
    ///
    /// Creates a task, yields status update events as the task progresses.
    /// This is a placeholder — actual Runner streaming integration comes later.
    ///
    /// # Errors
    ///
    /// Returns an error if task creation fails.
    pub async fn message_stream(
        &self,
        msg: Message,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        validate_message(&msg)?;

        // Idempotency check — return existing task as single-element stream
        let message_id = msg.id.0.clone();
        {
            let map = self.idempotency_map.read().await;
            if let Some(existing_task_id) = map.get(&message_id) {
                match self.task_store.get_task(existing_task_id).await {
                    Ok(entry) => {
                        let task = internal_task_to_wire(&entry)?;
                        let stream =
                            futures::stream::once(async move { Ok(StreamResponse::Task(task)) });
                        return Ok(stream.boxed());
                    }
                    Err(A2aError::TaskNotFound { .. }) => {
                        // Stale entry — remove and process as new
                        drop(map);
                        self.idempotency_map.write().await.remove(&message_id);
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
        self.executor.create_task(&task_id, &context_id).await?;

        // Add the incoming message to history
        self.task_store.add_history_message(&task_id, msg).await?;

        // Record idempotency mapping
        self.idempotency_map.write().await.insert(message_id, task_id.clone());

        // Get the task entry for the first SSE event
        let task_entry = self.task_store.get_task(&task_id).await?;
        let first_task = internal_task_to_wire(&task_entry)?;

        let executor = self.executor.clone();
        let tid = task_id.clone();
        let cid = context_id.clone();

        let stream = async_stream::stream! {
            // Emit Task as first SSE event
            yield Ok(StreamResponse::Task(first_task));

            // Transition to WORKING
            match executor.transition_state(&tid, &cid, TaskState::Working, None).await {
                Ok(event) => yield Ok(StreamResponse::StatusUpdate(event)),
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }

            // Transition to COMPLETED (placeholder — Runner integration later)
            match executor.transition_state(&tid, &cid, TaskState::Completed, None).await {
                Ok(event) => yield Ok(StreamResponse::StatusUpdate(event)),
                Err(e) => yield Err(e),
            }
        };

        Ok(stream.boxed())
    }

    /// Retrieves a task by ID from the task store.
    ///
    /// Optionally limits the number of history messages returned.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    pub async fn tasks_get(
        &self,
        task_id: &str,
        history_len: Option<u32>,
    ) -> Result<Task, A2aError> {
        validate_id(task_id, "taskId")?;
        let mut entry = self.task_store.get_task(task_id).await?;

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

    /// Cancels a task by transitioning it to CANCELED state.
    ///
    /// Validates that the task is not already in a terminal state before
    /// canceling.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or
    /// [`A2aError::TaskNotCancelable`] if the task is in a terminal state.
    pub async fn tasks_cancel(&self, task_id: &str) -> Result<Task, A2aError> {
        validate_id(task_id, "taskId")?;
        let entry = self.task_store.get_task(task_id).await?;

        // Check if task is in a terminal state
        if is_terminal_state(entry.status.state) {
            return Err(A2aError::TaskNotCancelable {
                task_id: task_id.to_string(),
                current_state: format!("{:?}", entry.status.state),
            });
        }

        // Transition to CANCELED via the executor (validates state machine)
        self.executor
            .transition_state(task_id, &entry.context_id, TaskState::Canceled, None)
            .await?;

        // Return the updated task
        let updated = self.task_store.get_task(task_id).await?;
        internal_task_to_wire(&updated)
    }

    /// Lists tasks matching the given parameters.
    ///
    /// Supports filtering by context_id, state, and pagination via page_size.
    pub async fn tasks_list(&self, params: ListTasksParams) -> Result<Vec<Task>, A2aError> {
        let entries = self.task_store.list_tasks(params).await?;
        entries.iter().map(internal_task_to_wire).collect()
    }

    /// Subscribes to task updates via SSE.
    ///
    /// Placeholder — returns a stream that yields the current task status.
    /// Full SSE subscription will be implemented in a later task.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    pub async fn tasks_subscribe(
        &self,
        task_id: &str,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        let entry = self.task_store.get_task(task_id).await?;

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

    /// Creates a push notification configuration for a task.
    ///
    /// Assigns a server-generated config ID and stores the config on the task.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    pub async fn push_config_create(
        &self,
        task_id: &str,
        mut config: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2aError> {
        // Verify task exists
        let mut entry = self.task_store.get_task(task_id).await?;

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
        self.task_store.delete_task(task_id).await?;
        self.task_store.create_task(entry).await?;

        Ok(config)
    }

    /// Retrieves a push notification configuration by task ID and config ID.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task or config does not exist.
    pub async fn push_config_get(
        &self,
        task_id: &str,
        config_id: &str,
    ) -> Result<TaskPushNotificationConfig, A2aError> {
        let entry = self.task_store.get_task(task_id).await?;

        entry.push_configs.iter().find(|c| c.id.as_deref() == Some(config_id)).cloned().ok_or_else(
            || A2aError::TaskNotFound {
                task_id: format!("push config {config_id} on task {task_id}"),
            },
        )
    }

    /// Lists all push notification configurations for a task.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    pub async fn push_config_list(
        &self,
        task_id: &str,
    ) -> Result<Vec<TaskPushNotificationConfig>, A2aError> {
        let entry = self.task_store.get_task(task_id).await?;
        Ok(entry.push_configs)
    }

    /// Deletes a push notification configuration.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task or config does not exist.
    pub async fn push_config_delete(&self, task_id: &str, config_id: &str) -> Result<(), A2aError> {
        let mut entry = self.task_store.get_task(task_id).await?;

        let original_len = entry.push_configs.len();
        entry.push_configs.retain(|c| c.id.as_deref() != Some(config_id));

        if entry.push_configs.len() == original_len {
            return Err(A2aError::TaskNotFound {
                task_id: format!("push config {config_id} on task {task_id}"),
            });
        }

        entry.updated_at = chrono::Utc::now();

        // Re-persist
        self.task_store.delete_task(task_id).await?;
        self.task_store.create_task(entry).await?;

        Ok(())
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
}
