//! A2A v1.0.0 JSON-RPC handler.
//!
//! Axum handler that reads a [`JsonRpcRequest`], dispatches to
//! [`RequestHandler`] based on the method name, and wraps results in
//! JSON-RPC responses. Streaming methods (`SendStreamingMessage`,
//! `SubscribeToTask`) return SSE streams with JSON-RPC–wrapped events.
//!
//! Every non-streaming answer, errors included, is HTTP 200 with
//! `Content-Type: application/json` (A2A §9.1). A body that is not JSON gets
//! JSON-RPC error `-32700`, and JSON that is not a request object gets
//! `-32600` (§9.5), so a client always receives a JSON-RPC response it can
//! parse.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use futures::StreamExt;
use serde_json::{Value, json};

use a2a_protocol_types::TaskPushNotificationConfig;
use a2a_protocol_types::jsonrpc::{
    JsonRpcError, JsonRpcErrorResponse, JsonRpcId, JsonRpcRequest, JsonRpcRequestId,
    JsonRpcSuccessResponse, JsonRpcVersion,
};
use a2a_protocol_types::params::{
    CancelTaskParams, DeletePushConfigParams, GetExtendedAgentCardParams, GetPushConfigParams,
    ListPushConfigsParams, MessageSendParams, TaskIdParams, TaskQueryParams,
};
use a2a_protocol_types::responses::{ListPushConfigsResponse, SendMessageResponse};

use super::error::A2aError;
use super::request_handler::{CallerScope, RequestHandler};
use super::task_store::ListTasksParams;
use crate::auth_bridge::AuthenticatedCaller;

/// JSON-RPC 2.0 `Parse error`, A2A `JSONParseError` (§9.5): the body is not JSON.
const PARSE_ERROR: i32 = -32700;

/// JSON-RPC 2.0 `Invalid Request`, A2A `InvalidRequestError` (§9.5): the JSON
/// is not a request object.
const INVALID_REQUEST: i32 = -32600;

/// Axum handler for A2A v1.0.0 JSON-RPC requests.
///
/// Reads the [`JsonRpcRequest`] from the body, dispatches to the appropriate
/// [`RequestHandler`] method, and returns either a JSON response or an SSE
/// stream for streaming methods. `SendMessage` answers with the v1
/// `SendMessageResponse` oneof, `{"task": …}`.
///
/// The body is read as bytes rather than through axum's `Json` extractor, so a
/// malformed request gets a JSON-RPC error instead of a plain-text HTTP 400 or
/// 422: `-32700` when the body is not JSON, and `-32600` when it is JSON but
/// not a request object (no `method`, `jsonrpc` other than `"2.0"`, an `id`
/// that is not a string, number or `null`, or a batch array). Those errors
/// carry the request's `id` when it can be read, and `null` otherwise.
///
/// Every operation runs for the [`AuthenticatedCaller`] read from the request
/// extensions (see [`RequestHandler::for_caller`]): an authenticated principal
/// owns the session and the tasks it creates, and cannot reach anyone else's.
/// Mount the route behind the middleware that authenticates callers.
///
/// This handler does not negotiate `A2A-Version`; mount it behind
/// [`version_negotiation`](super::version::version_negotiation) for that.
pub async fn jsonrpc_handler(
    State(handler): State<Arc<RequestHandler>>,
    caller: AuthenticatedCaller,
    body: Bytes,
) -> Response {
    let request = match parse_request(&body) {
        Ok(request) => request,
        Err(error) => return json_response(error),
    };
    let request_id = request.id.clone();
    let scope = handler.for_caller(caller.0);

    match request.method.as_str() {
        "SendMessage" => handle_send_message(&scope, request).await,
        "SendStreamingMessage" => handle_send_streaming_message(&scope, request).await,
        "GetTask" => handle_get_task(&scope, request).await,
        "CancelTask" => handle_cancel_task(&scope, request).await,
        "ListTasks" => handle_list_tasks(&scope, request).await,
        "SubscribeToTask" => handle_subscribe_to_task(&scope, request).await,
        "CreateTaskPushNotificationConfig" => handle_push_config_create(&scope, request).await,
        "GetTaskPushNotificationConfig" => handle_push_config_get(&scope, request).await,
        "ListTaskPushNotificationConfigs" => handle_push_config_list(&scope, request).await,
        "DeleteTaskPushNotificationConfig" => handle_push_config_delete(&scope, request).await,
        "GetExtendedAgentCard" => handle_get_extended_agent_card(&handler, request).await,
        _ => {
            let err = A2aError::MethodNotFound { method: request.method.clone() };
            json_response(make_error_response(request_id, &err))
        }
    }
}

// ── Envelope ─────────────────────────────────────────────────────────────────

/// Reads a JSON-RPC request object from `body`.
///
/// On failure, returns the JSON-RPC error response to send instead: `-32700`
/// when `body` is not JSON, and `-32600` when it is JSON but not a request
/// object.
fn parse_request(body: &[u8]) -> Result<JsonRpcRequest, Value> {
    let value: Value = serde_json::from_slice(body).map_err(|e| {
        envelope_error(None, PARSE_ERROR, "Invalid JSON payload", "JSON_PARSE", e.to_string())
    })?;
    let invalid = |id: JsonRpcId, detail: String| {
        envelope_error(
            id,
            INVALID_REQUEST,
            "Request payload validation error",
            "INVALID_REQUEST",
            detail,
        )
    };
    let Some(object) = value.as_object() else {
        // Batches (arrays) are not part of A2A, and scalars are not requests.
        return Err(invalid(None, "the request must be a JSON object".to_string()));
    };
    let id = match object.get("id") {
        None | Some(Value::Null) => None,
        Some(id @ (Value::String(_) | Value::Number(_))) => Some(id.clone()),
        Some(_) => {
            return Err(invalid(None, "`id` must be a string, a number or null".to_string()));
        }
    };
    serde_json::from_value(value).map_err(|e| invalid(id, e.to_string()))
}

/// Returns the `id` to answer a JSON-RPC request with, when `body` is a
/// JSON-RPC request object (a JSON object with a `jsonrpc` member); `None` when
/// it is not one, such as an HTTP+JSON body.
///
/// The inner value is the request's `id` when it is a string or a number, and
/// `None` (sent as `null`) otherwise.
pub(crate) fn jsonrpc_request_id(body: &[u8]) -> Option<JsonRpcId> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    object
        .contains_key("jsonrpc")
        .then(|| object.get("id").filter(|id| id.is_string() || id.is_number()).cloned())
}

/// Builds a JSON-RPC error response for an envelope error (`-32700`,
/// `-32600`), with `detail` in a `google.rpc.ErrorInfo` as A2A errors carry it.
fn envelope_error(id: JsonRpcId, code: i32, message: &str, reason: &str, detail: String) -> Value {
    let data = json!([{
        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
        "reason": reason,
        "domain": "a2a-protocol.org",
        "metadata": { "message": detail },
    }]);
    let resp = JsonRpcErrorResponse::new(id, JsonRpcError::with_data(code, message, data));
    serde_json::to_value(&resp).expect("error response serialization should not fail")
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Deserializes JSON-RPC params into the expected type.
fn parse_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, A2aError> {
    let value = params.unwrap_or(Value::Null);
    serde_json::from_value(value).map_err(|e| A2aError::InvalidParams { message: e.to_string() })
}

/// Builds a JSON-RPC success response.
fn make_success_response<T: serde::Serialize>(id: JsonRpcRequestId, result: &T) -> Value {
    let resp = JsonRpcSuccessResponse { jsonrpc: JsonRpcVersion, id: id.to_response_id(), result };
    serde_json::to_value(&resp).expect("success response serialization should not fail")
}

/// Builds a JSON-RPC error response from an [`A2aError`].
fn make_error_response(id: JsonRpcRequestId, err: &A2aError) -> Value {
    error_body(id.to_response_id(), err)
}

/// The JSON-RPC error response body for `err`, answering the request `id`.
fn error_body(id: JsonRpcId, err: &A2aError) -> Value {
    let resp = JsonRpcErrorResponse::new(
        id,
        JsonRpcError::with_data(err.json_rpc_code(), err.to_string(), err.to_error_info()),
    );
    serde_json::to_value(&resp).expect("error response serialization should not fail")
}

/// The JSON-RPC error response for `err`, answering the request `id`: HTTP
/// 200 with `Content-Type: application/json`, as the JSON-RPC binding answers
/// every error (A2A §9.1, §9.5).
pub(crate) fn error_response(id: JsonRpcId, err: &A2aError) -> Response {
    json_response(error_body(id, err))
}

/// Wraps a JSON value in a response with `Content-Type: application/json`,
/// the JSON-RPC binding's media type for requests and responses (A2A §9.1).
fn json_response(value: Value) -> Response {
    let body = serde_json::to_vec(&value).unwrap_or_default();
    ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response()
}

// ── Method handlers ──────────────────────────────────────────────────────────

async fn handle_send_message(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: MessageSendParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    match handler.message_send_with(params.message, params.configuration.as_ref()).await {
        Ok(task) => json_response(make_success_response(id, &SendMessageResponse::Task(task))),
        Err(e) => json_response(make_error_response(id, &e)),
    }
}

async fn handle_send_streaming_message(
    handler: &CallerScope<'_>,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    let params: MessageSendParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    let event_stream = match handler.message_stream(params.message).await {
        Ok(s) => s,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    let request_id = id.clone();
    let sse_stream = event_stream.map(move |result| -> Result<Event, Infallible> {
        match result {
            Ok(stream_resp) => {
                let resp = JsonRpcSuccessResponse {
                    jsonrpc: JsonRpcVersion,
                    id: request_id.to_response_id(),
                    result: &stream_resp,
                };
                let json = serde_json::to_string(&resp)
                    .expect("StreamResponse serialization should not fail");
                Ok(Event::default().data(json))
            }
            Err(e) => {
                let resp = JsonRpcErrorResponse::new(
                    request_id.to_response_id(),
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

async fn handle_get_task(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: TaskQueryParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    match handler.tasks_get(&params.id, params.history_length).await {
        Ok(task) => json_response(make_success_response(id, &task)),
        Err(e) => json_response(make_error_response(id, &e)),
    }
}

async fn handle_cancel_task(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: CancelTaskParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    match handler.tasks_cancel(&params.id).await {
        Ok(task) => json_response(make_success_response(id, &task)),
        Err(e) => json_response(make_error_response(id, &e)),
    }
}

async fn handle_list_tasks(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: a2a_protocol_types::params::ListTasksParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
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
        Ok(tasks) => json_response(make_success_response(id, &tasks)),
        Err(e) => json_response(make_error_response(id, &e)),
    }
}

async fn handle_subscribe_to_task(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: TaskIdParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    let event_stream = match handler.tasks_subscribe(&params.id).await {
        Ok(s) => s,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    let request_id = id.clone();
    let sse_stream = event_stream.map(move |result| -> Result<Event, Infallible> {
        match result {
            Ok(stream_resp) => {
                let resp = JsonRpcSuccessResponse {
                    jsonrpc: JsonRpcVersion,
                    id: request_id.to_response_id(),
                    result: &stream_resp,
                };
                let json = serde_json::to_string(&resp)
                    .expect("StreamResponse serialization should not fail");
                Ok(Event::default().data(json))
            }
            Err(e) => {
                let resp = JsonRpcErrorResponse::new(
                    request_id.to_response_id(),
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

async fn handle_push_config_create(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let config: TaskPushNotificationConfig = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    // `TaskPushNotificationConfig::task_id` became Option<String> in
    // a2a-protocol-types 0.12. An absent id is a malformed request here, not a
    // default: rejecting it is what the previous non-optional field enforced.
    let Some(task_id) = config.task_id.clone() else {
        return json_response(make_error_response(
            id,
            &A2aError::InvalidParams { message: "task_id is required".to_string() },
        ));
    };
    match handler.push_config_create(&task_id, config).await {
        Ok(created) => json_response(make_success_response(id, &created)),
        Err(e) => json_response(make_error_response(id, &e)),
    }
}

async fn handle_push_config_get(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: GetPushConfigParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    match handler.push_config_get(&params.task_id, &params.id).await {
        Ok(config) => json_response(make_success_response(id, &config)),
        Err(e) => json_response(make_error_response(id, &e)),
    }
}

async fn handle_push_config_list(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: ListPushConfigsParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    match handler.push_config_list(&params.task_id).await {
        Ok(configs) => {
            let page = ListPushConfigsResponse { configs, next_page_token: None };
            json_response(make_success_response(id, &page))
        }
        Err(e) => json_response(make_error_response(id, &e)),
    }
}

async fn handle_push_config_delete(handler: &CallerScope<'_>, request: JsonRpcRequest) -> Response {
    let id = request.id.clone();
    let params: DeletePushConfigParams = match parse_params(request.params) {
        Ok(p) => p,
        Err(e) => return json_response(make_error_response(id, &e)),
    };

    match handler.push_config_delete(&params.task_id, &params.id).await {
        Ok(()) => json_response(make_success_response(id, &json!({}))),
        Err(e) => json_response(make_error_response(id, &e)),
    }
}

async fn handle_get_extended_agent_card(
    handler: &RequestHandler,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    // GetExtendedAgentCard params are optional
    let _params: Option<GetExtendedAgentCardParams> =
        request.params.and_then(|v| serde_json::from_value(v).ok());

    match handler.agent_card_extended().await {
        Ok(card) => json_response(make_success_response(id, &card)),
        Err(e) => json_response(make_error_response(id, &e)),
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
        call_as(handler, AuthenticatedCaller::default(), method, params).await
    }

    /// Like [`call_handler`], with `caller` as the request's authenticated principal.
    async fn call_as(
        handler: Arc<RequestHandler>,
        caller: AuthenticatedCaller,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let request = JsonRpcRequest::with_params(serde_json::json!(1), method, params);
        let body = serde_json::to_vec(&request).expect("request serializes");
        let (_, value) = post_raw(handler, caller, body).await;
        value
    }

    /// Posts `body` to the handler as is and returns the response parts and its
    /// body as JSON.
    async fn post_raw(
        handler: Arc<RequestHandler>,
        caller: AuthenticatedCaller,
        body: impl Into<Bytes>,
    ) -> (axum::http::response::Parts, serde_json::Value) {
        let response = jsonrpc_handler(State(handler), caller, body.into()).await;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.expect("body collection should succeed").to_bytes();
        (parts, serde_json::from_slice(&bytes).expect("response should be valid JSON"))
    }

    fn send_message_params(message_id: &str) -> serde_json::Value {
        serde_json::json!({
            "message": { "messageId": message_id, "role": "ROLE_USER", "parts": [{"text": "hello"}] }
        })
    }

    fn user(user_id: &str) -> AuthenticatedCaller {
        AuthenticatedCaller(Some(crate::auth_bridge::RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        }))
    }

    #[tokio::test]
    async fn tasks_are_scoped_to_the_authenticated_caller() {
        let handler = make_handler();
        let created = call_as(
            handler.clone(),
            user("bob"),
            "SendMessage",
            serde_json::json!({
                "message": { "messageId": "msg-bob", "role": "ROLE_USER", "parts": [{"text": "hi"}] }
            }),
        )
        .await;
        let task_id = created["result"]["task"]["id"].as_str().unwrap().to_string();

        for method in ["GetTask", "CancelTask"] {
            let as_alice = call_as(
                handler.clone(),
                user("alice"),
                method,
                serde_json::json!({ "id": &task_id }),
            )
            .await;
            assert_eq!(as_alice["error"]["code"], -32001, "{method} must not reach bob's task");
        }

        let as_bob =
            call_as(handler, user("bob"), "GetTask", serde_json::json!({ "id": &task_id })).await;
        assert_eq!(as_bob["result"]["id"], task_id.as_str());
    }

    #[tokio::test]
    async fn send_message_returns_the_send_message_response_oneof() {
        let handler = make_handler();
        let resp = call_handler(handler, "SendMessage", send_message_params("msg-1")).await;

        // A2A §9.4.1: `result` is a SendMessageResponse, `{"task": …}` or `{"message": …}`.
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        let result = resp["result"].as_object().expect("result object");
        assert_eq!(result.keys().collect::<Vec<_>>(), ["task"], "got: {resp}");
        let task = &resp["result"]["task"];
        assert!(task["id"].is_string());
        assert!(task["contextId"].is_string());
        assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
        assert!(task.get("kind").is_none(), "v1 has no 0.3 `kind` discriminator");

        // The body reads back as the protocol type, which is what other SDKs decode.
        let decoded: SendMessageResponse =
            serde_json::from_value(resp["result"].clone()).expect("SendMessageResponse");
        assert!(matches!(decoded, SendMessageResponse::Task(_)));
    }

    #[tokio::test]
    async fn malformed_json_returns_parse_error_with_null_id() {
        let (parts, resp) =
            post_raw(make_handler(), AuthenticatedCaller::default(), "{not json").await;

        assert_eq!(parts.status, axum::http::StatusCode::OK);
        assert_eq!(parts.headers["content-type"], "application/json");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp["id"].is_null(), "the id cannot be read from invalid JSON: {resp}");
        assert_eq!(resp["error"]["code"], PARSE_ERROR);
        assert_eq!(resp["error"]["message"], "Invalid JSON payload");
        assert_eq!(resp["error"]["data"][0]["reason"], "JSON_PARSE");
        assert!(resp.get("result").is_none());
    }

    #[tokio::test]
    async fn a_request_without_method_returns_invalid_request_with_its_id() {
        let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "params": {} }).to_string();
        let (parts, resp) = post_raw(make_handler(), AuthenticatedCaller::default(), body).await;

        assert_eq!(parts.status, axum::http::StatusCode::OK);
        assert_eq!(parts.headers["content-type"], "application/json");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["error"]["code"], INVALID_REQUEST);
        assert_eq!(resp["error"]["message"], "Request payload validation error");
        assert_eq!(resp["error"]["data"][0]["reason"], "INVALID_REQUEST");
    }

    #[tokio::test]
    async fn json_that_is_not_a_request_object_returns_invalid_request() {
        let cases = [
            // A batch: arrays are not A2A requests, even one whose items line up with the fields.
            (serde_json::json!(["2.0", 1, "SendMessage", {}]), serde_json::Value::Null),
            (serde_json::json!("SendMessage"), serde_json::Value::Null),
            (serde_json::json!({ "jsonrpc": "1.0", "id": "a", "method": "GetTask" }), "a".into()),
            (serde_json::json!({ "jsonrpc": "2.0", "id": 7, "method": 42 }), 7.into()),
            (
                serde_json::json!({ "jsonrpc": "2.0", "id": {"x": 1}, "method": "GetTask" }),
                serde_json::Value::Null,
            ),
            (serde_json::json!({ "id": 3, "method": "GetTask" }), 3.into()),
        ];
        for (body, id) in cases {
            let (_, resp) =
                post_raw(make_handler(), AuthenticatedCaller::default(), body.to_string()).await;
            assert_eq!(resp["error"]["code"], INVALID_REQUEST, "{body} -> {resp}");
            assert_eq!(resp["id"], id, "{body} -> {resp}");
        }
    }

    #[test]
    fn only_json_rpc_objects_yield_a_request_id() {
        let id = |body: serde_json::Value| jsonrpc_request_id(body.to_string().as_bytes());
        assert_eq!(
            id(serde_json::json!({"jsonrpc": "2.0", "id": "r-1"})),
            Some(Some("r-1".into()))
        );
        assert_eq!(id(serde_json::json!({"jsonrpc": "2.0", "id": 4})), Some(Some(4.into())));
        assert_eq!(id(serde_json::json!({"jsonrpc": "2.0"})), Some(None));
        assert_eq!(id(serde_json::json!({"jsonrpc": "2.0", "id": [1]})), Some(None));
        assert_eq!(id(serde_json::json!({"message": {"messageId": "m"}})), None);
        assert_eq!(id(serde_json::json!([{"jsonrpc": "2.0"}])), None);
        assert_eq!(jsonrpc_request_id(b"{not json"), None);
        assert_eq!(jsonrpc_request_id(b""), None);
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
        let task_id = create_resp["result"]["task"]["id"].as_str().unwrap();

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

        // A2A §3.1.4: a ListTasksResponse, not a bare array.
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(
            resp["result"],
            serde_json::json!({"tasks": [], "nextPageToken": "", "pageSize": 50, "totalSize": 0})
        );
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
    async fn json_rpc_responses_use_application_json() {
        // A2A §9.1: the JSON-RPC binding uses `application/json` for requests and responses;
        // `application/a2a+json` belongs to the HTTP+JSON binding.
        let handler = make_handler();
        let send = JsonRpcRequest::with_params(
            serde_json::json!(1),
            "SendMessage",
            send_message_params("msg-ct"),
        );
        let missing = JsonRpcRequest::with_params(
            serde_json::json!(2),
            "GetTask",
            serde_json::json!({ "id": "nonexistent" }),
        );
        for request in [send, missing] {
            let body = serde_json::to_vec(&request).unwrap();
            let (parts, _) = post_raw(handler.clone(), AuthenticatedCaller::default(), body).await;
            assert_eq!(parts.status, axum::http::StatusCode::OK);
            assert_eq!(parts.headers["content-type"], "application/json", "{}", request.method);
        }
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
        let task_id = create_resp["result"]["task"]["id"].as_str().unwrap().to_string();

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
        assert_eq!(list_resp["result"]["configs"].as_array().unwrap().len(), 1);

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
        assert_eq!(list_resp2["result"]["configs"].as_array().unwrap().len(), 0);
    }
}
