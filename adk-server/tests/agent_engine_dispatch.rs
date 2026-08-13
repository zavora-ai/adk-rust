//! Integration tests for the Agent Engine dispatch surface.
//!
//! Exercises the two dispatch endpoints against an in-memory `SessionService`
//! and a deterministic echo agent, asserting against the shared wire fixture
//! (`tests/fixtures/agent_engine_wire.json`) that the future
//! `RemoteReasoningEngineAgent` client round-trips against.

#![cfg(feature = "agent-engine")]

use adk_server::agent_engine::{AgentEngineState, agent_engine_router};
use async_stream::stream;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

const FIXTURE: &str = include_str!("fixtures/agent_engine_wire.json");
const APP_NAME: &str = "test-app";

fn fixture() -> serde_json::Value {
    serde_json::from_str(FIXTURE).expect("fixture file is valid JSON")
}

fn fixture_request(name: &str) -> serde_json::Value {
    fixture()["requests"][name].clone()
}

/// The deterministic event the echo agent emits — identical to the fixture's
/// `streamed_events[0]` except for the runner-visible timestamp, which tests
/// normalize.
fn fixture_event() -> adk_core::Event {
    let mut event =
        adk_core::Event::with_id("agent-engine-fixture-event", "agent-engine-fixture-invocation");
    event.timestamp = chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    event.author = "echo-agent".to_string();
    event.llm_response.content = Some(adk_core::Content::new("model").with_text("echo: hi"));
    event
}

struct EchoAgent;

#[async_trait]
impl adk_core::Agent for EchoAgent {
    fn name(&self) -> &str {
        "echo-agent"
    }

    fn description(&self) -> &str {
        "Deterministic echo agent for dispatch tests"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let s = stream! {
            yield Ok(fixture_event());
        };
        Ok(Box::pin(s))
    }
}

fn build_state(memory: bool) -> AgentEngineState {
    let session_service = Arc::new(adk_session::InMemorySessionService::new());
    let runner = Arc::new(
        adk_runner::Runner::builder()
            .app_name(APP_NAME)
            .agent(Arc::new(EchoAgent))
            .session_service(session_service)
            .build()
            .expect("runner builds"),
    );
    let state = AgentEngineState::new(runner);
    if memory {
        state.with_memory_service(Arc::new(adk_memory::InMemoryMemoryService::new()))
    } else {
        state
    }
}

fn build_router(memory: bool) -> axum::Router {
    agent_engine_router(build_state(memory))
}

async fn post(app: &axum::Router, uri: &str, body: serde_json::Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn post_unary(app: &axum::Router, body: serde_json::Value) -> axum::response::Response {
    post(app, "/api/reasoning_engine", body).await
}

async fn post_stream(app: &axum::Router, body: serde_json::Value) -> axum::response::Response {
    post(app, "/api/stream_reasoning_engine", body).await
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Collects a streaming response into parsed NDJSON lines, asserting the
/// framing contract (200, `application/json`, one JSON object per line).
async fn stream_lines(response: axum::response::Response) -> Vec<serde_json::Value> {
    assert_eq!(response.status(), StatusCode::OK);
    let content_type =
        response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap().to_string();
    assert!(content_type.starts_with("application/json"), "content-type was {content_type}");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.ends_with('\n'), "stream must be newline-terminated");
    text.lines().map(|line| serde_json::from_str(line).expect("each line is JSON")).collect()
}

/// The expected `AdkApp`-shaped session dump, with volatile fields copied
/// from the actual value so whole-object equality stays meaningful.
fn expected_session(
    actual: &serde_json::Value,
    id: &str,
    state: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "app_name": APP_NAME,
        "user_id": "u",
        "state": state,
        "events": [],
        "last_update_time": actual["last_update_time"].clone(),
    })
}

// ── (a) unary class methods round-trip ───────────────────────────────────

#[tokio::test]
async fn session_crud_round_trips() {
    let app = build_router(false);

    // create_session with explicit ID and initial state
    let response = post_unary(&app, fixture_request("create_session")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let created = body_json(response).await;
    let expected =
        expected_session(&created["output"], "s-fixture", serde_json::json!({"kind": "fixture"}));
    assert_eq!(created, serde_json::json!({"output": expected}));

    // async_create_session is a wire-parity alias of create_session
    let response = post_unary(&app, fixture_request("async_create_session")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let created_async = body_json(response).await;
    let expected =
        expected_session(&created_async["output"], "s-fixture-async", serde_json::json!({}));
    assert_eq!(created_async, serde_json::json!({"output": expected}));

    // get_session / async_get_session return the same dump
    for name in ["get_session", "async_get_session"] {
        let response = post_unary(&app, fixture_request(name)).await;
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        let got = body_json(response).await;
        let expected =
            expected_session(&got["output"], "s-fixture", serde_json::json!({"kind": "fixture"}));
        assert_eq!(got, serde_json::json!({"output": expected}), "{name}");
    }

    // list_sessions / async_list_sessions wrap the dumps in {"sessions": [...]}
    for name in ["list_sessions", "async_list_sessions"] {
        let response = post_unary(&app, fixture_request(name)).await;
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        let listed = body_json(response).await;
        let sessions = listed["output"]["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2, "{name}");
        let mut ids: Vec<&str> = sessions.iter().map(|s| s["id"].as_str().unwrap()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["s-fixture", "s-fixture-async"], "{name}");
    }

    // delete_session / async_delete_session return null output
    let response = post_unary(&app, fixture_request("delete_session")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, serde_json::json!({"output": null}));

    let response = post_unary(&app, fixture_request("async_delete_session")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, serde_json::json!({"output": null}));

    // Both sessions are gone
    let response = post_unary(&app, fixture_request("get_session")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ── (b) streaming returns newline-delimited JSON ─────────────────────────

#[tokio::test]
async fn stream_query_streams_fixture_events() {
    let app = build_router(false);

    // The codelab payload: async_stream_query with no session_id (auto-create).
    let lines = stream_lines(post_stream(&app, fixture_request("async_stream_query")).await).await;

    let mut expected: Vec<serde_json::Value> =
        fixture()["streamed_events"].as_array().unwrap().clone();
    assert_eq!(lines.len(), expected.len());
    for (line, expected) in lines.iter().zip(expected.iter_mut()) {
        // The event timestamp is assigned at emission time; everything else
        // must match the fixture exactly.
        expected["timestamp"] = line["timestamp"].clone();
        assert_eq!(line, expected);
    }
}

#[tokio::test]
async fn stream_query_with_explicit_session_persists_events() {
    let state = build_state(false);
    let app = agent_engine_router(state.clone());

    let lines = stream_lines(post_stream(&app, fixture_request("stream_query")).await).await;
    assert_eq!(lines.len(), 1);

    // The auto-created session recorded the exchange: user message + echo.
    let session = state
        .session_service()
        .get(adk_session::GetRequest {
            app_name: APP_NAME.to_string(),
            user_id: "u".to_string(),
            session_id: "s-stream".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("session was auto-created");
    assert!(!session.events().is_empty());
}

#[tokio::test]
async fn streaming_agent_run_with_events_drives_the_runner() {
    let app = build_router(false);

    let lines =
        stream_lines(post_stream(&app, fixture_request("streaming_agent_run_with_events")).await)
            .await;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["author"], "echo-agent");
}

// ── (c) unknown method → 400 problem+json ────────────────────────────────

#[tokio::test]
async fn unknown_class_method_is_400_problem_json() {
    let app = build_router(false);

    for uri in ["/api/reasoning_engine", "/api/stream_reasoning_engine"] {
        let response = post(&app, uri, serde_json::json!({"class_method": "does_not_exist"})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "agent_engine.unknown_class_method", "{uri}");
    }
}

#[tokio::test]
async fn wrong_endpoint_is_400() {
    let app = build_router(false);

    // Streaming method on the unary endpoint
    let response = post_unary(&app, fixture_request("async_stream_query")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "agent_engine.wrong_endpoint");

    // Unary method on the streaming endpoint
    let response = post_stream(&app, fixture_request("create_session")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "agent_engine.wrong_endpoint");
}

// ── (d) register_operations matches the contract table ───────────────────

#[tokio::test]
async fn register_operations_matches_fixture() {
    let app = build_router(false);

    let response = post_unary(&app, fixture_request("register_operations")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body, serde_json::json!({"output": fixture()["register_operations_output"]}));
}

// ── memory class methods ─────────────────────────────────────────────────

#[tokio::test]
async fn memory_methods_are_unsupported_without_memory_service() {
    let app = build_router(false);

    for name in ["async_add_session_to_memory", "async_search_memory"] {
        let response = post_unary(&app, fixture_request(name)).await;
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED, "{name}");
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "agent_engine.memory_unavailable", "{name}");
    }
}

#[tokio::test]
async fn memory_methods_round_trip_with_memory_service() {
    let app = build_router(true);

    // Populate a session via stream_query, then extract it into memory.
    stream_lines(post_stream(&app, fixture_request("stream_query")).await).await;

    let response = post_unary(&app, fixture_request("async_add_session_to_memory")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, serde_json::json!({"output": null}));

    let response = post_unary(&app, fixture_request("async_search_memory")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    // Looser than whole-object equality: the in-memory backend's ranking is
    // not this contract's subject; shape and provenance are.
    let memories = body["output"]["memories"].as_array().unwrap();
    assert!(!memories.is_empty());
    assert!(memories.iter().any(|m| m["author"] == "echo-agent"));
    for memory in memories {
        assert!(memory["content"].is_object());
        assert!(memory["timestamp"].is_string());
    }
}

// ── shared wire fixture round-trip (prep for the remote client) ──────────

#[tokio::test]
async fn fixture_streamed_events_round_trip_as_adk_events() {
    for value in fixture()["streamed_events"].as_array().unwrap() {
        let event: adk_core::Event =
            serde_json::from_value(value.clone()).expect("fixture event parses as adk_core::Event");
        let reserialized = serde_json::to_value(&event).unwrap();
        assert_eq!(&reserialized, value, "fixture event must round-trip byte-for-byte");
    }
}

#[tokio::test]
async fn fixture_requests_parse_as_dispatch_requests() {
    use adk_server::agent_engine::DispatchRequest;
    use std::str::FromStr;

    let fixture = fixture();
    let requests = fixture["requests"].as_object().unwrap();
    // Every operation of the contract has a canonical fixture request.
    assert_eq!(requests.len(), 14);
    for (name, value) in requests {
        let request: DispatchRequest =
            serde_json::from_value(value.clone()).expect("fixture request parses");
        assert_eq!(&request.class_method, name);
        adk_server::agent_engine::ClassMethod::from_str(&request.class_method)
            .expect("fixture request names a known class method");
    }
}
