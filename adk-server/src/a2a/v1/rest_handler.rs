//! A2A v1.0.0 REST handler.
//!
//! Axum routes mapping HTTP methods and URL paths to [`RequestHandler`]
//! operations. Streaming endpoints (`/message:stream`,
//! `/tasks/{taskId}:subscribe`) return SSE streams with direct
//! [`StreamResponse`] JSON (no JSON-RPC wrapper).
//!
//! Content-Type for requests and responses is `application/a2a+json`
//! (also accepts `application/json` for backward compatibility).

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::Deserialize;

use a2a_protocol_types::TaskPushNotificationConfig;

use super::error::A2aError;
use super::request_handler::RequestHandler;
use super::task_store::ListTasksParams;

/// Creates the Axum router for A2A v1.0.0 REST endpoints.
///
/// All routes share the [`RequestHandler`] via Axum state.
pub fn rest_router(handler: Arc<RequestHandler>) -> Router {
    Router::new()
        .route("/message:send", post(handle_message_send))
        .route("/message:stream", post(handle_message_stream))
        .route("/tasks/{taskId}", get(handle_tasks_get))
        .route("/tasks/{taskId}/cancel", post(handle_tasks_cancel))
        .route("/tasks", get(handle_tasks_list))
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
        .with_state(handler)
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

/// `POST /message:send` — sends a message and returns a Task.
async fn handle_message_send(
    State(handler): State<Arc<RequestHandler>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let msg = match extract_message(params) {
        Ok(m) => m,
        Err(e) => return error_response(&e),
    };

    match handler.message_send(msg).await {
        Ok(task) => a2a_json_response(&task),
        Err(e) => error_response(&e),
    }
}

/// `POST /message:stream` — sends a message and returns an SSE stream.
async fn handle_message_stream(
    State(handler): State<Arc<RequestHandler>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let msg = match extract_message(params) {
        Ok(m) => m,
        Err(e) => return error_response(&e),
    };

    let event_stream = match handler.message_stream(msg).await {
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

    Sse::new(sse_stream).keep_alive(KeepAlive::default()).into_response()
}

/// `GET /tasks/{taskId}` — retrieves a task by ID.
async fn handle_tasks_get(
    State(handler): State<Arc<RequestHandler>>,
    Path(task_id): Path<String>,
    Query(query): Query<TaskGetQuery>,
) -> Response {
    match handler.tasks_get(&task_id, query.history_length).await {
        Ok(task) => a2a_json_response(&task),
        Err(e) => error_response(&e),
    }
}

/// `POST /tasks/{taskId}/cancel` — cancels a task.
async fn handle_tasks_cancel(
    State(handler): State<Arc<RequestHandler>>,
    Path(task_id): Path<String>,
) -> Response {
    match handler.tasks_cancel(&task_id).await {
        Ok(task) => a2a_json_response(&task),
        Err(e) => error_response(&e),
    }
}

/// `GET /tasks` — lists tasks with optional filtering and pagination.
async fn handle_tasks_list(
    State(handler): State<Arc<RequestHandler>>,
    Query(query): Query<TaskListQuery>,
) -> Response {
    let state = query.status.and_then(|s| parse_task_state(&s));

    let params = ListTasksParams {
        context_id: query.context_id,
        state,
        page_size: query.page_size,
        page_token: query.page_token,
        history_length: query.history_length,
        status_timestamp_after: query.status_timestamp_after,
        include_artifacts: query.include_artifacts,
    };

    match handler.tasks_list(params).await {
        Ok(tasks) => a2a_json_response(&tasks),
        Err(e) => error_response(&e),
    }
}

/// `POST /tasks/{taskId}/subscribe` — subscribes to task updates via SSE.
async fn handle_tasks_subscribe(
    State(handler): State<Arc<RequestHandler>>,
    Path(task_id): Path<String>,
) -> Response {
    let event_stream = match handler.tasks_subscribe(&task_id).await {
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

    Sse::new(sse_stream).keep_alive(KeepAlive::default()).into_response()
}

/// `POST /tasks/{taskId}/pushNotificationConfigs` — creates a push config.
async fn handle_push_config_create(
    State(handler): State<Arc<RequestHandler>>,
    Path(task_id): Path<String>,
    Json(mut config): Json<TaskPushNotificationConfig>,
) -> Response {
    config.task_id = task_id.clone();
    match handler.push_config_create(&task_id, config).await {
        Ok(created) => a2a_json_response(&created),
        Err(e) => error_response(&e),
    }
}

/// `GET /tasks/{taskId}/pushNotificationConfigs/{configId}` — gets a push config.
async fn handle_push_config_get(
    State(handler): State<Arc<RequestHandler>>,
    Path((task_id, config_id)): Path<(String, String)>,
) -> Response {
    match handler.push_config_get(&task_id, &config_id).await {
        Ok(config) => a2a_json_response(&config),
        Err(e) => error_response(&e),
    }
}

/// `GET /tasks/{taskId}/pushNotificationConfigs` — lists push configs for a task.
async fn handle_push_config_list(
    State(handler): State<Arc<RequestHandler>>,
    Path(task_id): Path<String>,
) -> Response {
    match handler.push_config_list(&task_id).await {
        Ok(configs) => a2a_json_response(&configs),
        Err(e) => error_response(&e),
    }
}

/// `DELETE /tasks/{taskId}/pushNotificationConfigs/{configId}` — deletes a push config.
async fn handle_push_config_delete(
    State(handler): State<Arc<RequestHandler>>,
    Path((task_id, config_id)): Path<(String, String)>,
) -> Response {
    match handler.push_config_delete(&task_id, &config_id).await {
        Ok(()) => (axum::http::StatusCode::NO_CONTENT, "").into_response(),
        Err(e) => error_response(&e),
    }
}

/// `GET /extendedAgentCard` — returns the extended agent card.
async fn handle_extended_agent_card(State(handler): State<Arc<RequestHandler>>) -> Response {
    match handler.agent_card_extended().await {
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
        let cached = Arc::new(RwLock::new(CachedAgentCard::new(card)));
        Arc::new(RequestHandler::new(executor, store, push_sender, cached))
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
    async fn post_message_send_returns_task() {
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

        let body = json_body(resp).await;
        assert!(body["id"].is_string());
        assert_eq!(body["status"]["state"], "TASK_STATE_COMPLETED");
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
        let task_id = create_body["id"].as_str().unwrap();

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
    async fn get_tasks_list_returns_array() {
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

        let body = json_body(resp).await;
        assert!(body.is_array());
        assert_eq!(body.as_array().unwrap().len(), 1);
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
        let task_id = body["id"].as_str().unwrap().to_string();

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
        assert_eq!(body.as_array().unwrap().len(), 1);

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
        assert_eq!(body.as_array().unwrap().len(), 0);
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
