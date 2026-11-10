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

use a2a_protocol_types::events::StreamResponse;
use a2a_protocol_types::params::SendMessageConfiguration;
use a2a_protocol_types::responses::TaskListResponse;
use a2a_protocol_types::task::{Task, TaskState};
use a2a_protocol_types::{AgentCard, Message, TaskPushNotificationConfig};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::card::CachedAgentCard;
use super::convert::internal_task_to_wire;
use super::error::A2aError;
use super::executor::V1Executor;
use super::processor::OutputMode;
use super::push::PushNotificationSender;
use super::run::{RunContext, adk_task_metadata};
use super::task_store::{ListTasksParams, TaskStore, TaskStoreEntry};
use crate::auth_bridge::RequestContext;

/// When the task's status last changed: its status timestamp, or the entry's update time
/// when the status has none. `statusTimestampAfter` filters on it, as a2a-go does.
fn status_time(entry: &TaskStoreEntry) -> chrono::DateTime<chrono::Utc> {
    entry
        .status
        .timestamp
        .as_deref()
        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
        .map_or(entry.updated_at, |time| time.with_timezone(&chrono::Utc))
}

/// The task a message runs on, and whether the agent runs: a waiting task asked again for
/// a missing answer does not run.
struct PreparedTask {
    task_id: String,
    context_id: String,
    runs: bool,
}

/// The IDs of the calls a waiting task's status message asks its caller to answer.
fn pending_call_ids(entry: &TaskStoreEntry) -> Vec<String> {
    let Some(message) = &entry.status.message else { return Vec::new() };
    message
        .parts
        .iter()
        .filter(|part| {
            part.metadata
                .as_ref()
                .and_then(|metadata| metadata.get(super::convert::ADK_TYPE_METADATA_KEY))
                .and_then(serde_json::Value::as_str)
                == Some("function_call")
        })
        .filter_map(|part| match &part.content {
            a2a_protocol_types::PartContent::Data(data) => {
                data.get("id").and_then(serde_json::Value::as_str)
            }
            _ => None,
        })
        .map(str::to_owned)
        .collect()
}

/// The pending call IDs among `pending` that `msg` answers with a function response.
fn answered_calls(msg: &Message, pending: &[String]) -> Vec<String> {
    msg.parts
        .iter()
        .filter_map(|part| super::convert::wire_part_to_adk(part).ok())
        .filter_map(|part| match part {
            adk_core::Part::FunctionResponse { id: Some(id), .. } if pending.contains(&id) => {
                Some(id)
            }
            _ => None,
        })
        .collect()
}

/// A push notification config's URL must be an absolute `http` or `https` URL with a host.
fn validate_push_url(url: &str) -> Result<(), A2aError> {
    let valid = reqwest::Url::parse(url).is_ok_and(|parsed| {
        matches!(parsed.scheme(), "http" | "https")
            && parsed.host_str().is_some_and(|host| !host.is_empty())
    });
    if valid {
        Ok(())
    } else {
        Err(A2aError::InvalidParams {
            message: format!("push notification URL {url:?} is not an absolute http(s) URL"),
        })
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

/// Builds the extended agent card for a caller (`GetExtendedAgentCard`), as a2a-go's
/// extended card producer: `None` is an unauthenticated caller.
pub type ExtendedCardProducer = Arc<
    dyn Fn(
            Option<RequestContext>,
        ) -> futures::future::BoxFuture<'static, Result<AgentCard, A2aError>>
        + Send
        + Sync,
>;

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
    agent_card: Arc<RwLock<CachedAgentCard>>,
    runner_config: Option<Arc<adk_runner::RunnerConfig>>,
    /// (owner, messageId) → taskId mapping for idempotent request handling.
    idempotency_map: RwLock<HashMap<IdempotencyKey, String>>,
    /// The agent runs in progress, so `tasks_cancel` can stop them.
    running: Arc<RunningTasks>,
    /// How a run's output is laid out in artifacts.
    output_mode: OutputMode,
    /// Builds the extended agent card, when the agent offers one.
    extended_card: Option<ExtendedCardProducer>,
}

impl RequestHandler {
    /// Creates a new request handler without a runner (stub mode).
    pub fn new(
        executor: Arc<V1Executor>,
        task_store: Arc<dyn TaskStore>,
        push_sender: Arc<dyn PushNotificationSender>,
        agent_card: Arc<RwLock<CachedAgentCard>>,
    ) -> Self {
        executor.set_push_sender(push_sender);
        Self {
            executor,
            task_store,
            agent_card,
            runner_config: None,
            idempotency_map: RwLock::new(HashMap::new()),
            running: Arc::default(),
            output_mode: OutputMode::default(),
            extended_card: None,
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
        executor.set_push_sender(push_sender);
        Self {
            executor,
            task_store,
            agent_card,
            runner_config: Some(runner_config),
            idempotency_map: RwLock::new(HashMap::new()),
            running: Arc::default(),
            output_mode: OutputMode::default(),
            extended_card: None,
        }
    }

    /// Serves `producer`'s card for `GetExtendedAgentCard`, built for each caller. Without
    /// one, the method answers `ExtendedAgentCardNotConfigured`.
    #[must_use]
    pub fn with_extended_card(mut self, producer: ExtendedCardProducer) -> Self {
        self.extended_card = Some(producer);
        self
    }

    /// Lays each run's output out in artifacts as `mode` says, as adk-go's executor option
    /// of the same name. The default is [`OutputMode::ArtifactPerRun`].
    #[must_use]
    pub fn with_output_mode(mut self, mode: OutputMode) -> Self {
        self.output_mode = mode;
        self
    }

    /// Returns a view of this handler that acts on behalf of `caller`.
    ///
    /// With `Some(caller)`, the agent session belongs to `caller.user_id`, every
    /// created task is owned by that user, and operations on tasks owned by anyone
    /// else fail with [`A2aError::TaskNotFound`]. With `None` — no authentication
    /// configured — the session user is `A2A_USER_{contextId}` and only ownerless tasks
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

    /// Streams a message with its `configuration` for an unauthenticated caller; see
    /// [`CallerScope::message_stream_with`].
    ///
    /// # Errors
    ///
    /// See [`CallerScope::message_stream_with`].
    pub async fn message_stream_with(
        &self,
        msg: Message,
        configuration: Option<&SendMessageConfiguration>,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        self.for_caller(None).message_stream_with(msg, configuration).await
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

    /// Returns the extended agent card for an unauthenticated caller; see
    /// [`agent_card_extended_for`](Self::agent_card_extended_for).
    ///
    /// # Errors
    ///
    /// See [`agent_card_extended_for`](Self::agent_card_extended_for).
    pub async fn agent_card_extended(&self) -> Result<AgentCard, A2aError> {
        self.agent_card_extended_for(None).await
    }

    /// Returns the extended agent card for `caller`, from the handler's
    /// [`ExtendedCardProducer`], as a2a-go serves it.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::UnsupportedOperation`] when the agent card says the agent offers
    /// no extended card (`capabilities.extendedAgentCard: false`),
    /// [`A2aError::ExtendedAgentCardNotConfigured`] when the handler has no producer, and
    /// the producer's own error.
    pub async fn agent_card_extended_for(
        &self,
        caller: Option<RequestContext>,
    ) -> Result<AgentCard, A2aError> {
        if self.agent_card.read().await.card.capabilities.extended_agent_card == Some(false) {
            return Err(A2aError::UnsupportedOperation {
                method: "GetExtendedAgentCard: the agent card declares no extended card"
                    .to_string(),
            });
        }
        let producer =
            self.extended_card.as_ref().ok_or(A2aError::ExtendedAgentCardNotConfigured)?;
        producer(caller).await
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
    /// The caller this scope acts for; `None` without authentication.
    pub fn caller(&self) -> Option<&RequestContext> {
        self.caller.as_ref()
    }

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

    /// Sends a message, creating a task or continuing one, and runs the agent on it.
    ///
    /// The agent runs on its own, detached from this call: a caller that goes away, or a
    /// request that times out, does not stop the run, and the run always settles the task.
    /// With `returnImmediately` set in `configuration`, the call returns the `SUBMITTED`
    /// task at once (A2A §3.2.2). Otherwise it returns once the task finishes or stops for
    /// the caller. Either way, [`tasks_cancel`](Self::tasks_cancel) stops the run.
    ///
    /// A message that names an `INPUT_REQUIRED` task must answer every call the task
    /// waits on; otherwise the task asks again, naming the call with no answer
    /// (`no input provided for function call ID "…"`), and the agent does not run.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] for a `taskId` that names no task of this
    /// caller's, [`A2aError::InvalidParams`] when the message's `contextId` is not that
    /// task's, [`A2aError::UnsupportedOperation`] when that task has ended or is still
    /// running, and an error if task creation or the store fails. A failed run is not an
    /// error: the task is returned `FAILED`, with the error as its status message.
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

        let idempotency_key = self.idempotency_key(&msg.id.0);
        if let Some(existing_task_id) = self.idempotent_task(&idempotency_key).await? {
            return self.tasks_get(&existing_task_id, None).await;
        }

        let prepared = self.prepare_task(&msg).await?;
        let task_id = prepared.task_id.clone();
        if let Some(config) = configuration.and_then(|c| c.task_push_notification_config.clone()) {
            self.push_config_create(&task_id, config).await?;
        }
        handler.idempotency_map.write().await.insert(idempotency_key, task_id.clone());

        if prepared.runs {
            let return_immediately =
                configuration.and_then(|c| c.return_immediately).unwrap_or(false);
            let submitted = handler.task_store.get_task(&task_id).await?;
            let run = self.start_run(&prepared, msg).await?;
            if return_immediately {
                return internal_task_to_wire(&submitted);
            }
            // The run is its own task; waiting for it here does not tie it to this call.
            if let Err(error) = run.await {
                tracing::warn!(%task_id, %error, "A2A run did not finish");
            }
        }

        let entry = handler.task_store.get_task(&task_id).await?;
        internal_task_to_wire(&entry)
    }

    /// The task an earlier request with the same idempotency key created, if it still
    /// exists.
    async fn idempotent_task(&self, key: &IdempotencyKey) -> Result<Option<String>, A2aError> {
        let handler = self.handler;
        let Some(existing) = handler.idempotency_map.read().await.get(key).cloned() else {
            return Ok(None);
        };
        match self.owned_task(&existing).await {
            Ok(_) => Ok(Some(existing)),
            Err(A2aError::TaskNotFound { .. }) => {
                handler.idempotency_map.write().await.remove(key);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Starts the run of a prepared task: the agent when the handler has one, otherwise
    /// the `WORKING` and `COMPLETED` transitions alone. Returns the run's handle.
    async fn start_run(
        &self,
        prepared: &PreparedTask,
        msg: Message,
    ) -> Result<tokio::task::JoinHandle<()>, A2aError> {
        let handler = self.handler;
        let (task_id, context_id) = (prepared.task_id.clone(), prepared.context_id.clone());
        let Some(runner_config) = handler.runner_config.clone() else {
            let executor = handler.executor.clone();
            executor.transition_state(&task_id, &context_id, TaskState::Working, None).await?;
            executor.transition_state(&task_id, &context_id, TaskState::Completed, None).await?;
            return Ok(tokio::spawn(async {}));
        };
        // Registered before the run starts, so a cancel that races it still stops it.
        let guard = handler.running.start(&task_id, runner_config.cancellation_token.as_ref());
        let cancellation = guard.token().clone();
        let run = RunContext {
            executor: handler.executor.clone(),
            runner_config,
            caller: self.caller.clone(),
            output_mode: handler.output_mode,
        };
        Ok(super::run::spawn(run, task_id, context_id, msg, cancellation, guard))
    }

    /// The `INPUT_REQUIRED` or `AUTH_REQUIRED` task `msg` continues, if any (see
    /// [`message_send_with`](Self::message_send_with)).
    ///
    /// A message that names its task continues it. A message that names only a context
    /// continues that context's `INPUT_REQUIRED` task when it answers one of the calls the
    /// task waits on; any other such message starts a new task in the context.
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
            && !answered_calls(msg, &pending_call_ids(&existing)).is_empty()
        {
            return Ok(Some(existing));
        }
        Ok(None)
    }

    /// Finds or creates the task `msg` runs on and records `msg` in its history.
    ///
    /// A new task is `SUBMITTED`. A waiting task is taken back to `SUBMITTED` for its run,
    /// unless `msg` leaves one of its calls unanswered: then it asks again and does not
    /// run (see [`message_send_with`](Self::message_send_with)).
    ///
    /// # Errors
    ///
    /// See [`task_to_continue`](Self::task_to_continue); also returns an error if task
    /// creation or the store fails.
    async fn prepare_task(&self, msg: &Message) -> Result<PreparedTask, A2aError> {
        let handler = self.handler;
        if let Some(existing) = self.task_to_continue(msg).await? {
            let pending = pending_call_ids(&existing);
            let answered = answered_calls(msg, &pending);
            if existing.status.state == TaskState::InputRequired
                && let Some(missing) = pending.iter().find(|id| !answered.contains(*id))
            {
                handler.executor.ask_again(&existing.id, msg.clone(), missing).await?;
                return Ok(PreparedTask {
                    task_id: existing.id,
                    context_id: existing.context_id,
                    runs: false,
                });
            }
            let claimed = handler.executor.claim_waiting_task(&existing.id, msg.clone()).await?;
            return Ok(PreparedTask {
                task_id: claimed.id,
                context_id: claimed.context_id,
                runs: true,
            });
        }

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
        handler.executor.add_history_message(&task_id, msg.clone()).await?;
        Ok(PreparedTask { task_id, context_id, runs: true })
    }

    /// Sends a streaming message; see [`message_stream_with`](Self::message_stream_with).
    ///
    /// # Errors
    ///
    /// See [`message_stream_with`](Self::message_stream_with).
    pub async fn message_stream(
        &self,
        msg: Message,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        self.message_stream_with(msg, None).await
    }

    /// Sends a message, as [`message_send_with`](Self::message_send_with) does, and
    /// streams the task's updates: the `SUBMITTED` task first, then each status and
    /// artifact update the run makes, until the task ends or stops for the caller.
    ///
    /// The run does not depend on the stream: a client that disconnects leaves it running,
    /// and [`tasks_subscribe`](Self::tasks_subscribe) follows it again. Each event the
    /// agent produces is one artifact update carrying every part of the event, laid out as
    /// the handler's [`OutputMode`] says; a model error, a cancel and calls that wait on
    /// the caller end the stream on the matching status. These are the updates adk-go's
    /// A2A executor makes.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`message_send_with`](Self::message_send_with), and
    /// [`A2aError::UnsupportedOperation`] when the agent card says the agent does not
    /// stream.
    pub async fn message_stream_with(
        &self,
        msg: Message,
        configuration: Option<&SendMessageConfiguration>,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
        self.handler.require(Capability::Streaming("SendStreamingMessage")).await?;
        validate_message(&msg)?;
        let handler = self.handler;
        if configuration.is_some_and(|c| c.task_push_notification_config.is_some()) {
            handler.require(Capability::PushNotifications).await?;
        }

        let idempotency_key = self.idempotency_key(&msg.id.0);
        if let Some(existing_task_id) = self.idempotent_task(&idempotency_key).await? {
            let task = self.tasks_get(&existing_task_id, None).await?;
            return Ok(futures::stream::once(async move { Ok(StreamResponse::Task(task)) }).boxed());
        }

        let prepared = self.prepare_task(&msg).await?;
        let task_id = prepared.task_id.clone();
        if let Some(config) = configuration.and_then(|c| c.task_push_notification_config.clone()) {
            self.push_config_create(&task_id, config).await?;
        }
        handler.idempotency_map.write().await.insert(idempotency_key, task_id.clone());

        // Subscribe before the run starts, so the stream sees every update it makes.
        let mut updates = handler.executor.subscribe(&task_id);
        let first = internal_task_to_wire(&handler.task_store.get_task(&task_id).await?)?;
        let settled = ends_subscription(first.status.state);
        if prepared.runs {
            // Detached: the handle is not awaited, so dropping the stream leaves the run.
            drop(self.start_run(&prepared, msg).await?);
        }

        let stream = async_stream::stream! {
            yield Ok(StreamResponse::Task(first));
            if settled && !prepared.runs {
                return;
            }
            while let Some(update) = updates.recv().await {
                let ends = matches!(
                    &update,
                    StreamResponse::StatusUpdate(event) if ends_subscription(event.status.state)
                );
                yield Ok(update);
                if ends {
                    return;
                }
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
            .update_status(
                task_id,
                &entry.context_id,
                TaskState::Canceled,
                Vec::new(),
                Some(serde_json::Map::new()),
            )
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

        // The remote tasks the waiting task waited on will get no answer now.
        if entry.status.state == TaskState::InputRequired
            && let Some(runner_config) = &self.handler.runner_config
        {
            super::run::cancel_waiting_remote_tasks(runner_config, self.caller.as_ref(), &entry)
                .await;
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
        // As a2a-go: a page size of 0 is no page size.
        let page_size = match params.page_size {
            None | Some(0) => DEFAULT_PAGE_SIZE,
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
            .filter(|entry| updated_after.is_none_or(|after| status_time(entry) >= after))
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

        // Subscribe before reading the task, so no update falls between the snapshot and the
        // first live one. One that arrives twice only repeats a state the client has.
        let mut updates = self.handler.executor.subscribe(task_id);
        let snapshot = self.owned_task(task_id).await?;
        let task = internal_task_to_wire(&snapshot)?;
        // A task with no run in progress has nothing more to send: the snapshot is all.
        let settled =
            ends_subscription(snapshot.status.state) || !self.handler.running.contains(task_id);

        let stream = async_stream::stream! {
            yield Ok(StreamResponse::Task(task));
            if settled {
                return;
            }
            while let Some(update) = updates.recv().await {
                let ends = matches!(
                    &update,
                    StreamResponse::StatusUpdate(event) if ends_subscription(event.status.state)
                );
                yield Ok(update);
                if ends {
                    return;
                }
            }
        };
        Ok(stream.boxed())
    }

    /// Creates a push notification configuration for one of this caller's tasks.
    ///
    /// Assigns a server-generated config ID when the config has none, and stores the
    /// config on the task, replacing a config with the same ID.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::InvalidParams`] when the config's URL is not an absolute
    /// `http` or `https` URL, and [`A2aError::TaskNotFound`] if the task does not exist or
    /// belongs to another caller.
    pub async fn push_config_create(
        &self,
        task_id: &str,
        mut config: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2aError> {
        self.handler.require(Capability::PushNotifications).await?;
        validate_push_url(&config.url)?;
        self.owned_task(task_id).await?;
        if config.id.as_deref().is_none_or(str::is_empty) {
            config.id = Some(uuid::Uuid::new_v4().to_string());
        }
        config.task_id = Some(task_id.to_string());
        self.handler.executor.save_push_config(task_id, config).await
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

    /// Deletes a push notification configuration. Deleting a config the task does not have
    /// succeeds, as A2A config stores do.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist or belongs to another
    /// caller.
    pub async fn push_config_delete(&self, task_id: &str, config_id: &str) -> Result<(), A2aError> {
        self.handler.require(Capability::PushNotifications).await?;
        self.owned_task(task_id).await?;
        self.handler.executor.delete_push_config(task_id, config_id).await
    }
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
    use a2a_protocol_types::artifact::Artifact;
    use a2a_protocol_types::events::{TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
    use a2a_protocol_types::task::{ContextId, TaskId};
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

    /// Whether an update comes from streamed partial events (`adk_partial`).
    fn is_partial(update: &TaskArtifactUpdateEvent) -> bool {
        update.metadata.as_ref().and_then(|meta| meta.get("adk_partial"))
            == Some(&serde_json::json!(true))
    }

    /// The answer a client builds from a stream's non-partial artifact updates.
    fn answer_text(updates: &[TaskArtifactUpdateEvent]) -> String {
        let mut text = String::new();
        for update in updates.iter().filter(|update| !is_partial(update)) {
            if update.append != Some(true) {
                text.clear();
            }
            text.extend(update.artifact.parts.iter().filter_map(part_text));
        }
        text
    }

    /// The text of a stored task's artifacts, without the temporary partial one.
    fn stored_answer(entry: &TaskStoreEntry) -> String {
        entry
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.metadata.as_ref().and_then(|meta| meta.get("adk_partial")).is_none()
            })
            .flat_map(|artifact| artifact.parts.iter().filter_map(part_text))
            .collect()
    }

    async fn stream_updates(
        handler: &RequestHandler,
    ) -> (String, Vec<TaskArtifactUpdateEvent>, Vec<TaskState>) {
        let mut stream = handler.message_stream(make_test_message()).await.expect("stream starts");
        let (mut task_id, mut artifacts, mut states) = (String::new(), Vec::new(), Vec::new());
        while let Some(item) = stream.next().await {
            match item.expect("no stream error") {
                StreamResponse::Task(task) => task_id = task.id.0,
                StreamResponse::ArtifactUpdate(e) => artifacts.push(e),
                StreamResponse::StatusUpdate(e) => states.push(e.status.state),
                _ => {}
            }
        }
        (task_id, artifacts, states)
    }

    #[tokio::test]
    async fn streamed_chunks_fill_a_temporary_artifact_and_the_answer_has_its_own() {
        let (handler, store) = handler_with_agent(Arc::new(ChunkingAgent {
            chunks: vec!["Hel".into(), "lo ".into(), "world".into()],
        }));
        let (task_id, artifacts, states) = stream_updates(&handler).await;

        // As adk-go's artifact-per-run layout: each chunk replaces a temporary artifact
        // marked `adk_partial`, the answer goes to its own artifact, and the temporary one
        // is emptied (`{}`, lastChunk) before the final status.
        assert_eq!(artifacts.len(), 4, "two chunks, the answer, the reset: {artifacts:?}");
        assert!(is_partial(&artifacts[0]) && is_partial(&artifacts[1]));
        assert_eq!(artifacts[0].artifact.id, artifacts[1].artifact.id);
        assert_eq!(artifacts[1].append, Some(false), "a chunk replaces the temporary artifact");
        assert!(!is_partial(&artifacts[2]));
        assert_ne!(artifacts[2].artifact.id, artifacts[0].artifact.id);
        assert_eq!(
            answer_text(&artifacts),
            "Hello world",
            "the final event carries the whole answer"
        );
        let reset = &artifacts[3];
        assert!(is_partial(reset) && reset.last_chunk == Some(true));
        assert_eq!(reset.artifact.id, artifacts[0].artifact.id);
        assert_eq!(states, vec![TaskState::Working, TaskState::Completed]);

        let entry = store.get_task(&task_id).await.unwrap();
        assert_eq!(stored_answer(&entry), "Hello world", "tasks/get sees the answer");
    }

    #[tokio::test]
    async fn an_authoritative_final_event_is_the_answer() {
        for prefix in ["", "Earlier. "] {
            for snapshot in ["Hello world", "Corrected", ""] {
                let (handler, store) = handler_with_agent(Arc::new(SnapshotAgent {
                    prefix: prefix.into(),
                    snapshot: snapshot.into(),
                }));
                let (task_id, artifacts, _) = stream_updates(&handler).await;
                let expected = format!("{prefix}{snapshot}");
                assert_eq!(answer_text(&artifacts), expected, "{prefix:?} {snapshot:?}");
                let entry = store.get_task(&task_id).await.unwrap();
                assert_eq!(stored_answer(&entry), expected, "{prefix:?} {snapshot:?}");

                let (handler, _) = handler_with_agent(Arc::new(SnapshotAgent {
                    prefix: prefix.into(),
                    snapshot: snapshot.into(),
                }));
                let task = handler.message_send(make_test_message()).await.unwrap();
                let entry = store_entry_of(&task);
                assert_eq!(stored_answer(&entry), expected, "buffered {prefix:?} {snapshot:?}");
            }
        }
    }

    /// A wire task as the store entry shape, for [`stored_answer`].
    fn store_entry_of(task: &Task) -> TaskStoreEntry {
        TaskStoreEntry {
            id: task.id.0.clone(),
            context_id: task.context_id.0.clone(),
            status: task.status.clone(),
            artifacts: task.artifacts.clone().unwrap_or_default(),
            history: Vec::new(),
            metadata: HashMap::new(),
            push_configs: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
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
        assert_eq!(
            started.status.state,
            TaskState::Submitted,
            "the send returns the submitted task without waiting for the run"
        );

        let mut state = TaskState::Submitted;
        for _ in 0..100 {
            state = store.get_task(&started.id.0).await.unwrap().status.state;
            if is_terminal_state(state) {
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

    // ── Push notifications ──────────────────────────────────────────────────

    /// Records what it would deliver: the URL and the update.
    #[derive(Default)]
    struct RecordingSender(std::sync::Mutex<Vec<(String, String)>>);

    #[async_trait::async_trait]
    impl super::super::push::PushNotificationSender for RecordingSender {
        async fn send_status_update(
            &self,
            url: &str,
            event: &TaskStatusUpdateEvent,
            _config: &TaskPushNotificationConfig,
        ) -> Result<(), A2aError> {
            self.0.lock().unwrap().push((url.to_string(), format!("{:?}", event.status.state)));
            Ok(())
        }

        async fn send_artifact_update(
            &self,
            url: &str,
            _event: &TaskArtifactUpdateEvent,
            _config: &TaskPushNotificationConfig,
        ) -> Result<(), A2aError> {
            self.0.lock().unwrap().push((url.to_string(), "artifact".to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_push_config_sent_with_the_message_gets_every_update() {
        let sender = Arc::new(RecordingSender::default());
        let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
        let mut card = make_test_agent_card();
        card.capabilities = AgentCapabilities::none().with_push_notifications(true);
        let handler = RequestHandler::with_runner(
            Arc::new(V1Executor::new(store.clone())),
            store,
            sender.clone(),
            Arc::new(RwLock::new(CachedAgentCard::new(card))),
            Arc::new(
                adk_runner::Runner::builder()
                    .app_name("push")
                    .agent(Arc::new(ChunkingAgent { chunks: vec!["done".into()] }))
                    .session_service(Arc::new(adk_session::InMemorySessionService::new()))
                    .build_config(),
            ),
        );
        let configuration: SendMessageConfiguration = serde_json::from_value(serde_json::json!({
            "taskPushNotificationConfig": {"url": "https://hooks.example.com/a2a"}
        }))
        .unwrap();

        let task =
            handler.message_send_with(make_test_message(), Some(&configuration)).await.unwrap();
        assert_eq!(handler.push_config_list(&task.id.0).await.unwrap().len(), 1);

        // Delivery runs beside the task; wait for the last update.
        let mut delivered = Vec::new();
        for _ in 0..100 {
            delivered = sender.0.lock().unwrap().clone();
            if delivered.iter().any(|(_, what)| what == "Completed") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let what: Vec<&str> = delivered.iter().map(|(_, what)| what.as_str()).collect();
        assert_eq!(what, vec!["Working", "artifact", "Completed"]);
        assert!(delivered.iter().all(|(url, _)| url == "https://hooks.example.com/a2a"));
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
        assert_eq!(metadata["adk_user_id"], format!("A2A_USER_{context}"));
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
        // As a2a-go: a card that says there is no extended card refuses the method.
        assert!(matches!(
            handler.agent_card_extended().await,
            Err(A2aError::UnsupportedOperation { .. })
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

        // As a2a-go: a page size of 0 is the default page size.
        let default_page = handler
            .tasks_list(ListTasksParams { page_size: Some(0), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(default_page.page_size, DEFAULT_PAGE_SIZE);

        for bad in [
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
        assert!(
            matches!(first.status.state, TaskState::Submitted | TaskState::Working),
            "the snapshot is the task as it stands: {:?}",
            first.status.state
        );
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

        // As a2a-go: a task with no run in progress answers with its snapshot alone.
        let updates =
            collect_subscription(handler.tasks_subscribe(&waiting.id.0).await.unwrap()).await;
        let [StreamResponse::Task(snapshot)] = updates.as_slice() else {
            panic!("the subscription is the snapshot alone: {updates:?}");
        };
        assert_eq!(snapshot.status.state, TaskState::InputRequired);

        handler.message_send(approval_for(&waiting.id, None)).await.unwrap();
        assert!(matches!(
            handler.tasks_subscribe(&waiting.id.0).await,
            Err(A2aError::UnsupportedOperation { .. })
        ));
    }

    // ── Runs detached from their request ────────────────────────────────────

    #[tokio::test]
    async fn a_run_outlives_the_stream_that_started_it() {
        let (handler, store, answered) = slow_handler(100);
        let mut stream = handler.message_stream(make_test_message()).await.unwrap();
        let Some(Ok(StreamResponse::Task(task))) = stream.next().await else {
            panic!("the stream starts with the task");
        };
        drop(stream);

        let mut state = TaskState::Submitted;
        for _ in 0..100 {
            state = store.get_task(&task.id.0).await.unwrap().status.state;
            if is_terminal_state(state) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(state, TaskState::Completed, "the client left; the run did not");
        assert!(answered.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// Panics as soon as its run is polled.
    struct PanickingAgent;

    #[async_trait::async_trait]
    impl adk_core::Agent for PanickingAgent {
        fn name(&self) -> &str {
            "panicking_agent"
        }

        fn description(&self) -> &str {
            "panics"
        }

        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            Ok(Box::pin(async_stream::stream! {
                if true {
                    panic!("boom");
                }
                yield Ok(adk_core::Event::new("never"));
            }))
        }
    }

    #[tokio::test]
    async fn a_panicking_agent_fails_its_task() {
        let (handler, _) = handler_with_agent(Arc::new(PanickingAgent));
        let task = handler.message_send(make_test_message()).await.unwrap();
        assert_eq!(task.status.state, TaskState::Failed);
        let text = status_text(&task.status);
        assert!(text.contains("agent run panicked: boom"), "{text}");
    }

    #[tokio::test]
    async fn the_waiting_message_moves_into_history_when_the_task_goes_on() {
        let (handler, _) = handler_with_agent(Arc::new(ApprovalAgent));
        let waiting = handler.message_send(make_test_message()).await.unwrap();
        assert_eq!(waiting.status.state, TaskState::InputRequired);

        let done = handler.message_send(approval_for(&waiting.id, None)).await.unwrap();
        let roles: Vec<MessageRole> =
            done.history.unwrap_or_default().iter().map(|message| message.role).collect();
        assert_eq!(
            roles,
            vec![MessageRole::User, MessageRole::Agent, MessageRole::User],
            "the question, the agent's waiting message, the answer"
        );
    }

    #[tokio::test]
    async fn push_configs_are_validated_and_replaced_by_id() {
        let handler = make_handler();
        let task = handler.message_send(make_test_message()).await.unwrap();
        let config = |value: serde_json::Value| -> TaskPushNotificationConfig {
            serde_json::from_value(value).unwrap()
        };

        for url in ["not a url", "ftp://hooks.example.com/a2a", "/relative"] {
            let err = handler
                .push_config_create(&task.id.0, config(serde_json::json!({"url": url})))
                .await
                .unwrap_err();
            assert!(matches!(err, A2aError::InvalidParams { .. }), "{url}: {err}");
        }
        handler
            .push_config_create(
                &task.id.0,
                config(serde_json::json!({"id": "c1", "url": "https://a.example/1"})),
            )
            .await
            .unwrap();
        handler
            .push_config_create(
                &task.id.0,
                config(serde_json::json!({"id": "c1", "url": "https://a.example/2"})),
            )
            .await
            .unwrap();
        let configs = handler.push_config_list(&task.id.0).await.unwrap();
        assert_eq!(configs.len(), 1, "a config with the same ID replaces the old one");
        assert_eq!(configs[0].url, "https://a.example/2");
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
    async fn deleting_a_push_config_the_task_lacks_succeeds() {
        let handler = make_handler();
        let msg = make_test_message();
        let task = handler.message_send(msg).await.unwrap();

        // As A2A config stores do: a delete leaves the config absent either way.
        handler.push_config_delete(&task.id.0, "nonexistent").await.unwrap();
        let err = handler.push_config_delete("no-such-task", "nonexistent").await.unwrap_err();
        assert!(matches!(err, A2aError::TaskNotFound { .. }), "{err}");
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
    async fn the_extended_card_is_built_for_each_caller() {
        // Without a producer there is no extended card to serve.
        assert!(matches!(
            make_handler().agent_card_extended().await,
            Err(A2aError::ExtendedAgentCardNotConfigured)
        ));

        let producer: ExtendedCardProducer = Arc::new(|caller: Option<RequestContext>| {
            Box::pin(async move {
                let mut card = make_test_agent_card();
                card.description = match caller {
                    Some(caller) => format!("for {}", caller.user_id),
                    None => "for anyone".to_string(),
                };
                Ok(card)
            })
        });
        let handler = make_handler().with_extended_card(producer);
        assert_eq!(handler.agent_card_extended().await.unwrap().description, "for anyone");
        let alice = RequestContext {
            user_id: "alice".to_string(),
            scopes: Vec::new(),
            metadata: HashMap::new(),
        };
        let card = handler.agent_card_extended_for(Some(alice)).await.unwrap();
        assert_eq!(card.description, "for alice");
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
    async fn a_context_message_continues_the_waiting_task_only_when_it_answers_its_call() {
        let handler = make_handler();
        let task_id = "resume-test";
        let ctx_id = "ctx-resume";
        handler.executor.create_task(task_id, ctx_id).await.unwrap();
        handler.executor.transition_state(task_id, ctx_id, TaskState::Working, None).await.unwrap();
        let call = super::super::convert::tagged_function_call_part(
            "approve",
            &serde_json::json!({}),
            Some("c1"),
            true,
        );
        handler
            .executor
            .transition_state_with_parts(task_id, ctx_id, TaskState::InputRequired, vec![call])
            .await
            .unwrap();

        // A plain follow-up in the context starts a new task there, as adk-go does.
        let mut follow_up = make_test_message();
        follow_up.id = MessageId::new("msg-follow-up");
        follow_up.context_id = Some(a2a_protocol_types::ContextId::new(ctx_id));
        let new_task = handler.message_send(follow_up).await.unwrap();
        assert_ne!(new_task.id.0, task_id);
        assert_eq!(new_task.context_id.0, ctx_id);
        let waiting = handler.tasks_get(task_id, None).await.unwrap();
        assert_eq!(waiting.status.state, TaskState::InputRequired);

        // An answer to the pending call continues the waiting task.
        let mut answer = make_test_message();
        answer.id = MessageId::new("msg-answer");
        answer.context_id = Some(a2a_protocol_types::ContextId::new(ctx_id));
        answer.parts = vec![super::super::convert::tagged_function_response_part(
            "approve",
            &serde_json::json!({"approved": true}),
            Some("c1"),
        )];
        let resumed = handler.message_send(answer).await.unwrap();
        assert_eq!(resumed.id.0, task_id);
        assert_eq!(resumed.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn an_answer_that_misses_a_pending_call_is_asked_again_without_running() {
        let handler = make_handler();
        let (task_id, ctx_id) = ("ask-again", "ctx-ask-again");
        handler.executor.create_task(task_id, ctx_id).await.unwrap();
        handler.executor.transition_state(task_id, ctx_id, TaskState::Working, None).await.unwrap();
        let calls = vec![
            super::super::convert::tagged_function_call_part(
                "approve",
                &serde_json::json!({}),
                Some("c1"),
                true,
            ),
            super::super::convert::tagged_function_call_part(
                "approve",
                &serde_json::json!({}),
                Some("c2"),
                true,
            ),
        ];
        handler
            .executor
            .transition_state_with_parts(task_id, ctx_id, TaskState::InputRequired, calls)
            .await
            .unwrap();

        let mut partial_answer = make_test_message();
        partial_answer.id = MessageId::new("msg-partial-answer");
        partial_answer.task_id = Some(TaskId::new(task_id));
        partial_answer.parts = vec![super::super::convert::tagged_function_response_part(
            "approve",
            &serde_json::json!({"approved": true}),
            Some("c1"),
        )];
        let task = handler.message_send(partial_answer).await.unwrap();

        assert_eq!(task.status.state, TaskState::InputRequired);
        let parts = task.status.message.expect("the task asks again").parts;
        assert_eq!(parts.len(), 3, "both pending calls, then the notice");
        assert_eq!(parts[2].text_content(), Some(r#"no input provided for function call ID "c2""#));
        assert_eq!(parts[2].metadata.as_ref().unwrap()["validation_error"], true);
        let history = task.history.expect("history");
        assert_eq!(history.last().unwrap().id.0, "msg-partial-answer");
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
