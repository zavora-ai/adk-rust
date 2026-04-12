//! A2A v1.0.0 JSON-RPC handler.
//!
//! Axum handler that deserializes a [`JsonRpcRequest`], dispatches to
//! [`RequestHandler`] based on the method name, and wraps results in
//! JSON-RPC responses. Streaming methods (`SendStreamingMessage`,
//! `SubscribeToTask`) return SSE streams with JSON-RPC–wrapped events.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use futures::StreamExt;

use a2a_protocol_types::TaskPushNotificationConfig;
use a2a_protocol_types::jsonrpc::{
    JsonRpcError, JsonRpcErrorResponse, JsonRpcRequest, JsonRpcSuccessResponse, JsonRpcVersion,
};
use a2a_protocol_types::params::{
    CancelTaskParams, DeletePushConfigParams, GetExtendedAgentCardParams, GetPushConfigParams,
    ListPushConfigsParams, MessageSendParams, TaskIdParams, TaskQueryParams,
};

use super::error::A2aError;
use super::request_handler::RequestHandler;
use super::task_store::ListTasksParams;

/// Axum handler for A2A v1.0.0 JSON-RPC requests.
///
/// Deserializes the incoming [`JsonRpcRequest`], dispatches to the appropriate
/// [`RequestHandler`] method, and returns either a JSON response or an SSE
/// stream for streaming methods.
pub async fn jsonrpc_handler(
    State(handler): State<Arc<RequestHandler>>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    let request_id = request.id.clone();

    match request.method.as_str() {
        "SendMessage" => handle_send_message(handler, request).await,
        "SendStreamingMessage" => handle_send_streaming_message(handler, request).await,
        "GetTask" => handle_get_task(handler, request).await,
        "CancelTask" => handle_cancel_task(handler, request).await,
        "ListTasks" => handle_list_tasks(handler, request).await,
        "SubscribeToTask" => handle_subscribe_to_task(handler, request).await,
        "CreateTaskPushNotificationConfig" => handle_push_config_create(handler, request).await,
        "GetTaskPushNotificationConfig" => handle_push_config_get(handler, request).await,
        "ListTaskPushNotificationConfigs" => handle_push_config_list(handler, request).await,
        "DeleteTaskPushNotificationConfig" => handle_push_config_delete(handler, request).await,
        "GetExtendedAgentCard" => handle_get_extended_agent_card(handler, request).await,
        _ => {
            let err = A2aError::MethodNotFound { method: request.method.clone() };
            a2a_json_response(make_error_response(request_id, &err))
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Deserializes JSON-RPC params into the expected type.
fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<serde_json::Value>,
) -> Result<T, A2aError> {
    let value = params.unwrap_or(serde_json::Value::Null);
    serde_json::from_value(value).map_err(|e| A2aError::InvalidParams { message: e.to_string() })
}

/// Builds a JSON-RPC success response.
fn make_success_response<T: serde::Serialize>(
    id: Option<serde_json::Value>,
    result: &T,
) -> serde_json::Value {
    let resp = JsonRpcSuccessResponse { jsonrpc: JsonRpcVersion, id, result };
    serde_json::to_value(&resp).expect("success response serialization should not fail")
}

/// Builds a JSON-RPC error response from an [`A2aError`].
fn make_error_response(id: Option<serde_json::Value>, err: &A2aError) -> serde_json::Value {
    let resp = JsonRpcErrorResponse::new(
        id,
        JsonRpcError::with_data(err.json_rpc_code(), err.to_string(), err.to_error_info()),
    );
    serde_json::to_value(&resp).expect("error response serialization should not fail")
}

/// Wraps a JSON value in a response with `Content-Type: application/a2a+json`.
fn a2a_json_response(value: serde_json::Value) -> Response {
    let body = serde_json::to_vec(&value).unwrap_or_default();
    ([(axum::http::header::CONTENT_TYPE, "application/a2a+json")], body).into_response()
}

// ── Method handlers ──────────────────────────────────────────────────────────

async fn handle_send_message(handler: Arc<RequestHandler>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: MessageSendParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    match handler.message_send(params.message).await {
        Ok(task) => a2a_json_response(make_success_response(id, &task)),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

async fn handle_send_streaming_message(
    handler: Arc<RequestHandler>,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    let params: MessageSendParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    let event_stream = match handler.message_stream(params.message).await {
        Ok(s) => s,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    let request_id = id.clone();
    let sse_stream = event_stream.map(move |result| -> Result<Event, Infallible> {
        match result {
            Ok(stream_resp) => {
                let resp = JsonRpcSuccessResponse {
                    jsonrpc: JsonRpcVersion,
                    id: request_id.clone(),
                    result: &stream_resp,
                };
                let json = serde_json::to_string(&resp)
                    .expect("StreamResponse serialization should not fail");
                Ok(Event::default().data(json))
            }
            Err(e) => {
                let resp = JsonRpcErrorResponse::new(
                    request_id.clone(),
                    JsonRpcError::with_data(e.json_rpc_code(), e.to_string(), e.to_error_info()),
                );
                let json = serde_json::to_string(&resp)
                    .expect("error response serialization should not fail");
                Ok(Event::default().data(json))
            }
        }
    });

    Sse::new(sse_stream).keep_alive(KeepAlive::default()).into_response()
}

async fn handle_get_task(handler: Arc<RequestHandler>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: TaskQueryParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    match handler.tasks_get(&params.id, params.history_length).await {
        Ok(task) => a2a_json_response(make_success_response(id, &task)),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

async fn handle_cancel_task(handler: Arc<RequestHandler>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: CancelTaskParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    match handler.tasks_cancel(&params.id).await {
        Ok(task) => a2a_json_response(make_success_response(id, &task)),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

async fn handle_list_tasks(handler: Arc<RequestHandler>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: a2a_protocol_types::params::ListTasksParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    // Convert wire params to internal ListTasksParams
    let internal_params = ListTasksParams {
        context_id: params.context_id,
        state: params.status,
        page_size: params.page_size,
        page_token: params.page_token,
        history_length: params.history_length,
        status_timestamp_after: params.status_timestamp_after,
        include_artifacts: params.include_artifacts,
    };

    match handler.tasks_list(internal_params).await {
        Ok(tasks) => a2a_json_response(make_success_response(id, &tasks)),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

async fn handle_subscribe_to_task(
    handler: Arc<RequestHandler>,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    let params: TaskIdParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    let event_stream = match handler.tasks_subscribe(&params.id).await {
        Ok(s) => s,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    let request_id = id.clone();
    let sse_stream = event_stream.map(move |result| -> Result<Event, Infallible> {
        match result {
            Ok(stream_resp) => {
                let resp = JsonRpcSuccessResponse {
                    jsonrpc: JsonRpcVersion,
                    id: request_id.clone(),
                    result: &stream_resp,
                };
                let json = serde_json::to_string(&resp)
                    .expect("StreamResponse serialization should not fail");
                Ok(Event::default().data(json))
            }
            Err(e) => {
                let resp = JsonRpcErrorResponse::new(
                    request_id.clone(),
                    JsonRpcError::with_data(e.json_rpc_code(), e.to_string(), e.to_error_info()),
                );
                let json = serde_json::to_string(&resp)
                    .expect("error response serialization should not fail");
                Ok(Event::default().data(json))
            }
        }
    });

    Sse::new(sse_stream).keep_alive(KeepAlive::default()).into_response()
}

async fn handle_push_config_create(
    handler: Arc<RequestHandler>,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    let config: TaskPushNotificationConfig = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    let task_id = config.task_id.clone();
    match handler.push_config_create(&task_id, config).await {
        Ok(created) => a2a_json_response(make_success_response(id, &created)),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

async fn handle_push_config_get(handler: Arc<RequestHandler>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: GetPushConfigParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    match handler.push_config_get(&params.task_id, &params.id).await {
        Ok(config) => a2a_json_response(make_success_response(id, &config)),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

async fn handle_push_config_list(
    handler: Arc<RequestHandler>,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    let params: ListPushConfigsParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    match handler.push_config_list(&params.task_id).await {
        Ok(configs) => a2a_json_response(make_success_response(id, &configs)),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

async fn handle_push_config_delete(
    handler: Arc<RequestHandler>,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    let params: DeletePushConfigParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return a2a_json_response(make_error_response(id, &e)),
    };

    match handler.push_config_delete(&params.task_id, &params.id).await {
        Ok(()) => a2a_json_response(make_success_response(id, &serde_json::json!({}))),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

async fn handle_get_extended_agent_card(
    handler: Arc<RequestHandler>,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    // GetExtendedAgentCard params are optional
    let _params: Option<GetExtendedAgentCardParams> =
        request.params.and_then(|v| serde_json::from_value(v).ok());

    match handler.agent_card_extended().await {
        Ok(card) => a2a_json_response(make_success_response(id, &card)),
        Err(e) => a2a_json_response(make_error_response(id, &e)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::card::CachedAgentCard;
    use super::super::executor::V1Executor;
    use super::super::push::NoOpPushNotificationSender;
    use super::super::task_store::InMemoryTaskStore;
    use super::*;

    use a2a_protocol_types::{AgentCapabilities, AgentCard, AgentInterface, AgentSkill};
    use http_body_util::BodyExt;
    use tokio::sync::RwLock;

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

    /// Calls the handler directly with a JSON-RPC request and returns the
    /// response body as a `serde_json::Value`.
    async fn call_handler(
        handler: Arc<RequestHandler>,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let request = JsonRpcRequest::with_params(serde_json::json!(1), method, params);
        let response = jsonrpc_handler(State(handler), Json(request)).await;
        let (_, body) = response.into_parts();
        let bytes = body.collect().await.expect("body collection should succeed").to_bytes();
        serde_json::from_slice(&bytes).expect("response should be valid JSON")
    }

    #[tokio::test]
    async fn send_message_returns_task() {
        let handler = make_handler();
        let resp = call_handler(
            handler,
            "SendMessage",
            serde_json::json!({
                "message": {
                    "messageId": "msg-1",
                    "role": "ROLE_USER",
                    "parts": [{"text": "hello"}]
                }
            }),
        )
        .await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp["result"].is_object(), "expected result object, got: {resp}");
        assert!(resp["result"]["id"].is_string());
        assert_eq!(resp["result"]["status"]["state"], "TASK_STATE_COMPLETED");
    }

    #[tokio::test]
    async fn get_task_returns_task() {
        let handler = make_handler();

        // Create a task first
        let create_resp = call_handler(
            handler.clone(),
            "SendMessage",
            serde_json::json!({
                "message": {
                    "messageId": "msg-1",
                    "role": "ROLE_USER",
                    "parts": [{"text": "hello"}]
                }
            }),
        )
        .await;
        let task_id = create_resp["result"]["id"].as_str().unwrap();

        // Get the task
        let resp = call_handler(handler, "GetTask", serde_json::json!({ "id": task_id })).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["result"]["id"], task_id);
    }

    #[tokio::test]
    async fn get_task_not_found() {
        let handler = make_handler();
        let resp =
            call_handler(handler, "GetTask", serde_json::json!({ "id": "nonexistent" })).await;

        assert!(resp["error"].is_object());
        assert_eq!(resp["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn cancel_task_not_found() {
        let handler = make_handler();
        let resp =
            call_handler(handler, "CancelTask", serde_json::json!({ "id": "nonexistent" })).await;

        assert!(resp["error"].is_object());
        assert_eq!(resp["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn list_tasks_empty() {
        let handler = make_handler();
        let resp = call_handler(handler, "ListTasks", serde_json::json!({})).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp["result"].is_array());
        assert_eq!(resp["result"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn unknown_method_returns_32601() {
        let handler = make_handler();
        let resp = call_handler(handler, "UnknownMethod", serde_json::json!({})).await;

        assert!(resp["error"].is_object());
        assert_eq!(resp["error"]["code"], -32601);
        assert!(resp["error"]["message"].as_str().unwrap().contains("UnknownMethod"));
    }

    #[tokio::test]
    async fn invalid_params_returns_error() {
        let handler = make_handler();
        // SendMessage requires a "message" field — sending empty params should fail
        let resp = call_handler(handler, "SendMessage", serde_json::json!({})).await;

        assert!(resp["error"].is_object());
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn get_extended_agent_card_returns_card() {
        let handler = make_handler();
        let resp = call_handler(handler, "GetExtendedAgentCard", serde_json::json!({})).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["result"]["name"], "test-agent");
        assert_eq!(resp["result"]["version"], "1.0.0");
    }

    #[tokio::test]
    async fn json_rpc_response_has_a2a_content_type() {
        let handler = make_handler();
        let request = JsonRpcRequest::with_params(
            serde_json::json!(1),
            "SendMessage",
            serde_json::json!({
                "message": {
                    "messageId": "msg-ct",
                    "role": "ROLE_USER",
                    "parts": [{"text": "hello"}]
                }
            }),
        );
        let response = jsonrpc_handler(State(handler), Json(request)).await;
        let (parts, _) = response.into_parts();
        let content_type = parts.headers.get("content-type").expect("should have content-type");
        assert_eq!(content_type, "application/a2a+json");
    }

    #[tokio::test]
    async fn push_config_lifecycle_via_jsonrpc() {
        let handler = make_handler();

        // Create a task
        let create_resp = call_handler(
            handler.clone(),
            "SendMessage",
            serde_json::json!({
                "message": {
                    "messageId": "msg-1",
                    "role": "ROLE_USER",
                    "parts": [{"text": "hello"}]
                }
            }),
        )
        .await;
        let task_id = create_resp["result"]["id"].as_str().unwrap().to_string();

        // Create push config
        let create_config_resp = call_handler(
            handler.clone(),
            "CreateTaskPushNotificationConfig",
            serde_json::json!({
                "taskId": &task_id,
                "url": "https://example.com/webhook"
            }),
        )
        .await;
        assert!(create_config_resp["result"].is_object());
        let config_id = create_config_resp["result"]["id"].as_str().unwrap().to_string();

        // Get push config
        let get_resp = call_handler(
            handler.clone(),
            "GetTaskPushNotificationConfig",
            serde_json::json!({
                "taskId": &task_id,
                "id": &config_id
            }),
        )
        .await;
        assert_eq!(get_resp["result"]["url"], "https://example.com/webhook");

        // List push configs
        let list_resp = call_handler(
            handler.clone(),
            "ListTaskPushNotificationConfigs",
            serde_json::json!({ "taskId": &task_id }),
        )
        .await;
        assert_eq!(list_resp["result"].as_array().unwrap().len(), 1);

        // Delete push config
        let delete_resp = call_handler(
            handler.clone(),
            "DeleteTaskPushNotificationConfig",
            serde_json::json!({
                "taskId": &task_id,
                "id": &config_id
            }),
        )
        .await;
        assert!(delete_resp["result"].is_object());
        assert!(delete_resp["error"].is_null());

        // Verify deleted
        let list_resp2 = call_handler(
            handler,
            "ListTaskPushNotificationConfigs",
            serde_json::json!({ "taskId": &task_id }),
        )
        .await;
        assert_eq!(list_resp2["result"].as_array().unwrap().len(), 0);
    }
}
