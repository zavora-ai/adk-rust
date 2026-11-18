//! Contract tests of `E2bClient` against the documented E2B request and response shapes.
//!
//! Every fixture follows a published E2B document, cited beside it:
//!
//! - control plane: <https://github.com/e2b-dev/E2B/blob/main/spec/openapi.yml>
//! - envd files: <https://github.com/e2b-dev/E2B/blob/main/spec/envd/envd.yaml>
//! - envd processes: <https://github.com/e2b-dev/E2B/blob/main/spec/envd/process/process.proto>
//!   over the Connect protocol, <https://connectrpc.com/docs/protocol/>
//!
//! The live test at the end runs against the real service and needs `E2B_API_KEY`.
#![cfg(feature = "remote-e2b")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_sandbox::workspace::{
    CommandRequest, Manifest, ManifestEntry, SandboxClient, SessionHandle, SnapshotId,
};
use adk_sandbox::{E2bClient, E2bConfig, SandboxError};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "e2b_contract_test_key";
const TOKEN: &str = "envd-access-token-123";

/// `Sandbox` (openapi.yml, `components.schemas.Sandbox`), as `POST /v2/sandboxes` answers 201.
fn sandbox_fixture(id: &str) -> Value {
    json!({
        "templateID": "base",
        "sandboxID": id,
        "alias": "base",
        "clientID": "6532622b",
        "envdVersion": "0.2.4",
        "envdAccessToken": TOKEN,
        "trafficAccessToken": null,
        "domain": null
    })
}

/// One Connect envelope: flags, big-endian length, JSON message
/// (<https://connectrpc.com/docs/protocol/#streaming-response>).
fn envelope(flags: u8, message: &Value) -> Vec<u8> {
    let payload = serde_json::to_vec(message).unwrap();
    let mut out = vec![flags];
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// The JSON message inside a single-envelope request body.
fn unwrap_envelope(body: &[u8]) -> Value {
    assert_eq!(body[0], 0, "a request envelope carries no flags");
    let length = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    assert_eq!(body.len(), 5 + length);
    serde_json::from_slice(&body[5..]).unwrap()
}

/// `process.Process/Start` server stream: `StartResponse { event: ProcessEvent }` messages
/// (process.proto), bytes fields base64 in proto3 JSON, then the end-of-stream envelope.
fn start_stream(stdout: &str, stderr: &str, exit_code: i32) -> Vec<u8> {
    let mut body = envelope(0, &json!({ "event": { "start": { "pid": 42 } } }));
    body.extend(envelope(
        0,
        &json!({ "event": { "data": { "stdout": STANDARD.encode(stdout) } } }),
    ));
    body.extend(envelope(0, &json!({ "event": { "keepalive": {} } })));
    body.extend(envelope(
        0,
        &json!({ "event": { "data": { "stderr": STANDARD.encode(stderr) } } }),
    ));
    let end = if exit_code == 0 {
        // Proto3 JSON omits the zero exit code.
        json!({ "event": { "end": { "exited": true, "status": "exit status 0" } } })
    } else {
        json!({ "event": { "end": { "exitCode": exit_code, "exited": true, "status": format!("exit status {exit_code}") } } })
    };
    body.extend(envelope(0, &end));
    body.extend(envelope(2, &json!({})));
    body
}

fn client(server: &MockServer) -> E2bClient {
    E2bClient::new(
        E2bConfig::new(KEY)
            .with_api_url(server.uri())
            .with_envd_url(server.uri())
            .with_timeout(Duration::from_secs(600))
            .with_internet_access(false)
            .with_env("APP_MODE", "test")
            .with_metadata("owner", "adk"),
    )
    .unwrap()
}

async fn mount_create(server: &MockServer, id: &str) {
    // `POST /v2/sandboxes` with `NewSandboxV2` (openapi.yml `paths./v2/sandboxes.post`).
    Mock::given(method("POST"))
        .and(path("/v2/sandboxes"))
        .and(header("X-API-Key", KEY))
        .and(body_json(json!({
            "templateID": "base",
            "timeout": 600,
            "allow_internet_access": false,
            "envVars": { "APP_MODE": "test" },
            "metadata": { "owner": "adk" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(sandbox_fixture(id)))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn provision_creates_a_sandbox_with_the_documented_request() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-1").await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    assert_eq!(handle, SessionHandle::new("sbx-1"));
    let caps = client.capabilities();
    assert!(caps.remote && caps.snapshot && caps.pause);
    assert_eq!(caps.max_lifetime, Some(Duration::from_secs(600)));
    assert_eq!(client.provider(), "e2b");
}

#[tokio::test]
async fn exec_starts_a_process_through_envd_and_collects_its_stream() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-2").await;
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let record = seen.clone();
    // `POST /process.Process/Start`, Connect server streaming with JSON codec.
    Mock::given(method("POST"))
        .and(path("/process.Process/Start"))
        .and(header("Content-Type", "application/connect+json"))
        .and(header("X-Access-Token", TOKEN))
        .and(header("E2b-Sandbox-Id", "sbx-2"))
        .and(header("E2b-Sandbox-Port", "49983"))
        // envd's process user: HTTP Basic, `user:` (base64 `dXNlcjo=`).
        .and(header("Authorization", "Basic dXNlcjo="))
        .and(move |request: &Request| {
            record.lock().unwrap().push(unwrap_envelope(&request.body));
            true
        })
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/connect+json")
                .set_body_bytes(start_stream("hello\n", "careful\n", 3)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let session = client.start(&handle).await.unwrap();
    let output = session
        .exec(
            CommandRequest::new("echo hello; exit 3")
                .working_dir("src")
                .env("GREETING", "hi")
                .timeout(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    assert_eq!((output.stdout.as_str(), output.stderr.as_str()), ("hello\n", "careful\n"));
    assert_eq!(output.exit_code, 3);
    assert!(!output.timed_out);
    // `StartRequest { process: ProcessConfig { cmd, args, envs, cwd }, stdin }`.
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({
            "process": {
                "cmd": "/bin/bash",
                "args": ["-l", "-c", "echo hello; exit 3"],
                "envs": { "GREETING": "hi" },
                "cwd": "/home/user/src"
            },
            "stdin": false
        })
    );
}

#[tokio::test]
async fn a_zero_exit_code_omitted_by_proto3_json_reads_as_success() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-3").await;
    Mock::given(method("POST"))
        .and(path("/process.Process/Start"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(start_stream("ok\n", "", 0)))
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let output = client.start(&handle).await.unwrap().exec_command("true", None).await.unwrap();
    assert_eq!((output.stdout.as_str(), output.exit_code), ("ok\n", 0));
}

#[tokio::test]
async fn an_end_of_stream_error_is_a_remote_error() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-4").await;
    // Connect end-of-stream message carrying an error (protocol "Error end-of-stream").
    let body = envelope(
        2,
        &json!({ "error": { "code": "permission_denied", "message": "invalid user" } }),
    );
    Mock::given(method("POST"))
        .and(path("/process.Process/Start"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let error = client.start(&handle).await.unwrap().exec_command("id", None).await.unwrap_err();
    assert!(
        matches!(&error, SandboxError::Remote { provider, message, .. } if provider == "e2b" && message.contains("permission_denied")),
        "{error}"
    );
}

#[tokio::test]
async fn files_go_through_envd_http() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-5").await;
    // `POST /files` with `application/octet-stream` and the `path` query (envd.yaml).
    Mock::given(method("POST"))
        .and(path("/files"))
        .and(query_param("path", "/home/user/notes/a.txt"))
        .and(query_param("username", "user"))
        .and(header("Content-Type", "application/octet-stream"))
        .and(header("X-Access-Token", TOKEN))
        .and(|request: &Request| request.body == b"contents")
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "path": "/home/user/notes/a.txt", "name": "a.txt", "type": "file" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    // `GET /files` answers the raw bytes (`DownloadSuccess`).
    Mock::given(method("GET"))
        .and(path("/files"))
        .and(query_param("path", "/home/user/notes/a.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"contents".to_vec()))
        .mount(&server)
        .await;
    // `FileNotFound`.
    Mock::given(method("GET"))
        .and(path("/files"))
        .and(query_param("path", "/home/user/missing.txt"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({ "code": 404, "message": "file not found" })),
        )
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let session = client.start(&handle).await.unwrap();
    session.write_file("notes/a.txt", b"contents").await.unwrap();
    assert_eq!(session.read_file("notes/a.txt").await.unwrap(), b"contents");
    assert!(matches!(
        session.read_file("missing.txt").await,
        Err(SandboxError::ExecutionFailed(message)) if message.contains("file not found")
    ));
    assert!(matches!(
        session.read_file("../etc/passwd").await,
        Err(SandboxError::PathTraversal { .. })
    ));
}

#[tokio::test]
async fn upload_and_download_move_host_files() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-6").await;
    Mock::given(method("POST"))
        .and(path("/files"))
        .and(query_param("path", "/home/user/data.bin"))
        .and(|request: &Request| request.body == [0u8, 1, 2, 3])
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/files"))
        .and(query_param("path", "/home/user/data.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8, 1, 2, 3]))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("data.bin");
    std::fs::write(&local, [0u8, 1, 2, 3]).unwrap();
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let session = client.start(&handle).await.unwrap();
    session.upload(&local, "data.bin").await.unwrap();
    let back = dir.path().join("back.bin");
    session.download("data.bin", &back).await.unwrap();
    assert_eq!(std::fs::read(back).unwrap(), [0u8, 1, 2, 3]);
}

#[tokio::test]
async fn a_manifest_is_written_into_the_new_sandbox() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-7").await;
    Mock::given(method("POST"))
        .and(path("/files"))
        .and(query_param("path", "/home/user/src/main.py"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let commands: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = commands.clone();
    Mock::given(method("POST"))
        .and(path("/process.Process/Start"))
        .and(move |request: &Request| {
            let message = unwrap_envelope(&request.body);
            record
                .lock()
                .unwrap()
                .push(message["process"]["args"][2].as_str().unwrap().to_string());
            true
        })
        .respond_with(ResponseTemplate::new(200).set_body_bytes(start_stream("", "", 0)))
        .mount(&server)
        .await;
    let client = client(&server);
    client
        .provision(&Manifest::new(vec![
            ManifestEntry::Directory { path: "out".into() },
            ManifestEntry::File { path: "src/main.py".into(), content: b"print(1)".to_vec() },
        ]))
        .await
        .unwrap();
    assert_eq!(commands.lock().unwrap().as_slice(), ["mkdir -p -- 'out'"]);
}

#[tokio::test]
async fn stop_kills_and_a_gone_sandbox_is_not_found() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-8").await;
    // `DELETE /sandboxes/{sandboxID}` answers 204, or 404 for an unknown sandbox.
    Mock::given(method("DELETE"))
        .and(path("/sandboxes/sbx-8"))
        .and(header("X-API-Key", KEY))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/sandboxes/gone"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({ "code": 404, "message": "sandbox not found" })),
        )
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    client.stop(&handle).await.unwrap();
    assert!(matches!(
        client.stop(&SessionHandle::new("gone")).await,
        Err(SandboxError::SessionNotFound { handle }) if handle == "gone"
    ));
}

#[tokio::test]
async fn pause_snapshot_and_resume_follow_the_documented_paths() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-9").await;
    // `POST /sandboxes/{sandboxID}/pause` answers 204.
    Mock::given(method("POST"))
        .and(path("/sandboxes/sbx-9/pause"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    // `POST /sandboxes/{sandboxID}/snapshots` answers 201 `SnapshotInfo`.
    Mock::given(method("POST"))
        .and(path("/sandboxes/sbx-9/snapshots"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "snapshotID": "team-slug/adk-snap:default",
            "names": ["team-slug/adk-snap:default"]
        })))
        .expect(1)
        .mount(&server)
        .await;
    // A snapshot is a template: `resume` creates a sandbox from it.
    Mock::given(method("POST"))
        .and(path("/v2/sandboxes"))
        .and(|request: &Request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["templateID"]
                == "team-slug/adk-snap:default"
        })
        .respond_with(ResponseTemplate::new(201).set_body_json(sandbox_fixture("sbx-from-snap")))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    client.pause(&handle).await.unwrap();
    let snapshot = client.snapshot(&handle).await.unwrap();
    assert_eq!(snapshot, SnapshotId::new("team-slug/adk-snap:default"));
    assert_eq!(client.resume(&snapshot).await.unwrap(), SessionHandle::new("sbx-from-snap"));
}

#[tokio::test]
async fn start_attaches_to_a_sandbox_created_elsewhere() {
    let server = MockServer::start().await;
    // `POST /v2/sandboxes/{sandboxID}/connect` (`ConnectSandboxV2`) answers 200 `Sandbox`.
    Mock::given(method("POST"))
        .and(path("/v2/sandboxes/sbx-other/connect"))
        .and(body_json(json!({ "timeout": 600 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "templateID": "base",
            "sandboxID": "sbx-other",
            "clientID": "6532622b",
            "envdVersion": "0.2.4",
            "envdAccessToken": "other-token"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/process.Process/Start"))
        .and(header("X-Access-Token", "other-token"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(start_stream("attached\n", "", 0)))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let session = client.start(&SessionHandle::new("sbx-other")).await.unwrap();
    assert_eq!(session.exec_command("echo attached", None).await.unwrap().stdout, "attached\n");
}

#[tokio::test]
async fn an_unauthorized_key_maps_to_unauthorized_and_is_not_echoed() {
    let server = MockServer::start().await;
    // `components.responses.401`: `Error { code, message }`.
    Mock::given(method("POST"))
        .and(path("/v2/sandboxes"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({ "code": 401, "message": "Invalid API key" })),
        )
        .mount(&server)
        .await;
    let error = client(&server).provision(&Manifest::new(vec![])).await.unwrap_err();
    assert!(!error.to_string().contains(KEY));
    let error = adk_core::AdkError::from(error);
    assert_eq!(error.category, adk_core::ErrorCategory::Unauthorized);
    assert_eq!(error.code, "code.sandbox_remote");
}

#[tokio::test]
async fn dropping_the_client_kills_the_sandboxes_it_created() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-10").await;
    Mock::given(method("DELETE"))
        .and(path("/sandboxes/sbx-10"))
        .respond_with(ResponseTemplate::new(204))
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
    panic!("dropping the client did not kill its sandbox");
}

/// An `envd` that starts the process, reports its pid, then never finishes, and records the
/// signal the client sends; wiremock cannot hold a stream open after a first message.
async fn hanging_envd() -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let signals: Arc<Mutex<Vec<Value>>> = Arc::default();
    let record = signals.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let record = record.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 8192];
                // Headers, then the body by Content-Length.
                let (head_end, length) = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                        break (end + 4, length);
                    }
                };
                while request.len() < head_end + length {
                    let n = socket.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                }
                let head = String::from_utf8_lossy(&request[..head_end]).to_string();
                if head.starts_with("POST /process.Process/SendSignal") {
                    record
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&request[head_end..]).unwrap());
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}")
                        .await;
                    return;
                }
                let first = envelope(0, &json!({ "event": { "start": { "pid": 4242 } } }));
                let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: application/connect+json\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
                response.extend(format!("{:x}\r\n", first.len()).as_bytes());
                response.extend(&first);
                response.extend(b"\r\n");
                let _ = socket.write_all(&response).await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
        }
    });
    (url, signals)
}

#[tokio::test]
async fn a_command_that_times_out_is_killed_with_sigkill() {
    let server = MockServer::start().await;
    mount_create(&server, "sbx-11").await;
    let (envd, signals) = hanging_envd().await;
    let client = E2bClient::new(
        E2bConfig::new(KEY)
            .with_api_url(server.uri())
            .with_envd_url(envd)
            .with_timeout(Duration::from_secs(600))
            .with_internet_access(false)
            .with_env("APP_MODE", "test")
            .with_metadata("owner", "adk"),
    )
    .unwrap();
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let session = client.start(&handle).await.unwrap();
    let started = std::time::Instant::now();
    let output = session
        .exec(CommandRequest::new("sleep 600").timeout(Duration::from_millis(300)))
        .await
        .unwrap();
    assert!(output.timed_out);
    assert!(started.elapsed() < Duration::from_secs(10));
    // `SendSignalRequest { process: ProcessSelector { pid }, signal: SIGNAL_SIGKILL }`.
    assert_eq!(
        signals.lock().unwrap().as_slice(),
        [json!({ "process": { "pid": 4242 }, "signal": "SIGNAL_SIGKILL" })]
    );
}

/// The live run against E2B: create, exec, files, snapshot-free teardown.
#[tokio::test]
#[ignore = "needs E2B_API_KEY; runs against the real E2B service"]
async fn live_e2b_round_trip() {
    load_repo_env("E2B_API_KEY");
    let client = E2bClient::new(
        E2bConfig::from_env().expect("E2B_API_KEY").with_timeout(Duration::from_secs(300)),
    )
    .unwrap();
    let handle = client
        .provision(&Manifest::new(vec![ManifestEntry::File {
            path: "hello.py".into(),
            content: b"print('hi from e2b')".to_vec(),
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
    assert_eq!(output.stdout, "hi from e2b\nmarked\n");
    session.write_file("out/data.txt", b"payload").await.unwrap();
    assert_eq!(session.read_file("out/data.txt").await.unwrap(), b"payload");
    let timed = session
        .exec(CommandRequest::new("sleep 30").timeout(Duration::from_secs(2)))
        .await
        .unwrap();
    assert!(timed.timed_out);
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
