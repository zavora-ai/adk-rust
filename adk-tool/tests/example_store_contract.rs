//! Contract tests for the Example Store v1beta1 data-plane client.
//!
//! A mock Axum server captures every request body and returns fixture JSON, so
//! the tests pin both directions of the wire contract: the client sends
//! exactly the documented request bodies and parses the documented responses.

#![cfg(feature = "example-store")]

use adk_core::Content;
use adk_tool::example_store::{
    ContentsExample, Example, ExampleStoreClient, ExampleStoreConfig, ExamplesArrayFilter,
    FetchExamplesRequest, SearchExamplesRequest, StoredContentsExample,
    StoredContentsExampleFilter, UpsertExamplesRequest,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct MockStoreState {
    /// Captured request bodies keyed by verb (`upsertExamples`, ...).
    bodies: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    /// Fixture responses keyed by verb.
    responses: Arc<Mutex<HashMap<String, (StatusCode, Value)>>>,
}

async fn handle_verb(
    State(state): State<MockStoreState>,
    Path((_project, _location, store_and_verb)): Path<(String, String, String)>,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let Some((store, verb)) = store_and_verb.split_once(':') else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "missing custom verb" })));
    };
    assert_eq!(store, "test-store");
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": error.to_string() } })),
            );
        }
    };
    state.bodies.lock().await.entry(verb.to_string()).or_default().push(parsed);
    match state.responses.lock().await.get(verb) {
        Some((status, value)) => (*status, Json(value.clone())),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "no fixture registered" }))),
    }
}

async fn test_client() -> (MockStoreState, ExampleStoreClient, tokio::task::JoinHandle<()>) {
    let state = MockStoreState::default();
    let app = Router::new()
        .route(
            "/v1beta1/projects/{project}/locations/{location}/exampleStores/{store_and_verb}",
            post(handle_verb),
        )
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock example store server should run");
    });

    let config = ExampleStoreConfig::new("test-project", "us-central1", "test-store")
        .with_endpoint(format!("http://{addr}"));
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    let client = ExampleStoreClient::with_credentials(config, credentials)
        .expect("build test example store client");

    (state, client, server)
}

async fn register_fixture(state: &MockStoreState, verb: &str, status: StatusCode, body: Value) {
    state.responses.lock().await.insert(verb.to_string(), (status, body));
}

async fn captured_bodies(state: &MockStoreState, verb: &str) -> Vec<Value> {
    state.bodies.lock().await.get(verb).cloned().unwrap_or_default()
}

fn example_fixture_json(example_id: &str, search_key: &str, user: &str, model: &str) -> Value {
    json!({
        "exampleId": example_id,
        "createTime": "2026-01-01T00:00:00Z",
        "storedContentsExample": {
            "searchKey": search_key,
            "contentsExample": {
                "contents": [{ "role": "user", "parts": [{ "text": user }] }],
                "expectedContents": [
                    { "content": { "role": "model", "parts": [{ "text": model }] } },
                ],
            },
        },
    })
}

#[tokio::test]
async fn test_upsert_examples_sends_documented_body_and_parses_mixed_results() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "upsertExamples",
        StatusCode::OK,
        json!({
            "results": [
                { "example": example_fixture_json("example-1", "What is 2+2?", "What is 2+2?", "4") },
                { "status": { "code": 3, "message": "invalid example" } },
            ],
        }),
    )
    .await;

    let request = UpsertExamplesRequest::new(vec![
        Example::new(
            StoredContentsExample::new(ContentsExample::new(
                vec![Content::new("user").with_text("What is 2+2?")],
                vec![Content::new("model").with_text("4")],
            ))
            .with_search_key("What is 2+2?"),
        ),
        Example::new(
            StoredContentsExample::new(ContentsExample::new(
                vec![Content::new("user").with_text("Capital of France?")],
                vec![Content::new("model").with_text("Paris.")],
            ))
            .with_last_entry_search_key(),
        )
        .with_display_name("capitals")
        .with_example_id("example-2"),
    ])
    .with_overwrite(true);

    let response = client.upsert_examples(request).await.expect("upsert should succeed");

    let bodies = captured_bodies(&state, "upsertExamples").await;
    assert_eq!(
        bodies,
        vec![json!({
            "examples": [
                {
                    "storedContentsExample": {
                        "searchKey": "What is 2+2?",
                        "contentsExample": {
                            "contents": [{ "role": "user", "parts": [{ "text": "What is 2+2?" }] }],
                            "expectedContents": [
                                { "content": { "role": "model", "parts": [{ "text": "4" }] } },
                            ],
                        },
                    },
                },
                {
                    "displayName": "capitals",
                    "exampleId": "example-2",
                    "storedContentsExample": {
                        "contentsExample": {
                            "contents": [{ "role": "user", "parts": [{ "text": "Capital of France?" }] }],
                            "expectedContents": [
                                { "content": { "role": "model", "parts": [{ "text": "Paris." }] } },
                            ],
                        },
                        "searchKeyGenerationMethod": { "lastEntry": {} },
                    },
                },
            ],
            "overwrite": true,
        })],
    );

    assert_eq!(response.results.len(), 2);
    let stored = response.results[0].example.as_ref().expect("first result stores the example");
    assert_eq!(stored.example_id.as_deref(), Some("example-1"));
    assert_eq!(stored.create_time.as_deref(), Some("2026-01-01T00:00:00Z"));
    assert!(response.results[0].status.is_none());
    let status = response.results[1].status.as_ref().expect("second result carries a status");
    assert_eq!(status.code, 3);
    assert_eq!(status.message, "invalid example");
    assert!(response.results[1].example.is_none());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_search_examples_serializes_top_k_as_string_with_search_key() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "searchExamples",
        StatusCode::OK,
        json!({
            "results": [
                {
                    "example": example_fixture_json("example-1", "What is 2+2?", "What is 2+2?", "4"),
                    "similarityScore": 0.87,
                },
            ],
        }),
    )
    .await;

    let request = SearchExamplesRequest::by_search_key("math questions", 5)
        .with_function_names(ExamplesArrayFilter::contains_any(vec!["calculator".to_string()]));
    let response = client.search_examples(request).await.expect("search should succeed");

    let bodies = captured_bodies(&state, "searchExamples").await;
    assert_eq!(
        bodies,
        vec![json!({
            "topK": "5",
            "parameters": {
                "storedContentsExampleParameters": {
                    "functionNames": {
                        "values": ["calculator"],
                        "arrayOperator": "CONTAINS_ANY",
                    },
                    "searchKey": "math questions",
                },
            },
        })],
    );
    // topK is a proto JSON int64: the wire value must be the STRING "5".
    assert!(bodies[0]["topK"].is_string());

    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].similarity_score, Some(0.87));
    assert_eq!(response.results[0].example.example_id.as_deref(), Some("example-1"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_search_examples_supports_content_search_key_queries() {
    let (state, client, server) = test_client().await;
    register_fixture(&state, "searchExamples", StatusCode::OK, json!({ "results": [] })).await;

    let request = SearchExamplesRequest::by_contents(
        vec![Content::new("user").with_text("How do I reset my password?")],
        3,
    );
    let response = client.search_examples(request).await.expect("search should succeed");

    let bodies = captured_bodies(&state, "searchExamples").await;
    assert_eq!(
        bodies,
        vec![json!({
            "topK": "3",
            "parameters": {
                "storedContentsExampleParameters": {
                    "contentSearchKey": {
                        "contents": [
                            { "role": "user", "parts": [{ "text": "How do I reset my password?" }] },
                        ],
                        "searchKeyGenerationMethod": { "lastEntry": {} },
                    },
                },
            },
        })],
    );
    assert!(response.results.is_empty());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_fetch_examples_sends_documented_body_and_parses_pagination() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "fetchExamples",
        StatusCode::OK,
        json!({
            "examples": [
                example_fixture_json("example-1", "What is 2+2?", "What is 2+2?", "4"),
                example_fixture_json("example-2", "Capital of France?", "Capital of France?", "Paris."),
            ],
            "nextPageToken": "page-2",
        }),
    )
    .await;

    let request = FetchExamplesRequest::new()
        .with_page_size(2)
        .with_page_token("page-1")
        .with_example_ids(vec!["example-1".to_string(), "example-2".to_string()])
        .with_filter(StoredContentsExampleFilter {
            search_keys: vec!["What is 2+2?".to_string()],
            function_names: Some(ExamplesArrayFilter::contains_all(vec!["calculator".to_string()])),
        });
    let response = client.fetch_examples(request).await.expect("fetch should succeed");

    let bodies = captured_bodies(&state, "fetchExamples").await;
    assert_eq!(
        bodies,
        vec![json!({
            "pageSize": 2,
            "pageToken": "page-1",
            "exampleIds": ["example-1", "example-2"],
            "storedContentsExampleFilter": {
                "searchKeys": ["What is 2+2?"],
                "functionNames": {
                    "values": ["calculator"],
                    "arrayOperator": "CONTAINS_ALL",
                },
            },
        })],
    );

    assert_eq!(response.examples.len(), 2);
    assert_eq!(response.examples[1].example_id.as_deref(), Some("example-2"));
    assert_eq!(response.next_page_token.as_deref(), Some("page-2"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_fetch_examples_omits_absent_optionals_entirely() {
    let (state, client, server) = test_client().await;
    register_fixture(&state, "fetchExamples", StatusCode::OK, json!({ "examples": [] })).await;

    let response =
        client.fetch_examples(FetchExamplesRequest::new()).await.expect("fetch should succeed");

    let bodies = captured_bodies(&state, "fetchExamples").await;
    assert_eq!(bodies, vec![json!({})]);
    assert!(response.examples.is_empty());
    assert!(response.next_page_token.is_none());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_upstream_error_statuses_map_to_adk_error_categories() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "searchExamples",
        StatusCode::NOT_FOUND,
        json!({ "error": { "code": 404, "message": "example store not found" } }),
    )
    .await;

    let error = client
        .search_examples(SearchExamplesRequest::by_search_key("q", 1))
        .await
        .expect_err("404 must surface as an error");
    assert!(error.is_not_found(), "unexpected error: {error:?}");
    assert_eq!(error.details.upstream_status_code, Some(404));

    server.abort();
    let _ = server.await;
}
