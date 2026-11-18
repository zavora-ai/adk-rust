//! Contract tests of `ModalClient` against Modal's gRPC protocol.
//!
//! The mock server is generated from the same vendored protobuf subset as the client
//! (`adk-sandbox/proto/modal/`, from `modal_proto` in <https://github.com/modal-labs/modal-client>)
//! but compiled apart from it (`tests/modal_mock/`), so every call crosses the real gRPC wire
//! encoding. Requests follow what Modal's Go and JavaScript SDKs send (`go/` and `js/src/` of the
//! same repository); responses follow what the live service returned to the same requests on
//! 2026-11-18, ids included.
//!
//! The live test at the end runs against Modal and needs `MODAL_TOKEN_ID` and
//! `MODAL_TOKEN_SECRET`.
#![cfg(feature = "remote-modal")]

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use adk_sandbox::workspace::{
    CommandRequest, Manifest, ManifestEntry, SandboxClient, SessionHandle, SnapshotId,
};
use adk_sandbox::{ModalClient, ModalConfig, SandboxError};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tonic::codegen::tokio_stream::{self, Stream};
use tonic::metadata::KeyAndValueRef;
use tonic::{Code, Request, Response, Status};

#[allow(dead_code, missing_docs, clippy::all)]
mod modal_proto {
    pub mod client {
        include!("modal_mock/modal.client.rs");
    }
    pub mod task_command_router {
        include!("modal_mock/modal.task_command_router.rs");
    }
}

use modal_proto::client as api;
use modal_proto::task_command_router as tcr;

const TOKEN_ID: &str = "ak-contract-test-id";
const TOKEN_SECRET: &str = "as-contract-test-secret";
/// Ids in the shapes the live service returned.
const APP: &str = "ap-pm1Crm5gKUuPlqWjDsDxKh";
const IMAGE: &str = "im-q8OGaa52WQdeOYSssOXSs3";
const SANDBOX: &str = "sb-01M7PH9H24HPDRTS5K7CKWVBHE";
const TASK: &str = "ta-01M7PH9H24R7EWZKVKCRHP1ABV";
const SNAPSHOT_IMAGE: &str = "im-01M7PH9S41W4R4G9KDN5JA3SS5";
/// A V1 sandbox id: `sb-` and 22 base-62 characters.
const V1_SANDBOX: &str = "sb-0123456789ABCDEFGHIJKL";

/// A JWT with an `exp` claim an hour away, as `AuthTokenGet` and the router access return.
fn jwt(subject: &str) -> String {
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        + 3600;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp},"sub":"{subject}"}}"#));
    format!("{header}.{payload}.signature")
}

fn metadata<T>(request: &Request<T>) -> HashMap<String, String> {
    request
        .metadata()
        .iter()
        .filter_map(|entry| match entry {
            KeyAndValueRef::Ascii(key, value) => {
                Some((key.as_str().to_string(), value.to_str().unwrap_or_default().to_string()))
            }
            KeyAndValueRef::Binary(..) => None,
        })
        .collect()
}

/// How a command the mock runs ends.
#[derive(Clone, Copy, Debug)]
enum End {
    Code(i32),
    Signal(i32),
}

/// What a command prints and how it ends, `after` its start.
#[derive(Clone, Debug)]
struct Outcome {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    end: End,
    after: Duration,
}

impl Outcome {
    fn exit(code: i32) -> Self {
        Self { stdout: Vec::new(), stderr: Vec::new(), end: End::Code(code), after: Duration::ZERO }
    }

    fn printing(stdout: &str, stderr: &str, code: i32) -> Self {
        Self { stdout: stdout.into(), stderr: stderr.into(), ..Self::exit(code) }
    }
}

#[derive(Clone)]
struct Exec {
    request: tcr::TaskExecStartRequest,
    metadata: HashMap<String, String>,
    stdin: Vec<u8>,
    eof: bool,
    started: Instant,
}

#[derive(Default)]
struct State {
    calls: Vec<(String, HashMap<String, String>)>,
    images: Vec<api::ImageGetOrCreateRequest>,
    joins: Vec<api::ImageJoinStreamingRequest>,
    creates: Vec<api::SandboxCreateV2Request>,
    terminated: Vec<String>,
    execs: HashMap<String, Exec>,
    commands: Vec<String>,
    files: HashMap<String, Vec<u8>>,
    snapshots: Vec<tcr::TaskSnapshotFilesystemRequest>,
}

/// Runs a shell command (`$1` of the client's wrapper script) in the mock.
type Shell = Arc<dyn Fn(&str) -> Outcome + Send + Sync>;

/// A Modal control plane and task command router in one server.
#[derive(Clone)]
struct Mock {
    state: Arc<Mutex<State>>,
    url: Arc<OnceLock<String>>,
    shell: Shell,
    /// `ImageGetOrCreate` answers with the build's result; otherwise `ImageJoinStreaming` does.
    image_ready: bool,
    image_result: api::GenericResult,
    /// `AuthTokenGet` fails with this status.
    auth_failure: Option<Code>,
    /// The router JWT `SandboxCreateV2` issues.
    issued_jwt: String,
    /// The router JWT the router accepts, and `SandboxGetCommandRouterAccess` issues.
    router_jwt: Arc<Mutex<String>>,
    /// `SandboxCreateV2` fails once with `Unavailable`.
    fail_create_once: Arc<AtomicBool>,
}

impl Mock {
    fn new() -> Self {
        let issued_jwt = jwt("router-1");
        Self {
            state: Arc::default(),
            url: Arc::default(),
            shell: Arc::new(|_| Outcome::exit(0)),
            image_ready: true,
            image_result: api::GenericResult {
                status: api::generic_result::GenericStatus::Success as i32,
                ..Default::default()
            },
            auth_failure: None,
            router_jwt: Arc::new(Mutex::new(issued_jwt.clone())),
            issued_jwt,
            fail_create_once: Arc::default(),
        }
    }

    fn with_shell(mut self, shell: impl Fn(&str) -> Outcome + Send + Sync + 'static) -> Self {
        self.shell = Arc::new(shell);
        self
    }

    fn record<T>(&self, method: &str, request: &Request<T>) {
        self.state.lock().unwrap().calls.push((method.to_string(), metadata(request)));
    }

    fn url(&self) -> String {
        self.url.get().cloned().unwrap()
    }

    fn authorize<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let expected = format!("Bearer {}", self.router_jwt.lock().unwrap());
        match request.metadata().get("authorization").and_then(|v| v.to_str().ok()) {
            Some(value) if value == expected => Ok(()),
            _ => Err(Status::unauthenticated("invalid or expired JWT")),
        }
    }

    /// What the exec does, once stdin is closed for a write, at its `after`.
    async fn outcome(&self, exec_id: &str) -> Result<Outcome, Status> {
        loop {
            let exec = self
                .state
                .lock()
                .unwrap()
                .execs
                .get(exec_id)
                .cloned()
                .ok_or_else(|| Status::not_found(format!("exec {exec_id} not found")))?;
            let args = &exec.request.command_args;
            let script = args.get(2).cloned().unwrap_or_default();
            if script.contains("cat > ") && !exec.eof {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }
            let path = args.get(4).cloned().unwrap_or_default();
            let outcome = if script.contains("cat > ") {
                self.state.lock().unwrap().files.insert(path, exec.stdin.clone());
                Outcome::exit(0)
            } else if script.starts_with("cat -- ") {
                match self.state.lock().unwrap().files.get(&path) {
                    Some(content) => Outcome { stdout: content.clone(), ..Outcome::exit(0) },
                    None => Outcome::printing(
                        "",
                        &format!("cat: {path}: No such file or directory\n"),
                        1,
                    ),
                }
            } else {
                (self.shell)(&path)
            };
            tokio::time::sleep_until((exec.started + outcome.after).into()).await;
            return Ok(outcome);
        }
    }
}

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl api::modal_client_server::ModalClient for Mock {
    async fn app_get_or_create(
        &self,
        request: Request<api::AppGetOrCreateRequest>,
    ) -> Result<Response<api::AppGetOrCreateResponse>, Status> {
        self.record("AppGetOrCreate", &request);
        let request = request.into_inner();
        assert_eq!(
            request.object_creation_type,
            api::ObjectCreationType::CreateIfMissing as i32,
            "the app is created on first use"
        );
        Ok(Response::new(api::AppGetOrCreateResponse { app_id: APP.into() }))
    }

    async fn auth_token_get(
        &self,
        request: Request<api::AuthTokenGetRequest>,
    ) -> Result<Response<api::AuthTokenGetResponse>, Status> {
        self.record("AuthTokenGet", &request);
        if let Some(code) = self.auth_failure {
            return Err(Status::new(
                code,
                "Token ID or secret is invalid. Please check your Modal token.",
            ));
        }
        Ok(Response::new(api::AuthTokenGetResponse { token: jwt("control") }))
    }

    async fn environment_get_or_create(
        &self,
        request: Request<api::EnvironmentGetOrCreateRequest>,
    ) -> Result<Response<api::EnvironmentGetOrCreateResponse>, Status> {
        self.record("EnvironmentGetOrCreate", &request);
        Ok(Response::new(api::EnvironmentGetOrCreateResponse {
            environment_id: "en-1".into(),
            metadata: Some(api::EnvironmentMetadata {
                name: "main".into(),
                settings: Some(api::EnvironmentSettings {
                    image_builder_version: "2025.06".into(),
                }),
            }),
        }))
    }

    async fn image_get_or_create(
        &self,
        request: Request<api::ImageGetOrCreateRequest>,
    ) -> Result<Response<api::ImageGetOrCreateResponse>, Status> {
        self.record("ImageGetOrCreate", &request);
        self.state.lock().unwrap().images.push(request.into_inner());
        Ok(Response::new(api::ImageGetOrCreateResponse {
            image_id: IMAGE.into(),
            result: self.image_ready.then(|| self.image_result.clone()),
        }))
    }

    type ImageJoinStreamingStream = BoxStream<api::ImageJoinStreamingResponse>;

    async fn image_join_streaming(
        &self,
        request: Request<api::ImageJoinStreamingRequest>,
    ) -> Result<Response<Self::ImageJoinStreamingStream>, Status> {
        self.record("ImageJoinStreaming", &request);
        self.state.lock().unwrap().joins.push(request.into_inner());
        // A log entry, then the result.
        let items = vec![
            Ok(api::ImageJoinStreamingResponse {
                result: None,
                entry_id: "1763452800000-0".into(),
                eof: false,
            }),
            Ok(api::ImageJoinStreamingResponse {
                result: Some(self.image_result.clone()),
                entry_id: "1763452800000-1".into(),
                eof: true,
            }),
        ];
        Ok(Response::new(Box::pin(tokio_stream::iter(items))))
    }

    async fn sandbox_create_v2(
        &self,
        request: Request<api::SandboxCreateV2Request>,
    ) -> Result<Response<api::SandboxCreateV2Response>, Status> {
        self.record("SandboxCreateV2", &request);
        if self.fail_create_once.swap(false, Ordering::SeqCst) {
            return Err(Status::unavailable("upstream connect error"));
        }
        let request = request.into_inner();
        if request.definition.as_ref().is_some_and(|d| d.image_id == "im-missing") {
            return Err(Status::not_found("Image im-missing not found"));
        }
        self.state.lock().unwrap().creates.push(request);
        Ok(Response::new(api::SandboxCreateV2Response {
            sandbox_id: SANDBOX.into(),
            task_id: TASK.into(),
            command_router_access: Some(api::CommandRouterAccess {
                jwt: self.issued_jwt.clone(),
                url: self.url(),
            }),
        }))
    }

    async fn sandbox_get_command_router_access(
        &self,
        request: Request<api::SandboxGetCommandRouterAccessRequest>,
    ) -> Result<Response<api::SandboxGetCommandRouterAccessResponse>, Status> {
        self.record("SandboxGetCommandRouterAccess", &request);
        Ok(Response::new(api::SandboxGetCommandRouterAccessResponse {
            jwt: self.router_jwt.lock().unwrap().clone(),
            url: self.url(),
        }))
    }

    async fn sandbox_get_task_id(
        &self,
        request: Request<api::SandboxGetTaskIdRequest>,
    ) -> Result<Response<api::SandboxGetTaskIdResponse>, Status> {
        self.record("SandboxGetTaskId", &request);
        Ok(Response::new(api::SandboxGetTaskIdResponse {
            task_id: Some(TASK.into()),
            task_result: None,
        }))
    }

    async fn sandbox_get_task_id_v2(
        &self,
        request: Request<api::SandboxGetTaskIdRequest>,
    ) -> Result<Response<api::SandboxGetTaskIdResponse>, Status> {
        self.record("SandboxGetTaskIdV2", &request);
        let id = request.into_inner().sandbox_id;
        if self.state.lock().unwrap().terminated.contains(&id) {
            // A finished sandbox answers with its result and no task.
            return Ok(Response::new(api::SandboxGetTaskIdResponse {
                task_id: None,
                task_result: Some(api::GenericResult {
                    status: api::generic_result::GenericStatus::Terminated as i32,
                    ..Default::default()
                }),
            }));
        }
        Ok(Response::new(api::SandboxGetTaskIdResponse {
            task_id: Some(TASK.into()),
            task_result: None,
        }))
    }

    async fn sandbox_list(
        &self,
        request: Request<api::SandboxListRequest>,
    ) -> Result<Response<api::SandboxListResponse>, Status> {
        self.record("SandboxList", &request);
        Ok(Response::new(api::SandboxListResponse { sandboxes: Vec::new() }))
    }

    async fn sandbox_list_v2(
        &self,
        request: Request<api::SandboxListRequest>,
    ) -> Result<Response<api::SandboxListResponse>, Status> {
        self.record("SandboxListV2", &request);
        let request = request.into_inner();
        assert_eq!(request.app_id, APP);
        let state = self.state.lock().unwrap();
        // One page: the next request, before the oldest sandbox, is empty.
        let sandboxes = if request.before_timestamp == 0.0
            && !state.creates.is_empty()
            && !state.terminated.contains(&SANDBOX.to_string())
        {
            vec![api::SandboxInfo {
                id: SANDBOX.into(),
                created_at: 1_763_452_800.5,
                app_id: APP.into(),
                tags: Vec::new(),
                name: String::new(),
            }]
        } else {
            Vec::new()
        };
        Ok(Response::new(api::SandboxListResponse { sandboxes }))
    }

    async fn sandbox_terminate(
        &self,
        request: Request<api::SandboxTerminateRequest>,
    ) -> Result<Response<api::SandboxTerminateResponse>, Status> {
        self.record("SandboxTerminate", &request);
        self.state.lock().unwrap().terminated.push(request.into_inner().sandbox_id);
        Ok(Response::new(api::SandboxTerminateResponse { existing_result: None }))
    }

    async fn sandbox_terminate_v2(
        &self,
        request: Request<api::SandboxTerminateRequest>,
    ) -> Result<Response<api::SandboxTerminateResponse>, Status> {
        self.record("SandboxTerminateV2", &request);
        let id = request.into_inner().sandbox_id;
        // What the live service answers for a malformed and an unknown id.
        if id.len() != SANDBOX.len() {
            return Err(Status::invalid_argument(format!("\"{id}\" is not a valid sandbox ID")));
        }
        if id != SANDBOX {
            return Err(Status::not_found(format!("sandbox {id} not found")));
        }
        self.state.lock().unwrap().terminated.push(id);
        Ok(Response::new(api::SandboxTerminateResponse { existing_result: None }))
    }

    async fn task_get_command_router_access(
        &self,
        request: Request<api::TaskGetCommandRouterAccessRequest>,
    ) -> Result<Response<api::TaskGetCommandRouterAccessResponse>, Status> {
        self.record("TaskGetCommandRouterAccess", &request);
        assert_eq!(request.into_inner().task_id, TASK);
        Ok(Response::new(api::TaskGetCommandRouterAccessResponse {
            jwt: self.router_jwt.lock().unwrap().clone(),
            url: self.url(),
        }))
    }
}

#[tonic::async_trait]
impl tcr::task_command_router_server::TaskCommandRouter for Mock {
    async fn task_exec_start(
        &self,
        request: Request<tcr::TaskExecStartRequest>,
    ) -> Result<Response<tcr::TaskExecStartResponse>, Status> {
        self.record("TaskExecStart", &request);
        self.authorize(&request)?;
        let metadata = metadata(&request);
        let request = request.into_inner();
        assert_eq!(request.task_id, TASK);
        let mut state = self.state.lock().unwrap();
        if let Some(command) = request.command_args.get(4)
            && !request.command_args[2].starts_with("cat")
            && !request.command_args[2].starts_with("mkdir")
        {
            state.commands.push(command.clone());
        }
        state.execs.insert(
            request.exec_id.clone(),
            Exec { request, metadata, stdin: Vec::new(), eof: false, started: Instant::now() },
        );
        Ok(Response::new(tcr::TaskExecStartResponse {}))
    }

    async fn task_exec_stdin_write(
        &self,
        request: Request<tcr::TaskExecStdinWriteRequest>,
    ) -> Result<Response<tcr::TaskExecStdinWriteResponse>, Status> {
        self.authorize(&request)?;
        let request = request.into_inner();
        let mut state = self.state.lock().unwrap();
        let exec = state
            .execs
            .get_mut(&request.exec_id)
            .ok_or_else(|| Status::not_found("exec not found"))?;
        assert_eq!(request.offset, exec.stdin.len() as u64, "writes resume at their offset");
        exec.stdin.extend_from_slice(&request.data);
        exec.eof |= request.eof;
        Ok(Response::new(tcr::TaskExecStdinWriteResponse {}))
    }

    type TaskExecStdioReadStream = BoxStream<tcr::TaskExecStdioReadResponse>;

    async fn task_exec_stdio_read(
        &self,
        request: Request<tcr::TaskExecStdioReadRequest>,
    ) -> Result<Response<Self::TaskExecStdioReadStream>, Status> {
        self.authorize(&request)?;
        let request = request.into_inner();
        let outcome = self.outcome(&request.exec_id).await?;
        let data = if request.file_descriptor == tcr::TaskExecStdioFileDescriptor::Stderr as i32 {
            outcome.stderr
        } else {
            outcome.stdout
        };
        // The data from the requested offset, in two messages.
        let rest = data[usize::try_from(request.offset).unwrap().min(data.len())..].to_vec();
        let half = rest.len() / 2;
        let chunks = [rest[..half].to_vec(), rest[half..].to_vec()]
            .into_iter()
            .filter(|chunk| !chunk.is_empty())
            .map(|data| Ok(tcr::TaskExecStdioReadResponse { data }))
            .collect::<Vec<_>>();
        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    async fn task_exec_wait(
        &self,
        request: Request<tcr::TaskExecWaitRequest>,
    ) -> Result<Response<tcr::TaskExecWaitResponse>, Status> {
        self.authorize(&request)?;
        let outcome = self.outcome(&request.into_inner().exec_id).await?;
        let exit_status = match outcome.end {
            End::Code(code) => tcr::task_exec_wait_response::ExitStatus::Code(code),
            End::Signal(signal) => tcr::task_exec_wait_response::ExitStatus::Signal(signal),
        };
        Ok(Response::new(tcr::TaskExecWaitResponse { exit_status: Some(exit_status) }))
    }

    async fn task_snapshot_filesystem(
        &self,
        request: Request<tcr::TaskSnapshotFilesystemRequest>,
    ) -> Result<Response<tcr::TaskSnapshotFilesystemResponse>, Status> {
        self.record("TaskSnapshotFilesystem", &request);
        self.authorize(&request)?;
        self.state.lock().unwrap().snapshots.push(request.into_inner());
        Ok(Response::new(tcr::TaskSnapshotFilesystemResponse { image_id: SNAPSHOT_IMAGE.into() }))
    }
}

/// Serves `mock` on a local port; returns its URL.
async fn serve(mock: &Mock) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    mock.url.set(url.clone()).unwrap();
    let router = tonic::transport::Server::builder()
        .add_service(api::modal_client_server::ModalClientServer::new(mock.clone()))
        .add_service(tcr::task_command_router_server::TaskCommandRouterServer::new(mock.clone()));
    tokio::spawn(router.serve_with_incoming(tonic::transport::server::TcpIncoming::from(listener)));
    url
}

fn config(url: &str) -> ModalConfig {
    ModalConfig::new(TOKEN_ID, TOKEN_SECRET).with_server_url(url)
}

async fn started(mock: &Mock, config: impl FnOnce(ModalConfig) -> ModalConfig) -> ModalClient {
    let url = serve(mock).await;
    ModalClient::new(config(self::config(&url))).unwrap()
}

fn calls(mock: &Mock, method: &str) -> Vec<HashMap<String, String>> {
    let state = mock.state.lock().unwrap();
    state.calls.iter().filter(|(m, _)| m == method).map(|(_, md)| md.clone()).collect()
}

fn exec_requests(mock: &Mock) -> Vec<Exec> {
    let state = mock.state.lock().unwrap();
    let mut execs: Vec<Exec> = state.execs.values().cloned().collect();
    execs.sort_by_key(|exec| exec.started);
    execs
}

#[tokio::test]
async fn provision_builds_the_image_and_creates_a_sandbox_with_the_documented_request() {
    let mock = Mock::new();
    let client = started(&mock, |config| {
        config
            .with_environment("main")
            .with_app_name("agents")
            .with_timeout(Duration::from_secs(600))
            .with_idle_timeout(Duration::from_secs(120))
            .with_network_blocked(true)
            .with_cpu(0.5)
            .with_memory_mib(1024)
            .with_tag("team", "research")
            .with_name_prefix("agent")
    })
    .await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    assert_eq!(handle, SessionHandle::new(SANDBOX));

    let state = mock.state.lock().unwrap();
    // `ImageGetOrCreate`: the registry image plus `git` and `patch`, in the app, with the
    // environment's builder version.
    let image = &state.images[0];
    assert_eq!(image.app_id, APP);
    assert_eq!(image.builder_version, "2025.06");
    let commands = &image.image.as_ref().unwrap().dockerfile_commands;
    assert_eq!(commands[0], "FROM python:3.13-slim");
    assert!(commands[1].starts_with("RUN apt-get update") && commands[1].contains("git patch"));
    // `SandboxCreateV2`.
    let create = &state.creates[0];
    assert_eq!(create.app_id, APP);
    assert_eq!(
        create.tags,
        vec![api::SandboxTag { tag_name: "team".into(), tag_value: "research".into() }]
    );
    assert!(create.secret_sources.is_empty());
    let mut definition = create.definition.clone().unwrap();
    let name = definition.name.take().unwrap();
    assert!(name.starts_with("agent-") && name.len() == "agent-".len() + 12, "{name}");
    assert_eq!(
        definition,
        api::Sandbox {
            entrypoint_args: Vec::new(),
            image_id: IMAGE.into(),
            resources: Some(api::Resources { memory_mb: 1024, milli_cpu: 500 }),
            timeout_secs: 600,
            workdir: Some("/root".into()),
            network_access: Some(api::NetworkAccess {
                network_access_type: api::network_access::NetworkAccessType::Blocked as i32,
                allowed_cidrs: Vec::new(),
            }),
            name: None,
            idle_timeout_secs: Some(120),
        }
    );
    drop(state);

    // Every control-plane call identifies the client as the SDKs do; the token is fetched once.
    let token_calls = calls(&mock, "AuthTokenGet");
    assert_eq!(token_calls.len(), 1);
    assert!(!token_calls[0].contains_key("x-modal-auth-token"));
    for method in
        ["AppGetOrCreate", "EnvironmentGetOrCreate", "ImageGetOrCreate", "SandboxCreateV2"]
    {
        let md = &calls(&mock, method)[0];
        assert_eq!(md["x-modal-token-id"], TOKEN_ID, "{method}");
        assert_eq!(md["x-modal-token-secret"], TOKEN_SECRET, "{method}");
        assert_eq!(md["x-modal-client-type"], "7", "{method}");
        assert_eq!(md["x-modal-client-version"], "1.0.0", "{method}");
        assert_eq!(
            md["x-modal-libmodal-version"],
            concat!("adk-sandbox/", env!("CARGO_PKG_VERSION")),
            "{method}"
        );
        assert_eq!(md["x-modal-host"], "127.0.0.1", "{method}");
        assert_eq!(md["x-retry-attempt"], "0", "{method}");
        assert_eq!(md["x-idempotency-key"].len(), 36, "{method}");
        assert!(md["x-modal-auth-token"].starts_with("eyJ"), "{method}");
        assert!(md.contains_key("grpc-timeout"), "{method} carries a deadline");
    }

    let caps = client.capabilities();
    assert!(caps.remote && caps.snapshot && caps.network_blocked && !caps.pause);
    assert_eq!(caps.max_lifetime, Some(Duration::from_secs(600)));
    assert_eq!(client.provider(), "modal");
    assert!(!format!("{client:?}").contains(TOKEN_SECRET));
}

#[tokio::test]
async fn an_image_still_building_is_awaited_through_image_join_streaming() {
    let mut mock = Mock::new();
    mock.image_ready = false;
    let client = started(&mock, |config| config.with_image("alpine:3.21")).await;
    client.provision(&Manifest::new(vec![])).await.unwrap();
    let state = mock.state.lock().unwrap();
    assert_eq!(
        state.images[0].image.as_ref().unwrap().dockerfile_commands,
        ["FROM alpine:3.21"],
        "with_image drops the default image's additions"
    );
    assert_eq!(state.joins[0].image_id, IMAGE);
    assert_eq!(state.joins[0].timeout, 55.0);
    assert_eq!(state.joins[0].last_entry_id, "");
    assert_eq!(state.creates[0].definition.as_ref().unwrap().image_id, IMAGE);
}

#[tokio::test]
async fn a_failed_image_build_is_a_provision_failure_and_creates_nothing() {
    let mut mock = Mock::new();
    mock.image_ready = false;
    mock.image_result = api::GenericResult {
        status: api::generic_result::GenericStatus::Failure as i32,
        exception: "manifest for python:9 not found".into(),
        exitcode: 1,
    };
    let client = started(&mock, |config| config.with_image("python:9")).await;
    let error = client.provision(&Manifest::new(vec![])).await.unwrap_err();
    assert!(
        matches!(&error, SandboxError::ProvisionFailed { reason, .. }
            if reason.contains("GENERIC_STATUS_FAILURE") && reason.contains("manifest for python:9 not found")),
        "{error}"
    );
    assert!(mock.state.lock().unwrap().creates.is_empty());
}

#[tokio::test]
async fn an_image_id_skips_the_build() {
    let mock = Mock::new();
    let client = started(&mock, |config| config.with_image_id("im-prebuilt")).await;
    client.provision(&Manifest::new(vec![])).await.unwrap();
    let state = mock.state.lock().unwrap();
    assert!(state.images.is_empty());
    assert_eq!(state.creates[0].definition.as_ref().unwrap().image_id, "im-prebuilt");
}

#[tokio::test]
async fn exec_runs_through_the_command_router_with_its_environment_and_timeout() {
    let mock = Mock::new().with_shell(|command| {
        assert_eq!(command, "echo $GREETING; echo warn >&2; exit 2");
        Outcome::printing("hi\n", "warn\n", 2)
    });
    let client = started(&mock, |config| config.with_env("BASE", "1")).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let output = client
        .start(&handle)
        .await
        .unwrap()
        .exec(
            CommandRequest::new("echo $GREETING; echo warn >&2; exit 2")
                .working_dir("src")
                .env("GREETING", "hi")
                .timeout(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    assert_eq!(
        (output.stdout.as_str(), output.stderr.as_str(), output.exit_code),
        ("hi\n", "warn\n", 2)
    );
    assert!(!output.timed_out);

    let exec = exec_requests(&mock).pop().unwrap();
    let request = &exec.request;
    assert_eq!(&request.command_args[..2], ["sh", "-c"]);
    assert!(request.command_args[2].contains(r#"exec bash -c "$1""#));
    assert_eq!(request.command_args[3..], ["sh", "echo $GREETING; echo warn >&2; exit 2"]);
    assert_eq!(request.workdir.as_deref(), Some("/root/src"));
    assert_eq!(request.timeout_secs, Some(5));
    assert_eq!(request.stdout_config, tcr::TaskExecStdoutConfig::Pipe as i32);
    assert_eq!(request.stderr_config, tcr::TaskExecStderrConfig::Pipe as i32);
    let env = match &request.secret_sources[..] {
        [api::SecretSource { source: Some(api::secret_source::Source::Env(map)) }] => {
            map.contents.clone().into_iter().collect::<BTreeMap<_, _>>()
        }
        other => panic!("one inline env source expected: {other:?}"),
    };
    assert_eq!(
        env,
        BTreeMap::from([("BASE".into(), "1".into()), ("GREETING".into(), "hi".into())])
    );
    // The router gets the sandbox's JWT and never the Modal token.
    assert!(exec.metadata["authorization"].starts_with("Bearer eyJ"));
    assert!(!exec.metadata.contains_key("x-modal-token-secret"));
    assert!(!exec.metadata.contains_key("x-modal-token-id"));
    assert!(exec.metadata.contains_key("grpc-timeout"));
}

#[tokio::test]
async fn a_command_modal_kills_at_its_timeout_is_timed_out() {
    // The live service reports a command it killed at `timeout_secs` as exit code 137.
    let mock = Mock::new().with_shell(|_| Outcome {
        after: Duration::from_millis(1100),
        ..Outcome::printing("start\n", "", 137)
    });
    let client = started(&mock, |config| config).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let output = client
        .start(&handle)
        .await
        .unwrap()
        .exec(CommandRequest::new("echo start; sleep 20").timeout(Duration::from_secs(1)))
        .await
        .unwrap();
    assert!(output.timed_out);
    assert_eq!(output.exit_code, -1);
    assert_eq!(output.stdout, "start\n");
    assert_eq!(exec_requests(&mock).pop().unwrap().request.timeout_secs, Some(1));
}

#[tokio::test]
async fn a_signal_before_the_timeout_is_an_exit_not_a_timeout() {
    let mock = Mock::new().with_shell(|_| Outcome { end: End::Signal(15), ..Outcome::exit(0) });
    let client = started(&mock, |config| config).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let output =
        client.start(&handle).await.unwrap().exec_command("kill -TERM $$", None).await.unwrap();
    assert_eq!(output.exit_code, 143);
    assert!(!output.timed_out);
    assert_eq!(exec_requests(&mock).pop().unwrap().request.timeout_secs, Some(120));
}

#[tokio::test]
async fn files_go_through_cat_over_stdin_and_stdout() {
    let mock = Mock::new();
    let client = started(&mock, |config| config).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let session = client.start(&handle).await.unwrap();
    let content = b"payload\x00\xff\n".to_vec();
    session.write_file("notes/a.txt", &content).await.unwrap();
    let write = exec_requests(&mock).pop().unwrap();
    assert_eq!(write.request.command_args[2], r#"mkdir -p -- "$(dirname -- "$1")" && cat > "$1""#);
    assert_eq!(write.request.command_args[4], "/root/notes/a.txt");
    assert_eq!((write.stdin.as_slice(), write.eof), (content.as_slice(), true));

    assert_eq!(session.read_file("notes/a.txt").await.unwrap(), content);
    let read = exec_requests(&mock).pop().unwrap();
    assert_eq!(read.request.command_args[2], r#"cat -- "$1""#);
    assert!(matches!(
        session.read_file("missing.txt").await,
        Err(SandboxError::ExecutionFailed(message)) if message == "file not found: missing.txt"
    ));
    let before = exec_requests(&mock).len();
    assert!(matches!(
        session.read_file("../etc/passwd").await,
        Err(SandboxError::PathTraversal { .. })
    ));
    assert_eq!(exec_requests(&mock).len(), before, "an escaping path never reaches Modal");

    // An empty file closes stdin with one empty write.
    session.write_file("empty.txt", b"").await.unwrap();
    assert_eq!(mock.state.lock().unwrap().files["/root/empty.txt"], b"");
}

#[tokio::test]
async fn upload_and_download_move_host_files() {
    let mock = Mock::new();
    let client = started(&mock, |config| config).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let session = client.start(&handle).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("data.bin");
    std::fs::write(&local, [1u8, 2, 3]).unwrap();
    session.upload(&local, "in/data.bin").await.unwrap();
    let back = dir.path().join("back.bin");
    session.download("in/data.bin", &back).await.unwrap();
    assert_eq!(std::fs::read(back).unwrap(), [1, 2, 3]);
}

#[tokio::test]
async fn a_manifest_is_written_into_the_new_sandbox() {
    let mock = Mock::new();
    let client = started(&mock, |config| config).await;
    client
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
    let state = mock.state.lock().unwrap();
    assert_eq!(
        state.commands,
        ["git clone --depth 1 --branch 'main' -- 'https://github.com/zavora-ai/adk-rust' 'repo'"]
    );
    assert_eq!(state.files["/root/main.py"], b"print(1)");
}

#[tokio::test]
async fn a_failed_manifest_terminates_the_sandbox() {
    let mock =
        Mock::new().with_shell(|_| Outcome::printing("", "fatal: repository not found\n", 128));
    let client = started(&mock, |config| config).await;
    let error = client
        .provision(&Manifest::new(vec![ManifestEntry::GitRepo {
            url: "https://example.invalid/repo".into(),
            branch: None,
            path: "repo".into(),
        }]))
        .await
        .unwrap_err();
    assert!(matches!(error, SandboxError::ProvisionFailed { .. }), "{error}");
    assert_eq!(mock.state.lock().unwrap().terminated, [SANDBOX]);
}

#[tokio::test]
async fn stop_terminates_and_an_unknown_sandbox_is_not_found() {
    let mock = Mock::new();
    let client = started(&mock, |config| config).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    client.stop(&handle).await.unwrap();
    assert_eq!(mock.state.lock().unwrap().terminated, [SANDBOX]);
    for id in ["sb-bogus", "sb-01M7PH9H24HPDRTS5K7CKWVBHF"] {
        assert!(
            matches!(
                client.stop(&SessionHandle::new(id)).await,
                Err(SandboxError::SessionNotFound { handle }) if handle == id
            ),
            "{id}"
        );
    }
    // A V1 id goes to the V1 RPC.
    client.stop(&SessionHandle::new(V1_SANDBOX)).await.unwrap();
    assert_eq!(calls(&mock, "SandboxTerminate").len(), 1);
}

#[tokio::test]
async fn snapshot_and_resume_go_through_the_router_and_a_new_sandbox() {
    let mock = Mock::new();
    let client = started(&mock, |config| config.with_snapshot_ttl(Duration::from_secs(3600))).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let snapshot = client.snapshot(&handle).await.unwrap();
    assert_eq!(snapshot, SnapshotId::new(SNAPSHOT_IMAGE));
    {
        let state = mock.state.lock().unwrap();
        let request = &state.snapshots[0];
        assert_eq!(request.task_id, TASK);
        assert_eq!(request.ttl_seconds, Some(3600));
        assert_eq!(request.snapshot_id.len(), 36);
    }
    client.resume(&snapshot).await.unwrap();
    {
        let state = mock.state.lock().unwrap();
        assert_eq!(state.images.len(), 1, "resume builds nothing");
        assert_eq!(state.creates[1].definition.as_ref().unwrap().image_id, SNAPSHOT_IMAGE);
    }
    assert!(matches!(
        client.resume(&SnapshotId::new("im-missing")).await,
        Err(SandboxError::SnapshotNotFound { id }) if id == "im-missing"
    ));
}

#[tokio::test]
async fn start_attaches_to_a_sandbox_created_elsewhere() {
    let mock = Mock::new().with_shell(|_| Outcome::printing("attached\n", "", 0));
    let client = started(&mock, |config| config).await;
    let session = client.start(&SessionHandle::new(SANDBOX)).await.unwrap();
    assert_eq!(session.exec_command("echo attached", None).await.unwrap().stdout, "attached\n");
    assert_eq!(calls(&mock, "SandboxGetTaskIdV2").len(), 1);
    assert_eq!(calls(&mock, "SandboxGetCommandRouterAccess").len(), 1);
    // A V1 sandbox goes through the V1 RPCs.
    let session = client.start(&SessionHandle::new(V1_SANDBOX)).await.unwrap();
    session.exec_command("echo attached", None).await.unwrap();
    assert_eq!(calls(&mock, "SandboxGetTaskId").len(), 1);
    assert_eq!(calls(&mock, "TaskGetCommandRouterAccess").len(), 1);
    // Dropping the client leaves sandboxes it only attached to alone.
    drop(client);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(mock.state.lock().unwrap().terminated.is_empty());
}

#[tokio::test]
async fn a_finished_sandbox_cannot_be_attached() {
    let mock = Mock::new();
    let client = started(&mock, |config| config).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    client.stop(&handle).await.unwrap();
    let other = ModalClient::new(config(&mock.url())).unwrap();
    assert!(matches!(
        other.start(&handle).await,
        Err(SandboxError::SessionNotFound { handle: id }) if id == SANDBOX
    ));
}

#[tokio::test]
async fn a_refused_router_token_is_refreshed_once() {
    let mock = Mock::new();
    *mock.router_jwt.lock().unwrap() = jwt("router-2");
    let client = started(&mock, |config| config).await;
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    let output = client.start(&handle).await.unwrap().exec_command("true", None).await.unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(calls(&mock, "SandboxGetCommandRouterAccess").len(), 1);
    assert_eq!(calls(&mock, "TaskExecStart").len(), 2, "refused once, then accepted");
}

#[tokio::test]
async fn a_transient_failure_is_retried_with_the_same_idempotency_key() {
    let mock = Mock::new();
    mock.fail_create_once.store(true, Ordering::SeqCst);
    let client = started(&mock, |config| config).await;
    client.provision(&Manifest::new(vec![])).await.unwrap();
    let attempts = calls(&mock, "SandboxCreateV2");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["x-idempotency-key"], attempts[1]["x-idempotency-key"]);
    assert_eq!(
        (attempts[0]["x-retry-attempt"].as_str(), attempts[1]["x-retry-attempt"].as_str()),
        ("0", "1")
    );
    assert_eq!(mock.state.lock().unwrap().creates.len(), 1);
}

#[tokio::test]
async fn an_invalid_token_maps_to_unauthorized_and_is_not_echoed() {
    let mut mock = Mock::new();
    mock.auth_failure = Some(Code::Unauthenticated);
    let client = started(&mock, |config| config).await;
    let error = client.provision(&Manifest::new(vec![])).await.unwrap_err();
    let text = error.to_string();
    assert!(!text.contains(TOKEN_SECRET) && !text.contains(TOKEN_ID), "{text}");
    assert!(text.contains("Unauthenticated"), "{text}");
    let error = adk_core::AdkError::from(error);
    assert_eq!(error.category, adk_core::ErrorCategory::Unauthorized);
}

#[tokio::test]
async fn dropping_the_client_terminates_the_sandboxes_it_created() {
    let mock = Mock::new();
    let client = started(&mock, |config| config).await;
    client.provision(&Manifest::new(vec![])).await.unwrap();
    drop(client);
    for _ in 0..50 {
        if !mock.state.lock().unwrap().terminated.is_empty() {
            assert_eq!(mock.state.lock().unwrap().terminated, [SANDBOX]);
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("dropping the client did not terminate its sandbox");
}

#[tokio::test]
async fn running_sandboxes_lists_the_apps_sandboxes() {
    let mock = Mock::new();
    let client = started(&mock, |config| config).await;
    assert!(client.running_sandboxes().await.unwrap().is_empty());
    let handle = client.provision(&Manifest::new(vec![])).await.unwrap();
    assert_eq!(client.running_sandboxes().await.unwrap(), [SANDBOX]);
    client.stop(&handle).await.unwrap();
    assert!(client.running_sandboxes().await.unwrap().is_empty());
    assert!(!calls(&mock, "SandboxList").is_empty(), "V1 sandboxes are listed too");
}

#[tokio::test]
async fn pause_is_unsupported() {
    let mock = Mock::new();
    let client = started(&mock, |config| config).await;
    assert!(matches!(
        client.pause(&SessionHandle::new(SANDBOX)).await,
        Err(SandboxError::Unsupported { provider, operation }) if provider == "modal" && operation == "pause"
    ));
}

/// The live run against Modal: build, create, exec, files, a timeout, snapshot and resume,
/// teardown, and nothing left running.
#[tokio::test]
#[ignore = "needs MODAL_TOKEN_ID and MODAL_TOKEN_SECRET; runs against Modal"]
async fn live_modal_round_trip() {
    load_repo_env("MODAL_TOKEN_ID");
    load_repo_env("MODAL_TOKEN_SECRET");
    let client = ModalClient::new(
        ModalConfig::from_env()
            .expect("MODAL_TOKEN_ID and MODAL_TOKEN_SECRET")
            .with_timeout(Duration::from_secs(600))
            .with_tag("adk-live", "round-trip"),
    )
    .unwrap();
    let started = Instant::now();
    let handle = client
        .provision(&Manifest::new(vec![ManifestEntry::File {
            path: "hello.py".into(),
            content: b"print('hi from modal')\n".to_vec(),
        }]))
        .await
        .unwrap();
    println!("provisioned {} in {:?}", handle.as_str(), started.elapsed());
    let session = client.start(&handle).await.unwrap();
    let output = session
        .exec(
            CommandRequest::new("python3 hello.py && echo $MARK && echo to-stderr >&2")
                .env("MARK", "marked")
                .timeout(Duration::from_secs(60)),
        )
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0, "{output:?}");
    assert_eq!(output.stdout, "hi from modal\nmarked\n");
    assert_eq!(output.stderr, "to-stderr\n");

    let binary: Vec<u8> = (0..=255u8).cycle().take(64 * 1024).collect();
    session.write_file("out/data.bin", &binary).await.unwrap();
    assert_eq!(session.read_file("out/data.bin").await.unwrap(), binary);
    let names: Vec<_> =
        session.list_dir("out").await.unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["data.bin"]);
    session
        .apply_patch(
            "--- hello.py\n+++ hello.py\n@@ -1 +1 @@\n-print('hi from modal')\n+print('patched')\n",
        )
        .await
        .unwrap();
    assert_eq!(session.exec_command("python3 hello.py", None).await.unwrap().stdout, "patched\n");

    let timed = session
        .exec(
            CommandRequest::new("echo start; sleep 30; echo never").timeout(Duration::from_secs(2)),
        )
        .await
        .unwrap();
    println!("timed-out command: {timed:?}");
    assert!(timed.timed_out && timed.stdout == "start\n", "{timed:?}");

    let snapshot = client.snapshot(&handle).await.unwrap();
    let restored = client.resume(&snapshot).await.unwrap();
    let restored_session = client.start(&restored).await.unwrap();
    assert_eq!(restored_session.read_file("out/data.bin").await.unwrap(), binary);
    println!("snapshot {} restored into {}", snapshot.as_str(), restored.as_str());

    client.stop(&handle).await.unwrap();
    client.stop(&restored).await.unwrap();
    // Termination is asynchronous; the sandboxes leave the running list shortly after.
    let ours = [handle.as_str().to_string(), restored.as_str().to_string()];
    for _ in 0..60 {
        let running = client.running_sandboxes().await.unwrap();
        if !running.iter().any(|id| ours.contains(id)) {
            println!("running in the app after teardown: {running:?}");
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    panic!("the sandboxes were still running two minutes after stop");
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
