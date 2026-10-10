use crate::ServerConfig;
use crate::a2a::{
    AgentCard, Executor, ExecutorConfig, JsonRpcError, JsonRpcRequest, JsonRpcResponse, Message,
    MessageSendParams, Task, TaskState, TaskStatus, TaskStatusUpdateEvent, TasksCancelParams,
    TasksGetParams, UpdateEvent, build_agent_card, jsonrpc,
};
use crate::auth_bridge::{AuthenticatedCaller, RequestContext};
use adk_runner::{Runner, RunnerConfig};
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Json,
        sse::{Event, Sse},
    },
};
use futures::stream::Stream;
use serde_json::Value;
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify, RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// How many finished A2A task records a [`TaskStore`] keeps, and for how long.
///
/// A record is kept so `tasks/get` can report the outcome after `message/send`
/// returns. Every record in the store belongs to an execution that has ended —
/// running tasks are tracked separately and are never evicted.
///
/// The default keeps the newest 1,000 records for at most one hour.
///
/// # Example
///
/// ```rust
/// use adk_server::A2aTaskRetention;
/// use std::time::Duration;
///
/// let retention =
///     A2aTaskRetention::default().with_max_finished(500).with_ttl(Duration::from_secs(600));
/// assert_eq!(retention.max_finished, Some(500));
/// assert_eq!(retention.ttl, Some(Duration::from_secs(600)));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct A2aTaskRetention {
    /// How many finished task records to keep, newest first. `None` keeps every record.
    pub max_finished: Option<usize>,
    /// How long a finished task record stays readable. `None` disables expiry.
    pub ttl: Option<Duration>,
}

impl Default for A2aTaskRetention {
    fn default() -> Self {
        Self { max_finished: Some(1000), ttl: Some(Duration::from_secs(3600)) }
    }
}

impl A2aTaskRetention {
    /// Keeps every finished task record forever. The store then grows without bound.
    pub fn unlimited() -> Self {
        Self { max_finished: None, ttl: None }
    }

    /// Keeps at most `count` finished task records, evicting the oldest first.
    pub fn with_max_finished(mut self, count: usize) -> Self {
        self.max_finished = Some(count);
        self
    }

    /// Expires a finished task record `ttl` after its execution ended.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }
}

/// A finished task, the principal that started it, and when it ended.
struct StoredTask {
    task: Task,
    owner: Option<String>,
    finished_at: Instant,
}

/// In-memory storage for finished A2A tasks.
///
/// Each record carries the authenticated user that started the task, or `None`
/// when no authentication was configured. Owner-scoped reads
/// ([`get_for`](Self::get_for)) only return a record to the same principal.
/// Records are evicted according to the store's [`A2aTaskRetention`].
pub struct TaskStore {
    tasks: RwLock<HashMap<String, StoredTask>>,
    retention: A2aTaskRetention,
}

impl Default for TaskStore {
    fn default() -> Self {
        Self::with_retention(A2aTaskRetention::default())
    }
}

impl TaskStore {
    /// Creates a store with the default [`A2aTaskRetention`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a store that evicts records according to `retention`.
    pub fn with_retention(retention: A2aTaskRetention) -> Self {
        Self { tasks: RwLock::new(HashMap::new()), retention }
    }

    /// Stores `task` without an owner.
    ///
    /// An ownerless record is visible only to unauthenticated callers. Use
    /// [`store_for`](Self::store_for) to record the principal that started it.
    pub async fn store(&self, task: Task) {
        self.store_for(None, task).await;
    }

    /// Stores `task` as owned by `owner`, then applies the retention policy.
    pub async fn store_for(&self, owner: Option<&str>, task: Task) {
        let mut tasks = self.tasks.write().await;
        tasks.insert(
            task.id.clone(),
            StoredTask { task, owner: owner.map(str::to_string), finished_at: Instant::now() },
        );
        self.evict(&mut tasks, Instant::now());
    }

    /// Returns a stored task regardless of its owner.
    ///
    /// This is for programmatic access by the server operator; request handlers
    /// use [`get_for`](Self::get_for).
    pub async fn get(&self, task_id: &str) -> Option<Task> {
        let tasks = self.tasks.read().await;
        tasks
            .get(task_id)
            .filter(|stored| !self.is_expired(stored))
            .map(|stored| stored.task.clone())
    }

    /// Returns a stored task only when it belongs to `owner`.
    ///
    /// A task owned by someone else is reported exactly like a missing one, so the
    /// response does not disclose that the ID exists.
    pub async fn get_for(&self, owner: Option<&str>, task_id: &str) -> Option<Task> {
        let tasks = self.tasks.read().await;
        tasks
            .get(task_id)
            .filter(|stored| !self.is_expired(stored) && stored.owner.as_deref() == owner)
            .map(|stored| stored.task.clone())
    }

    /// Removes a stored task regardless of its owner.
    pub async fn remove(&self, task_id: &str) -> Option<Task> {
        self.tasks.write().await.remove(task_id).map(|stored| stored.task)
    }

    /// Returns the number of records held, including expired ones not yet evicted.
    pub async fn len(&self) -> usize {
        self.tasks.read().await.len()
    }

    /// Returns `true` when the store holds no records.
    pub async fn is_empty(&self) -> bool {
        self.tasks.read().await.is_empty()
    }

    /// The owner of an unexpired record, or `None` when there is no such record.
    async fn owner_of(&self, task_id: &str) -> Option<Option<String>> {
        let tasks = self.tasks.read().await;
        tasks
            .get(task_id)
            .filter(|stored| !self.is_expired(stored))
            .map(|stored| stored.owner.clone())
    }

    fn is_expired(&self, stored: &StoredTask) -> bool {
        self.retention.ttl.is_some_and(|ttl| stored.finished_at.elapsed() >= ttl)
    }

    fn evict(&self, tasks: &mut HashMap<String, StoredTask>, now: Instant) {
        if let Some(ttl) = self.retention.ttl {
            tasks.retain(|_, stored| now.saturating_duration_since(stored.finished_at) < ttl);
        }
        if let Some(max_finished) = self.retention.max_finished
            && tasks.len() > max_finished
        {
            let mut by_age: Vec<(Instant, String)> =
                tasks.iter().map(|(id, stored)| (stored.finished_at, id.clone())).collect();
            by_age.sort();
            let excess = tasks.len() - max_finished;
            for (_, task_id) in by_age.into_iter().take(excess) {
                tasks.remove(&task_id);
            }
        }
    }
}

#[derive(Clone)]
struct ActiveTask {
    token: CancellationToken,
    abort_handle: tokio::task::AbortHandle,
    completion: Arc<Notify>,
    context_id: String,
    owner: Option<String>,
}

enum StreamTaskMessage {
    Update(Box<UpdateEvent>),
    Error(String),
}

/// A task that was registered and spawned.
struct StartedTask {
    result: oneshot::Receiver<adk_core::Result<Task>>,
    updates: Option<mpsc::Receiver<StreamTaskMessage>>,
}

/// Controller for A2A protocol endpoints.
///
/// When the request carries an authenticated principal, the principal owns the
/// session and every task it starts: the session user is the principal's
/// `user_id`, and `tasks/get` and `tasks/cancel` only reach that principal's
/// tasks. Without authentication the session user is `A2A_USER_{contextId}`.
#[derive(Clone)]
pub struct A2aController {
    config: ServerConfig,
    agent_card: AgentCard,
    task_store: Arc<TaskStore>,
    active_tasks: Arc<Mutex<HashMap<String, ActiveTask>>>,
}

impl A2aController {
    pub fn new(config: ServerConfig, base_url: &str) -> Self {
        Self::build(config, base_url, None)
    }

    /// Create a controller whose agent card also lists the skills in `skill_index`.
    ///
    /// The indexed skills are appended to the agent-derived `skills[]` entries
    /// via [`agent_skills_from_index`](crate::a2a::agent_skills_from_index) and
    /// served at `/.well-known/agent-card.json` and `/.well-known/agent.json`.
    pub fn with_skill_index(
        config: ServerConfig,
        base_url: &str,
        skill_index: Arc<adk_skill::SkillIndex>,
    ) -> Self {
        Self::build(config, base_url, Some(skill_index))
    }

    /// Replaces the bounds on retained finished-task records.
    ///
    /// Call this before the controller serves requests: it starts a fresh store.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::{A2aController, A2aTaskRetention};
    ///
    /// let controller = A2aController::new(config, "http://localhost:8080")
    ///     .with_task_retention(A2aTaskRetention::default().with_max_finished(100));
    /// ```
    pub fn with_task_retention(mut self, retention: A2aTaskRetention) -> Self {
        self.task_store = Arc::new(TaskStore::with_retention(retention));
        self
    }

    fn build(
        config: ServerConfig,
        base_url: &str,
        skill_index: Option<Arc<adk_skill::SkillIndex>>,
    ) -> Self {
        let root_agent = config.agent_loader.root_agent();
        let invoke_url = format!("{}/a2a", base_url.trim_end_matches('/'));
        let mut agent_card = build_agent_card(root_agent.as_ref(), &invoke_url);
        if let Some(skill_index) = skill_index {
            let indexed = crate::a2a::agent_skills_from_index(&skill_index);
            tracing::debug!(skill.count = indexed.len(), "appending indexed skills to agent card");
            agent_card.skills.extend(indexed);
        }

        Self {
            config,
            agent_card,
            task_store: Arc::new(TaskStore::new()),
            active_tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

fn build_runner_config(
    controller: &A2aController,
    root_agent: Arc<dyn adk_core::Agent>,
    cancellation_token: Option<CancellationToken>,
    request_context: Option<RequestContext>,
) -> Arc<RunnerConfig> {
    let mut builder = Runner::builder()
        .app_name(root_agent.name())
        .agent(root_agent)
        .session_service(controller.config.session_service.clone())
        .governance(controller.config.governance.clone());
    if let Some(ref artifact_service) = controller.config.artifact_service {
        builder = builder.artifact_service(artifact_service.clone());
    }
    if let Some(ref memory_service) = controller.config.memory_service {
        builder = builder.memory_service(memory_service.clone());
    }
    if let Some(ref compaction_config) = controller.config.compaction_config {
        builder = builder.compaction_config(compaction_config.clone());
    }
    if let Some(ref context_cache_config) = controller.config.context_cache_config {
        builder = builder.context_cache_config(context_cache_config.clone());
    }
    if let Some(ref cache_capable) = controller.config.cache_capable {
        builder = builder.cache_capable(cache_capable.clone());
    }
    if let Some(cancellation_token) = cancellation_token {
        builder = builder.cancellation_token(cancellation_token);
    }
    if let Some(request_context) = request_context {
        builder = builder.request_context(request_context);
    }
    Arc::new(builder.build_config())
}

fn build_task_from_events(task_id: &str, context_id: &str, events: &[UpdateEvent]) -> Task {
    let mut task = Task {
        id: task_id.to_string(),
        context_id: Some(context_id.to_string()),
        status: TaskStatus { state: TaskState::Completed, message: None },
        artifacts: Some(vec![]),
        history: None,
    };

    for event in events {
        match event {
            UpdateEvent::TaskStatusUpdate(status) => {
                task.status = status.status.clone();
            }
            UpdateEvent::TaskArtifactUpdate(artifact) => {
                if let Some(ref mut artifacts) = task.artifacts {
                    artifacts.push(artifact.artifact.clone());
                }
            }
        }
    }

    task
}

fn build_failed_task(task_id: &str, context_id: &str, message: impl Into<String>) -> Task {
    Task {
        id: task_id.to_string(),
        context_id: Some(context_id.to_string()),
        status: TaskStatus { state: TaskState::Failed, message: Some(message.into()) },
        artifacts: None,
        history: None,
    }
}

fn build_canceled_task(task_id: &str, context_id: &str) -> Task {
    Task {
        id: task_id.to_string(),
        context_id: Some(context_id.to_string()),
        status: TaskStatus { state: TaskState::Canceled, message: None },
        artifacts: None,
        history: None,
    }
}

fn sanitize_internal_error(config: &ServerConfig, error: &adk_core::AdkError) -> String {
    if config.security.expose_error_details {
        error.to_string()
    } else {
        "Internal server error".to_string()
    }
}

/// The error for a task the caller cannot see, whether missing or owned by another user.
fn task_not_found(task_id: &str) -> JsonRpcError {
    JsonRpcError::internal_error(format!("Task not found: {task_id}"))
}

/// Registers and spawns a task on behalf of `caller`.
///
/// # Errors
///
/// Returns an `invalid_params` JSON-RPC error when `task_id` names a running task,
/// or a finished task owned by another principal.
async fn start_task(
    controller: &A2aController,
    caller: AuthenticatedCaller,
    context_id: String,
    task_id: String,
    message: Message,
    stream_updates: bool,
) -> Result<StartedTask, JsonRpcError> {
    let owner = caller.user_id().map(str::to_string);

    // The registry lock is held from the duplicate check until the entry is inserted.
    // The spawned task removes its entry under the same lock, so it cannot finish
    // before it is registered and leave a permanent `Working` entry behind.
    let mut active_tasks = controller.active_tasks.lock().await;
    let reused_by_other = controller
        .task_store
        .owner_of(&task_id)
        .await
        .is_some_and(|stored_owner| stored_owner != owner);
    if active_tasks.contains_key(&task_id) || reused_by_other {
        tracing::warn!(task.id = %task_id, "rejected a2a message with a task id already in use");
        return Err(JsonRpcError::invalid_params(format!("task id '{task_id}' is already in use")));
    }

    let token = CancellationToken::new();
    let completion = Arc::new(Notify::new());
    let (task_tx, task_rx) = oneshot::channel();
    let (stream_tx, stream_rx) = if stream_updates {
        let (tx, rx) = mpsc::channel(32);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let root_agent = controller.config.agent_loader.root_agent();
    let executor = Executor::new(ExecutorConfig {
        app_name: root_agent.name().to_string(),
        runner_config: build_runner_config(controller, root_agent, Some(token.clone()), caller.0),
        cancellation_token: Some(token.clone()),
        #[cfg(feature = "a2a-interceptors")]
        interceptor_chain: controller.config.interceptor_chain.clone(),
    });

    let controller_clone = controller.clone();
    let completion_clone = completion.clone();
    let task_id_for_task = task_id.clone();
    let context_id_for_task = context_id.clone();
    let owner_for_task = owner.clone();

    let join_handle = tokio::spawn(async move {
        let result = executor
            .execute_for_user(
                owner_for_task.as_deref(),
                &context_id_for_task,
                &task_id_for_task,
                &message,
            )
            .await;

        match result {
            Ok(events) => {
                if let Some(sender) = stream_tx {
                    for event in &events {
                        if sender
                            .send(StreamTaskMessage::Update(Box::new(event.clone())))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }

                let task = build_task_from_events(&task_id_for_task, &context_id_for_task, &events);
                controller_clone
                    .task_store
                    .store_for(owner_for_task.as_deref(), task.clone())
                    .await;
                let _ = task_tx.send(Ok(task));
            }
            Err(error) => {
                let message = sanitize_internal_error(&controller_clone.config, &error);
                if let Some(sender) = stream_tx {
                    let _ = sender.send(StreamTaskMessage::Error(message.clone())).await;
                }
                controller_clone
                    .task_store
                    .store_for(
                        owner_for_task.as_deref(),
                        build_failed_task(&task_id_for_task, &context_id_for_task, message),
                    )
                    .await;
                let _ = task_tx.send(Err(error));
            }
        }

        controller_clone.active_tasks.lock().await.remove(&task_id_for_task);
        completion_clone.notify_waiters();
    });

    active_tasks.insert(
        task_id,
        ActiveTask {
            token,
            abort_handle: join_handle.abort_handle(),
            completion,
            context_id,
            owner,
        },
    );

    Ok(StartedTask { result: task_rx, updates: stream_rx })
}

/// GET /.well-known/agent-card.json and /.well-known/agent.json - Serve the agent card
pub async fn get_agent_card(State(controller): State<A2aController>) -> impl IntoResponse {
    Json(controller.agent_card.clone())
}

/// POST /a2a - JSON-RPC endpoint for A2A protocol, without caller identity.
///
/// Every request is treated as unauthenticated: sessions belong to
/// `A2A_USER_{contextId}` and only ownerless tasks are visible, even when an
/// authentication layer ran in front of the route.
#[deprecated(
    note = "ignores the authenticated caller; mount `handle_jsonrpc_for_caller`, which the built-in routers use"
)]
pub async fn handle_jsonrpc(
    State(controller): State<A2aController>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    handle_jsonrpc_for_caller(State(controller), AuthenticatedCaller::default(), Json(request))
        .await
}

/// POST /a2a - JSON-RPC endpoint for the A2A protocol.
///
/// The authenticated principal, when present, owns the session and the tasks the
/// request starts. `tasks/get` and `tasks/cancel` answer "not found" for a task
/// owned by anyone else. Without authentication the session user is
/// `A2A_USER_{contextId}`.
pub async fn handle_jsonrpc_for_caller(
    State(controller): State<A2aController>,
    caller: AuthenticatedCaller,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    if request.jsonrpc != "2.0" {
        return Json(JsonRpcResponse::error(
            request.id,
            JsonRpcError::invalid_request("Invalid JSON-RPC version"),
        ));
    }

    match request.method.as_str() {
        jsonrpc::methods::MESSAGE_SEND => {
            handle_message_send(&controller, caller, request.params, request.id).await
        }
        jsonrpc::methods::TASKS_GET => {
            handle_tasks_get(&controller, &caller, request.params, request.id).await
        }
        jsonrpc::methods::TASKS_CANCEL => {
            handle_tasks_cancel(&controller, &caller, request.params, request.id).await
        }
        _ => Json(JsonRpcResponse::error(
            request.id,
            JsonRpcError::method_not_found(&request.method),
        )),
    }
}

/// POST /a2a/stream - SSE streaming endpoint for A2A protocol, without caller identity.
///
/// Every request is treated as unauthenticated; see [`handle_jsonrpc`].
#[deprecated(
    note = "ignores the authenticated caller; mount `handle_jsonrpc_stream_for_caller`, which the built-in routers use"
)]
pub async fn handle_jsonrpc_stream(
    State(controller): State<A2aController>,
    Json(request): Json<JsonRpcRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<JsonRpcResponse>)>
{
    handle_jsonrpc_stream_for_caller(
        State(controller),
        AuthenticatedCaller::default(),
        Json(request),
    )
    .await
}

/// POST /a2a/stream - SSE streaming endpoint for the A2A protocol.
///
/// Binds the session and the started task to the authenticated principal, as
/// [`handle_jsonrpc_for_caller`] does.
pub async fn handle_jsonrpc_stream_for_caller(
    State(controller): State<A2aController>,
    caller: AuthenticatedCaller,
    Json(request): Json<JsonRpcRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<JsonRpcResponse>)>
{
    let bad_request = |error: JsonRpcError| {
        (StatusCode::BAD_REQUEST, Json(JsonRpcResponse::error(request.id.clone(), error)))
    };

    if request.jsonrpc != "2.0" {
        return Err(bad_request(JsonRpcError::invalid_request("Invalid JSON-RPC version")));
    }

    if request.method != jsonrpc::methods::MESSAGE_SEND_STREAM
        && request.method != jsonrpc::methods::MESSAGE_SEND
    {
        return Err(bad_request(JsonRpcError::method_not_found(&request.method)));
    }

    let params: MessageSendParams = match request.params.clone() {
        Some(p) => serde_json::from_value(p)
            .map_err(|e| bad_request(JsonRpcError::invalid_params(e.to_string())))?,
        None => return Err(bad_request(JsonRpcError::invalid_params("Missing params"))),
    };

    let context_id =
        params.message.context_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let task_id =
        params.message.task_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let started = start_task(&controller, caller, context_id, task_id, params.message, true)
        .await
        .map_err(bad_request)?;

    let stream = create_message_stream(started.updates, request.id.clone());

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)).text("ping"),
    ))
}

fn create_message_stream(
    updates: Option<mpsc::Receiver<StreamTaskMessage>>,
    request_id: Option<Value>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let Some(mut stream_rx) = updates else {
            yield Ok(Event::default().event("done").data(""));
            return;
        };

        while let Some(message) = stream_rx.recv().await {
            match message {
                StreamTaskMessage::Update(event) => {
                    let event_data = match event.as_ref() {
                        UpdateEvent::TaskStatusUpdate(status) => {
                            serde_json::to_string(&JsonRpcResponse::success(
                                request_id.clone(),
                                serde_json::to_value(status).unwrap_or_default(),
                            ))
                        }
                        UpdateEvent::TaskArtifactUpdate(artifact) => {
                            serde_json::to_string(&JsonRpcResponse::success(
                                request_id.clone(),
                                serde_json::to_value(artifact).unwrap_or_default(),
                            ))
                        }
                    };

                    if let Ok(data) = event_data {
                        yield Ok(Event::default().data(data));
                    }
                }
                StreamTaskMessage::Error(message) => {
                    let error_response = JsonRpcResponse::error(
                        request_id.clone(),
                        JsonRpcError::internal_error(message),
                    );
                    if let Ok(data) = serde_json::to_string(&error_response) {
                        yield Ok(Event::default().data(data));
                    }
                }
            }
        }

        // Send done event
        yield Ok(Event::default().event("done").data(""));
    }
}

async fn handle_message_send(
    controller: &A2aController,
    caller: AuthenticatedCaller,
    params: Option<Value>,
    id: Option<Value>,
) -> Json<JsonRpcResponse> {
    let params: MessageSendParams = match params {
        Some(p) => match serde_json::from_value(p) {
            Ok(p) => p,
            Err(e) => {
                return Json(JsonRpcResponse::error(
                    id,
                    JsonRpcError::invalid_params(e.to_string()),
                ));
            }
        },
        None => {
            return Json(JsonRpcResponse::error(
                id,
                JsonRpcError::invalid_params("Missing params"),
            ));
        }
    };

    let context_id =
        params.message.context_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let task_id =
        params.message.task_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let started =
        match start_task(controller, caller, context_id, task_id, params.message, false).await {
            Ok(started) => started,
            Err(error) => return Json(JsonRpcResponse::error(id, error)),
        };

    match started.result.await {
        Ok(Ok(task)) => {
            Json(JsonRpcResponse::success(id, serde_json::to_value(task).unwrap_or_default()))
        }
        Ok(Err(e)) => Json(JsonRpcResponse::error(
            id,
            JsonRpcError::internal_error_sanitized(
                &e,
                controller.config.security.expose_error_details,
            ),
        )),
        Err(_) => {
            Json(JsonRpcResponse::error(id, JsonRpcError::internal_error("Task execution aborted")))
        }
    }
}

async fn handle_tasks_get(
    controller: &A2aController,
    caller: &AuthenticatedCaller,
    params: Option<Value>,
    id: Option<Value>,
) -> Json<JsonRpcResponse> {
    let params: TasksGetParams = match params {
        Some(p) => match serde_json::from_value(p) {
            Ok(p) => p,
            Err(e) => {
                return Json(JsonRpcResponse::error(
                    id,
                    JsonRpcError::invalid_params(e.to_string()),
                ));
            }
        },
        None => {
            return Json(JsonRpcResponse::error(
                id,
                JsonRpcError::invalid_params("Missing params"),
            ));
        }
    };

    let owner = caller.user_id();
    let active_task = controller
        .active_tasks
        .lock()
        .await
        .get(&params.task_id)
        .filter(|task| task.owner.as_deref() == owner)
        .cloned();
    if let Some(active_task) = active_task {
        let task = Task {
            id: params.task_id.clone(),
            context_id: Some(active_task.context_id),
            status: TaskStatus { state: TaskState::Working, message: None },
            artifacts: None,
            history: None,
        };

        return Json(JsonRpcResponse::success(id, serde_json::to_value(task).unwrap_or_default()));
    }

    match controller.task_store.get_for(owner, &params.task_id).await {
        Some(task) => {
            Json(JsonRpcResponse::success(id, serde_json::to_value(task).unwrap_or_default()))
        }
        None => Json(JsonRpcResponse::error(id, task_not_found(&params.task_id))),
    }
}

async fn handle_tasks_cancel(
    controller: &A2aController,
    caller: &AuthenticatedCaller,
    params: Option<Value>,
    id: Option<Value>,
) -> Json<JsonRpcResponse> {
    let params: TasksCancelParams = match params {
        Some(p) => match serde_json::from_value(p) {
            Ok(p) => p,
            Err(e) => {
                return Json(JsonRpcResponse::error(
                    id,
                    JsonRpcError::invalid_params(e.to_string()),
                ));
            }
        },
        None => {
            return Json(JsonRpcResponse::error(
                id,
                JsonRpcError::invalid_params("Missing params"),
            ));
        }
    };

    let owner = caller.user_id();
    let active_task = controller
        .active_tasks
        .lock()
        .await
        .get(&params.task_id)
        .filter(|task| task.owner.as_deref() == owner)
        .cloned();

    if let Some(active_task) = active_task {
        active_task.token.cancel();

        if tokio::time::timeout(Duration::from_secs(5), active_task.completion.notified())
            .await
            .is_err()
        {
            active_task.abort_handle.abort();
            controller.active_tasks.lock().await.remove(&params.task_id);
            controller
                .task_store
                .store_for(
                    active_task.owner.as_deref(),
                    build_canceled_task(&params.task_id, &active_task.context_id),
                )
                .await;
        }

        let status = TaskStatusUpdateEvent {
            task_id: params.task_id,
            context_id: Some(active_task.context_id),
            status: TaskStatus { state: TaskState::Canceled, message: None },
            final_update: true,
        };

        return Json(JsonRpcResponse::success(
            id,
            serde_json::to_value(status).unwrap_or_default(),
        ));
    }

    let Some(stored) = controller.task_store.get_for(owner, &params.task_id).await else {
        return Json(JsonRpcResponse::error(id, task_not_found(&params.task_id)));
    };

    let status = TaskStatusUpdateEvent {
        task_id: params.task_id,
        context_id: stored.context_id,
        status: TaskStatus { state: TaskState::Canceled, message: None },
        final_update: true,
    };

    Json(JsonRpcResponse::success(id, serde_json::to_value(status).unwrap_or_default()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult, SingleAgentLoader};
    use adk_session::InMemorySessionService;
    use async_trait::async_trait;
    use futures::stream;

    struct TestAgent;

    #[async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str {
            "card_agent"
        }

        fn description(&self) -> &str {
            "A card test agent"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
            Ok(Box::pin(stream::empty()))
        }
    }

    fn test_config() -> ServerConfig {
        let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(TestAgent)));
        let session_service = Arc::new(InMemorySessionService::new());
        ServerConfig::new(agent_loader, session_service)
    }

    fn skill_doc(name: &str) -> adk_skill::SkillDocument {
        adk_skill::SkillDocument {
            id: format!("{name}-0123456789ab"),
            name: name.to_string(),
            description: format!("{name} description"),
            version: None,
            license: None,
            compatibility: None,
            tags: vec!["indexed".to_string()],
            allowed_tools: vec![],
            references: vec![],
            trigger: false,
            hint: None,
            metadata: Default::default(),
            body: String::new(),
            path: format!("skills/{name}.skill.md").into(),
            hash: "0123456789ab".to_string(),
            last_modified: None,
            triggers: vec![],
        }
    }

    #[test]
    fn with_skill_index_appends_indexed_skills_to_card() {
        let index = Arc::new(adk_skill::SkillIndex::new(vec![
            skill_doc("skill-one"),
            skill_doc("skill-two"),
        ]));

        let controller =
            A2aController::with_skill_index(test_config(), "http://localhost:8080", index);

        let expected = serde_json::json!([
            {
                "id": "card_agent",
                "name": "card_agent",
                "description": "A card test agent",
                "tags": ["agent"],
            },
            {
                "id": "skill-one",
                "name": "skill-one",
                "description": "skill-one description",
                "tags": ["indexed"],
            },
            {
                "id": "skill-two",
                "name": "skill-two",
                "description": "skill-two description",
                "tags": ["indexed"],
            },
        ]);
        assert_eq!(serde_json::to_value(&controller.agent_card.skills).unwrap(), expected);
    }

    #[test]
    fn new_leaves_card_skills_agent_derived() {
        let controller = A2aController::new(test_config(), "http://localhost:8080");

        let expected = serde_json::json!([
            {
                "id": "card_agent",
                "name": "card_agent",
                "description": "A card test agent",
                "tags": ["agent"],
            },
        ]);
        assert_eq!(serde_json::to_value(&controller.agent_card.skills).unwrap(), expected);
    }

    // ── Task store hygiene ─────────────────────────────────────────────────

    /// Replies once `gate` is notified, so a test controls when a task finishes.
    struct GatedAgent {
        gate: Arc<Notify>,
    }

    #[async_trait]
    impl Agent for GatedAgent {
        fn name(&self) -> &str {
            "gated_agent"
        }

        fn description(&self) -> &str {
            "replies when released"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
            let gate = self.gate.clone();
            let invocation_id = ctx.invocation_id().to_string();
            Ok(Box::pin(async_stream::stream! {
                gate.notified().await;
                let mut event = adk_core::Event::new(invocation_id);
                event.author = "gated_agent".to_string();
                event.llm_response.content =
                    Some(adk_core::Content::new("model").with_text("released"));
                yield Ok(event);
            }))
        }
    }

    fn controller_with(agent: Arc<dyn Agent>) -> A2aController {
        let agent_loader = Arc::new(SingleAgentLoader::new(agent));
        let config = ServerConfig::new(agent_loader, Arc::new(InMemorySessionService::new()));
        A2aController::new(config, "http://localhost:8080")
    }

    fn text_message(text: &str) -> Message {
        Message::builder()
            .role(crate::a2a::Role::User)
            .parts(vec![crate::a2a::Part::text(text.to_string())])
            .message_id(uuid::Uuid::new_v4().to_string())
            .build()
    }

    /// A data part the converter does not understand, so execution fails before the agent runs.
    fn unconvertible_message() -> Message {
        let mut data = serde_json::Map::new();
        data.insert("not_a_function_call".to_string(), serde_json::json!("db-password=hunter2"));
        Message::builder()
            .role(crate::a2a::Role::User)
            .parts(vec![crate::a2a::Part::Data { data, metadata: None }])
            .message_id(uuid::Uuid::new_v4().to_string())
            .build()
    }

    fn finished_task(task_id: &str) -> Task {
        build_task_from_events(task_id, "ctx", &[])
    }

    #[tokio::test]
    async fn store_evicts_the_oldest_records_beyond_max_finished() {
        let store = TaskStore::with_retention(A2aTaskRetention::unlimited().with_max_finished(2));
        for task_id in ["t1", "t2", "t3"] {
            store.store(finished_task(task_id)).await;
        }

        assert_eq!(store.len().await, 2);
        assert!(store.get("t1").await.is_none(), "the oldest record is evicted first");
        assert!(store.get("t2").await.is_some());
        assert!(store.get("t3").await.is_some());
    }

    #[tokio::test]
    async fn store_expires_records_after_their_ttl() {
        let store = TaskStore::with_retention(
            A2aTaskRetention::unlimited().with_ttl(Duration::from_secs(60)),
        );
        let long_ago = Instant::now().checked_sub(Duration::from_secs(120)).unwrap();
        store.tasks.write().await.insert(
            "old".to_string(),
            StoredTask { task: finished_task("old"), owner: None, finished_at: long_ago },
        );

        assert!(store.get("old").await.is_none(), "an expired record is not readable");

        store.store(finished_task("new")).await;
        assert_eq!(store.len().await, 1, "storing a record evicts expired ones");
        assert!(store.get("new").await.is_some());
    }

    #[tokio::test]
    async fn store_reads_are_scoped_to_the_owner() {
        let store = TaskStore::new();
        store.store_for(Some("alice"), finished_task("t1")).await;

        assert!(store.get_for(Some("bob"), "t1").await.is_none());
        assert!(store.get_for(None, "t1").await.is_none());
        assert_eq!(
            serde_json::to_value(store.get_for(Some("alice"), "t1").await).unwrap(),
            serde_json::to_value(finished_task("t1")).unwrap()
        );
    }

    #[tokio::test]
    async fn duplicate_task_id_is_rejected_and_the_running_task_keeps_its_handle() {
        let gate = Arc::new(Notify::new());
        let controller = controller_with(Arc::new(GatedAgent { gate: gate.clone() }));
        let caller = AuthenticatedCaller::default();

        let first = start_task(
            &controller,
            caller.clone(),
            "ctx".to_string(),
            "dup".to_string(),
            text_message("one"),
            false,
        )
        .await
        .expect("the first task starts");
        let original_handle = controller.active_tasks.lock().await["dup"].abort_handle.id();

        let duplicate = start_task(
            &controller,
            caller,
            "ctx".to_string(),
            "dup".to_string(),
            text_message("two"),
            false,
        )
        .await;

        let error = duplicate.err().expect("a running task id cannot be reused");
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({ "code": -32602, "message": "task id 'dup' is already in use" })
        );
        assert_eq!(
            controller.active_tasks.lock().await["dup"].abort_handle.id(),
            original_handle,
            "the running task's registration must not be replaced"
        );

        gate.notify_one();
        let task = first.result.await.unwrap().unwrap();
        assert_eq!(task.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn finished_task_id_of_another_owner_cannot_be_reused() {
        let controller = controller_with(Arc::new(TestAgent));
        controller.task_store.store_for(Some("bob"), finished_task("bobs-task")).await;

        let alice = AuthenticatedCaller(Some(RequestContext {
            user_id: "alice".to_string(),
            scopes: vec![],
            metadata: Default::default(),
        }));
        let attempt = start_task(
            &controller,
            alice,
            "ctx".to_string(),
            "bobs-task".to_string(),
            text_message("overwrite"),
            false,
        )
        .await;

        assert!(attempt.is_err(), "another owner's record must not be overwritten");
        assert!(controller.task_store.get_for(Some("bob"), "bobs-task").await.is_some());
    }

    #[tokio::test]
    async fn failed_task_record_carries_the_sanitized_error() {
        let controller = controller_with(Arc::new(TestAgent));

        let started = start_task(
            &controller,
            AuthenticatedCaller::default(),
            "ctx".to_string(),
            "failing".to_string(),
            unconvertible_message(),
            false,
        )
        .await
        .unwrap();
        assert!(started.result.await.unwrap().is_err());

        let stored = controller.task_store.get_for(None, "failing").await.unwrap();
        assert_eq!(
            serde_json::to_value(stored).unwrap(),
            serde_json::to_value(build_failed_task("failing", "ctx", "Internal server error"))
                .unwrap()
        );
    }

    #[tokio::test]
    async fn fast_failing_tasks_leave_no_working_entry_behind() {
        let controller = controller_with(Arc::new(TestAgent));

        for attempt in 0..50 {
            let task_id = format!("fast-{attempt}");
            let started = start_task(
                &controller,
                AuthenticatedCaller::default(),
                "ctx".to_string(),
                task_id.clone(),
                unconvertible_message(),
                false,
            )
            .await
            .unwrap();
            let _ = started.result.await;

            // The spawned task removes its entry after reporting; wait for that step.
            for _ in 0..100 {
                if !controller.active_tasks.lock().await.contains_key(&task_id) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                !controller.active_tasks.lock().await.contains_key(&task_id),
                "a finished task must not stay registered as running"
            );
        }
    }
}
