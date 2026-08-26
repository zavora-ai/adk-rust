//! Contract tests for the Vertex AI RAG Engine backend against a mock
//! server: `:retrieveContexts` bodies are compared as whole JSON values —
//! they are the wire contract adk-python's `VertexAiRagRetrieval` shares —
//! and corpus/file reads exercise pagination and not-found handling.

#![cfg(feature = "vertex-rag")]

use adk_core::{CallbackContext, Content, EventActions, ReadonlyContext, Tool, ToolContext};
use adk_rag::vertex_rag::{
    RetrieveContextsRequest, VertexAiRagRetrievalTool, VertexRagConfig, VertexRagEngineClient,
};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const PROJECT: &str = "test-project";
const LOCATION: &str = "us-central1";
const CORPUS: &str = "4242";

fn corpus_name(id: &str) -> String {
    format!("projects/{PROJECT}/locations/{LOCATION}/ragCorpora/{id}")
}

#[derive(Default)]
struct MockState {
    retrieve_bodies: Vec<Value>,
    /// Queue of `:retrieveContexts` responses, popped per request.
    retrieve_responses: Vec<Value>,
    /// Fixture returned for `GET ragCorpora/4242`; anything else is 404.
    corpus_fixture: Option<Value>,
    /// Queue of `ragCorpora` list pages, popped per request.
    corpora_pages: Vec<Value>,
    /// `pageToken` values observed on `ragCorpora` list requests.
    corpora_page_tokens: Vec<Option<String>>,
    /// Queue of `ragFiles` list pages, popped per request.
    file_pages: Vec<Value>,
    /// `pageToken` values observed on `ragFiles` list requests.
    file_page_tokens: Vec<Option<String>>,
}

type SharedState = Arc<Mutex<MockState>>;

async fn start_mock(state: SharedState) -> String {
    let location_path = format!("/v1beta1/projects/{PROJECT}/locations/{LOCATION}");
    let app =
        Router::new()
            .route(
                &format!("{location_path}:retrieveContexts"),
                post(|State(state): State<SharedState>, Json(body): Json<Value>| async move {
                    let mut state = state.lock().await;
                    state.retrieve_bodies.push(body);
                    let response = if state.retrieve_responses.is_empty() {
                        json!({ "contexts": { "contexts": [] } })
                    } else {
                        state.retrieve_responses.remove(0)
                    };
                    Json(response)
                }),
            )
            .route(
                &format!("{location_path}/ragCorpora"),
                get(
                    |State(state): State<SharedState>,
                     Query(query): Query<HashMap<String, String>>| async move {
                        let mut state = state.lock().await;
                        state.corpora_page_tokens.push(query.get("pageToken").cloned());
                        let page = if state.corpora_pages.is_empty() {
                            json!({ "ragCorpora": [] })
                        } else {
                            state.corpora_pages.remove(0)
                        };
                        Json(page)
                    },
                ),
            )
            .route(
                &format!("{location_path}/ragCorpora/{{corpus}}"),
                get(|State(state): State<SharedState>, Path(corpus): Path<String>| async move {
                    let state = state.lock().await;
                    match (&state.corpus_fixture, corpus.as_str()) {
                        (Some(fixture), CORPUS) => Ok(Json(fixture.clone())),
                        _ => Err(StatusCode::NOT_FOUND),
                    }
                }),
            )
            .route(
                &format!("{location_path}/ragCorpora/{{corpus}}/ragFiles"),
                get(
                    |State(state): State<SharedState>,
                     Query(query): Query<HashMap<String, String>>| async move {
                        let mut state = state.lock().await;
                        state.file_page_tokens.push(query.get("pageToken").cloned());
                        let page = if state.file_pages.is_empty() {
                            json!({ "ragFiles": [] })
                        } else {
                            state.file_pages.remove(0)
                        };
                        Json(page)
                    },
                ),
            )
            .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

fn build_client(endpoint: &str) -> VertexRagEngineClient {
    let config = VertexRagConfig::new(PROJECT, LOCATION).with_endpoint(endpoint);
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    VertexRagEngineClient::with_credentials(config, credentials).expect("build test client")
}

/// Minimal ToolContext for exercising the tool end-to-end.
struct TestContext {
    content: Content,
    actions: std::sync::Mutex<EventActions>,
}

impl TestContext {
    fn arc() -> Arc<dyn ToolContext> {
        Arc::new(Self {
            content: Content::new("user"),
            actions: std::sync::Mutex::new(EventActions::default()),
        })
    }
}

#[async_trait::async_trait]
impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn session_id(&self) -> &str {
        "session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait::async_trait]
impl CallbackContext for TestContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait::async_trait]
impl ToolContext for TestContext {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn retrieve_contexts_sends_the_modernized_wire_body() {
    let state = SharedState::default();
    state.lock().await.retrieve_responses.push(json!({
        "contexts": {
            "contexts": [
                {
                    "sourceUri": "gs://docs/refunds.md",
                    "sourceDisplayName": "refunds.md",
                    "text": "Refunds are processed within 5 business days.",
                    "score": 0.87,
                    "chunk": {
                        "chunkId": "c-1",
                        "fileId": "f-1",
                        "text": "Refunds are processed within 5 business days.",
                        "pageSpan": { "firstPage": 1, "lastPage": 1 }
                    }
                }
            ]
        }
    }));
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let request = RetrieveContextsRequest::new("what is the refund policy?", [CORPUS])
        .similarity_top_k(4)
        .vector_distance_threshold(0.55);
    let contexts = client.retrieve_contexts(&request).await.unwrap();

    let captured = state.lock().await;
    assert_eq!(captured.retrieve_bodies.len(), 1);
    // Whole-value comparison: the deprecated `query.similarityTopK` and
    // `vertexRagStore.vectorDistanceThreshold` spellings must never appear —
    // both knobs ride ragRetrievalConfig, the current wire path.
    assert_eq!(
        captured.retrieve_bodies[0],
        json!({
            "vertexRagStore": {
                "ragResources": [
                    { "ragCorpus": corpus_name(CORPUS) }
                ]
            },
            "query": {
                "text": "what is the refund policy?",
                "ragRetrievalConfig": {
                    "topK": 4,
                    "filter": { "vectorDistanceThreshold": 0.55 }
                }
            }
        }),
    );

    assert_eq!(contexts.len(), 1);
    let context = &contexts[0];
    assert_eq!(context.source_uri.as_deref(), Some("gs://docs/refunds.md"));
    assert_eq!(context.score, Some(0.87));
    let chunk = context.chunk.as_ref().unwrap();
    assert_eq!(chunk.chunk_id.as_deref(), Some("c-1"));
    assert_eq!(chunk.page_span.as_ref().unwrap().last_page, Some(1));
}

#[tokio::test]
async fn retrieve_contexts_without_knobs_omits_the_retrieval_config() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let full_name = corpus_name("77");
    let request = RetrieveContextsRequest::new("q", [full_name.clone()]);
    let contexts = client.retrieve_contexts(&request).await.unwrap();
    assert!(contexts.is_empty());

    let captured = state.lock().await;
    assert_eq!(
        captured.retrieve_bodies[0],
        json!({
            "vertexRagStore": { "ragResources": [{ "ragCorpus": full_name }] },
            "query": { "text": "q" }
        }),
    );
}

#[tokio::test]
async fn retrieve_contexts_rejects_invalid_requests_before_transport() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let blank = RetrieveContextsRequest::new("  ", [CORPUS]);
    assert_eq!(client.retrieve_contexts(&blank).await.unwrap_err().http_status_code(), 400);

    let no_corpora = RetrieveContextsRequest::new("q", Vec::<String>::new());
    assert_eq!(client.retrieve_contexts(&no_corpora).await.unwrap_err().http_status_code(), 400);

    let both_thresholds = RetrieveContextsRequest::new("q", [CORPUS])
        .vector_distance_threshold(0.5)
        .vector_similarity_threshold(0.5);
    let error = client.retrieve_contexts(&both_thresholds).await.unwrap_err();
    assert_eq!(error.http_status_code(), 400);
    assert!(error.message.contains("mutually exclusive"), "{}", error.message);

    assert!(state.lock().await.retrieve_bodies.is_empty(), "nothing reached the wire");
}

#[tokio::test]
async fn get_corpus_parses_the_resource_and_maps_missing_corpora_to_guidance() {
    let state = SharedState::default();
    state.lock().await.corpus_fixture = Some(json!({
        "name": corpus_name(CORPUS),
        "displayName": "support docs",
        "corpusStatus": { "state": "ACTIVE" },
        "ragFilesCount": 3,
        "createTime": "2025-01-01T00:00:00Z",
        "updateTime": "2025-02-02T00:00:00Z",
    }));
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let corpus = client.get_corpus(CORPUS).await.unwrap();
    assert_eq!(corpus.name.as_deref(), Some(corpus_name(CORPUS).as_str()));
    assert_eq!(corpus.rag_files_count, Some(3));
    assert_eq!(corpus.corpus_status.unwrap().state.as_deref(), Some("ACTIVE"));

    let error = client.get_corpus("does-not-exist").await.unwrap_err();
    assert_eq!(error.http_status_code(), 404);
    assert!(error.message.contains("was not found"), "{}", error.message);
    assert!(error.message.contains("Vertex AI console"), "{}", error.message);
}

#[tokio::test]
async fn ensure_corpus_ready_rejects_an_empty_corpus_with_import_guidance() {
    let state = SharedState::default();
    state.lock().await.corpus_fixture = Some(json!({
        "name": corpus_name(CORPUS),
        "corpusStatus": { "state": "ACTIVE" },
        "ragFilesCount": 0,
    }));
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let error = client.ensure_corpus_ready(CORPUS).await.unwrap_err();
    assert_eq!(error.http_status_code(), 400);
    assert!(error.message.contains("no imported files"), "{}", error.message);
    assert!(error.message.contains("Import documents"), "{}", error.message);
}

#[tokio::test]
async fn ensure_corpus_ready_surfaces_the_error_state() {
    let state = SharedState::default();
    state.lock().await.corpus_fixture = Some(json!({
        "name": corpus_name(CORPUS),
        "corpusStatus": { "state": "ERROR", "errorStatus": "vector db unreachable" },
    }));
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let error = client.ensure_corpus_ready(CORPUS).await.unwrap_err();
    assert!(error.message.contains("vector db unreachable"), "{}", error.message);
}

#[tokio::test]
async fn list_corpora_follows_pagination() {
    let state = SharedState::default();
    {
        let mut lock = state.lock().await;
        lock.corpora_pages.push(json!({
            "ragCorpora": [
                { "name": corpus_name("1"), "displayName": "a" },
                { "name": corpus_name("2"), "displayName": "b" },
            ],
            "nextPageToken": "page-2",
        }));
        lock.corpora_pages.push(json!({
            "ragCorpora": [ { "name": corpus_name("3"), "displayName": "c" } ],
        }));
    }
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let corpora = client.list_corpora().await.unwrap();
    let names: Vec<_> = corpora.iter().filter_map(|corpus| corpus.name.as_deref()).collect();
    assert_eq!(names, [corpus_name("1"), corpus_name("2"), corpus_name("3")]);

    let captured = state.lock().await;
    assert_eq!(captured.corpora_page_tokens, [None, Some("page-2".to_string())]);
}

#[tokio::test]
async fn list_rag_files_follows_pagination_and_parses_leniently() {
    let state = SharedState::default();
    {
        let mut lock = state.lock().await;
        lock.file_pages.push(json!({
            "ragFiles": [
                {
                    "name": format!("{}/ragFiles/f-1", corpus_name(CORPUS)),
                    "displayName": "refunds.md",
                    "sizeBytes": "1024",
                    "ragFileType": "RAG_FILE_TYPE_TXT",
                    "fileStatus": { "state": "ACTIVE" },
                }
            ],
            "nextPageToken": "page-2",
        }));
        lock.file_pages.push(json!({
            "ragFiles": [
                { "name": format!("{}/ragFiles/f-2", corpus_name(CORPUS)), "sizeBytes": 2048 }
            ],
        }));
    }
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let files = client.list_rag_files(CORPUS).await.unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].size_bytes, Some(1024), "int64-as-string parses");
    assert_eq!(files[1].size_bytes, Some(2048), "int64-as-number parses");

    let captured = state.lock().await;
    assert_eq!(captured.file_page_tokens, [None, Some("page-2".to_string())]);
}

#[tokio::test]
async fn the_retrieval_tool_runs_end_to_end_against_the_mock() {
    let state = SharedState::default();
    state.lock().await.retrieve_responses.push(json!({
        "contexts": {
            "contexts": [
                {
                    "sourceUri": "gs://docs/refunds.md",
                    "sourceDisplayName": "refunds.md",
                    "text": "Refunds are processed within 5 business days.",
                    "score": 0.87
                },
                // A minimal context: optional fields stay absent in the output.
                { "text": "Store credit is issued instantly." }
            ]
        }
    }));
    let endpoint = start_mock(state.clone()).await;
    let client = Arc::new(build_client(&endpoint));

    let tool = VertexAiRagRetrievalTool::new(client, vec![CORPUS.to_string()])
        .similarity_top_k(4)
        .vector_distance_threshold(0.55);
    assert!(tool.is_read_only());
    assert!(tool.is_concurrency_safe());
    assert_eq!(
        tool.parameters_schema().unwrap()["required"],
        json!(["query"]),
        "query is the single required parameter",
    );

    let output = tool
        .execute(TestContext::arc(), json!({ "query": "what is the refund policy?" }))
        .await
        .unwrap();

    assert_eq!(
        output,
        json!([
            {
                "text": "Refunds are processed within 5 business days.",
                "sourceUri": "gs://docs/refunds.md",
                "sourceDisplayName": "refunds.md",
                "score": 0.87
            },
            { "text": "Store credit is issued instantly." }
        ]),
    );

    let captured = state.lock().await;
    assert_eq!(
        captured.retrieve_bodies[0],
        json!({
            "vertexRagStore": { "ragResources": [{ "ragCorpus": corpus_name(CORPUS) }] },
            "query": {
                "text": "what is the refund policy?",
                "ragRetrievalConfig": {
                    "topK": 4,
                    "filter": { "vectorDistanceThreshold": 0.55 }
                }
            }
        }),
    );
}

#[tokio::test]
async fn the_tool_rejects_calls_without_a_query() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = Arc::new(build_client(&endpoint));

    let tool = VertexAiRagRetrievalTool::new(client, vec![CORPUS.to_string()]);
    let error = tool.execute(TestContext::arc(), json!({})).await.unwrap_err();
    assert!(error.message.contains("query"), "{}", error.message);
    assert!(state.lock().await.retrieve_bodies.is_empty(), "nothing reached the wire");
}

// ===== Live tests (require GCP credentials and a provisioned corpus) =====

fn live_env() -> Option<(VertexRagConfig, String)> {
    let corpus = std::env::var("VERTEX_RAG_CORPUS").ok()?;
    let config = VertexRagConfig::from_env().ok()?;
    Some((config, corpus))
}

#[tokio::test]
#[ignore = "requires GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, VERTEX_RAG_CORPUS, and ADC"]
async fn live_corpus_reads() {
    let (config, corpus) = live_env().expect(
        "set GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, and VERTEX_RAG_CORPUS to run live tests",
    );
    let client = VertexRagEngineClient::new_with_adc(config).unwrap();

    let ready = client.ensure_corpus_ready(&corpus).await.unwrap();
    assert!(ready.name.is_some());

    let corpora = client.list_corpora().await.unwrap();
    assert!(!corpora.is_empty(), "the project has at least the test corpus");

    let files = client.list_rag_files(&corpus).await.unwrap();
    assert!(!files.is_empty(), "the test corpus has imported files");
}

#[tokio::test]
#[ignore = "requires GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, VERTEX_RAG_CORPUS, and ADC"]
async fn live_retrieve_contexts_via_the_tool() {
    let (config, corpus) = live_env().expect(
        "set GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, and VERTEX_RAG_CORPUS to run live tests",
    );
    let client = Arc::new(VertexRagEngineClient::new_with_adc(config).unwrap());

    let tool = VertexAiRagRetrievalTool::new(client, vec![corpus]).similarity_top_k(3);
    let output =
        tool.execute(TestContext::arc(), json!({ "query": "what is this corpus about?" })).await;
    let contexts = output.unwrap();
    assert!(contexts.is_array());
}
