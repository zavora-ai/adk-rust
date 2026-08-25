//! Contract tests for the Skill Registry v1beta1 read-only client.
//!
//! A mock Axum server captures every request's query string and body and
//! returns fixture JSON, so the tests pin both directions of the wire
//! contract: the client sends exactly the documented GET requests and parses
//! the documented responses. Extraction safety tests craft hostile zips
//! violating each server-side validation rule and assert the matching typed
//! error.

#![cfg(feature = "vertex-skill-registry")]

use adk_skill::SkillError;
use adk_skill::registry::{
    MAX_ARCHIVE_BYTES, MAX_ENTRIES, RetrievedSkill, SkillRegistryClient, SkillRegistryConfig,
    SkillState, extract_skill_archive,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use base64::Engine as _;
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Mutex;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

// ===== Mock server =====

/// A captured request: its query params and its body length.
type CapturedRequest = (HashMap<String, String>, usize);

#[derive(Clone, Default)]
struct MockRegistryState {
    /// Captured requests keyed by op name.
    requests: Arc<Mutex<HashMap<String, Vec<CapturedRequest>>>>,
    /// Fixture responses keyed by op name.
    responses: Arc<Mutex<HashMap<String, (StatusCode, Value)>>>,
}

async fn handle_op(
    state: MockRegistryState,
    op: String,
    query: HashMap<String, String>,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    state.requests.lock().await.entry(op.clone()).or_default().push((query, body.len()));
    match state.responses.lock().await.get(&op) {
        Some((status, value)) => (*status, Json(value.clone())),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "no fixture registered" }))),
    }
}

fn op_route(
    state: &MockRegistryState,
    op: &'static str,
) -> axum::routing::MethodRouter<MockRegistryState> {
    let _ = state;
    get(
        move |State(state): State<MockRegistryState>,
              Query(query): Query<HashMap<String, String>>,
              body: Bytes| handle_op(state, op.to_string(), query, body),
    )
}

async fn test_client() -> (MockRegistryState, SkillRegistryClient, tokio::task::JoinHandle<()>) {
    let state = MockRegistryState::default();
    let parent = "/v1beta1/projects/test-project/locations/us-central1";
    let app = Router::new()
        .route(&format!("{parent}/skills"), op_route(&state, "list"))
        // `skills:retrieve` is a single path segment; the static `skills`
        // segment above takes priority for plain list requests.
        .route(&format!("{parent}/{{skills_verb}}"), op_route(&state, "retrieve"))
        .route(&format!("{parent}/skills/{{skill}}"), op_route(&state, "get"))
        .route(&format!("{parent}/skills/{{skill}}/revisions"), op_route(&state, "list_revisions"))
        .route(
            &format!("{parent}/skills/{{skill}}/revisions/{{revision}}"),
            op_route(&state, "get_revision"),
        )
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock skill registry server should run");
    });

    let config = SkillRegistryConfig::new("test-project", "us-central1")
        .with_endpoint(format!("http://{addr}"));
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    let client = SkillRegistryClient::with_credentials(config, credentials)
        .expect("build test skill registry client");

    (state, client, server)
}

async fn register_fixture(state: &MockRegistryState, op: &str, status: StatusCode, body: Value) {
    state.responses.lock().await.insert(op.to_string(), (status, body));
}

async fn captured_requests(state: &MockRegistryState, op: &str) -> Vec<CapturedRequest> {
    state.requests.lock().await.get(op).cloned().unwrap_or_default()
}

// ===== Zip fixtures =====

const SKILL_MD: &str = "---\nname: demo-skill\ndescription: A demo skill for contract tests.\nlicense: Apache-2.0\n---\nUse the demo tool wisely.\n";

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        writer.start_file(*name, SimpleFileOptions::default()).expect("start zip entry");
        writer.write_all(data).expect("write zip entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

fn skill_fixture_json(zip_bytes: &[u8]) -> Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(zip_bytes);
    let digest = format!("{:x}", Sha256::digest(zip_bytes));
    json!({
        "name": "projects/test-project/locations/us-central1/skills/demo-skill",
        "displayName": "demo-skill",
        "description": "A demo skill for contract tests.",
        "license": "Apache-2.0",
        "zippedFilesystem": encoded,
        "state": "ACTIVE",
        "labels": { "env": "test" },
        "sha256": digest,
        "skillSource": "USER",
        "createTime": "2026-01-01T00:00:00Z",
        "updateTime": "2026-01-02T00:00:00Z",
    })
}

// ===== Wire contract =====

#[tokio::test]
async fn test_get_skill_parses_full_resource_including_payload() {
    let (state, client, server) = test_client().await;
    let zip_bytes = build_zip(&[("SKILL.md", SKILL_MD.as_bytes())]);
    register_fixture(&state, "get", StatusCode::OK, skill_fixture_json(&zip_bytes)).await;

    let skill = client.get_skill("demo-skill").await.expect("get should succeed");

    let requests = captured_requests(&state, "get").await;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].0.is_empty(), "get sends no query params: {:?}", requests[0].0);

    assert_eq!(skill.name, "projects/test-project/locations/us-central1/skills/demo-skill");
    assert_eq!(skill.display_name, "demo-skill");
    assert_eq!(skill.state, Some(SkillState::Active));
    assert_eq!(skill.labels.get("env").map(String::as_str), Some("test"));
    assert!(skill.zipped_filesystem.is_some(), "get returns the payload");
    assert!(skill.sha256.is_some());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_list_skills_sends_pagination_and_tolerates_elided_payload() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "list",
        StatusCode::OK,
        json!({
            "skills": [
                {
                    "name": "projects/test-project/locations/us-central1/skills/demo-skill",
                    "displayName": "demo-skill",
                    "description": "A demo skill.",
                    "state": "ACTIVE",
                },
            ],
            "nextPageToken": "page-2",
        }),
    )
    .await;

    let response = client.list_skills(Some(25), Some("page-1")).await.expect("list should succeed");

    let requests = captured_requests(&state, "list").await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].0,
        HashMap::from([
            ("pageSize".to_string(), "25".to_string()),
            ("pageToken".to_string(), "page-1".to_string()),
        ]),
    );

    assert_eq!(response.skills.len(), 1);
    // zippedFilesystem may be elided on list responses.
    assert!(response.skills[0].zipped_filesystem.is_none());
    assert_eq!(response.next_page_token.as_deref(), Some("page-2"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_search_skills_sends_query_params_with_empty_body_and_keeps_order() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "retrieve",
        StatusCode::OK,
        json!({
            "retrievedSkills": [
                {
                    "skillName": "projects/test-project/locations/us-central1/skills/first",
                    "description": "Best match.",
                },
                {
                    "skillName": "projects/test-project/locations/us-central1/skills/second",
                    "description": "Second match.",
                },
            ],
        }),
    )
    .await;

    let results =
        client.search_skills("data analysis", Some(2)).await.expect("search should succeed");

    let requests = captured_requests(&state, "retrieve").await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].0,
        HashMap::from([
            ("query".to_string(), "data analysis".to_string()),
            ("topK".to_string(), "2".to_string()),
        ]),
    );
    // skills:retrieve takes an EMPTY request body.
    assert_eq!(requests[0].1, 0, "retrieve must send no body");

    // No scores on the wire: ranking is array order.
    assert_eq!(
        results,
        vec![
            RetrievedSkill {
                skill_name: "projects/test-project/locations/us-central1/skills/first".to_string(),
                description: "Best match.".to_string(),
            },
            RetrievedSkill {
                skill_name: "projects/test-project/locations/us-central1/skills/second".to_string(),
                description: "Second match.".to_string(),
            },
        ],
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_search_skills_rejects_top_k_over_the_documented_maximum() {
    let (state, client, server) = test_client().await;

    let error = client.search_skills("q", Some(101)).await.expect_err("topK over 100 must fail");
    assert_eq!(error.code, "skill.registry.invalid_input");
    assert!(captured_requests(&state, "retrieve").await.is_empty(), "no request must be sent");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_list_skill_revisions_sends_filter_and_tolerates_undocumented_fields() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "list_revisions",
        StatusCode::OK,
        json!({
            "skillRevisions": [
                {
                    "name": "projects/test-project/locations/us-central1/skills/demo-skill/revisions/2",
                    "createTime": "2026-01-02T00:00:00Z",
                    "state": "ACTIVE",
                    // Undocumented field observed in real responses.
                    "updateTime": "2026-01-02T00:00:00Z",
                },
            ],
            "nextPageToken": "rev-page-2",
        }),
    )
    .await;

    let response = client
        .list_skill_revisions("demo-skill", Some(10), Some("rev-page-1"), Some("labels.env=test"))
        .await
        .expect("list revisions should succeed");

    let requests = captured_requests(&state, "list_revisions").await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].0,
        HashMap::from([
            ("pageSize".to_string(), "10".to_string()),
            ("pageToken".to_string(), "rev-page-1".to_string()),
            ("filter".to_string(), "labels.env=test".to_string()),
        ]),
    );

    assert_eq!(response.skill_revisions.len(), 1);
    assert_eq!(response.skill_revisions[0].state, Some(SkillState::Active));
    // The embedded skill snapshot is optional.
    assert!(response.skill_revisions[0].skill.is_none());
    assert_eq!(response.next_page_token.as_deref(), Some("rev-page-2"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_get_skill_revision_parses_embedded_snapshot() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "get_revision",
        StatusCode::OK,
        json!({
            "name": "projects/test-project/locations/us-central1/skills/demo-skill/revisions/1",
            "createTime": "2026-01-01T00:00:00Z",
            "state": "ACTIVE",
            "skill": {
                "name": "projects/test-project/locations/us-central1/skills/demo-skill",
                "displayName": "demo-skill",
                "description": "A demo skill.",
                // zippedFilesystem population on revisions is undocumented;
                // this fixture elides it.
            },
        }),
    )
    .await;

    let revision =
        client.get_skill_revision("demo-skill", "1").await.expect("get revision should succeed");

    assert_eq!(
        revision.name,
        "projects/test-project/locations/us-central1/skills/demo-skill/revisions/1",
    );
    let snapshot = revision.skill.expect("fixture embeds a snapshot");
    assert_eq!(snapshot.display_name, "demo-skill");
    assert!(snapshot.zipped_filesystem.is_none());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_fetch_skill_content_roundtrips_payload_with_sha256_verification() {
    let (state, client, server) = test_client().await;
    let zip_bytes = build_zip(&[
        ("SKILL.md", SKILL_MD.as_bytes()),
        ("references/data.json", br#"{"answer": 42}"#),
    ]);
    register_fixture(&state, "get", StatusCode::OK, skill_fixture_json(&zip_bytes)).await;

    let content = client.fetch_skill_content("demo-skill").await.expect("fetch should succeed");

    // The base64 payload is consumed into the extracted file map.
    assert!(content.skill.zipped_filesystem.is_none());
    assert_eq!(content.sha256, format!("{:x}", Sha256::digest(&zip_bytes)));
    assert_eq!(content.files.keys().collect::<Vec<_>>(), vec!["SKILL.md", "references/data.json"],);
    assert_eq!(content.files["references/data.json"], br#"{"answer": 42}"#.to_vec());

    // The SKILL.md bytes feed the existing parser path unchanged.
    let skill_md = content.skill_md().expect("SKILL.md at the archive root");
    let parsed = adk_skill::parse_skill_markdown(
        std::path::Path::new("SKILL.md"),
        std::str::from_utf8(skill_md).expect("SKILL.md is UTF-8"),
    )
    .expect("registry SKILL.md parses through the standard path");
    assert_eq!(parsed.name, "demo-skill");
    assert_eq!(parsed.license.as_deref(), Some("Apache-2.0"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_fetch_skill_content_rejects_sha256_mismatch() {
    let (state, client, server) = test_client().await;
    let zip_bytes = build_zip(&[("SKILL.md", SKILL_MD.as_bytes())]);
    let mut fixture = skill_fixture_json(&zip_bytes);
    fixture["sha256"] = json!("0".repeat(64));
    register_fixture(&state, "get", StatusCode::OK, fixture).await;

    let error =
        client.fetch_skill_content("demo-skill").await.expect_err("digest mismatch must fail");
    assert_eq!(error.code, "skill.registry.checksum_mismatch");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_fetch_skill_content_rejects_missing_and_undecodable_payloads() {
    let (state, client, server) = test_client().await;
    let mut fixture = skill_fixture_json(&build_zip(&[("SKILL.md", SKILL_MD.as_bytes())]));
    fixture["zippedFilesystem"] = Value::Null;
    register_fixture(&state, "get", StatusCode::OK, fixture.clone()).await;

    let error =
        client.fetch_skill_content("demo-skill").await.expect_err("missing payload must fail");
    assert_eq!(error.code, "skill.registry.invalid_response");

    fixture["zippedFilesystem"] = json!("not-valid-base64!!!");
    register_fixture(&state, "get", StatusCode::OK, fixture).await;
    let error =
        client.fetch_skill_content("demo-skill").await.expect_err("undecodable payload must fail");
    assert_eq!(error.code, "skill.registry.payload_decode");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_upstream_error_statuses_map_to_adk_error_categories() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "get",
        StatusCode::NOT_FOUND,
        json!({ "error": { "code": 404, "message": "skill not found" } }),
    )
    .await;

    let error = client.get_skill("missing-skill").await.expect_err("404 must surface as an error");
    assert!(error.is_not_found(), "unexpected error: {error:?}");
    assert_eq!(error.details.upstream_status_code, Some(404));

    server.abort();
    let _ = server.await;
}

// ===== Extraction safety =====

#[test]
fn test_extract_rejects_path_traversal() {
    let bytes = build_zip(&[("../evil.txt", b"boom")]);
    let error = extract_skill_archive(&bytes).expect_err("traversal must be rejected");
    assert!(
        matches!(&error, SkillError::ArchivePathTraversal { name } if name == "../evil.txt"),
        "unexpected error: {error}",
    );
}

#[test]
fn test_extract_rejects_nested_path_traversal() {
    let bytes = build_zip(&[("safe/../../evil.txt", b"boom")]);
    let error = extract_skill_archive(&bytes).expect_err("nested traversal must be rejected");
    assert!(matches!(error, SkillError::ArchivePathTraversal { .. }), "unexpected error: {error}");
}

#[test]
fn test_extract_rejects_absolute_paths() {
    for name in ["/etc/passwd", r"\windows\system32"] {
        let bytes = build_zip(&[(name, b"boom")]);
        let error = extract_skill_archive(&bytes).expect_err("absolute path must be rejected");
        assert!(
            matches!(error, SkillError::ArchiveAbsolutePath { .. }),
            "unexpected error for `{name}`: {error}",
        );
    }
}

#[test]
fn test_extract_rejects_symlinks() {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer.start_file("SKILL.md", SimpleFileOptions::default()).expect("start entry");
    writer.write_all(SKILL_MD.as_bytes()).expect("write entry");
    writer
        .add_symlink("link.md", "/etc/passwd", SimpleFileOptions::default())
        .expect("add symlink");
    let bytes = writer.finish().expect("finish zip").into_inner();

    let error = extract_skill_archive(&bytes).expect_err("symlink must be rejected");
    assert!(
        matches!(&error, SkillError::ArchiveSymlink { name } if name == "link.md"),
        "unexpected error: {error}",
    );
}

#[test]
fn test_extract_rejects_too_many_entries() {
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for index in 0..=MAX_ENTRIES {
        writer.start_file(format!("f{index}"), stored).expect("start entry");
    }
    let bytes = writer.finish().expect("finish zip").into_inner();

    let error = extract_skill_archive(&bytes).expect_err("entry count must be rejected");
    assert!(
        matches!(
            error,
            SkillError::ArchiveTooManyEntries { count, limit }
                if count == MAX_ENTRIES + 1 && limit == MAX_ENTRIES
        ),
        "unexpected error: {error}",
    );
}

#[test]
fn test_extract_rejects_excessive_directory_depth() {
    // Nine directory levels for a file entry; the limit is eight.
    let bytes = build_zip(&[("a/b/c/d/e/f/g/h/i/file.txt", b"deep")]);
    let error = extract_skill_archive(&bytes).expect_err("depth must be rejected");
    assert!(
        matches!(error, SkillError::ArchiveDepthExceeded { depth: 9, limit: 8, .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn test_extract_accepts_the_maximum_directory_depth() {
    let bytes = build_zip(&[("a/b/c/d/e/f/g/h/file.txt", b"ok"), ("SKILL.md", b"x")]);
    let files = extract_skill_archive(&bytes).expect("eight levels are allowed");
    assert_eq!(files.len(), 2);
}

#[test]
fn test_extract_rejects_duplicate_entry_names() {
    // ZipWriter refuses duplicate names, so forge them: write two entries
    // with same-length names, then byte-patch both names (in the local
    // headers and the central directory) to the same value.
    let mut bytes = build_zip(&[("dup0.txt", b"first"), ("dup1.txt", b"second")]);
    replace_all(&mut bytes, b"dup0.txt", b"dupX.txt");
    replace_all(&mut bytes, b"dup1.txt", b"dupX.txt");

    let error = extract_skill_archive(&bytes).expect_err("duplicates must be rejected");
    assert!(
        matches!(&error, SkillError::ArchiveDuplicateEntry { name } if name == "dupX.txt"),
        "unexpected error: {error}",
    );
}

/// Replaces every occurrence of `from` with the same-length `to` in place.
fn replace_all(bytes: &mut [u8], from: &[u8], to: &[u8]) {
    assert_eq!(from.len(), to.len(), "replacement must preserve offsets");
    let mut start = 0;
    while start + from.len() <= bytes.len() {
        if &bytes[start..start + from.len()] == from {
            bytes[start..start + from.len()].copy_from_slice(to);
        }
        start += 1;
    }
}

#[test]
fn test_extract_rejects_excessive_compression_ratio() {
    // 5 MB of zeros deflates to a few KB — a ratio far beyond 100.
    let bytes = build_zip(&[("zeros.bin", vec![0u8; 5 * 1024 * 1024].as_slice())]);
    let error = extract_skill_archive(&bytes).expect_err("zip bomb must be rejected");
    assert!(
        matches!(error, SkillError::ArchiveCompressionRatio { limit: 100, .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn test_extract_rejects_uncompressed_total_over_the_limit() {
    // Six 90 MB entries (540 MB total) built from 1 KB of pseudo-random
    // bytes followed by 89 KB of zeros per unit: each entry's own ratio
    // stays under 100, the archive stays under 10 MB, and the declared
    // total crosses 500 MB — so the size rule is what trips.
    let mut unit = pseudo_random_bytes(1024);
    unit.resize(90 * 1024, 0);
    let entry: Vec<u8> = unit.iter().copied().cycle().take(90 * 1024 * 1024).collect();

    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for index in 0..6 {
        writer
            .start_file(format!("bulk{index}.bin"), SimpleFileOptions::default())
            .expect("start entry");
        writer.write_all(&entry).expect("write entry");
    }
    let bytes = writer.finish().expect("finish zip").into_inner();
    assert!(bytes.len() as u64 <= MAX_ARCHIVE_BYTES, "fixture archive must stay under 10 MB");

    let error = extract_skill_archive(&bytes).expect_err("uncompressed total must be rejected");
    assert!(
        matches!(
            error,
            SkillError::ArchiveUncompressedTooLarge { total, limit }
                if total > limit && limit == 500 * 1024 * 1024
        ),
        "unexpected error: {error}",
    );
}

#[test]
fn test_extract_rejects_oversized_archives_before_parsing() {
    let bytes = vec![0u8; usize::try_from(MAX_ARCHIVE_BYTES).unwrap() + 1];
    let error = extract_skill_archive(&bytes).expect_err("oversized archive must be rejected");
    assert!(
        matches!(
            error,
            SkillError::ArchiveTooLarge { size, limit }
                if size == MAX_ARCHIVE_BYTES + 1 && limit == MAX_ARCHIVE_BYTES
        ),
        "unexpected error: {error}",
    );
}

#[test]
fn test_extract_rejects_garbage_bytes_as_invalid_format() {
    let error = extract_skill_archive(b"not a zip archive").expect_err("garbage must be rejected");
    assert!(matches!(error, SkillError::ArchiveFormat { .. }), "unexpected error: {error}");
}

#[test]
fn test_extract_to_dir_writes_validated_files_only() {
    let bytes = build_zip(&[
        ("SKILL.md", SKILL_MD.as_bytes()),
        ("references/data.json", br#"{"answer": 42}"#),
    ]);
    let dir = tempfile::tempdir().expect("create temp dir");

    let written = adk_skill::registry::extract_skill_archive_to_dir(&bytes, dir.path())
        .expect("extraction to dir should succeed");

    assert_eq!(written, vec![dir.path().join("SKILL.md"), dir.path().join("references/data.json")],);
    assert_eq!(std::fs::read(dir.path().join("SKILL.md")).unwrap(), SKILL_MD.as_bytes());
}

/// Deterministic incompressible-ish bytes without a rand dependency.
fn pseudo_random_bytes(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        bytes.extend_from_slice(&state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
    }
    bytes.truncate(len);
    bytes
}

// ===== Live (ignored) =====

/// Live read-only test against a real Skill Registry.
///
/// Requires ADC (`gcloud auth application-default login`) and:
///
/// - `GOOGLE_CLOUD_PROJECT` — the Google Cloud project ID
/// - `GOOGLE_CLOUD_LOCATION` — `us-central1`, `europe-west4`, or `us-east5`
/// - `VERTEX_SKILL_NAME` — a skill ID or full resource name to read
///
/// Never creates, updates, or deletes skills.
#[tokio::test]
#[ignore = "requires ADC credentials and a provisioned skill (GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, VERTEX_SKILL_NAME)"]
async fn skill_registry_live_read_only_roundtrip() {
    let skill_name =
        std::env::var("VERTEX_SKILL_NAME").expect("VERTEX_SKILL_NAME must name an existing skill");
    let config = SkillRegistryConfig::from_env().expect("skill registry env vars must be set");
    let client = SkillRegistryClient::new_with_adc(config).expect("build ADC client");

    let listed = client.list_skills(Some(10), None).await.expect("list should succeed");
    assert!(!listed.skills.is_empty(), "expected at least one skill in the registry");

    let skill = client.get_skill(&skill_name).await.expect("get should succeed");
    assert!(!skill.display_name.is_empty());

    let revisions = client
        .list_skill_revisions(&skill_name, Some(5), None, None)
        .await
        .expect("list revisions should succeed");
    if let Some(revision) = revisions.skill_revisions.first() {
        let revision_id =
            revision.name.rsplit('/').next().expect("revision names end in an ID").to_string();
        client
            .get_skill_revision(&skill_name, &revision_id)
            .await
            .expect("get revision should succeed");
    }

    let results =
        client.search_skills(&skill.description, Some(5)).await.expect("search should succeed");
    assert!(results.len() <= 5);

    let content = client.fetch_skill_content(&skill_name).await.expect("fetch should succeed");
    assert!(content.skill_md().is_some(), "expected SKILL.md at the archive root");
}
