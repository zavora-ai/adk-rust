//! Registry skill loading, merging, and the search tool.
//!
//! The golden-equivalence test pins the core guarantee of the registry
//! loader: a skill package served by the registry produces a
//! [`SkillDocument`] identical to loading the same `SKILL.md` from disk —
//! every content-derived field byte-equal — so the whole downstream skill
//! runtime (injection, selection, coordination) works unchanged.

#![cfg(feature = "vertex-skill-registry")]

use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Tool, ToolContext,
};
use adk_skill::registry::{
    RegistrySkillFilter, SkillRegistryClient, SkillRegistryConfig, SkillSearchTool,
    load_skill_index_from_registry, merge_skill_indexes,
};
use adk_skill::{SkillIndex, load_skill_index};
use async_trait::async_trait;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Mutex;

const PARENT: &str = "projects/test-project/locations/us-central1";

const GOLDEN_SKILL_MD: &str = r#"---
name: golden-skill
description: Writes quarterly reports from raw metrics.
version: "1.2.0"
license: Apache-2.0
compatibility: "Requires network access"
tags:
  - reporting
  - finance
allowed-tools:
  - web_search
references:
  - references/data.json
trigger: true
hint: "Provide the metrics CSV"
metadata:
  category: analytics
triggers:
  - "*.csv"
---
Load the metrics, aggregate by quarter, and draft the report.
"#;

// ===== Mock registry =====

/// Fixture responses keyed by route (`skill:{id}`, `retrieve`,
/// `revision:{id}:{rev}`).
type Fixtures = Arc<Mutex<HashMap<String, Value>>>;

async fn respond(fixtures: &Fixtures, key: &str) -> (StatusCode, Json<Value>) {
    match fixtures.lock().await.get(key) {
        Some(value) => (StatusCode::OK, Json(value.clone())),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "no fixture" } }))),
    }
}

async fn serve_mock(fixtures: Fixtures) -> (String, tokio::task::JoinHandle<()>) {
    let prefix = format!("/v1beta1/{PARENT}");
    let app = Router::new()
        .route(
            &format!("{prefix}/{{skills_verb}}"),
            get(|State(fixtures): State<Fixtures>| async move {
                respond(&fixtures, "retrieve").await
            }),
        )
        .route(
            &format!("{prefix}/skills/{{skill}}"),
            get(|State(fixtures): State<Fixtures>, AxumPath(skill): AxumPath<String>| async move {
                respond(&fixtures, &format!("skill:{skill}")).await
            }),
        )
        .route(
            &format!("{prefix}/skills/{{skill}}/revisions/{{revision}}"),
            get(
                |State(fixtures): State<Fixtures>,
                 AxumPath((skill, revision)): AxumPath<(String, String)>| async move {
                    respond(&fixtures, &format!("revision:{skill}:{revision}")).await
                },
            ),
        )
        .with_state(fixtures);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock skill registry server should run");
    });
    (format!("http://{addr}"), server)
}

fn test_client(endpoint: &str) -> SkillRegistryClient {
    let config = SkillRegistryConfig::new("test-project", "us-central1").with_endpoint(endpoint);
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    SkillRegistryClient::with_credentials(config, credentials).expect("build test client")
}

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start zip entry");
        writer.write_all(data).expect("write zip entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

fn skill_json(skill_id: &str, description: &str, skill_md: &str) -> Value {
    let zip_bytes = build_zip(&[
        ("SKILL.md", skill_md.as_bytes()),
        ("references/data.json", br#"{"quarters": 4}"#),
    ]);
    json!({
        "name": format!("{PARENT}/skills/{skill_id}"),
        "displayName": skill_id,
        "description": description,
        "zippedFilesystem": base64::engine::general_purpose::STANDARD.encode(&zip_bytes),
        "sha256": format!("{:x}", Sha256::digest(&zip_bytes)),
        "state": "ACTIVE",
    })
}

// ===== Golden equivalence =====

/// WP14 acceptance: a registry-served skill produces a SkillDocument equal to
/// the same fixture loaded from disk. Provenance necessarily differs — the
/// registry document carries a virtual resource-name path and no filesystem
/// mtime — so those two fields are asserted explicitly and normalized before
/// the whole-object comparison covering every content-derived field.
#[tokio::test]
async fn registry_load_is_golden_equivalent_to_disk_load() {
    // Disk side: the same SKILL.md under .skills/.
    let temp = tempfile::tempdir().expect("create temp dir");
    std::fs::create_dir_all(temp.path().join(".skills/golden-skill")).expect("mkdir");
    std::fs::write(temp.path().join(".skills/golden-skill/SKILL.md"), GOLDEN_SKILL_MD)
        .expect("write fixture");
    let disk_index = load_skill_index(temp.path()).expect("disk load succeeds");
    assert_eq!(disk_index.len(), 1);

    // Registry side: the same SKILL.md packaged in a zip payload.
    let fixtures: Fixtures = Arc::default();
    fixtures.lock().await.insert(
        "skill:golden-skill".to_string(),
        skill_json("golden-skill", "Writes quarterly reports from raw metrics.", GOLDEN_SKILL_MD),
    );
    let (endpoint, server) = serve_mock(fixtures).await;
    let client = test_client(&endpoint);
    let registry_index =
        load_skill_index_from_registry(&client, RegistrySkillFilter::by_names(["golden-skill"]))
            .await
            .expect("registry load succeeds");
    assert_eq!(registry_index.len(), 1);

    let mut disk = serde_json::to_value(&disk_index.skills()[0]).expect("serialize disk doc");
    let mut remote =
        serde_json::to_value(&registry_index.skills()[0]).expect("serialize registry doc");

    // Provenance fields differ by design; pin their expected values first.
    assert_eq!(remote["path"], json!(format!("{PARENT}/skills/golden-skill/SKILL.md")));
    assert_eq!(remote["last_modified"], Value::Null);
    assert!(disk["last_modified"].is_number(), "disk load records the mtime");

    disk["path"] = Value::Null;
    remote["path"] = Value::Null;
    disk["last_modified"] = Value::Null;

    // Whole-object equality: id, name, description, version, license,
    // compatibility, tags, allowed_tools, references, trigger, hint,
    // metadata, body, hash, and triggers must be byte-identical.
    assert_eq!(disk, remote);

    server.abort();
    let _ = server.await;
}

// ===== Loader behavior =====

#[tokio::test]
async fn query_selector_resolves_names_through_retrieve() {
    let fixtures: Fixtures = Arc::default();
    {
        let mut lock = fixtures.lock().await;
        lock.insert(
            "retrieve".to_string(),
            json!({
                "retrievedSkills": [
                    { "skillName": format!("{PARENT}/skills/beta"), "description": "Beta." },
                    { "skillName": format!("{PARENT}/skills/alpha"), "description": "Alpha." },
                ],
            }),
        );
        lock.insert(
            "skill:alpha".to_string(),
            skill_json(
                "alpha",
                "Alpha.",
                "---\nname: alpha\ndescription: Alpha.\n---\nAlpha body.\n",
            ),
        );
        lock.insert(
            "skill:beta".to_string(),
            skill_json("beta", "Beta.", "---\nname: beta\ndescription: Beta.\n---\nBeta body.\n"),
        );
    }
    let (endpoint, server) = serve_mock(fixtures).await;
    let client = test_client(&endpoint);

    let index = load_skill_index_from_registry(
        &client,
        RegistrySkillFilter::by_query("greek letters").with_top_k(2),
    )
    .await
    .expect("query load succeeds");

    // The index is sorted by name regardless of retrieval order.
    let names: Vec<&str> = index.skills().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta"]);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn revision_pin_loads_the_embedded_snapshot() {
    let fixtures: Fixtures = Arc::default();
    fixtures.lock().await.insert(
        "revision:golden-skill:3".to_string(),
        json!({
            "name": format!("{PARENT}/skills/golden-skill/revisions/3"),
            "state": "ACTIVE",
            "skill": skill_json(
                "golden-skill",
                "Writes quarterly reports from raw metrics.",
                GOLDEN_SKILL_MD,
            ),
        }),
    );
    let (endpoint, server) = serve_mock(fixtures).await;
    let client = test_client(&endpoint);

    let index = load_skill_index_from_registry(
        &client,
        RegistrySkillFilter::by_names(["golden-skill"]).with_revision("3"),
    )
    .await
    .expect("pinned load succeeds");

    assert_eq!(index.len(), 1);
    assert_eq!(index.skills()[0].name, "golden-skill");
    assert_eq!(index.skills()[0].version.as_deref(), Some("1.2.0"));

    server.abort();
    let _ = server.await;
}

#[test]
fn merge_prefers_local_skills_on_name_collision() {
    let local_dir = tempfile::tempdir().expect("create temp dir");
    std::fs::create_dir_all(local_dir.path().join(".skills")).expect("mkdir");
    std::fs::write(
        local_dir.path().join(".skills/shared.md"),
        "---\nname: shared\ndescription: Local shared.\n---\nLocal body.",
    )
    .expect("write local shared");
    std::fs::write(
        local_dir.path().join(".skills/local-only.md"),
        "---\nname: local-only\ndescription: Local only.\n---\nBody.",
    )
    .expect("write local-only");
    let local = load_skill_index(local_dir.path()).expect("local load");

    let remote_dir = tempfile::tempdir().expect("create temp dir");
    std::fs::create_dir_all(remote_dir.path().join(".skills")).expect("mkdir");
    std::fs::write(
        remote_dir.path().join(".skills/shared.md"),
        "---\nname: shared\ndescription: Remote shared.\n---\nRemote body.",
    )
    .expect("write remote shared");
    std::fs::write(
        remote_dir.path().join(".skills/remote-only.md"),
        "---\nname: remote-only\ndescription: Remote only.\n---\nBody.",
    )
    .expect("write remote-only");
    let remote = load_skill_index(remote_dir.path()).expect("remote load");

    let merged = merge_skill_indexes(local, remote);

    let names: Vec<&str> = merged.skills().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["local-only", "remote-only", "shared"]);
    assert_eq!(merged.find_by_name("shared").expect("shared present").description, "Local shared.",);
}

#[test]
fn merge_with_empty_local_keeps_all_remote_skills() {
    let remote_dir = tempfile::tempdir().expect("create temp dir");
    std::fs::create_dir_all(remote_dir.path().join(".skills")).expect("mkdir");
    std::fs::write(
        remote_dir.path().join(".skills/only.md"),
        "---\nname: only\ndescription: Only.\n---\nBody.",
    )
    .expect("write skill");
    let remote = load_skill_index(remote_dir.path()).expect("remote load");

    let merged = merge_skill_indexes(SkillIndex::default(), remote);
    assert_eq!(merged.len(), 1);
}

// ===== Search tool =====

struct TestContext {
    content: Content,
    actions: std::sync::Mutex<EventActions>,
}

impl TestContext {
    fn new() -> Self {
        Self {
            content: Content::new("user"),
            actions: std::sync::Mutex::new(EventActions::default()),
        }
    }
}

#[async_trait]
impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str {
        "test"
    }
    fn agent_name(&self) -> &str {
        "test"
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

#[async_trait]
impl CallbackContext for TestContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for TestContext {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().expect("lock actions").clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().expect("lock actions") = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn search_tool_returns_ranked_summaries() {
    let fixtures: Fixtures = Arc::default();
    fixtures.lock().await.insert(
        "retrieve".to_string(),
        json!({
            "retrievedSkills": [
                { "skillName": format!("{PARENT}/skills/best"), "description": "Best match." },
                { "skillName": format!("{PARENT}/skills/second"), "description": "Second." },
            ],
        }),
    );
    let (endpoint, server) = serve_mock(fixtures).await;
    let tool = SkillSearchTool::new(Arc::new(test_client(&endpoint)));

    assert_eq!(tool.name(), "search_skills");
    assert!(tool.is_read_only());
    assert!(tool.is_concurrency_safe());
    let schema = tool.parameters_schema().expect("tool declares parameters");
    assert_eq!(schema["required"], json!(["query"]));

    let ctx = Arc::new(TestContext::new());
    let output = tool
        .execute(ctx.clone(), json!({ "query": "reporting", "top_k": 2 }))
        .await
        .expect("search succeeds");

    assert_eq!(
        output,
        json!([
            {
                "name": "best",
                "skillName": format!("{PARENT}/skills/best"),
                "description": "Best match.",
            },
            {
                "name": "second",
                "skillName": format!("{PARENT}/skills/second"),
                "description": "Second.",
            },
        ]),
    );

    // Missing `query` is rejected with the registry's invalid-input code.
    let error =
        tool.execute(ctx, json!({ "top_k": 1 })).await.expect_err("missing query must be rejected");
    assert_eq!(error.code, "skill.registry.invalid_input");

    server.abort();
    let _ = server.await;
}

// ===== Live (ignored) =====

/// Live read-only test against a real Skill Registry.
///
/// Requires ADC and `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, and
/// `VERTEX_SKILL_NAME`. Never creates, updates, or deletes skills.
#[tokio::test]
#[ignore = "requires ADC credentials and a provisioned skill (GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, VERTEX_SKILL_NAME)"]
async fn skill_registry_load_live_read_only() {
    let skill_name =
        std::env::var("VERTEX_SKILL_NAME").expect("VERTEX_SKILL_NAME must name an existing skill");
    let config = SkillRegistryConfig::from_env().expect("skill registry env vars must be set");
    let client = SkillRegistryClient::new_with_adc(config).expect("build ADC client");

    let index =
        load_skill_index_from_registry(&client, RegistrySkillFilter::by_names([&skill_name]))
            .await
            .expect("registry load succeeds");
    assert_eq!(index.len(), 1);
    let skill = &index.skills()[0];
    assert!(!skill.name.is_empty());
    assert!(!skill.body.is_empty());

    let tool = SkillSearchTool::new(Arc::new(client));
    let ctx = Arc::new(TestContext::new());
    let results = tool
        .execute(ctx, json!({ "query": skill.description, "top_k": 3 }))
        .await
        .expect("search succeeds");
    assert!(results.as_array().is_some_and(|hits| hits.len() <= 3));
}
