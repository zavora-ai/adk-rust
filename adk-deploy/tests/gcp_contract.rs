//! Contract tests for the Agent Engine deployment client.
//!
//! DTO golden tests pin the wire shapes against JSON transcribed from the
//! v1beta1 REST reference; the mock-server tests exercise all four client
//! operations end-to-end.

#![cfg(feature = "gcp")]

use adk_deploy::gcp::{
    CreateReasoningEngineRequest, DeploymentSpec, EnvVar, GcpDeployClient, GcpDeployConfig,
    SecretEnvVar, SecretRef, default_class_methods, gcloud_build_submit_command,
};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const PROJECT: &str = "test-project";
const LOCATION: &str = "us-central1";

fn operation_name() -> String {
    format!("projects/{PROJECT}/locations/{LOCATION}/operations/9001")
}

fn engine_name() -> String {
    format!("projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/4242")
}

// ── DTO golden tests (shapes from the v1beta1 REST reference) ─────────────

/// The full BYOC create body: every supported knob set once, serialized to
/// exactly the reference shape. Field placement matters — env, scaling, and
/// resource limits live under `deploymentSpec`, not `containerSpec`.
#[test]
fn create_request_serializes_to_the_reference_shape() {
    let request = CreateReasoningEngineRequest::byoc(
        "my-agent",
        "us-central1-docker.pkg.dev/test-project/agents/my-agent:latest",
    )
    .with_service_account("agent-runner@test-project.iam.gserviceaccount.com")
    .with_kms_key("projects/test-project/locations/us-central1/keyRings/kr/cryptoKeys/k")
    .with_deployment_spec(DeploymentSpec {
        env: vec![EnvVar { name: "LOG_LEVEL".to_string(), value: "info".to_string() }],
        secret_env: vec![SecretEnvVar {
            name: "GOOGLE_API_KEY".to_string(),
            secret_ref: SecretRef {
                secret: "google-api-key".to_string(),
                version: Some("latest".to_string()),
            },
        }],
        psc_interface_config: Some(json!({
            "networkAttachment": "projects/test-project/regions/us-central1/networkAttachments/na"
        })),
        resource_limits: Some(BTreeMap::from([
            ("cpu".to_string(), "4".to_string()),
            ("memory".to_string(), "8Gi".to_string()),
        ])),
        min_instances: Some(1),
        max_instances: Some(10),
        container_concurrency: Some(9),
    });

    let expected = json!({
        "displayName": "my-agent",
        "spec": {
            "containerSpec": {
                "imageUri": "us-central1-docker.pkg.dev/test-project/agents/my-agent:latest",
            },
            "deploymentSpec": {
                "env": [ { "name": "LOG_LEVEL", "value": "info" } ],
                "secretEnv": [
                    {
                        "name": "GOOGLE_API_KEY",
                        "secretRef": { "secret": "google-api-key", "version": "latest" },
                    }
                ],
                "pscInterfaceConfig": {
                    "networkAttachment": "projects/test-project/regions/us-central1/networkAttachments/na",
                },
                "resourceLimits": { "cpu": "4", "memory": "8Gi" },
                "minInstances": 1,
                "maxInstances": 10,
                "containerConcurrency": 9,
            },
            "classMethods": default_class_methods(),
            "agentFramework": "google-adk",
            "serviceAccount": "agent-runner@test-project.iam.gserviceaccount.com",
        },
        "encryptionSpec": {
            "kmsKeyName": "projects/test-project/locations/us-central1/keyRings/kr/cryptoKeys/k",
        },
    });
    assert_eq!(serde_json::to_value(&request).unwrap(), expected);
}

/// The minimal BYOC body: no optional field leaks into the JSON.
#[test]
fn minimal_create_request_omits_every_optional_field() {
    let request = CreateReasoningEngineRequest::byoc("my-agent", "gcr.io/p/agent:latest");
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value,
        json!({
            "displayName": "my-agent",
            "spec": {
                "containerSpec": { "imageUri": "gcr.io/p/agent:latest" },
                "classMethods": default_class_methods(),
                "agentFramework": "google-adk",
            },
        }),
    );
}

#[test]
fn build_command_is_the_documented_gcloud_invocation() {
    assert_eq!(
        gcloud_build_submit_command("us-central1-docker.pkg.dev/p/agents/a:latest"),
        "gcloud builds submit --tag us-central1-docker.pkg.dev/p/agents/a:latest",
    );
}

// ── Mock-server tests for the four operations ─────────────────────────────

#[derive(Default)]
struct MockState {
    create_bodies: Vec<Value>,
    polls: usize,
    deleted: Vec<String>,
}

type SharedState = Arc<Mutex<MockState>>;

async fn start_mock(state: SharedState) -> String {
    let parent = format!("/v1beta1/projects/{PROJECT}/locations/{LOCATION}");
    let app = Router::new()
        .route(
            &format!("{parent}/reasoningEngines"),
            post(|State(state): State<SharedState>, Json(body): Json<Value>| async move {
                state.lock().await.create_bodies.push(body);
                Json(json!({ "name": operation_name(), "done": false }))
            }),
        )
        .route(
            &format!("{parent}/operations/{{op}}"),
            get(|State(state): State<SharedState>| async move {
                state.lock().await.polls += 1;
                Json(json!({
                    "name": operation_name(),
                    "done": true,
                    "response": {
                        "@type": "type.googleapis.com/google.cloud.aiplatform.v1beta1.ReasoningEngine",
                        "name": engine_name(),
                        "displayName": "my-agent",
                    },
                }))
            }),
        )
        .route(
            &format!("{parent}/reasoningEngines/{{id}}"),
            get(|| async move {
                Json(json!({
                    "name": engine_name(),
                    "displayName": "my-agent",
                    "createTime": "2025-03-01T00:00:00Z",
                }))
            })
            .delete(
                |State(state): State<SharedState>,
                 axum::extract::Path(id): axum::extract::Path<String>| async move {
                    state.lock().await.deleted.push(id);
                    Json(json!({ "name": operation_name(), "done": true }))
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

async fn build_client(endpoint: &str) -> GcpDeployClient {
    let config = GcpDeployConfig::new(PROJECT, LOCATION).with_endpoint(endpoint);
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    GcpDeployClient::with_credentials(config, credentials).expect("build test client")
}

#[tokio::test]
async fn create_posts_the_byoc_body_and_wait_polls_to_completion() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint).await;

    let request = CreateReasoningEngineRequest::byoc("my-agent", "gcr.io/p/agent:latest");
    let operation = client.create_reasoning_engine(&request).await.unwrap();
    assert!(!operation.done);

    let response = client.wait_for_operation(operation).await.unwrap().unwrap();
    assert_eq!(response["name"], engine_name());

    let captured = state.lock().await;
    assert_eq!(captured.create_bodies.len(), 1);
    assert_eq!(captured.create_bodies[0], serde_json::to_value(&request).unwrap());
    assert_eq!(captured.polls, 1);
}

#[tokio::test]
async fn get_resolves_numeric_ids() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint).await;

    let engine = client.get_reasoning_engine("4242").await.unwrap();
    assert_eq!(engine.name, engine_name());
    assert_eq!(engine.display_name, "my-agent");
    assert_eq!(engine.create_time.as_deref(), Some("2025-03-01T00:00:00Z"));
}

#[tokio::test]
async fn delete_accepts_full_resource_names() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint).await;

    let operation = client.delete_reasoning_engine(&engine_name()).await.unwrap();
    assert!(operation.done);
    assert_eq!(state.lock().await.deleted, ["4242"]);
}

#[tokio::test]
async fn foreign_operations_are_refused() {
    let state = SharedState::default();
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint).await;

    let error = client
        .poll_operation("projects/other-project/locations/us-central1/operations/1")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not belong"), "{error}");
}
