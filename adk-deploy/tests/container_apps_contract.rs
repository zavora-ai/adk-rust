//! Contract tests for the Azure Container Apps target.
//!
//! The `PUT` body is pinned against the `Microsoft.App/containerApps` shape in the Azure
//! Resource Manager REST reference; responses are recorded fixtures transcribed from its
//! examples. Every request carries the bearer token and the `api-version` query parameter.

#![cfg(feature = "azure")]

use std::path::Path;
use std::time::Duration;

use adk_deploy::DeploymentManifest;
use adk_deploy::azure::{ContainerAppsClient, ContainerAppsTarget, ContainerAppsTargetConfig};
use adk_deploy::targets::{DeployRequest, DeployTarget, DeploymentOutcome, DeploymentStatus};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const IMAGE: &str = "acme.azurecr.io/support-desk:1.4.0";
const SUBSCRIPTION: &str = "/subscriptions/00000000-0000-0000-0000-000000000000";
const APP: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/agents/providers/Microsoft.App/containerApps/support-desk";
const IDENTITY: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/agents/providers/Microsoft.ManagedIdentity/userAssignedIdentities/support-desk";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name))
        .unwrap()
}

fn manifest() -> DeploymentManifest {
    toml::from_str(&fixture("azure-container-apps.toml")).unwrap()
}

fn target(server: &MockServer, manifest: &DeploymentManifest) -> ContainerAppsTarget {
    let config: ContainerAppsTargetConfig =
        manifest.target_config(ContainerAppsTarget::NAME).unwrap();
    let client = ContainerAppsClient::with_static_token("test-arm-token")
        .unwrap()
        .with_endpoint(&server.uri())
        .unwrap();
    ContainerAppsTarget::with_client(client, config)
        .with_polling(Duration::from_secs(5), Duration::from_millis(10))
}

fn arm(verb: &str) -> wiremock::MockBuilder {
    Mock::given(method(verb))
        .and(path(APP))
        .and(query_param("api-version", "2024-03-01"))
        .and(header("authorization", "Bearer test-arm-token"))
}

fn json_response(status: u16, name: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(fixture(name), "application/json")
}

fn expected_body() -> Value {
    let probe = |kind: &str| {
        json!({
            "type": kind,
            "httpGet": { "path": "/api/health", "port": 8080 },
            "periodSeconds": 10,
            "timeoutSeconds": 3,
            "failureThreshold": 3,
        })
    };
    let secret = |name: &str, vault_name: &str| {
        json!({
            "name": name,
            "keyVaultUrl": format!("https://acme-agents.vault.azure.net/secrets/{vault_name}"),
            "identity": IDENTITY,
        })
    };
    json!({
        "location": "eastus",
        "identity": { "type": "UserAssigned", "userAssignedIdentities": { IDENTITY: {} } },
        "tags": { "adk-agent": "support-desk", "adk-version": "1.4.0", "managed-by": "adk-deploy" },
        "properties": {
            "environmentId": format!("{SUBSCRIPTION}/resourceGroups/agents/providers/Microsoft.App/managedEnvironments/agents"),
            "configuration": {
                "activeRevisionsMode": "Single",
                "ingress": { "external": true, "targetPort": 8080, "transport": "auto", "allowInsecure": false },
                "secrets": [
                    secret("adk-api-token", "adk-api-token"),
                    secret("anthropic-api-key", "anthropic-api-key"),
                    secret("database-url", "support-desk-database-url"),
                ],
                "registries": [ { "server": "acme.azurecr.io", "identity": IDENTITY } ],
            },
            "template": {
                "containers": [{
                    "name": "agent",
                    "image": IMAGE,
                    "env": [
                        { "name": "ADK_AGENT_NAME", "value": "support-desk" },
                        { "name": "ADK_AGENT_VERSION", "value": "1.4.0" },
                        { "name": "ADK_ALLOWED_HOSTS", "value": "support.example.com,.azurecontainerapps.io" },
                        { "name": "ADK_AUTH_MODE", "value": "bearer" },
                        { "name": "ADK_AUTH_REQUIRED_SCOPES", "value": "agent:invoke" },
                        { "name": "ADK_DEPLOY_TARGET", "value": "azure-container-apps" },
                        { "name": "BIND_HOST", "value": "0.0.0.0" },
                        { "name": "PORT", "value": "8080" },
                        { "name": "RUST_LOG", "value": "info" },
                        { "name": "ADK_API_TOKEN", "secretRef": "adk-api-token" },
                        { "name": "ADK_SERVICE_SESSIONS_URL", "secretRef": "database-url" },
                        { "name": "ANTHROPIC_API_KEY", "secretRef": "anthropic-api-key" },
                    ],
                    "probes": [probe("Liveness"), probe("Readiness")],
                    "resources": { "cpu": 0.5, "memory": "1Gi" },
                }],
                "scale": {
                    "minReplicas": 1,
                    "maxReplicas": 5,
                    "rules": [ { "name": "http-concurrency", "http": { "metadata": { "concurrentRequests": "40" } } } ],
                },
            },
        },
    })
}

#[tokio::test]
async fn plan_renders_the_container_app_in_the_reference_shape() {
    let server = MockServer::start().await;
    let manifest = manifest();
    let plan = target(&server, &manifest).plan(&DeployRequest::new(manifest, IMAGE)).unwrap();
    assert_eq!(plan.resources.len(), 1);
    assert_eq!(plan.resources[0].kind, "arm:PUT Microsoft.App/containerApps");
    assert_eq!(plan.resources[0].name, APP);
    assert_eq!(serde_json::from_str::<Value>(&plan.resources[0].body).unwrap(), expected_body());
    assert!(server.received_requests().await.unwrap().is_empty(), "planning contacts nothing");
}

#[tokio::test]
async fn deploy_puts_the_app_and_waits_until_it_runs() {
    let server = MockServer::start().await;
    arm("PUT")
        .respond_with(json_response(201, "container_apps/put-created.json"))
        .expect(1)
        .mount(&server)
        .await;
    arm("GET")
        .respond_with(json_response(200, "container_apps/put-created.json"))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    arm("GET")
        .respond_with(json_response(200, "container_apps/get-succeeded.json"))
        .with_priority(2)
        .mount(&server)
        .await;

    let manifest = manifest();
    let outcome =
        target(&server, &manifest).deploy(&DeployRequest::new(manifest, IMAGE)).await.unwrap();
    assert_eq!(
        outcome,
        DeploymentOutcome::new(
            "azure-container-apps",
            APP,
            Some(
                "https://support-desk.happyhill-70162bb9.eastus.azurecontainerapps.io".to_string()
            ),
            DeploymentStatus::Ready,
        )
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4, "the PUT, two provisioning reads, one succeeded read");
    assert_eq!(serde_json::from_slice::<Value>(&requests[0].body).unwrap(), expected_body());
    assert_eq!(requests[0].headers.get("content-type").unwrap(), "application/json");
}

#[tokio::test]
async fn status_maps_provisioning_state_and_absence() {
    let server = MockServer::start().await;
    arm("GET")
        .respond_with(json_response(200, "container_apps/get-failed.json"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    arm("GET")
        .respond_with(json_response(404, "container_apps/not-found.json"))
        .with_priority(2)
        .mount(&server)
        .await;
    let target = target(&server, &manifest());
    assert_eq!(
        target.status(APP).await.unwrap(),
        DeploymentStatus::Failed { reason: "provisioning failed".to_string() }
    );
    assert_eq!(target.status(APP).await.unwrap(), DeploymentStatus::NotFound);
}

#[tokio::test]
async fn destroy_deletes_the_app() {
    let server = MockServer::start().await;
    arm("DELETE").respond_with(ResponseTemplate::new(202)).expect(1).mount(&server).await;
    target(&server, &manifest()).destroy(APP).await.unwrap();
}

#[tokio::test]
async fn arm_errors_carry_the_code_and_message() {
    let server = MockServer::start().await;
    arm("PUT")
        .respond_with(json_response(403, "container_apps/authorization-failed.json"))
        .mount(&server)
        .await;
    let manifest = manifest();
    let error =
        target(&server, &manifest).deploy(&DeployRequest::new(manifest, IMAGE)).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "control-plane request failed: Azure Resource Manager PUT failed (403): AuthorizationFailed: \
         The client 'ci@acme.example' does not have authorization to perform action \
         'Microsoft.App/containerApps/write' over scope \
         '/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/agents'."
    );
}

#[tokio::test]
async fn incomplete_settings_are_refused_before_any_request() {
    let server = MockServer::start().await;
    let refused = |edit: fn(&mut DeploymentManifest)| {
        let mut manifest = manifest();
        edit(&mut manifest);
        target(&server, &manifest)
            .plan(&DeployRequest::new(manifest.clone(), IMAGE))
            .unwrap_err()
            .to_string()
    };
    let error = refused(|m| {
        m.targets
            .get_mut("azure-container-apps")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("identity");
    });
    assert!(error.contains("needs identity"), "{error}");
    let error = refused(|m| {
        m.targets
            .get_mut("azure-container-apps")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("keyVaultUrl");
    });
    assert!(error.contains("needs keyVaultUrl"), "{error}");
    let error = refused(|m| {
        m.targets
            .get_mut("azure-container-apps")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("environmentId");
    });
    assert!(error.contains("needs environmentId"), "{error}");
    let error = refused(|m| m.auth = None);
    assert!(error.contains("exposes the agent publicly, so callers must authenticate"), "{error}");
    let error = refused(|m| m.health.failure_threshold = 30);
    assert!(error.contains("failureThreshold from 1 to 10"), "{error}");
    let error = refused(|m| {
        m.services[0].kind = adk_deploy::ServiceKind::Sqlite;
        m.scaling.min_instances = 1;
        m.scaling.max_instances = 1;
    });
    assert!(error.contains("no persistent block volume for SQLite"), "{error}");

    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn internal_ingress_needs_no_auth_and_is_not_external() {
    let mut manifest = manifest();
    manifest.auth = None;
    manifest.targets.get_mut("azure-container-apps").unwrap()["ingress"] = json!("internal");
    let config: ContainerAppsTargetConfig =
        manifest.target_config(ContainerAppsTarget::NAME).unwrap();
    let client = ContainerAppsClient::with_static_token("t").unwrap();
    let plan = ContainerAppsTarget::with_client(client, config)
        .plan(&DeployRequest::new(manifest, IMAGE))
        .unwrap();
    let body: Value = serde_json::from_str(&plan.resources[0].body).unwrap();
    assert_eq!(body["properties"]["configuration"]["ingress"]["external"], false);
}
