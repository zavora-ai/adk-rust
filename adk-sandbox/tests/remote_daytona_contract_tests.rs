//! Contract tests of `DaytonaClient` against the documented Daytona request and response
//! shapes.
//!
//! Every fixture follows a published Daytona document, cited beside it:
//!
//! - control plane: <https://www.daytona.io/docs/openapi.json>
//! - toolbox: <https://www.daytona.io/docs/toolbox-openapi.json>, reached at
//!   `{toolboxProxyUrl}/{sandboxId}` (<https://www.daytona.io/docs/en/tools/api/>)
//!
//! The live test at the end runs against the real service and needs `DAYTONA_API_KEY`.
#![cfg(feature = "remote-daytona")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_sandbox::workspace::{
    CommandRequest, Manifest, ManifestEntry, SandboxClient, SessionHandle, SnapshotId,
};
use adk_sandbox::{DaytonaClient, DaytonaConfig, SandboxError};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "dtn_contract_test_key";

/// `Sandbox` (openapi.json `components.schemas.Sandbox`, required fields plus state and
/// toolbox URL). `toolbox` is the mock server's `/toolbox`, so requests exercise the real
/// `{toolboxProxyUrl}/{sandboxId}` composition.
fn sandbox_fixture(server: &MockServer, id: &str, state: &str) -> Value {
    json!({
        "id": id,
        "organizationId": "org-1",
        "name": id,
        "user": "daytona",
        "env": {},
        "labels": { "owner": "adk" },
        "public": false,
        "networkBlockAll": true,
        "kvm": false,
        "target": "us",
        "cpu": 1,
        "gpu": 0,
        "memory": 1,
        "disk": 3,
        "state": state,
        "toolboxProxyUrl": format!("{}/toolbox", server.uri())
    })
}

fn client(server: &MockServer) -> DaytonaClient {
    DaytonaClient::new(
        DaytonaConfig::new(KEY)
            .with_api_url(server.uri())
            .with_organization("org-1")
            .with_target("us")
            .with_label("owner", "adk")
            .with_network_blocked(true)
            .with_ttl(Duration::from_secs(30 * 60))
            .with_start_timeout(Duration::from_secs(10)),
    )
    .unwrap()
}

/// `POST /sandbox` (`CreateSandbox`) answering `creating`, then `GET /sandbox/{id}`
/// answering `started`.
async fn mount_create(server: &MockServer, id: &str) {
    Mock::given(method("POST"))
        .and(path("/sandbox"))
        .and(header("Authorization", format!("Bearer {KEY}").as_str()))
        .and(header("X-Daytona-Organization-ID", "org-1"))
        .and(body_json(json!({
            "target": "us",
            "labels": { "owner": "adk" },
            "networkBlockAll": true,
            "autoStopInterval": 15,
            "autoDeleteInterval": 0,
            "ttlMinutes": 30
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sandbox_fixture(server, id, "creating")),
        )
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/sandbox/{id}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sandbox_fixture(server, id, "started")),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn provision_creates_and_waits_for_started() {
    let server = MockServer::start().await;
    mount_create(&server, "dtn-1").await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    assert_eq!(handle, SessionHandle::new("dtn-1"));
    let caps = client.capabilities();
    assert!(caps.remote && caps.snapshot && caps.pause);
    assert_eq!(caps.max_lifetime, Some(Duration::from_secs(30 * 60)));
    assert_eq!(client.provider(), "daytona");
}

#[tokio::test]
async fn a_sandbox_that_fails_to_start_is_deleted_and_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sandbox"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(sandbox_fixture(&server, "dtn-bad", "creating")),
        )
        .mount(&server)
        .await;
    let mut failed = sandbox_fixture(&server, "dtn-bad", "error");
    failed["errorReason"] = json!("no capacity in region");
    Mock::given(method("GET"))
        .and(path("/sandbox/dtn-bad"))
        .respond_with(ResponseTemplate::new(200).set_body_json(failed))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/sandbox/dtn-bad"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sandbox_fixture(
            &server,
            "dtn-bad",
            "destroying",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let error = client(&server).provision(&Manifest::new(vec![])).await.unwrap_err();
    assert!(
        matches!(&error, SandboxError::ProvisionFailed { reason, .. } if reason.contains("no capacity in region")),
        "{error}"
    );
}

#[tokio::test]
async fn exec_posts_to_the_toolbox_with_cwd_envs_and_timeout() {
    let server = MockServer::start().await;
    mount_create(&server, "dtn-2").await;
    // Toolbox `POST /process/execute` (`ExecuteRequest` → `ExecuteResponse`).
    Mock::given(method("POST"))
        .and(path("/toolbox/dtn-2/process/execute"))
        .and(header("Authorization", format!("Bearer {KEY}").as_str()))
        .and(header("X-Daytona-Organization-ID", "org-1"))
        .and(body_json(json!({
            "command": "echo $GREETING; exit 2",
            "cwd": "/home/daytona/src",
            "envs": { "GREETING": "hi" },
            "timeout": 5
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "exitCode": 2, "result": "hi\n" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let output = client
        .start(&handle)
        .await
        .unwrap()
        .exec(
            CommandRequest::new("echo $GREETING; exit 2")
                .working_dir("src")
                .env("GREETING", "hi")
                .timeout(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    assert_eq!((output.stdout.as_str(), output.stderr.as_str(), output.exit_code), ("hi\n", "", 2));
    assert!(!output.timed_out);
}

#[tokio::test]
async fn an_exit_code_named_code_is_read() {
    let server = MockServer::start().await;
    mount_create(&server, "dtn-9").await;
    Mock::given(method("POST"))
        .and(path("/toolbox/dtn-9/process/execute"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "code": 3, "result": "" })))
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let output = client.start(&handle).await.unwrap().exec_command("exit 3", None).await.unwrap();
    assert_eq!(output.exit_code, 3);
}

#[tokio::test]
async fn a_toolbox_timeout_is_a_timed_out_output() {
    let server = MockServer::start().await;
    mount_create(&server, "dtn-3").await;
    // `408 Request Timeout` (`ErrorResponse`).
    Mock::given(method("POST"))
        .and(path("/toolbox/dtn-3/process/execute"))
        .respond_with(ResponseTemplate::new(408).set_body_json(json!({
            "message": "command execution timeout",
            "path": "/process/execute",
            "statusCode": 408,
            "timestamp": "2026-11-17T12:00:00Z"
        })))
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let output = client
        .start(&handle)
        .await
        .unwrap()
        .exec(CommandRequest::new("sleep 60").timeout(Duration::from_secs(1)))
        .await
        .unwrap();
    assert!(output.timed_out);
}

#[tokio::test]
async fn files_go_through_the_toolbox() {
    let server = MockServer::start().await;
    mount_create(&server, "dtn-4").await;
    let mkdirs: Arc<Mutex<Vec<Value>>> = Arc::default();
    let record = mkdirs.clone();
    Mock::given(method("POST"))
        .and(path("/toolbox/dtn-4/process/execute"))
        .and(move |request: &Request| {
            record.lock().unwrap().push(serde_json::from_slice(&request.body).unwrap());
            true
        })
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "exitCode": 0, "result": "" })),
        )
        .mount(&server)
        .await;
    // Toolbox `POST /files/upload-v2?path=` with a multipart `file` part.
    Mock::given(method("POST"))
        .and(path("/toolbox/dtn-4/files/upload-v2"))
        .and(query_param("path", "/home/daytona/notes/a.txt"))
        .and(|request: &Request| {
            let content_type = request.headers.get("content-type").unwrap().to_str().unwrap();
            let body = String::from_utf8_lossy(&request.body);
            content_type.starts_with("multipart/form-data")
                && body.contains("name=\"file\"")
                && body.contains("contents")
        })
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "path": "/home/daytona/notes/a.txt" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    // Toolbox `GET /files/download?path=` answers `application/octet-stream`.
    Mock::given(method("GET"))
        .and(path("/toolbox/dtn-4/files/download"))
        .and(query_param("path", "/home/daytona/notes/a.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"contents".to_vec()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/toolbox/dtn-4/files/download"))
        .and(query_param("path", "/home/daytona/missing.txt"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "file not found",
            "path": "/files/download",
            "statusCode": 404,
            "timestamp": "2026-11-17T12:00:00Z"
        })))
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let session = client.start(&handle).await.unwrap();
    session.write_file("notes/a.txt", b"contents").await.unwrap();
    assert_eq!(mkdirs.lock().unwrap()[0]["command"], "mkdir -p -- '/home/daytona/notes'");
    assert_eq!(session.read_file("notes/a.txt").await.unwrap(), b"contents");
    assert!(matches!(
        session.read_file("missing.txt").await,
        Err(SandboxError::ExecutionFailed(message)) if message.contains("file not found")
    ));
}

#[tokio::test]
async fn a_manifest_is_written_into_the_new_sandbox() {
    let server = MockServer::start().await;
    mount_create(&server, "dtn-5").await;
    let commands: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = commands.clone();
    Mock::given(method("POST"))
        .and(path("/toolbox/dtn-5/process/execute"))
        .and(move |request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            record.lock().unwrap().push(body["command"].as_str().unwrap().to_string());
            true
        })
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "exitCode": 0, "result": "" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/toolbox/dtn-5/files/upload-v2"))
        .and(query_param("path", "/home/daytona/main.py"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .provision(&Manifest::new(vec![
            ManifestEntry::GitRepo {
                url: "https://github.com/zavora-ai/adk-rust".into(),
                branch: Some("main".into()),
                path: "repo".into(),
            },
            ManifestEntry::File { path: "main.py".into(), content: b"print(1)".to_vec() },
        ]))
        .await
        .unwrap();
    assert_eq!(
        commands.lock().unwrap().as_slice(),
        ["git clone --depth 1 --branch 'main' -- 'https://github.com/zavora-ai/adk-rust' 'repo'"]
    );
}

#[tokio::test]
async fn stop_deletes_pause_and_snapshot_follow_the_documented_paths() {
    let server = MockServer::start().await;
    mount_create(&server, "dtn-6").await;
    // `POST /sandbox/{sandboxIdOrName}/pause`.
    Mock::given(method("POST"))
        .and(path("/sandbox/dtn-6/pause"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sandbox_fixture(&server, "dtn-6", "pausing")),
        )
        .expect(1)
        .mount(&server)
        .await;
    // `POST /sandbox/{sandboxIdOrName}/snapshot` (`CreateSandboxSnapshot { name }`) answers
    // the sandbox while it snapshots; it is polled back to `started`.
    Mock::given(method("POST"))
        .and(path("/sandbox/dtn-6/snapshot"))
        .and(|request: &Request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["name"]
                .as_str()
                .unwrap()
                .starts_with("adk-dtn-6-")
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(sandbox_fixture(
            &server,
            "dtn-6",
            "snapshotting",
        )))
        .expect(1)
        .mount(&server)
        .await;
    // `DELETE /sandbox/{sandboxIdOrName}`.
    Mock::given(method("DELETE"))
        .and(path("/sandbox/dtn-6"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sandbox_fixture(
            &server,
            "dtn-6",
            "destroying",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    client.pause(&handle).await.unwrap();
    let snapshot = client.snapshot(&handle).await.unwrap();
    assert!(snapshot.as_str().starts_with("adk-dtn-6-"));
    client.stop(&handle).await.unwrap();
}

#[tokio::test]
async fn resume_creates_a_sandbox_from_the_snapshot() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sandbox"))
        .and(|request: &Request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["snapshot"] == "adk-snap"
        })
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sandbox_fixture(&server, "dtn-7", "started")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let handle = client(&server).resume(&SnapshotId::new("adk-snap")).await.unwrap();
    assert_eq!(handle, SessionHandle::new("dtn-7"));
}

#[tokio::test]
async fn start_attaches_to_and_starts_a_stopped_sandbox() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sandbox/dtn-other"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sandbox_fixture(
            &server,
            "dtn-other",
            "stopped",
        )))
        .mount(&server)
        .await;
    // `POST /sandbox/{sandboxIdOrName}/start` ("Start or resume sandbox").
    Mock::given(method("POST"))
        .and(path("/sandbox/dtn-other/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sandbox_fixture(
            &server,
            "dtn-other",
            "started",
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/toolbox/dtn-other/process/execute"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "exitCode": 0, "result": "attached\n" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let session = client(&server).start(&SessionHandle::new("dtn-other")).await.unwrap();
    assert_eq!(session.exec_command("echo attached", None).await.unwrap().stdout, "attached\n");
}

#[tokio::test]
async fn a_forbidden_key_maps_to_forbidden_and_is_not_echoed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sandbox"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({ "statusCode": 403, "message": "Forbidden resource" })),
        )
        .mount(&server)
        .await;
    let error = client(&server).provision(&Manifest::new(vec![])).await.unwrap_err();
    assert!(!error.to_string().contains(KEY));
    let error = adk_core::AdkError::from(error);
    assert_eq!(error.category, adk_core::ErrorCategory::Forbidden);
}

#[tokio::test]
async fn dropping_the_client_deletes_the_sandboxes_it_created() {
    let server = MockServer::start().await;
    mount_create(&server, "dtn-8").await;
    Mock::given(method("DELETE"))
        .and(path("/sandbox/dtn-8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sandbox_fixture(
            &server,
            "dtn-8",
            "destroying",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    client.provision(&Manifest::new(vec![])).await.unwrap();
    drop(client);
    for _ in 0..50 {
        if server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.method == wiremock::http::Method::DELETE)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("dropping the client did not delete its sandbox");
}

/// The live run against Daytona: create, exec, files, teardown.
#[tokio::test]
#[ignore = "needs DAYTONA_API_KEY; runs against the real Daytona service"]
async fn live_daytona_round_trip() {
    load_repo_env("DAYTONA_API_KEY");
    let client = DaytonaClient::new(
        DaytonaConfig::from_env().expect("DAYTONA_API_KEY").with_ttl(Duration::from_secs(600)),
    )
    .unwrap();
    let handle = client
        .provision(&Manifest::new(vec![ManifestEntry::File {
            path: "hello.py".into(),
            content: b"print('hi from daytona')".to_vec(),
        }]))
        .await
        .unwrap();
    let session = client.start(&handle).await.unwrap();
    let output = session
        .exec(
            CommandRequest::new("python3 hello.py && echo $MARK")
                .env("MARK", "marked")
                .timeout(Duration::from_secs(60)),
        )
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0, "{output:?}");
    assert_eq!(output.stdout, "hi from daytona\nmarked\n");
    session.write_file("out/data.txt", b"payload").await.unwrap();
    assert_eq!(session.read_file("out/data.txt").await.unwrap(), b"payload");
    client.stop(&handle).await.unwrap();
}

/// Reads `key` from the repository `.env` when the environment lacks it.
fn load_repo_env(key: &str) {
    if std::env::var(key).is_ok() {
        return;
    }
    let Ok(env) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../.env")) else {
        return;
    };
    let prefix = format!("{key}=");
    if let Some(value) = env.lines().find_map(|line| line.strip_prefix(&prefix)) {
        // SAFETY: set before this test starts any thread that reads the environment.
        unsafe { std::env::set_var(key, value.trim().trim_matches('"')) };
    }
}
