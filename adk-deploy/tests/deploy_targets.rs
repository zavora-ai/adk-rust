//! Deploy target contract: the guarantees every target enforces through `RuntimeSpec`, the
//! container target's rendered files (snapshots, re-parsed as YAML), and endpoint verification
//! against a recorded agent.
//!
//! Set `ADK_DEPLOY_UPDATE_SNAPSHOTS=1` to rewrite `tests/snapshots/` after an intended change.

use std::collections::BTreeMap;
use std::path::Path;

use adk_deploy::targets::container::ContainerTarget;
use adk_deploy::targets::{
    DeployRequest, DeployTarget, RuntimeOptions, RuntimeSpec, verify_endpoint,
};
use adk_deploy::{DeploymentManifest, HealthCheckConfig};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const IMAGE: &str = "ghcr.io/acme/support-desk:1.4.0";

fn fixture() -> DeploymentManifest {
    let raw = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/support-desk.toml"),
    )
    .unwrap();
    toml::from_str(&raw).unwrap()
}

fn minimal(extra: &str) -> DeploymentManifest {
    toml::from_str(&format!(
        r#"
        [agent]
        name = "demo"
        binary = "demo"
        {extra}
        "#
    ))
    .unwrap()
}

fn error(manifest: &DeploymentManifest, options: RuntimeOptions) -> String {
    RuntimeSpec::from_manifest(manifest, IMAGE, &options).unwrap_err().to_string()
}

const IN_MEMORY: &str = r#"
[[services]]
name = "sessions"
kind = "in-memory"
"#;

// ── Guarantees ────────────────────────────────────────────────────────────

#[test]
fn runtime_spec_carries_the_deploy_contract() {
    let runtime = RuntimeSpec::from_manifest(
        &fixture(),
        IMAGE,
        &RuntimeOptions::new("container")
            .with_allowed_hosts(vec!["support.example.com".to_string()])
            .with_public_ingress(true),
    )
    .unwrap();
    let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
    };
    let expected = json!({
        "name": "support-desk",
        "version": "1.4.0",
        "image": IMAGE,
        "port": 8080,
        "args": [],
        "env": env(&[
            ("ADK_AGENT_NAME", "support-desk"),
            ("ADK_AGENT_VERSION", "1.4.0"),
            ("ADK_ALLOWED_HOSTS", "support.example.com"),
            ("ADK_AUTH_MODE", "bearer"),
            ("ADK_AUTH_REQUIRED_SCOPES", "agent:invoke"),
            ("ADK_DEPLOY_TARGET", "container"),
            ("BIND_HOST", "0.0.0.0"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otel-collector:4317"),
            ("OTEL_SERVICE_NAME", "support-desk"),
            ("PORT", "8080"),
            ("RUST_LOG", "info"),
        ]),
        "secretEnv": env(&[
            ("ADK_API_TOKEN", "ADK_API_TOKEN"),
            ("ADK_SERVICE_SESSIONS_URL", "DATABASE_URL"),
            ("ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY"),
        ]),
        "health": HealthCheckConfig {
            path: "/api/health".to_string(),
            interval_secs: 10,
            timeout_secs: 3,
            failure_threshold: 3,
        },
        "scaling": fixture().scaling,
        "public": true,
        "authRequired": true,
        "warnings": [],
    });
    assert_eq!(serde_json::to_value(&runtime).unwrap(), expected);
}

#[test]
fn a_literal_secret_is_refused() {
    let manifest = minimal(&format!("{IN_MEMORY}\n[env]\nANTHROPIC_API_KEY = \"sk-ant-literal\""));
    assert_eq!(
        error(&manifest, RuntimeOptions::new("container")),
        "invalid deployment manifest: env 'ANTHROPIC_API_KEY' holds a literal value under a \
         secret-bearing name; declare [[secrets]] key = \"ANTHROPIC_API_KEY\" and set \
         ANTHROPIC_API_KEY = { secretRef = \"ANTHROPIC_API_KEY\" }"
    );
}

#[test]
fn a_password_in_a_connection_url_is_refused() {
    let manifest = minimal(
        r#"
        [[services]]
        name = "sessions"
        kind = "postgres"
        mode = "external"
        connectionUrl = "postgres://app:hunter2@db:5432/app"
        "#,
    );
    assert!(error(&manifest, RuntimeOptions::new("container")).contains(
        "service binding 'sessions' has a password in connectionUrl; move the URL into a [[secrets]] key"
    ));
}

#[test]
fn the_session_store_must_be_declared() {
    assert!(
        error(&minimal(""), RuntimeOptions::new("container"))
            .contains("declare the session store target 'container' runs on")
    );
}

#[test]
fn managed_bindings_need_a_provider_that_supplies_them() {
    let manifest = minimal(
        r#"
        [[services]]
        name = "sessions"
        kind = "postgres"
        mode = "managed"
        "#,
    );
    assert!(error(&manifest, RuntimeOptions::new("container")).contains(
        "service binding 'sessions' is managed, and target 'container' provisions no stores"
    ));
    RuntimeSpec::from_manifest(
        &manifest,
        IMAGE,
        &RuntimeOptions::new("agent-engine").with_managed_stores(true),
    )
    .unwrap();
}

#[test]
fn sqlite_is_refused_beyond_one_replica() {
    let manifest = minimal(
        r#"
        [scaling]
        minInstances = 1
        maxInstances = 3

        [[services]]
        name = "sessions"
        kind = "sqlite"
        mode = "external"
        connectionUrl = "sqlite:///data/sessions.db"
        "#,
    );
    assert!(error(&manifest, RuntimeOptions::new("container")).contains(
        "service binding 'sessions' is SQLite, which each replica would hold its own copy of"
    ));
}

#[test]
fn public_ingress_requires_auth() {
    let manifest = minimal(IN_MEMORY);
    assert!(
        error(&manifest, RuntimeOptions::new("container").with_public_ingress(true)).contains(
            "target 'container' exposes the agent publicly, so callers must authenticate"
        )
    );
    // A provider that authenticates callers itself, such as Agent Engine, accepts it.
    RuntimeSpec::from_manifest(
        &manifest,
        IMAGE,
        &RuntimeOptions::new("agent-engine")
            .with_public_ingress(true)
            .with_provider_authentication(true),
    )
    .unwrap();
}

#[test]
fn contract_variables_cannot_be_overridden() {
    let manifest = minimal(&format!("{IN_MEMORY}\n[env]\nPORT = \"9000\""));
    assert!(
        error(&manifest, RuntimeOptions::new("container"))
            .contains("env 'PORT' is set by target 'container' from the manifest")
    );
}

#[test]
fn a_binding_secret_must_be_declared() {
    let manifest = minimal(
        r#"
        [[services]]
        name = "sessions"
        kind = "redis"
        mode = "external"
        secretRef = "REDIS_URL"
        "#,
    );
    assert!(
        error(&manifest, RuntimeOptions::new("container"))
            .contains("service binding 'sessions' references undeclared secret 'REDIS_URL'")
    );
}

#[test]
fn accepted_risks_are_reported_as_warnings() {
    let manifest = minimal(&format!("{IN_MEMORY}\n[[secrets]]\nkey = \"UNUSED_TOKEN\""));
    let runtime =
        RuntimeSpec::from_manifest(&manifest, IMAGE, &RuntimeOptions::new("container")).unwrap();
    assert_eq!(
        runtime.warnings,
        [
            "no [auth]: the agent must authenticate callers itself, or stay on a private network",
            "service binding 'sessions' keeps sessions in memory: they are lost on every restart and are not shared between replicas",
            "secret 'UNUSED_TOKEN' is declared but no [env] entry or service binding references it, so the container does not receive it",
        ]
    );
}

#[test]
fn an_unknown_target_setting_is_refused() {
    let manifest = minimal(&format!("{IN_MEMORY}\n[targets.container]\nnamespce = \"agents\""));
    let error = ContainerTarget::from_manifest(".", &manifest).unwrap_err().to_string();
    assert!(error.contains("[targets.container] is invalid: unknown field `namespce`"), "{error}");
}

// ── Container target files ────────────────────────────────────────────────

fn assert_snapshot(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/container").join(name);
    if std::env::var_os("ADK_DEPLOY_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("missing snapshot {}; run with ADK_DEPLOY_UPDATE_SNAPSHOTS=1", path.display())
    });
    assert_eq!(
        actual, expected,
        "snapshot {name} differs; rerun with ADK_DEPLOY_UPDATE_SNAPSHOTS=1 if intended"
    );
}

fn container_plan() -> adk_deploy::targets::DeployPlan {
    let manifest = fixture();
    let target = ContainerTarget::from_manifest(".", &manifest).unwrap();
    target.plan(&DeployRequest::new(manifest, IMAGE)).unwrap()
}

#[test]
fn container_files_match_their_snapshots() {
    let plan = container_plan();
    let names: Vec<&str> = plan.resources.iter().map(|resource| resource.name.as_str()).collect();
    assert_eq!(names, ["Dockerfile", "Dockerfile.dockerignore", "compose.yaml", "kubernetes.yaml"]);
    for resource in &plan.resources {
        assert_snapshot(&resource.name, &resource.body);
    }
}

#[test]
fn compose_file_parses_and_reads_secrets_from_the_environment() {
    let plan = container_plan();
    let compose: Value = serde_saphyr::from_str(&plan.resources[2].body).unwrap();
    let service = &compose["services"]["agent"];
    assert_eq!(compose["name"], "support-desk");
    assert_eq!(service["image"], IMAGE);
    assert_eq!(service["ports"], json!(["0.0.0.0:8080:8080"]));
    assert_eq!(service["read_only"], true);
    assert_eq!(
        service["environment"]["ANTHROPIC_API_KEY"],
        "${ANTHROPIC_API_KEY:?ANTHROPIC_API_KEY must be set in the environment or an --env-file}"
    );
    assert_eq!(service["environment"]["PORT"], "8080");
    assert_eq!(
        service["build"],
        json!({ "context": "../..", "dockerfile": ".adk-deploy/container/Dockerfile" })
    );
}

#[test]
fn kubernetes_documents_parse_and_wire_probes_secrets_and_scaling() {
    let plan = container_plan();
    let documents: Vec<Value> = serde_saphyr::from_multiple(&plan.resources[3].body).unwrap();
    let kinds: Vec<&str> = documents.iter().map(|doc| doc["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["Deployment", "Service", "HorizontalPodAutoscaler"]);

    let deployment = &documents[0];
    assert_eq!(deployment["metadata"]["namespace"], "agents");
    assert_eq!(deployment["spec"]["replicas"], 2);
    let container = &deployment["spec"]["template"]["spec"]["containers"][0];
    let probe = json!({
        "httpGet": { "path": "/api/health", "port": "http" },
        "periodSeconds": 10,
        "timeoutSeconds": 3,
        "failureThreshold": 3,
    });
    assert_eq!(container["readinessProbe"], probe);
    assert_eq!(container["livenessProbe"], probe);
    let env: Vec<&Value> = container["env"].as_array().unwrap().iter().collect();
    assert!(env.contains(&&json!({
        "name": "ADK_SERVICE_SESSIONS_URL",
        "valueFrom": { "secretKeyRef": { "name": "support-desk-secrets", "key": "DATABASE_URL" } },
    })));
    assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
    assert_eq!(documents[1]["spec"]["type"], "LoadBalancer");
    assert_eq!(documents[2]["spec"]["minReplicas"], 2);
    assert_eq!(documents[2]["spec"]["maxReplicas"], 6);
    assert_eq!(documents[2]["spec"]["metrics"][0]["resource"]["target"]["averageUtilization"], 65);
}

#[test]
fn a_sqlite_store_needs_a_persistent_volume() {
    let manifest = minimal(
        r#"
        [scaling]
        maxInstances = 1

        [[services]]
        name = "sessions"
        kind = "sqlite"
        mode = "external"
        connectionUrl = "sqlite:///data/sessions.db?mode=rwc"
        "#,
    );
    let target = ContainerTarget::from_manifest(".", &manifest).unwrap();
    let error = target.plan(&DeployRequest::new(manifest, "demo:0.1.0")).unwrap_err().to_string();
    assert!(
        error.contains("a SQLite session store is durable only on a persistent volume"),
        "{error}"
    );
}

#[test]
fn a_sqlite_store_gets_a_volume_and_one_replica() {
    let manifest = minimal(
        r#"
        [scaling]
        minInstances = 1
        maxInstances = 1

        [[services]]
        name = "sessions"
        kind = "sqlite"
        mode = "external"
        connectionUrl = "sqlite:///data/sessions.db?mode=rwc"

        [targets.container]
        dataDir = "/data"
        "#,
    );
    let target = ContainerTarget::from_manifest(".", &manifest).unwrap();
    let plan = target.plan(&DeployRequest::new(manifest, "demo:0.1.0")).unwrap();
    let documents: Vec<Value> = serde_saphyr::from_multiple(&plan.resources[3].body).unwrap();
    let kinds: Vec<&str> = documents.iter().map(|doc| doc["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["Deployment", "Service", "PersistentVolumeClaim"]);
    assert_eq!(documents[0]["spec"]["strategy"], json!({ "type": "Recreate" }));
    // The non-root agent owns the volume: the image creates the directory for UID 65532 and the
    // pod's fsGroup makes the claim writable.
    assert_eq!(documents[0]["spec"]["template"]["spec"]["securityContext"]["fsGroup"], 65532);
    let dockerfile = &plan.resources[0].body;
    assert!(dockerfile.contains("RUN mkdir -p /adk-data\n"), "{dockerfile}");
    assert!(
        dockerfile.contains("COPY --from=builder --chown=65532:65532 /adk-data /data\n"),
        "{dockerfile}"
    );
    assert!(dockerfile.contains("USER 65532:65532\n"), "{dockerfile}");
    let compose: Value = serde_saphyr::from_str(&plan.resources[2].body).unwrap();
    assert_eq!(compose["services"]["agent"]["volumes"], json!(["demo-data:/data"]));
    assert_eq!(compose["services"]["agent"]["ports"], json!(["127.0.0.1:8080:8080"]));
}

// ── Endpoint verification ─────────────────────────────────────────────────

async fn agent(apps_status: u16, health_status: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(
            ResponseTemplate::new(health_status).set_body_json(json!({ "status": "healthy" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/apps"))
        .respond_with(ResponseTemplate::new(apps_status))
        .mount(&server)
        .await;
    server
}

fn authenticated_runtime() -> RuntimeSpec {
    RuntimeSpec::from_manifest(&fixture(), IMAGE, &RuntimeOptions::new("container")).unwrap()
}

#[tokio::test]
async fn verification_passes_an_authenticated_healthy_agent() {
    let server = agent(401, 200).await;
    let report = verify_endpoint(&server.uri(), &authenticated_runtime()).await.unwrap();
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        json!({
            "healthStatus": 200,
            "unauthenticatedStatus": 401,
            "warnings": [],
        })
    );
}

#[tokio::test]
async fn verification_fails_an_agent_that_answers_without_credentials() {
    let server = agent(200, 200).await;
    let error = verify_endpoint(&server.uri(), &authenticated_runtime()).await.unwrap_err();
    assert!(error.to_string().contains("/api/apps answered 200 without credentials"), "{error}");
}

#[tokio::test]
async fn verification_fails_an_unhealthy_agent() {
    let server = agent(401, 503).await;
    let error = verify_endpoint(&server.uri(), &authenticated_runtime()).await.unwrap_err();
    assert!(error.to_string().contains("answered 503, not 200"), "{error}");
}
