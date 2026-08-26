//! Contract tests for [`RemoteReasoningEngineAgent`] against a mock
//! `reasoningEngines:streamQuery` endpoint: canonical camelCase envelope,
//! `alt=sse` framing, class-method fallback, error mapping, and the shared
//! Wave-1 wire fixture round-trip that pins client/server compatibility.

#![cfg(feature = "vertex-remote-engine")]

use adk_core::{
    Agent, CallbackContext, Content, Event, InvocationContext, ReadonlyContext, RunConfig, Session,
    State,
};
use adk_server::agent_engine::remote::RemoteReasoningEngineAgent;
use axum::extract::{Path, Query, State as AxumState};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const RESOURCE: &str = "projects/test-project/locations/us-central1/reasoningEngines/42";

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/agent_engine_wire.json")).expect("fixture parses")
}

// ── minimal invocation context ────────────────────────────────────────────

struct MockState;
impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "s-playground"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "u"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct MockContext {
    content: Content,
    config: RunConfig,
    session: MockSession,
}

impl MockContext {
    fn new() -> Self {
        Self {
            content: Content::new("user").with_text("hi"),
            config: RunConfig::default(),
            session: MockSession,
        }
    }
}

#[async_trait::async_trait]
impl ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "remote-contract-invocation"
    }
    fn agent_name(&self) -> &str {
        "remote-engine"
    }
    fn user_id(&self) -> &str {
        "u"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "s-playground"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait::async_trait]
impl CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait::async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not used by the remote agent")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

// ── mock streamQuery server ───────────────────────────────────────────────

#[derive(Default)]
struct MockEngine {
    /// (class_method, whole request body, alt query value) per call.
    calls: Vec<(String, Value, Option<String>)>,
    /// class_method → behavior.
    behaviors: HashMap<String, Behavior>,
}

enum Behavior {
    Sse(Vec<Value>),
    RawSse(String),
    Status(u16),
}

type Shared = Arc<Mutex<MockEngine>>;

async fn start_mock(state: Shared) -> String {
    let app = Router::new()
        .route(
            "/v1/projects/{project}/locations/{location}/reasoningEngines/{action}",
            post(
                |AxumState(state): AxumState<Shared>,
                 Path((_, _, action)): Path<(String, String, String)>,
                 Query(params): Query<HashMap<String, String>>,
                 Json(body): Json<Value>| async move {
                    assert!(action.ends_with(":streamQuery"), "unexpected action {action}");
                    let mut state = state.lock().await;
                    let class_method = body["classMethod"].as_str().unwrap_or_default().to_string();
                    state.calls.push((class_method.clone(), body, params.get("alt").cloned()));
                    match state.behaviors.get(&class_method) {
                        Some(Behavior::Sse(events)) => {
                            let body: String =
                                events.iter().map(|event| format!("data: {event}\n\n")).collect();
                            (StatusCode::OK, [(header::CONTENT_TYPE, "text/event-stream")], body)
                                .into_response()
                        }
                        Some(Behavior::RawSse(raw)) => (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            raw.clone(),
                        )
                            .into_response(),
                        Some(Behavior::Status(code)) => (
                            StatusCode::from_u16(*code).unwrap(),
                            Json(json!({ "error": { "code": code, "message": "mock rejection" } })),
                        )
                            .into_response(),
                        None => (
                            StatusCode::NOT_FOUND,
                            Json(json!({ "error": { "code": 404, "message": "unknown method" } })),
                        )
                            .into_response(),
                    }
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{address}")
}

async fn build_agent(endpoint: &str) -> RemoteReasoningEngineAgent {
    RemoteReasoningEngineAgent::builder("remote-engine")
        .resource_name(RESOURCE)
        .endpoint(endpoint)
        .credentials(api_key_credentials::Builder::new("test-api-key").build())
        .build()
        .await
        .expect("build remote agent")
}

async fn collect(agent: &RemoteReasoningEngineAgent) -> Vec<adk_core::Result<Event>> {
    let stream = agent.run(Arc::new(MockContext::new())).await.expect("run");
    stream.collect().await
}

// ── tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn envelope_is_canonical_camelcase_with_the_fixture_input() {
    let state = Shared::default();
    state.lock().await.behaviors.insert(
        "streaming_agent_run_with_events".to_string(),
        Behavior::Sse(fixture()["streamed_events"].as_array().unwrap().clone()),
    );
    let endpoint = start_mock(state.clone()).await;
    let agent = build_agent(&endpoint).await;

    let events = collect(&agent).await;
    assert!(events.iter().all(Result::is_ok));

    let state = state.lock().await;
    let (class_method, body, alt) = &state.calls[0];
    assert_eq!(class_method, "streaming_agent_run_with_events");
    assert_eq!(alt.as_deref(), Some("sse"), "the client must request SSE framing");

    // The input payload matches the shared fixture's request shape: the
    // dispatcher-side AgentRunRequest carried as a JSON string.
    let fixture_input = fixture()["requests"]["streaming_agent_run_with_events"]["input"].clone();
    let expected_request: Value =
        serde_json::from_str(fixture_input["request_json"].as_str().unwrap()).unwrap();
    let sent_request: Value =
        serde_json::from_str(body["input"]["request_json"].as_str().unwrap()).unwrap();
    assert_eq!(sent_request, expected_request);
    // Canonical public-API envelope: camelCase key, no snake_case sibling.
    assert!(body.get("class_method").is_none());
}

#[tokio::test]
async fn fixture_streamed_events_round_trip_through_the_client_parser() {
    let fixture_events = fixture()["streamed_events"].as_array().unwrap().clone();
    let state = Shared::default();
    state.lock().await.behaviors.insert(
        "streaming_agent_run_with_events".to_string(),
        Behavior::Sse(fixture_events.clone()),
    );
    let endpoint = start_mock(state.clone()).await;
    let agent = build_agent(&endpoint).await;

    let received: Vec<Event> =
        collect(&agent).await.into_iter().map(|event| event.expect("event")).collect();
    let expected: Vec<Event> = fixture_events
        .iter()
        .map(|value| serde_json::from_value(value.clone()).expect("fixture event"))
        .collect();

    // WP1's dispatcher output → this parser → identical Event values.
    let received_json: Vec<Value> =
        received.iter().map(|event| serde_json::to_value(event).unwrap()).collect();
    let expected_json: Vec<Value> =
        expected.iter().map(|event| serde_json::to_value(event).unwrap()).collect();
    assert_eq!(received_json, expected_json, "client and server must stay wire-compatible");
}

#[tokio::test]
async fn unknown_primary_method_falls_back_to_stream_query_once() {
    let state = Shared::default();
    {
        let mut state = state.lock().await;
        state
            .behaviors
            .insert("streaming_agent_run_with_events".to_string(), Behavior::Status(404));
        state.behaviors.insert(
            "stream_query".to_string(),
            Behavior::Sse(fixture()["streamed_events"].as_array().unwrap().clone()),
        );
    }
    let endpoint = start_mock(state.clone()).await;
    let agent = build_agent(&endpoint).await;

    let events = collect(&agent).await;
    assert!(events.iter().all(Result::is_ok), "fallback must succeed");

    let state = state.lock().await;
    assert_eq!(state.calls.len(), 2);
    assert_eq!(state.calls[0].0, "streaming_agent_run_with_events");
    assert_eq!(state.calls[1].0, "stream_query");
    let fallback_input = &state.calls[1].1["input"];
    assert_eq!(
        fallback_input,
        &json!({ "user_id": "u", "session_id": "s-playground", "message": "hi" }),
    );
}

#[tokio::test]
async fn non_success_statuses_yield_a_branded_error_with_upstream_status() {
    let state = Shared::default();
    state
        .lock()
        .await
        .behaviors
        .insert("streaming_agent_run_with_events".to_string(), Behavior::Status(500));
    let endpoint = start_mock(state.clone()).await;
    let agent = build_agent(&endpoint).await;

    let events = collect(&agent).await;
    assert_eq!(events.len(), 1);
    let error = events.into_iter().next().unwrap().expect_err("500 must be an error");
    assert_eq!(error.details.upstream_status_code, Some(500));
    assert_eq!(error.code, "server.remote_engine.unavailable");
}

#[tokio::test]
async fn mid_stream_garbage_becomes_an_error_event_and_terminates() {
    let event = fixture()["streamed_events"].as_array().unwrap()[0].clone();
    let state = Shared::default();
    state.lock().await.behaviors.insert(
        "streaming_agent_run_with_events".to_string(),
        Behavior::RawSse(format!("data: {event}\n\ndata: not json\n\ndata: {event}\n\n")),
    );
    let endpoint = start_mock(state.clone()).await;
    let agent = build_agent(&endpoint).await;

    let events = collect(&agent).await;
    assert_eq!(events.len(), 2, "stream must terminate after the parse failure");
    assert!(events[0].is_ok());
    let error_event = events[1].as_ref().expect("error event, not stream error");
    assert!(
        error_event
            .llm_response
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("event JSON")),
        "{error_event:?}",
    );
}

#[tokio::test]
async fn urns_resolve_to_engine_resource_names_via_the_registry() {
    use adk_tool::{AgentRegistryClient, AgentRegistryConfig};

    let urn = "urn:agent:projects-1234:projects:1234:locations:us-central1:reasoningEngines:42";
    let agent_entry = json!({
        "name": "projects/test-project/locations/us-central1/agents/a-1",
        "agentId": urn,
        "displayName": "研究 agent",
        "attributes": {
            "agentregistry.googleapis.com/system/RuntimeReference": {
                "uri": format!("//aiplatform.googleapis.com/{RESOURCE}"),
            },
        },
    });
    let search_response = json!({ "agents": [agent_entry] });
    let get_response = agent_entry.clone();

    let app = Router::new()
        .route(
            "/v1/projects/test-project/locations/us-central1/{action}",
            post(move |Path(action): Path<String>| {
                let search_response = search_response.clone();
                async move {
                    assert_eq!(action, "agents:search");
                    Json(search_response)
                }
            }),
        )
        .route(
            "/v1/projects/test-project/locations/us-central1/agents/{agent}",
            get(move || {
                let get_response = get_response.clone();
                async move { Json(get_response) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let registry = AgentRegistryClient::with_credentials(
        AgentRegistryConfig::new("test-project", "us-central1")
            .with_endpoint(format!("http://{address}")),
        api_key_credentials::Builder::new("test-api-key").build(),
    )
    .expect("build registry client");

    let agent = RemoteReasoningEngineAgent::builder("remote-engine")
        .urn(urn)
        .registry(registry)
        .endpoint("http://127.0.0.1:1") // never contacted in this test
        .credentials(api_key_credentials::Builder::new("test-api-key").build())
        .build()
        .await
        .expect("URN must resolve via the registry");

    assert_eq!(agent.resource_name(), RESOURCE);
}
