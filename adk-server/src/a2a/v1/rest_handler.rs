//! A2A v1.0.0 REST handler.
//!
//! Axum routes mapping HTTP methods and URL paths to [`RequestHandler`]
//! operations. Streaming endpoints (`/message:stream`,
//! `/tasks/{taskId}/subscribe`) return SSE streams of `StreamResponse`
//! JSON (no JSON-RPC wrapper).
//!
//! Content-Type for requests and responses is `application/a2a+json`
//! (also accepts `application/json` for backward compatibility).

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::Deserialize;

use a2a_protocol_types::TaskPushNotificationConfig;
use a2a_protocol_types::responses::{ListPushConfigsResponse, SendMessageResponse};

use super::error::A2aError;
use super::request_handler::RequestHandler;
use super::task_store::ListTasksParams;
use crate::auth_bridge::AuthenticatedCaller;

/// Creates the Axum router for A2A v1.0.0 REST endpoints.
///
/// All routes share the [`RequestHandler`] via Axum state.
pub fn rest_router(handler: Arc<RequestHandler>) -> Router {
    Router::new()
        .route("/message:send", post(handle_message_send))
        .route("/message:stream", post(handle_message_stream))
        // `/tasks/{id}:cancel` and `/tasks/{id}:subscribe` (A2A §11.3) share this route with
        // `GET /tasks/{id}`: the router cannot match a verb after a parameter.
        .route("/tasks/{taskId}", get(handle_task_get_or_subscribe).post(handle_task_verb))
        .route("/tasks", get(handle_tasks_list))
        // The paths adk-server served before 3.0.
        .route("/tasks/{taskId}/cancel", post(handle_tasks_cancel))
        .route("/tasks/{taskId}/subscribe", post(handle_tasks_subscribe))
        .route(
            "/tasks/{taskId}/pushNotificationConfigs",
            post(handle_push_config_create).get(handle_push_config_list),
        )
        .route(
            "/tasks/{taskId}/pushNotificationConfigs/{configId}",
            get(handle_push_config_get).delete(handle_push_config_delete),
        )
        .route("/extendedAgentCard", get(handle_extended_agent_card))
        .layer(axum::middleware::from_fn_with_state(handler.clone(), require_extensions))
        .with_state(handler)
}

/// Refuses a request that leaves out an extension the agent card requires (400
/// `FAILED_PRECONDITION`, reason `EXTENSION_SUPPORT_REQUIRED`), as a2a-go does.
async fn require_extensions(
    State(handler): State<Arc<RequestHandler>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let requested = super::extensions::RequestedExtensions::from_headers(request.headers());
    match handler.require_extensions(&requested).await {
        Ok(()) => next.run(request).await,
        Err(error) => error_response(&error),
    }
}

// ── Error response helpers ───────────────────────────────────────────────────

/// Builds an AIP-193 HTTP error response from an [`A2aError`].
fn error_response(err: &A2aError) -> Response {
    let status = axum::http::StatusCode::from_u16(err.http_status())
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    let body = err.to_http_error_response();
    (status, Json(body)).into_response()
}

// ── Query parameter structs ──────────────────────────────────────────────────

/// Query parameters for `GET /tasks/{taskId}`.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TaskGetQuery {
    history_length: Option<u32>,
}

/// Query parameters for `GET /tasks`.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TaskListQuery {
    context_id: Option<String>,
    status: Option<String>,
    page_size: Option<u32>,
    page_token: Option<String>,
    history_length: Option<u32>,
    status_timestamp_after: Option<String>,
    include_artifacts: Option<bool>,
}

// ── Route handlers ───────────────────────────────────────────────────────────

/// `POST /message:send` — sends a message and returns the v1
/// `SendMessageResponse` oneof, `{"task": …}` (A2A §11.4, §6.1).
async fn handle_message_send(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    extensions: super::extensions::RequestedExtensions,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let configuration = match extract_configuration(&params) {
        Ok(configuration) => configuration,
        Err(e) => return error_response(&e),
    };
    let msg = match extract_message(params) {
        Ok(m) => m,
        Err(e) => return error_response(&e),
    };

    match handler
        .for_caller(caller.0)
        .with_extensions(extensions)
        .message_send_with(msg, configuration.as_ref())
        .await
    {
        Ok(task) => a2a_json_response(&SendMessageResponse::Task(task)),
        Err(e) => error_response(&e),
    }
}

/// `POST /message:stream` — sends a message and returns an SSE stream.
async fn handle_message_stream(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    extensions: super::extensions::RequestedExtensions,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let configuration = match extract_configuration(&params) {
        Ok(configuration) => configuration,
        Err(e) => return error_response(&e),
    };
    let msg = match extract_message(params) {
        Ok(m) => m,
        Err(e) => return error_response(&e),
    };

    let event_stream = match handler
        .for_caller(caller.0)
        .with_extensions(extensions)
        .message_stream_with(msg, configuration.as_ref())
        .await
    {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };

    let sse_stream = event_stream.map(|result| -> Result<Event, Infallible> {
        match result {
            Ok(stream_resp) => {
                let json = serde_json::to_string(&stream_resp)
                    .expect("StreamResponse serialization should not fail");
                Ok(Event::default().data(json))
            }
            Err(e) => {
                let body = e.to_http_error_response();
                let json = serde_json::to_string(&body)
                    .expect("error response serialization should not fail");
                Ok(Event::default().data(json))
            }
        }
    });

    super::jsonrpc_handler::sse_response(sse_stream)
}

/// `GET /tasks/{taskId}` — retrieves a task by ID.
/// The task ID and verb of a `/tasks/{taskId}:verb` segment, for a verb A2A defines.
fn task_verb(segment: &str) -> Option<(&str, &str)> {
    segment
        .rsplit_once(':')
        .filter(|(task_id, verb)| !task_id.is_empty() && matches!(*verb, "cancel" | "subscribe"))
}

/// `GET /tasks/{taskId}` gets a task; `GET /tasks/{taskId}:subscribe` subscribes to it
/// (A2A §11.3).
async fn handle_task_get_or_subscribe(
    state: State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path(segment): Path<String>,
    query: Query<TaskGetQuery>,
) -> Response {
    match task_verb(&segment) {
        Some((task_id, "subscribe")) => {
            handle_tasks_subscribe(state, caller, Path(task_id.to_string())).await
        }
        _ => handle_tasks_get(state, caller, Path(segment), query).await,
    }
}

/// `POST /tasks/{taskId}:cancel` cancels a task (A2A §11.3). `POST /tasks/{taskId}:subscribe`
/// also subscribes, for clients that post it.
async fn handle_task_verb(
    state: State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path(segment): Path<String>,
) -> Response {
    match task_verb(&segment) {
        Some((task_id, "cancel")) => {
            handle_tasks_cancel(state, caller, Path(task_id.to_string())).await
        }
        Some((task_id, _)) => {
            handle_tasks_subscribe(state, caller, Path(task_id.to_string())).await
        }
        None => {
            error_response(&A2aError::MethodNotFound { method: format!("POST /tasks/{segment}") })
        }
    }
}

async fn handle_tasks_get(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path(task_id): Path<String>,
    Query(query): Query<TaskGetQuery>,
) -> Response {
    match handler.for_caller(caller.0).tasks_get(&task_id, query.history_length).await {
        Ok(task) => a2a_json_response(&task),
        Err(e) => error_response(&e),
    }
}

/// `POST /tasks/{taskId}/cancel` — cancels a task.
async fn handle_tasks_cancel(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path(task_id): Path<String>,
) -> Response {
    match handler.for_caller(caller.0).tasks_cancel(&task_id).await {
        Ok(task) => a2a_json_response(&task),
        Err(e) => error_response(&e),
    }
}

/// `GET /tasks` — lists tasks with optional filtering and pagination.
async fn handle_tasks_list(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Query(query): Query<TaskListQuery>,
) -> Response {
    let state = match query.status.as_deref().map(|s| (s, parse_task_state(s))) {
        None => None,
        Some((_, Some(state))) => Some(state),
        Some((status, None)) => {
            return error_response(&A2aError::InvalidParams {
                message: format!("unknown task state {status:?}"),
            });
        }
    };

    let params = ListTasksParams {
        context_id: query.context_id,
        state,
        page_size: query.page_size,
        page_token: query.page_token,
        history_length: query.history_length,
        status_timestamp_after: query.status_timestamp_after,
        include_artifacts: query.include_artifacts,
    };

    match handler.for_caller(caller.0).tasks_list(params).await {
        Ok(tasks) => a2a_json_response(&tasks),
        Err(e) => error_response(&e),
    }
}

/// `POST /tasks/{taskId}/subscribe` — subscribes to task updates via SSE.
async fn handle_tasks_subscribe(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path(task_id): Path<String>,
) -> Response {
    let event_stream = match handler.for_caller(caller.0).tasks_subscribe(&task_id).await {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };

    let sse_stream = event_stream.map(|result| -> Result<Event, Infallible> {
        match result {
            Ok(stream_resp) => {
                let json = serde_json::to_string(&stream_resp)
                    .expect("StreamResponse serialization should not fail");
                Ok(Event::default().data(json))
            }
            Err(e) => {
                let body = e.to_http_error_response();
                let json = serde_json::to_string(&body)
                    .expect("error response serialization should not fail");
                Ok(Event::default().data(json))
            }
        }
    });

    super::jsonrpc_handler::sse_response(sse_stream)
}

/// `POST /tasks/{taskId}/pushNotificationConfigs` — creates a push config.
async fn handle_push_config_create(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path(task_id): Path<String>,
    Json(mut config): Json<TaskPushNotificationConfig>,
) -> Response {
    config.task_id = Some(task_id.clone());
    match handler.for_caller(caller.0).push_config_create(&task_id, config).await {
        Ok(created) => a2a_json_response(&created),
        Err(e) => error_response(&e),
    }
}

/// `GET /tasks/{taskId}/pushNotificationConfigs/{configId}` — gets a push config.
async fn handle_push_config_get(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path((task_id, config_id)): Path<(String, String)>,
) -> Response {
    match handler.for_caller(caller.0).push_config_get(&task_id, &config_id).await {
        Ok(config) => a2a_json_response(&config),
        Err(e) => error_response(&e),
    }
}

/// `GET /tasks/{taskId}/pushNotificationConfigs` — lists push configs for a task.
async fn handle_push_config_list(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path(task_id): Path<String>,
) -> Response {
    match handler.for_caller(caller.0).push_config_list(&task_id).await {
        Ok(configs) => {
            a2a_json_response(&ListPushConfigsResponse { configs, next_page_token: None })
        }
        Err(e) => error_response(&e),
    }
}

/// `DELETE /tasks/{taskId}/pushNotificationConfigs/{configId}` — deletes a push config.
async fn handle_push_config_delete(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    Path((task_id, config_id)): Path<(String, String)>,
) -> Response {
    match handler.for_caller(caller.0).push_config_delete(&task_id, &config_id).await {
        Ok(()) => (axum::http::StatusCode::NO_CONTENT, "").into_response(),
        Err(e) => error_response(&e),
    }
}

/// `GET /extendedAgentCard` — returns the extended agent card.
async fn handle_extended_agent_card(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
) -> Response {
    match handler.agent_card_extended_for(caller.0).await {
        Ok(card) => a2a_json_response(&card),
        Err(e) => error_response(&e),
    }
}

// ── Utility functions ────────────────────────────────────────────────────────

/// Extracts a `Message` from a JSON body that may contain a top-level
/// `"message"` field (like `MessageSendParams`) or be the message directly.
fn extract_message(value: serde_json::Value) -> Result<a2a_protocol_types::Message, A2aError> {
    // Try MessageSendParams shape first: { "message": { ... } }
    if let Some(msg_value) = value.get("message") {
        return serde_json::from_value(msg_value.clone())
            .map_err(|e| A2aError::InvalidParams { message: e.to_string() });
    }
    // Fall back to direct Message shape
    serde_json::from_value(value).map_err(|e| A2aError::InvalidParams { message: e.to_string() })
}

/// Reads the `configuration` of a `SendMessageRequest` body (A2A §3.2.2), if it has one.
fn extract_configuration(
    value: &serde_json::Value,
) -> Result<Option<a2a_protocol_types::params::SendMessageConfiguration>, A2aError> {
    value
        .get("configuration")
        .filter(|configuration| !configuration.is_null())
        .map(|configuration| serde_json::from_value(configuration.clone()))
        .transpose()
        .map_err(|e| A2aError::InvalidParams { message: format!("configuration: {e}") })
}

/// Parses a task state string (e.g., `"TASK_STATE_WORKING"`) into a `TaskState`.
fn parse_task_state(s: &str) -> Option<a2a_protocol_types::TaskState> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

/// Returns a JSON response with `Content-Type: application/a2a+json`.
fn a2a_json_response<T: serde::Serialize>(value: &T) -> Response {
    let body = serde_json::to_vec(value).expect("serialization should not fail");
    (axum::http::StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "application/a2a+json")], body)
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::super::card::CachedAgentCard;
    use super::super::executor::V1Executor;
    use super::super::push::NoOpPushNotificationSender;
    use super::super::task_store::InMemoryTaskStore;
    use super::*;

    use a2a_protocol_types::{AgentCapabilities, AgentCard, AgentInterface, AgentSkill};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    fn make_handler() -> Arc<RequestHandler> {
        let store = Arc::new(InMemoryTaskStore::new());
        let executor = Arc::new(V1Executor::new(store.clone()));
        let push_sender = Arc::new(NoOpPushNotificationSender);
        let card = AgentCard {
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
        };
        // The extended card the handler serves: the public card, for any caller.
        let extended = card.clone();
        let producer: super::super::request_handler::ExtendedCardProducer =
            Arc::new(move |_caller| {
                let card = extended.clone();
                Box::pin(async move { Ok(card) })
            });
        let cached = Arc::new(RwLock::new(CachedAgentCard::new(card)));
        Arc::new(
            RequestHandler::new(executor, store, push_sender, cached).with_extended_card(producer),
        )
    }

    fn make_router(handler: Arc<RequestHandler>) -> Router {
        rest_router(handler)
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body collection should succeed")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("response should be valid JSON")
    }

    /// A `WORKING` task, so it can be cancelled.
    async fn working_task(handler: &RequestHandler, task_id: &str) {
        handler.executor().create_task(task_id, "ctx").await.unwrap();
        handler
            .executor()
            .transition_state(task_id, "ctx", a2a_protocol_types::TaskState::Working, None)
            .await
            .unwrap();
    }

    async fn request(handler: &Arc<RequestHandler>, method: &str, uri: &str) -> Response {
        let req = Request::builder().method(method).uri(uri).body(Body::empty()).unwrap();
        make_router(handler.clone()).oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn the_spec_s_cancel_and_subscribe_paths_and_the_old_ones_all_route() {
        let handler = make_handler();

        // A2A §11.3: `POST /tasks/{id}:cancel`.
        working_task(&handler, "t-1").await;
        let resp = request(&handler, "POST", "/tasks/t-1:cancel").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["status"]["state"], "TASK_STATE_CANCELED");

        // `GET /tasks/{id}:subscribe` reaches subscribe: a finished task is refused there.
        let resp = request(&handler, "GET", "/tasks/t-1:subscribe").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(json_body(resp).await.to_string().contains("UNSUPPORTED_OPERATION"));

        // `GET /tasks/{id}` still gets the task.
        let resp = request(&handler, "GET", "/tasks/t-1").await;
        assert_eq!(json_body(resp).await["id"], "t-1");

        // The paths adk-server served before 3.0.
        working_task(&handler, "t-2").await;
        let resp = request(&handler, "POST", "/tasks/t-2/cancel").await;
        assert_eq!(resp.status(), StatusCode::OK);

        // An unknown verb is not a task ID.
        let resp = request(&handler, "POST", "/tasks/t-1:frobnicate").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    fn send_message_body() -> String {
        serde_json::json!({
            "message": {
                "messageId": "msg-1",
                "role": "ROLE_USER",
                "parts": [{"text": "hello"}]
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn post_message_send_returns_the_send_message_response_oneof() {
        let handler = make_handler();
        let app = make_router(handler);

        let req = Request::builder()
            .method("POST")
            .uri("/message:send")
            .header("content-type", "application/json")
            .body(Body::from(send_message_body()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // A2A §6.1 and §11.4: the body is a SendMessageResponse, `{"task": …}`.
        let body = json_body(resp).await;
        let keys: Vec<_> = body.as_object().expect("object body").keys().collect();
        assert_eq!(keys, ["task"], "got: {body}");
        assert!(body["task"]["id"].is_string());
        assert!(body["task"]["contextId"].is_string());
        assert_eq!(body["task"]["status"]["state"], "TASK_STATE_COMPLETED");
        let decoded: SendMessageResponse = serde_json::from_value(body).expect("oneof");
        assert!(matches!(decoded, SendMessageResponse::Task(_)));
    }

    #[tokio::test]
    async fn post_message_send_returns_a2a_content_type() {
        let handler = make_handler();
        let app = make_router(handler);

        let req = Request::builder()
            .method("POST")
            .uri("/message:send")
            .header("content-type", "application/json")
            .body(Body::from(send_message_body()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert_eq!(ct, "application/a2a+json");
    }

    #[tokio::test]
    async fn post_message_send_malformed_body_returns_400() {
        let handler = make_handler();
        let app = make_router(handler);

        let req = Request::builder()
            .method("POST")
            .uri("/message:send")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = json_body(resp).await;
        assert!(body["error"].is_object());
        assert_eq!(body["error"]["code"], 400);
    }

    #[tokio::test]
    async fn get_tasks_by_id_returns_task() {
        let handler = make_handler();
        let app = make_router(handler.clone());

        // Create a task first
        let create_req = Request::builder()
            .method("POST")
            .uri("/message:send")
            .header("content-type", "application/json")
            .body(Body::from(send_message_body()))
            .unwrap();
        let create_resp = app.oneshot(create_req).await.unwrap();
        let create_body = json_body(create_resp).await;
        let task_id = create_body["task"]["id"].as_str().unwrap();

        // Get the task
        let app = make_router(handler);
        let get_req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{task_id}"))
            .body(Body::empty())
            .unwrap();
        let get_resp = app.oneshot(get_req).await.unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);

        let body = json_body(get_resp).await;
        assert_eq!(body["id"], task_id);
    }

    #[tokio::test]
    async fn get_task_not_found_returns_404() {
        let handler = make_handler();
        let app = make_router(handler);

        let req =
            Request::builder().method("GET").uri("/tasks/nonexistent").body(Body::empty()).unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let body = json_body(resp).await;
        assert_eq!(body["error"]["code"], 404);
        assert_eq!(body["error"]["status"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn get_tasks_list_returns_a_list_tasks_response() {
        let handler = make_handler();

        // Create a task
        let app = make_router(handler.clone());
        let req = Request::builder()
            .method("POST")
            .uri("/message:send")
            .header("content-type", "application/json")
            .body(Body::from(send_message_body()))
            .unwrap();
        app.oneshot(req).await.unwrap();

        // List tasks
        let app = make_router(handler);
        let req = Request::builder().method("GET").uri("/tasks").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // A2A §3.1.4: a ListTasksResponse, not a bare array.
        let body = json_body(resp).await;
        assert_eq!(body["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(body["totalSize"], 1);
        assert_eq!(body["nextPageToken"], "");
    }

    #[tokio::test]
    async fn get_extended_agent_card_returns_card() {
        let handler = make_handler();
        let app = make_router(handler);

        let req =
            Request::builder().method("GET").uri("/extendedAgentCard").body(Body::empty()).unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = json_body(resp).await;
        assert_eq!(body["name"], "test-agent");
        assert_eq!(body["version"], "1.0.0");
    }

    #[tokio::test]
    async fn push_config_lifecycle_via_rest() {
        let handler = make_handler();

        // Create a task
        let app = make_router(handler.clone());
        let req = Request::builder()
            .method("POST")
            .uri("/message:send")
            .header("content-type", "application/json")
            .body(Body::from(send_message_body()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = json_body(resp).await;
        let task_id = body["task"]["id"].as_str().unwrap().to_string();

        // Create push config
        let app = make_router(handler.clone());
        let config_body = serde_json::json!({
            "taskId": &task_id,
            "url": "https://example.com/webhook"
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/tasks/{task_id}/pushNotificationConfigs"))
            .header("content-type", "application/json")
            .body(Body::from(config_body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        let config_id = body["id"].as_str().unwrap().to_string();

        // Get push config
        let app = make_router(handler.clone());
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{task_id}/pushNotificationConfigs/{config_id}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        assert_eq!(body["url"], "https://example.com/webhook");

        // List push configs
        let app = make_router(handler.clone());
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{task_id}/pushNotificationConfigs"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        assert_eq!(body["configs"].as_array().unwrap().len(), 1);

        // Delete push config
        let app = make_router(handler.clone());
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/tasks/{task_id}/pushNotificationConfigs/{config_id}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Verify deleted
        let app = make_router(handler);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{task_id}/pushNotificationConfigs"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = json_body(resp).await;
        assert_eq!(body["configs"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn cancel_task_not_found_returns_404() {
        let handler = make_handler();
        let app = make_router(handler);

        let req = Request::builder()
            .method("POST")
            .uri("/tasks/nonexistent/cancel")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
