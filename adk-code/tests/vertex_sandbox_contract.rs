//! Contract tests for the Vertex AI Agent Engine sandbox client against a
//! mock server: create polls its LRO and re-GETs the sparse operation
//! response, `:execute` round-trips the code-execution chunk conventions,
//! the executor lazily creates and recreates per-session sandboxes, the
//! 100 MB request-side file limit rejects before sending, list paginates,
//! and delete waits its LRO.
//!
//! Captured request bodies are compared as whole JSON values — they are the
//! wire contract adk-python's `AgentEngineSandboxCodeExecutor` shares.

#![cfg(feature = "vertex-sandbox")]

use adk_code::vertex_sandbox::{
    CodeExecutionEnvironment, CodeLanguage, CreateSandboxRequest, InputFile, MachineConfig,
    OutputFile, SandboxCodeExecutor, SandboxEnvironment, SandboxEnvironmentSpec,
    SandboxExecutionResult, SandboxState, VertexSandboxClient, VertexSandboxConfig,
};
use adk_core::ErrorCategory;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const PROJECT: &str = "test-project";
const LOCATION: &str = "us-central1";
const ENGINE: &str = "4242";

fn engine_name() -> String {
    format!("projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/{ENGINE}")
}

fn sandbox_name(id: &str) -> String {
    format!("{}/sandboxEnvironments/{id}", engine_name())
}

fn operation_name(id: &str) -> String {
    format!("{}/operations/{id}", engine_name())
}

/// The full sandbox resource the mock serves on GET.
fn sandbox_fixture(id: &str, state: &str) -> Value {
    json!({
        "name": sandbox_name(id),
        "displayName": "default_sandbox",
        "createTime": "2026-01-01T00:00:00Z",
        "updateTime": "2026-01-01T00:00:00Z",
        "state": state,
        "spec": {
            "codeExecutionEnvironment": {
                "machineConfig": "MACHINE_CONFIG_UNSPECIFIED",
                "codeLanguage": "LANGUAGE_PYTHON",
            },
        },
        "expireTime": "2027-01-01T00:00:00Z",
    })
}

/// One queued GET response for a sandbox.
enum SandboxGet {
    Found(Value),
    NotFound,
}

#[derive(Default)]
struct MockState {
    create_bodies: Vec<Value>,
    /// IDs assigned to successive creates (defaults to "111").
    next_create_ids: Vec<String>,
    /// Operation ID → sandbox ID minted by that create.
    operations: HashMap<String, String>,
    operation_polls: usize,
    /// Sandbox ID → queue of GET responses, popped per request; the last
    /// repeats.
    get_responses: HashMap<String, Vec<SandboxGet>>,
    /// Captured `(sandbox id, body)` per `:execute` call.
    execute_bodies: Vec<(String, Value)>,
    /// The `:execute` response body.
    execute_response: Value,
    /// Captured `(pageSize, pageToken)` query pairs per list call.
    list_queries: Vec<(Option<String>, Option<String>)>,
    /// Queue of list responses, popped per request.
    list_responses: Vec<Value>,
    deleted: Vec<String>,
}

type SharedState = Arc<Mutex<MockState>>;

async fn start_mock(state: SharedState) -> String {
    let collection_path = format!("/v1beta1/{}/sandboxEnvironments", engine_name());
    let item_path = format!("{collection_path}/{{id}}");
    let operations_path = format!("/v1beta1/{}/operations/{{op}}", engine_name());

    let app =
        Router::new()
            .route(
                &collection_path,
                get(
                    |State(state): State<SharedState>,
                     Query(query): Query<HashMap<String, String>>| async move {
                        let mut state = state.lock().await;
                        state.list_queries.push((
                            query.get("pageSize").cloned(),
                            query.get("pageToken").cloned(),
                        ));
                        let response = if state.list_responses.is_empty() {
                            json!({ "sandboxEnvironments": [] })
                        } else {
                            state.list_responses.remove(0)
                        };
                        Json(response)
                    },
                )
                .post(
                    |State(state): State<SharedState>, Json(body): Json<Value>| async move {
                        let mut state = state.lock().await;
                        state.create_bodies.push(body);
                        let index = state.create_bodies.len() - 1;
                        let id = state
                            .next_create_ids
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| "111".to_string());
                        let op = format!("create-{index}");
                        state.operations.insert(op.clone(), id);
                        // Not done yet: the client must poll the operation.
                        Json(json!({ "name": operation_name(&op), "done": false }))
                    },
                ),
            )
            .route(
                &operations_path,
                get(|State(state): State<SharedState>, Path(op): Path<String>| async move {
                    let mut state = state.lock().await;
                    state.operation_polls += 1;
                    let mut operation = json!({ "name": operation_name(&op), "done": true });
                    if let Some(id) = state.operations.get(&op) {
                        // Deliberately sparse: only the name, forcing the re-GET.
                        operation["response"] = json!({ "name": sandbox_name(id) });
                    }
                    Json(operation)
                }),
            )
            .route(
                &item_path,
                get(|State(state): State<SharedState>, Path(id): Path<String>| async move {
                    let mut state = state.lock().await;
                    let queue = state.get_responses.entry(id.clone()).or_insert_with(|| {
                        vec![SandboxGet::Found(sandbox_fixture(&id, "STATE_RUNNING"))]
                    });
                    let next = if queue.len() > 1 {
                        queue.remove(0)
                    } else {
                        queue.first().unwrap().clone_response()
                    };
                    match next {
                        SandboxGet::Found(value) => Json(value).into_response(),
                        SandboxGet::NotFound => {
                            (StatusCode::NOT_FOUND, "sandbox not found").into_response()
                        }
                    }
                })
                .post(
                    |State(state): State<SharedState>,
                     Path(id): Path<String>,
                     Json(body): Json<Value>| async move {
                        let mut state = state.lock().await;
                        let id =
                            id.strip_suffix(":execute").expect("POST is only :execute").to_string();
                        state.execute_bodies.push((id, body));
                        let response = state.execute_response.clone();
                        Json(response)
                    },
                )
                .delete(
                    |State(state): State<SharedState>, Path(id): Path<String>| async move {
                        let mut state = state.lock().await;
                        state.deleted.push(id.clone());
                        let op = format!("delete-{id}");
                        state.operations.insert(op.clone(), id);
                        Json(json!({ "name": operation_name(&op), "done": false }))
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

impl SandboxGet {
    fn clone_response(&self) -> Self {
        match self {
            Self::Found(value) => Self::Found(value.clone()),
            Self::NotFound => Self::NotFound,
        }
    }
}

fn build_client(endpoint: &str) -> VertexSandboxClient {
    VertexSandboxClient::with_credentials(
        VertexSandboxConfig::new(PROJECT, LOCATION).with_endpoint(endpoint),
        api_key_credentials::Builder::new("test-api-key").build(),
    )
    .expect("build test client")
}

/// The default `:execute` response: one console chunk saying "ok\n".
fn console_response(msg_out: &str, msg_err: &str) -> Value {
    json!({
        "outputs": [{
            "mimeType": "application/json",
            "data": BASE64.encode(json!({ "msg_out": msg_out, "msg_err": msg_err }).to_string()),
        }],
    })
}

fn expected_sandbox(id: &str, state: SandboxState) -> SandboxEnvironment {
    SandboxEnvironment {
        name: Some(sandbox_name(id)),
        display_name: Some("default_sandbox".to_string()),
        create_time: Some("2026-01-01T00:00:00Z".to_string()),
        update_time: Some("2026-01-01T00:00:00Z".to_string()),
        state: Some(state),
        spec: Some(SandboxEnvironmentSpec::code_execution(CodeExecutionEnvironment {
            machine_config: Some(MachineConfig::Unspecified),
            code_language: Some(CodeLanguage::Python),
        })),
        expire_time: Some("2027-01-01T00:00:00Z".to_string()),
        ..SandboxEnvironment::default()
    }
}

#[tokio::test]
async fn create_polls_the_lro_and_regets_the_sandbox() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let request = CreateSandboxRequest::new("default_sandbox")
        .with_ttl("31536000s")
        .with_spec(SandboxEnvironmentSpec::code_execution(CodeExecutionEnvironment::default()));
    let sandbox = client.create_sandbox(ENGINE, request).await.unwrap();

    assert_eq!(sandbox, expected_sandbox("111", SandboxState::Running));

    let state = state.lock().await;
    assert_eq!(
        state.create_bodies,
        vec![json!({
            "displayName": "default_sandbox",
            "ttl": "31536000s",
            "spec": { "codeExecutionEnvironment": {} },
        })],
    );
    assert!(state.operation_polls >= 1, "the create LRO was never polled");
}

#[tokio::test]
async fn create_accepts_a_full_engine_resource_name() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let sandbox = client
        .create_sandbox(&engine_name(), CreateSandboxRequest::new("default_sandbox"))
        .await
        .unwrap();
    assert_eq!(sandbox.name, Some(sandbox_name("111")));
}

#[tokio::test]
async fn engine_and_sandbox_references_outside_the_scope_are_rejected() {
    let endpoint = start_mock(SharedState::default()).await;
    let client = build_client(&endpoint);

    let foreign_engine = "projects/other/locations/us-central1/reasoningEngines/1";
    let error =
        client.create_sandbox(foreign_engine, CreateSandboxRequest::new("x")).await.unwrap_err();
    assert_eq!(error.category, ErrorCategory::InvalidInput);
    assert!(error.message.contains("does not belong to"), "{}", error.message);

    let foreign_sandbox =
        "projects/other/locations/us-central1/reasoningEngines/1/sandboxEnvironments/2";
    let error = client.get_sandbox(foreign_sandbox).await.unwrap_err();
    assert_eq!(error.code, "code.vertex_sandbox.invalid_input");

    let error = client.get_sandbox(&engine_name()).await.unwrap_err();
    assert!(error.message.contains("sandboxEnvironments"), "{}", error.message);
}

#[tokio::test]
async fn execute_round_trips_the_chunk_conventions() {
    let state = SharedState::default();
    state.lock().await.execute_response = json!({
        "outputs": [
            {
                "mimeType": "application/json",
                "data": BASE64.encode(
                    json!({ "msg_out": "hello\n", "msg_err": "warning\n" }).to_string(),
                ),
            },
            {
                // Output file: no mimeType, base64 file_name attribute.
                "data": BASE64.encode(b"file-bytes"),
                "metadata": { "attributes": { "file_name": BASE64.encode("result.txt") } },
            },
        ],
    });
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let files = [InputFile::new("in.txt", "text/plain", b"abc".to_vec())];
    let result = client.execute_code(&sandbox_name("111"), "print('x')", &files).await.unwrap();

    assert_eq!(
        result,
        SandboxExecutionResult {
            stdout: "hello\n".to_string(),
            stderr: "warning\n".to_string(),
            output_files: vec![OutputFile {
                name: "result.txt".to_string(),
                mime_type: None,
                data: b"file-bytes".to_vec(),
            }],
        },
    );

    let state = state.lock().await;
    assert_eq!(
        state.execute_bodies,
        vec![(
            "111".to_string(),
            json!({
                "inputs": [
                    {
                        "mimeType": "application/json",
                        "data": BASE64.encode(json!({ "code": "print('x')" }).to_string()),
                    },
                    {
                        "mimeType": "text/plain",
                        "data": BASE64.encode(b"abc"),
                        "metadata": { "attributes": { "file_name": BASE64.encode("in.txt") } },
                    },
                ],
            }),
        )],
    );
}

#[tokio::test]
async fn executor_creates_lazily_and_recreates_when_not_running() {
    let state = SharedState::default();
    {
        let mut state = state.lock().await;
        state.next_create_ids = vec!["111".to_string(), "222".to_string()];
        state.execute_response = console_response("ok\n", "");
        // First GET (create's re-GET) sees RUNNING; the second call's
        // liveness check sees TERMINATED, forcing a recreate.
        state.get_responses.insert(
            "111".to_string(),
            vec![
                SandboxGet::Found(sandbox_fixture("111", "STATE_RUNNING")),
                SandboxGet::Found(sandbox_fixture("111", "STATE_TERMINATED")),
            ],
        );
    }
    let endpoint = start_mock(state.clone()).await;
    let client = Arc::new(build_client(&endpoint));
    let executor = SandboxCodeExecutor::for_engine(client, ENGINE);

    let first = executor.execute_for_session("session-1", "print(1)", &[]).await.unwrap();
    assert_eq!(first.stdout, "ok\n");
    let second = executor.execute_for_session("session-1", "print(2)", &[]).await.unwrap();
    assert_eq!(second.stdout, "ok\n");

    let state = state.lock().await;
    assert_eq!(state.create_bodies.len(), 2, "the terminated sandbox was not recreated");
    let executed: Vec<&str> = state.execute_bodies.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(executed, vec!["111", "222"]);
    // Lazy creates carry the adk-python parity defaults.
    assert_eq!(
        state.create_bodies[0],
        json!({
            "displayName": "default_sandbox",
            "ttl": "31536000s",
            "spec": { "codeExecutionEnvironment": {} },
        }),
    );
}

#[tokio::test]
async fn executor_recreates_when_the_sandbox_is_gone() {
    let state = SharedState::default();
    {
        let mut state = state.lock().await;
        state.next_create_ids = vec!["111".to_string(), "222".to_string()];
        state.execute_response = console_response("ok\n", "");
        state.get_responses.insert(
            "111".to_string(),
            vec![SandboxGet::Found(sandbox_fixture("111", "STATE_RUNNING")), SandboxGet::NotFound],
        );
    }
    let endpoint = start_mock(state.clone()).await;
    let client = Arc::new(build_client(&endpoint));
    let executor = SandboxCodeExecutor::for_engine(client, ENGINE);

    executor.execute_for_session("session-1", "print(1)", &[]).await.unwrap();
    executor.execute_for_session("session-1", "print(2)", &[]).await.unwrap();

    let state = state.lock().await;
    assert_eq!(state.create_bodies.len(), 2, "the missing sandbox was not recreated");
    let executed: Vec<&str> = state.execute_bodies.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(executed, vec!["111", "222"]);
}

#[tokio::test]
async fn sessions_get_separate_sandboxes() {
    let state = SharedState::default();
    {
        let mut state = state.lock().await;
        state.next_create_ids = vec!["111".to_string(), "222".to_string()];
        state.execute_response = console_response("ok\n", "");
    }
    let endpoint = start_mock(state.clone()).await;
    let client = Arc::new(build_client(&endpoint));
    let executor = SandboxCodeExecutor::for_engine(client, ENGINE);

    executor.execute_for_session("session-a", "print(1)", &[]).await.unwrap();
    executor.execute_for_session("session-b", "print(2)", &[]).await.unwrap();
    executor.execute_for_session("session-a", "print(3)", &[]).await.unwrap();

    let state = state.lock().await;
    assert_eq!(state.create_bodies.len(), 2, "each session creates exactly one sandbox");
    let executed: Vec<&str> = state.execute_bodies.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(executed, vec!["111", "222", "111"]);
}

#[tokio::test]
async fn fixed_sandbox_executor_requires_a_running_sandbox() {
    let state = SharedState::default();
    state.lock().await.get_responses.insert(
        "111".to_string(),
        vec![SandboxGet::Found(sandbox_fixture("111", "STATE_PROVISIONING"))],
    );
    let endpoint = start_mock(state.clone()).await;
    let client = Arc::new(build_client(&endpoint));
    let executor = SandboxCodeExecutor::for_sandbox(client, sandbox_name("111"));

    let error = executor.execute_for_session("session-1", "print(1)", &[]).await.unwrap_err();
    assert_eq!(error.category, ErrorCategory::Unavailable);
    assert!(error.message.contains("not running"), "{}", error.message);
    assert!(state.lock().await.execute_bodies.is_empty());
}

#[tokio::test]
async fn oversized_requests_are_rejected_before_sending() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let too_big = vec![InputFile::new(
        "big.bin",
        "application/octet-stream",
        vec![0_u8; 100 * 1024 * 1024 + 1],
    )];
    let error = client.execute_code(&sandbox_name("111"), "print(1)", &too_big).await.unwrap_err();

    assert_eq!(error.category, ErrorCategory::InvalidInput);
    assert_eq!(error.code, "code.vertex_sandbox.invalid_input");
    assert!(error.message.contains("100 MB"), "{}", error.message);
    assert!(state.lock().await.execute_bodies.is_empty(), "the request must not be sent");
}

#[tokio::test]
async fn list_follows_pagination() {
    let state = SharedState::default();
    state.lock().await.list_responses = vec![
        json!({
            "sandboxEnvironments": [sandbox_fixture("111", "STATE_RUNNING")],
            "nextPageToken": "token-1",
        }),
        json!({
            "sandboxEnvironments": [sandbox_fixture("222", "STATE_PROVISIONING")],
            "nextPageToken": "",
        }),
    ];
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let sandboxes = client.list_sandboxes(ENGINE).await.unwrap();
    assert_eq!(
        sandboxes,
        vec![
            expected_sandbox("111", SandboxState::Running),
            expected_sandbox("222", SandboxState::Provisioning),
        ],
    );

    let state = state.lock().await;
    assert_eq!(
        state.list_queries,
        vec![
            (Some("100".to_string()), None),
            (Some("100".to_string()), Some("token-1".to_string())),
        ],
    );
}

#[tokio::test]
async fn delete_waits_the_lro() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    client.delete_sandbox(&sandbox_name("111")).await.unwrap();

    let state = state.lock().await;
    assert_eq!(state.deleted, vec!["111".to_string()]);
    assert!(state.operation_polls >= 1, "the delete LRO was never polled");
}
