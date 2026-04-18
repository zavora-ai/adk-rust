use crate::ServerConfig;
use crate::a2a::{
    AgentCard, Executor, ExecutorConfig, JsonRpcError, JsonRpcRequest, JsonRpcResponse, Message,
    MessageSendParams, Task, TaskState, TaskStatus, TaskStatusUpdateEvent, TasksCancelParams,
    TasksGetParams, UpdateEvent, build_agent_card, jsonrpc,
};
use adk_runner::RunnerConfig;
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
use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Notify, RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// In-memory task storage
#[derive(Default)]
pub struct TaskStore {
    tasks: RwLock<HashMap<String, Task>>,
}

impl TaskStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn store(&self, task: Task) {
        self.tasks.write().await.insert(task.id.clone(), task);
    }

    pub async fn get(&self, task_id: &str) -> Option<Task> {
        self.tasks.read().await.get(task_id).cloned()
    }

    pub async fn remove(&self, task_id: &str) -> Option<Task> {
        self.tasks.write().await.remove(task_id)
    }
}

#[derive(Clone)]
struct ActiveTask {
    token: CancellationToken,
    abort_handle: tokio::task::AbortHandle,
    completion: Arc<Notify>,
    context_id: String,
}

enum StreamTaskMessage {
    Update(Box<UpdateEvent>),
    Error(String),
}

/// Controller for A2A protocol endpoints
#[derive(Clone)]
pub struct A2aController {
    config: ServerConfig,
    agent_card: AgentCard,
    task_store: Arc<TaskStore>,
    active_tasks: Arc<Mutex<HashMap<String, ActiveTask>>>,
}

impl A2aController {
    pub fn new(config: ServerConfig, base_url: &str) -> Self {
        let root_agent = config.agent_loader.root_agent();
        let invoke_url = format!("{}/a2a", base_url.trim_end_matches('/'));
        let agent_card = build_agent_card(root_agent.as_ref(), &invoke_url);

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
) -> Arc<RunnerConfig> {
    Arc::new(RunnerConfig {
        app_name: root_agent.name().to_string(),
        agent: root_agent,
        session_service: controller.config.session_service.clone(),
        artifact_service: controller.config.artifact_service.clone(),
        memory_service: controller.config.memory_service.clone(),
        plugin_manager: None,
        run_config: None,
        compaction_config: controller.config.compaction_config.clone(),
        context_cache_config: controller.config.context_cache_config.clone(),
        cache_capable: controller.config.cache_capable.clone(),
        request_context: None,
        cancellation_token,
        intra_compaction_config: None,
        intra_compaction_summarizer: None,
    })
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

async fn start_task(
    controller: &A2aController,
    context_id: String,
    task_id: String,
    message: Message,
    stream_updates: bool,
) -> (oneshot::Receiver<adk_core::Result<Task>>, Option<mpsc::Receiver<StreamTaskMessage>>) {
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
        runner_config: build_runner_config(controller, root_agent, Some(token.clone())),
        cancellation_token: Some(token.clone()),
    });

    let controller_clone = controller.clone();
    let completion_clone = completion.clone();
    let task_id_for_task = task_id.clone();
    let context_id_for_task = context_id.clone();
    let stream_tx_for_task = stream_tx.clone();

    let join_handle = tokio::spawn(async move {
        let result = executor.execute(&context_id_for_task, &task_id_for_task, &message).await;

        match result {
            Ok(events) => {
                if let Some(sender) = stream_tx_for_task {
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
                controller_clone.task_store.store(task.clone()).await;
                let _ = task_tx.send(Ok(task));
            }
            Err(error) => {
                if let Some(sender) = stream_tx_for_task {
                    let _ = sender
                        .send(StreamTaskMessage::Error(sanitize_internal_error(
                            &controller_clone.config,
                            &error,
                        )))
                        .await;
                }
                controller_clone
                    .task_store
                    .store(build_failed_task(
                        &task_id_for_task,
                        &context_id_for_task,
                        error.to_string(),
                    ))
                    .await;
                let _ = task_tx.send(Err(error));
            }
        }

        controller_clone.active_tasks.lock().await.remove(&task_id_for_task);
        completion_clone.notify_waiters();
    });

    controller.active_tasks.lock().await.insert(
        task_id,
        ActiveTask { token, abort_handle: join_handle.abort_handle(), completion, context_id },
    );

    (task_rx, stream_rx)
}

/// GET /.well-known/agent.json - Serve the agent card
pub async fn get_agent_card(State(controller): State<A2aController>) -> impl IntoResponse {
    Json(controller.agent_card.clone())
}

/// POST /a2a - JSON-RPC endpoint for A2A protocol
pub async fn handle_jsonrpc(
    State(controller): State<A2aController>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if request.jsonrpc != "2.0" {
        return Json(JsonRpcResponse::error(
            request.id,
            JsonRpcError::invalid_request("Invalid JSON-RPC version"),
        ));
    }

    match request.method.as_str() {
        jsonrpc::methods::MESSAGE_SEND => {
            handle_message_send(&controller, request.params, request.id).await
        }
        jsonrpc::methods::TASKS_GET => {
            handle_tasks_get(&controller, request.params, request.id).await
        }
        jsonrpc::methods::TASKS_CANCEL => {
            handle_tasks_cancel(&controller, request.params, request.id).await
        }
        _ => Json(JsonRpcResponse::error(
            request.id,
            JsonRpcError::method_not_found(&request.method),
        )),
    }
}

/// POST /a2a/stream - SSE streaming endpoint for A2A protocol
pub async fn handle_jsonrpc_stream(
    State(controller): State<A2aController>,
    Json(request): Json<JsonRpcRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<JsonRpcResponse>)>
{
    if request.jsonrpc != "2.0" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(JsonRpcResponse::error(
                request.id.clone(),
                JsonRpcError::invalid_request("Invalid JSON-RPC version"),
            )),
        ));
    }

    if request.method != jsonrpc::methods::MESSAGE_SEND_STREAM
        && request.method != jsonrpc::methods::MESSAGE_SEND
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(JsonRpcResponse::error(
                request.id.clone(),
                JsonRpcError::method_not_found(&request.method),
            )),
        ));
    }

    let params: MessageSendParams = match request.params {
        Some(p) => serde_json::from_value(p).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(JsonRpcResponse::error(
                    request.id.clone(),
                    JsonRpcError::invalid_params(e.to_string()),
                )),
            )
        })?,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(JsonRpcResponse::error(
                    request.id.clone(),
                    JsonRpcError::invalid_params("Missing params"),
                )),
            ));
        }
    };

    let request_id = request.id.clone();
    let stream = create_message_stream(controller, params, request_id);

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)).text("ping"),
    ))
}

fn create_message_stream(
    controller: A2aController,
    params: MessageSendParams,
    request_id: Option<Value>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let context_id = params
            .message
            .context_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let task_id = params
            .message
            .task_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let (_task_rx, maybe_stream_rx) = start_task(
            &controller,
            context_id.clone(),
            task_id.clone(),
            params.message.clone(),
            true,
        )
        .await;

        let Some(mut stream_rx) = maybe_stream_rx else {
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

    let (task_rx, _) =
        start_task(controller, context_id.clone(), task_id.clone(), params.message, false).await;

    match task_rx.await {
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

    if let Some(active_task) = controller.active_tasks.lock().await.get(&params.task_id).cloned() {
        let task = Task {
            id: params.task_id.clone(),
            context_id: Some(active_task.context_id),
            status: TaskStatus { state: TaskState::Working, message: None },
            artifacts: None,
            history: None,
        };

        return Json(JsonRpcResponse::success(id, serde_json::to_value(task).unwrap_or_default()));
    }

    match controller.task_store.get(&params.task_id).await {
        Some(task) => {
            Json(JsonRpcResponse::success(id, serde_json::to_value(task).unwrap_or_default()))
        }
        None => Json(JsonRpcResponse::error(
            id,
            JsonRpcError::internal_error(format!("Task not found: {}", params.task_id)),
        )),
    }
}

async fn handle_tasks_cancel(
    controller: &A2aController,
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

    let active_task = controller.active_tasks.lock().await.get(&params.task_id).cloned();

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
                .store(build_canceled_task(&params.task_id, &active_task.context_id))
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

    let status = TaskStatusUpdateEvent {
        task_id: params.task_id,
        context_id: Some(uuid::Uuid::new_v4().to_string()),
        status: TaskStatus { state: TaskState::Canceled, message: None },
        final_update: true,
    };

    Json(JsonRpcResponse::success(id, serde_json::to_value(status).unwrap_or_default()))
}
