use crate::ServerConfig;
use crate::a2a::{
    AgentCard, Executor, ExecutorConfig, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    MessageSendParams, Task, TaskState, TaskStatus, TasksCancelParams, TasksGetParams, UpdateEvent,
    build_agent_card, jsonrpc,
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
use tokio::sync::RwLock;

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

/// Controller for A2A protocol endpoints
#[derive(Clone)]
pub struct A2aController {
    config: ServerConfig,
    agent_card: AgentCard,
    task_store: Arc<TaskStore>,
}

impl A2aController {
    pub fn new(config: ServerConfig, base_url: &str) -> Self {
        let root_agent = config.agent_loader.root_agent();
        let invoke_url = format!("{}/a2a", base_url.trim_end_matches('/'));
        let agent_card = build_agent_card(root_agent.as_ref(), &invoke_url);

        Self { config, agent_card, task_store: Arc::new(TaskStore::new()) }
    }
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

        let root_agent = controller.config.agent_loader.root_agent();

        let executor = Executor::new(ExecutorConfig {
            app_name: root_agent.name().to_string(),
            runner_config: Arc::new(RunnerConfig {
                app_name: root_agent.name().to_string(),
                agent: root_agent,
                session_service: controller.config.session_service.clone(),
                artifact_service: controller.config.artifact_service.clone(),
                memory_service: None,
                plugin_manager: None,
                run_config: None,
        compaction_config: None,
                context_cache_config: None,
                cache_capable: None,
            }),
        });

        match executor.execute(&context_id, &task_id, &params.message).await {
            Ok(events) => {
                for event in events {
                    let event_data = match &event {
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
            }
            Err(e) => {
                let error_response = JsonRpcResponse::error(
                    request_id.clone(),
                    JsonRpcError::internal_error_sanitized(
                        &e,
                        controller.config.security.expose_error_details,
                    ),
                );
                if let Ok(data) = serde_json::to_string(&error_response) {
                    yield Ok(Event::default().data(data));
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

    let root_agent = controller.config.agent_loader.root_agent();

    let executor = Executor::new(ExecutorConfig {
        app_name: root_agent.name().to_string(),
        runner_config: Arc::new(RunnerConfig {
            app_name: root_agent.name().to_string(),
            agent: root_agent,
            session_service: controller.config.session_service.clone(),
            artifact_service: controller.config.artifact_service.clone(),
            memory_service: None,
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
        }),
    });

    match executor.execute(&context_id, &task_id, &params.message).await {
        Ok(events) => {
            // Build task from events
            let mut task = Task {
                id: task_id,
                context_id: Some(context_id),
                status: TaskStatus { state: TaskState::Completed, message: None },
                artifacts: Some(vec![]),
                history: None,
            };

            for event in events {
                match event {
                    UpdateEvent::TaskStatusUpdate(status) => {
                        task.status = status.status;
                    }
                    UpdateEvent::TaskArtifactUpdate(artifact) => {
                        if let Some(ref mut artifacts) = task.artifacts {
                            artifacts.push(artifact.artifact);
                        }
                    }
                }
            }

            // Store task for later retrieval
            controller.task_store.store(task.clone()).await;

            Json(JsonRpcResponse::success(id, serde_json::to_value(task).unwrap_or_default()))
        }
        Err(e) => Json(JsonRpcResponse::error(
            id,
            JsonRpcError::internal_error_sanitized(
                &e,
                controller.config.security.expose_error_details,
            ),
        )),
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

    let root_agent = controller.config.agent_loader.root_agent();

    let executor = Executor::new(ExecutorConfig {
        app_name: root_agent.name().to_string(),
        runner_config: Arc::new(RunnerConfig {
            app_name: root_agent.name().to_string(),
            agent: root_agent,
            session_service: controller.config.session_service.clone(),
            artifact_service: controller.config.artifact_service.clone(),
            memory_service: None,
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
        }),
    });

    // Use a default context_id for cancel
    let context_id = uuid::Uuid::new_v4().to_string();

    match executor.cancel(&context_id, &params.task_id).await {
        Ok(status) => {
            Json(JsonRpcResponse::success(id, serde_json::to_value(status).unwrap_or_default()))
        }
        Err(e) => Json(JsonRpcResponse::error(
            id,
            JsonRpcError::internal_error_sanitized(
                &e,
                controller.config.security.expose_error_details,
            ),
        )),
    }
}
