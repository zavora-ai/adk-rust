//! Contract tests for the shared GCP client and LRO poller against a mock
//! server: auth headers are attached, LROs poll to completion with pinned
//! identity, error results map to categories, and oversized or non-JSON
//! responses are rejected.

use adk_core::{ErrorCategory, ErrorComponent};
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use google_cloud_auth::credentials::api_key_credentials;
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const PROJECT: &str = "test-project";
const LOCATION: &str = "us-central1";

const CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "memory.vertex.invalid_input",
    unauthorized: "memory.vertex.unauthorized",
    forbidden: "memory.vertex.forbidden",
    not_found: "memory.vertex.not_found",
    rate_limited: "memory.vertex.rate_limited",
    timeout: "memory.vertex.timeout",
    unavailable: "memory.vertex.unavailable",
    credentials_unavailable: "memory.vertex.credentials_unavailable",
    invalid_response: "memory.vertex.invalid_response",
    invalid_request: "memory.vertex.invalid_request",
    upstream_error: "memory.vertex.upstream_error",
    operation_failed: "memory.vertex.operation_failed",
};

fn operation_name() -> String {
    format!("projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/4242/operations/777")
}

#[derive(Default)]
struct MockState {
    start_headers: Vec<HeaderMap>,
    start_bodies: Vec<Value>,
    polls: usize,
    /// Queue of poll responses, popped per request; the last repeats.
    poll_responses: Vec<Value>,
}

type SharedState = Arc<Mutex<MockState>>;

async fn start_mock(state: SharedState) -> String {
    let operation_path = format!("/v1beta1/{}", operation_name());
    let app =
        Router::new()
            .route(
                "/v1beta1/things:start",
                post(
                    |State(state): State<SharedState>,
                     headers: HeaderMap,
                     Json(body): Json<Value>| async move {
                        let mut state = state.lock().await;
                        state.start_headers.push(headers);
                        state.start_bodies.push(body);
                        Json(json!({ "name": operation_name(), "done": false }))
                    },
                ),
            )
            .route(
                &operation_path,
                get(|State(state): State<SharedState>| async move {
                    let mut state = state.lock().await;
                    state.polls += 1;
                    let response =
                        if state.poll_responses.len() > 1 {
                            state.poll_responses.remove(0)
                        } else {
                            state.poll_responses.first().cloned().unwrap_or_else(
                                || json!({ "name": operation_name(), "done": true }),
                            )
                        };
                    Json(response)
                }),
            )
            .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

fn build_client(endpoint: &str) -> GcpHttpClient {
    GcpHttpClient::builder(
        GcpErrorContext::new(ErrorComponent::Memory, CODES, "vertex memory"),
        endpoint,
    )
    .credentials(api_key_credentials::Builder::new("test-api-key").build())
    .build()
    .expect("build test client")
}

#[tokio::test]
async fn requests_carry_credential_headers_and_json_bodies() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let request = client
        .request(Method::POST, "things:start")
        .await
        .unwrap()
        .json(&json!({ "key": "value" }));
    let operation = client.send_value(request).await.unwrap();
    assert_eq!(operation, json!({ "name": operation_name(), "done": false }));

    let state = state.lock().await;
    assert_eq!(state.start_bodies, vec![json!({ "key": "value" })]);
    let headers = &state.start_headers[0];
    assert_eq!(
        headers.get("x-goog-api-key").map(|value| value.to_str().unwrap()),
        Some("test-api-key"),
    );
}

#[tokio::test]
async fn lro_polls_to_completion_and_returns_the_response() {
    let state = SharedState::default();
    state.lock().await.poll_responses = vec![
        json!({ "name": operation_name(), "done": false }),
        json!({ "name": operation_name(), "done": true, "response": { "ok": true } }),
    ];
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let request = client.request(Method::POST, "things:start").await.unwrap().json(&json!({}));
    let operation = client.send_value(request).await.unwrap();
    let response = LroPoller::new()
        .with_initial_delay(Duration::from_millis(1))
        .wait_for_operation(&client, operation, "things start", true, PROJECT, LOCATION)
        .await
        .unwrap();

    assert_eq!(response, Some(json!({ "ok": true })));
    assert_eq!(state.lock().await.polls, 2);
}

#[tokio::test]
async fn lro_operation_errors_map_grpc_codes_to_categories() {
    let state = SharedState::default();
    state.lock().await.poll_responses = vec![json!({
        "name": operation_name(),
        "done": true,
        "error": { "code": 8, "message": "quota exhausted" },
    })];
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let request = client.request(Method::POST, "things:start").await.unwrap().json(&json!({}));
    let operation = client.send_value(request).await.unwrap();
    let error = LroPoller::new()
        .with_initial_delay(Duration::from_millis(1))
        .wait_for_operation(&client, operation, "things start", false, PROJECT, LOCATION)
        .await
        .unwrap_err();

    assert_eq!(error.category, ErrorCategory::RateLimited);
    assert_eq!(error.code, "memory.vertex.operation_failed");
    assert!(error.message.contains("quota exhausted"), "{}", error.message);
}

#[tokio::test]
async fn lro_refuses_to_follow_a_changed_operation_identity() {
    let state = SharedState::default();
    state.lock().await.poll_responses = vec![json!({
        "name": format!("projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/4242/operations/999"),
        "done": true,
    })];
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let request = client.request(Method::POST, "things:start").await.unwrap().json(&json!({}));
    let operation = client.send_value(request).await.unwrap();
    let error = LroPoller::new()
        .with_initial_delay(Duration::from_millis(1))
        .wait_for_operation(&client, operation, "things start", false, PROJECT, LOCATION)
        .await
        .unwrap_err();

    assert!(error.message.contains("changed operation identity"), "{}", error.message);
}

#[tokio::test]
async fn lro_rejects_operations_outside_the_configured_scope() {
    let endpoint = start_mock(SharedState::default()).await;
    let client = build_client(&endpoint);

    let foreign = json!({
        "name": "projects/other-project/locations/us-central1/reasoningEngines/1/operations/2",
        "done": false,
    });
    let error = LroPoller::new()
        .wait_for_operation(&client, foreign, "things start", false, PROJECT, LOCATION)
        .await
        .unwrap_err();

    assert!(error.message.contains("does not belong to"), "{}", error.message);
}

#[tokio::test]
async fn lro_requires_a_response_when_asked() {
    let endpoint = start_mock(SharedState::default()).await;
    let client = build_client(&endpoint);

    let done_without_response = json!({ "name": operation_name(), "done": true });
    let error = LroPoller::new()
        .wait_for_operation(&client, done_without_response, "things start", true, PROJECT, LOCATION)
        .await
        .unwrap_err();

    assert!(error.message.contains("completed without a response"), "{}", error.message);
}

#[tokio::test]
async fn lro_deadline_produces_a_timeout_error() {
    let state = SharedState::default();
    state.lock().await.poll_responses = vec![json!({ "name": operation_name(), "done": false })];
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let request = client.request(Method::POST, "things:start").await.unwrap().json(&json!({}));
    let operation = client.send_value(request).await.unwrap();
    let error = LroPoller::new()
        .with_poll_timeout(Duration::from_millis(50))
        .with_initial_delay(Duration::from_millis(5))
        .wait_for_operation(&client, operation, "things start", false, PROJECT, LOCATION)
        .await
        .unwrap_err();

    assert_eq!(error.category, ErrorCategory::Timeout);
    assert_eq!(error.code, "memory.vertex.timeout");
}

#[tokio::test]
async fn non_success_statuses_map_to_branded_errors() {
    let app = Router::new().route(
        "/v1beta1/things:start",
        post(|| async { (axum::http::StatusCode::TOO_MANY_REQUESTS, "slow down") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = build_client(&format!("http://{address}"));

    let request = client.request(Method::POST, "things:start").await.unwrap().json(&json!({}));
    let error = client.send_value(request).await.unwrap_err();

    assert_eq!(error.category, ErrorCategory::RateLimited);
    assert_eq!(error.code, "memory.vertex.rate_limited");
    assert_eq!(error.details.upstream_status_code, Some(429));
    assert!(error.message.contains("slow down"), "{}", error.message);
}

#[tokio::test]
async fn not_found_is_optional_only_when_allowed() {
    let app = Router::new().route(
        "/v1beta1/things/missing",
        get(|| async { (axum::http::StatusCode::NOT_FOUND, "gone") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = build_client(&format!("http://{address}"));

    let request = client.request(Method::GET, "things/missing").await.unwrap();
    let value = client.send_value_allow_not_found(request).await.unwrap();
    assert_eq!(value, None);

    let request = client.request(Method::GET, "things/missing").await.unwrap();
    let error = client.send_value(request).await.unwrap_err();
    assert_eq!(error.category, ErrorCategory::NotFound);
    assert_eq!(error.code, "memory.vertex.not_found");
}

#[tokio::test]
async fn oversized_responses_are_rejected() {
    let app = Router::new()
        .route("/v1beta1/things/huge", get(|| async { format!("\"{}\"", "x".repeat(1024)) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = GcpHttpClient::builder(
        GcpErrorContext::new(ErrorComponent::Memory, CODES, "vertex memory"),
        format!("http://{address}"),
    )
    .credentials(api_key_credentials::Builder::new("test-api-key").build())
    .max_response_bytes(256)
    .build()
    .unwrap();

    let request = client.request(Method::GET, "things/huge").await.unwrap();
    let error = client.send_value(request).await.unwrap_err();

    assert_eq!(error.code, "memory.vertex.invalid_response");
    assert!(error.message.contains("exceeds the 256-byte limit"), "{}", error.message);
}

#[tokio::test]
async fn empty_success_bodies_parse_as_an_empty_object() {
    let app = Router::new()
        .route("/v1beta1/things/empty", get(|| async { "" }))
        .route("/v1beta1/things/garbage", get(|| async { "not json" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = build_client(&format!("http://{address}"));

    let request = client.request(Method::GET, "things/empty").await.unwrap();
    let value = client.send_value(request).await.unwrap();
    assert_eq!(value, json!({}));

    let request = client.request(Method::GET, "things/garbage").await.unwrap();
    let error = client.send_value(request).await.unwrap_err();
    assert_eq!(error.code, "memory.vertex.invalid_response");
}
