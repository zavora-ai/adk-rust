//! Contract tests for the Vertex AI Memory Bank backend against a mock
//! server: `memories:generate` returns an LRO that completes on poll,
//! `memories:retrieve` returns fixtures, and deletion enumerates a scope.
//!
//! The captured request bodies are compared as whole JSON values — they are
//! the wire contract adk-python's `VertexAiMemoryBankService` shares.

#![cfg(feature = "vertex-memory")]

use adk_core::Content;
use adk_memory::{
    MemoryEntry, MemoryService, SearchRequest, VertexAiMemoryBankService, VertexAiMemoryConfig,
};
use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{TimeZone, Utc};
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;

const PROJECT: &str = "test-project";
const LOCATION: &str = "us-central1";
const ENGINE: &str = "4242";
const APP: &str = "weather-app";
const USER: &str = "u-1";

#[derive(Default)]
struct MockState {
    generate_bodies: Vec<Value>,
    retrieve_bodies: Vec<Value>,
    deleted_memories: Vec<String>,
    generate_polls: usize,
    /// Queue of `memories:retrieve` responses, popped per request.
    retrieve_responses: Vec<Value>,
    /// When set, the generate operation completes with this error.
    operation_error: Option<Value>,
}

type SharedState = Arc<Mutex<MockState>>;

fn operation_name() -> String {
    format!("projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/{ENGINE}/operations/777")
}

fn memory_name(id: &str) -> String {
    format!("projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/{ENGINE}/memories/{id}")
}

async fn start_mock(state: SharedState) -> String {
    let engine_path =
        format!("/v1beta1/projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/{ENGINE}");
    let app = Router::new()
        .route(
            &format!("{engine_path}/memories:generate"),
            post(|State(state): State<SharedState>, Json(body): Json<Value>| async move {
                let mut state = state.lock().await;
                state.generate_bodies.push(body);
                // Not done yet: the client must poll the operation.
                Json(json!({ "name": operation_name(), "done": false }))
            }),
        )
        .route(
            &format!("{engine_path}/memories:retrieve"),
            post(|State(state): State<SharedState>, Json(body): Json<Value>| async move {
                let mut state = state.lock().await;
                state.retrieve_bodies.push(body);
                let response = if state.retrieve_responses.is_empty() {
                    json!({ "retrievedMemories": [] })
                } else {
                    state.retrieve_responses.remove(0)
                };
                Json(response)
            }),
        )
        .route(
            &format!("{engine_path}/operations/{{op}}"),
            get(|State(state): State<SharedState>| async move {
                let mut state = state.lock().await;
                state.generate_polls += 1;
                let mut operation = json!({ "name": operation_name(), "done": true });
                if let Some(error) = &state.operation_error {
                    operation["error"] = error.clone();
                }
                Json(operation)
            }),
        )
        .route(
            &format!("{engine_path}/memories/{{memory}}"),
            delete(|State(state): State<SharedState>, Path(memory): Path<String>| async move {
                let mut state = state.lock().await;
                state.deleted_memories.push(memory);
                // Delete completes synchronously: done with no error.
                Json(json!({ "name": operation_name(), "done": true }))
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

async fn build_service(endpoint: &str) -> VertexAiMemoryBankService {
    let config = VertexAiMemoryConfig::new(PROJECT, LOCATION)
        .with_reasoning_engine(ENGINE)
        .with_endpoint(endpoint);
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    VertexAiMemoryBankService::with_credentials(config, credentials).expect("build test service")
}

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
    }
}

#[tokio::test]
async fn add_session_sends_direct_contents_and_polls_the_operation() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let service = build_service(&endpoint).await;

    service
        .add_session(APP, USER, "s-1", vec![entry("I live in Hamburg"), entry("I like tea")])
        .await
        .unwrap();

    let captured = state.lock().await;
    assert_eq!(captured.generate_bodies.len(), 1);
    // Whole-value comparison: this is the exact adk-python-compatible body.
    assert_eq!(
        captured.generate_bodies[0],
        json!({
            "directContentsSource": {
                "events": [
                    { "content": { "role": "user", "parts": [{ "text": "I live in Hamburg" }] } },
                    { "content": { "role": "user", "parts": [{ "text": "I like tea" }] } },
                ]
            },
            "scope": { "app_name": APP, "user_id": USER },
        }),
    );
    assert_eq!(captured.generate_polls, 1, "client polls the LRO to done");
}

#[tokio::test]
async fn add_session_with_no_entries_is_a_no_op() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let service = build_service(&endpoint).await;

    service.add_session(APP, USER, "s-1", vec![]).await.unwrap();

    let captured = state.lock().await;
    assert!(captured.generate_bodies.is_empty(), "no request for an empty session");
}

#[tokio::test]
async fn add_events_to_memory_skips_content_free_events() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let service = build_service(&endpoint).await;

    let mut with_content = adk_core::Event::new("inv-1");
    with_content.llm_response.content = Some(Content::new("user").with_text("remember me"));
    let without_content = adk_core::Event::new("inv-2");

    service.add_events_to_memory(APP, USER, &[with_content, without_content]).await.unwrap();

    let captured = state.lock().await;
    assert_eq!(captured.generate_bodies.len(), 1);
    let events = captured.generate_bodies[0]["directContentsSource"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "content-free events are skipped");
}

#[tokio::test]
async fn failed_operation_surfaces_the_operation_error() {
    let state = SharedState::default();
    state.lock().await.operation_error =
        Some(json!({ "code": 7, "message": "memory bank is not provisioned" }));
    let endpoint = start_mock(state.clone()).await;
    let service = build_service(&endpoint).await;

    let error = service.add_session(APP, USER, "s-1", vec![entry("x")]).await.unwrap_err();
    assert_eq!(error.http_status_code(), 403);
    assert!(error.message.contains("memory bank is not provisioned"), "{}", error.message);
}

#[tokio::test]
async fn search_sends_similarity_params_and_maps_memories() {
    let state = SharedState::default();
    state.lock().await.retrieve_responses.push(json!({
        "retrievedMemories": [
            {
                "memory": {
                    "name": memory_name("m-1"),
                    "fact": "User lives in Hamburg",
                    "createTime": "2025-01-01T00:00:00Z",
                    "updateTime": "2025-02-02T00:00:00Z",
                },
                "distance": 0.12,
            }
        ]
    }));
    let endpoint = start_mock(state.clone()).await;
    let service = build_service(&endpoint).await;

    let response = service
        .search(SearchRequest {
            query: "where does the user live".to_string(),
            user_id: USER.to_string(),
            app_name: APP.to_string(),
            limit: Some(5),
            min_score: None,
            project_id: None,
        })
        .await
        .unwrap();

    let captured = state.lock().await;
    assert_eq!(
        captured.retrieve_bodies[0],
        json!({
            "scope": { "app_name": APP, "user_id": USER },
            "similaritySearchParams": { "searchQuery": "where does the user live", "topK": 5 },
        }),
    );

    assert_eq!(response.memories.len(), 1);
    let memory = &response.memories[0];
    assert_eq!(
        serde_json::to_value(&memory.content).unwrap(),
        json!({ "role": "model", "parts": [{ "text": "User lives in Hamburg" }] }),
    );
    assert_eq!(memory.author, "memory_bank");
    assert_eq!(memory.timestamp, Utc.with_ymd_and_hms(2025, 2, 2, 0, 0, 0).unwrap());
}

#[tokio::test]
async fn search_rejects_project_scoping() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let service = build_service(&endpoint).await;

    let error = service
        .search(SearchRequest {
            query: "q".to_string(),
            user_id: USER.to_string(),
            app_name: APP.to_string(),
            limit: None,
            min_score: None,
            project_id: Some("proj-x".to_string()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.http_status_code(), 400);
}

#[tokio::test]
async fn delete_user_enumerates_the_scope_and_deletes_each_memory() {
    let state = SharedState::default();
    {
        let mut lock = state.lock().await;
        // Two pages: pagination must be followed.
        lock.retrieve_responses.push(json!({
            "retrievedMemories": [
                { "memory": { "name": memory_name("m-1"), "fact": "a" } },
                { "memory": { "name": memory_name("m-2"), "fact": "b" } },
            ],
            "nextPageToken": "page-2",
        }));
        lock.retrieve_responses.push(json!({
            "retrievedMemories": [
                { "memory": { "name": memory_name("m-3"), "fact": "c" } },
            ],
        }));
    }
    let endpoint = start_mock(state.clone()).await;
    let service = build_service(&endpoint).await;

    service.delete_user(APP, USER).await.unwrap();

    let captured = state.lock().await;
    assert_eq!(
        captured.retrieve_bodies[0],
        json!({
            "scope": { "app_name": APP, "user_id": USER },
            "simpleRetrievalParams": { "pageSize": 100 },
        }),
    );
    assert_eq!(
        captured.retrieve_bodies[1],
        json!({
            "scope": { "app_name": APP, "user_id": USER },
            "simpleRetrievalParams": { "pageSize": 100, "pageToken": "page-2" },
        }),
    );
    assert_eq!(captured.deleted_memories, ["m-1", "m-2", "m-3"]);
}

#[tokio::test]
async fn adapter_exposes_the_backend_as_core_memory() {
    let state = SharedState::default();
    state.lock().await.retrieve_responses.push(json!({
        "retrievedMemories": [
            { "memory": { "name": memory_name("m-1"), "fact": "User likes tea" } }
        ]
    }));
    let endpoint = start_mock(state.clone()).await;
    let service = Arc::new(build_service(&endpoint).await);

    // ToolContext::search_memory consumes adk_core::Memory; the existing
    // adapter binds app and user so the backend needs nothing extra.
    let memory: Arc<dyn adk_core::Memory> =
        Arc::new(adk_memory::MemoryServiceAdapter::new(service, APP, USER));
    let results = memory.search("tea").await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        serde_json::to_value(&results[0].content).unwrap(),
        json!({ "role": "model", "parts": [{ "text": "User likes tea" }] }),
    );
}
