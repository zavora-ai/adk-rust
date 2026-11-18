//! E2B: workspaces in Firecracker microVMs (feature `remote-e2b`).
//!
//! The control plane (`https://api.e2b.app`, header `X-API-Key`) creates, connects to,
//! pauses, snapshots and kills sandboxes; its OpenAPI document is
//! <https://github.com/e2b-dev/E2B/blob/main/spec/openapi.yml>. Commands and files go to
//! the `envd` daemon inside each sandbox at `https://49983-{sandboxID}.{domain}`, header
//! `X-Access-Token`: files over plain HTTP
//! (<https://github.com/e2b-dev/E2B/blob/main/spec/envd/envd.yaml>), processes over the
//! Connect protocol with JSON messages
//! (<https://github.com/e2b-dev/E2B/blob/main/spec/envd/process/process.proto>,
//! <https://connectrpc.com/docs/protocol/>).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    COMMAND_GRACE, MAX_FILE_BYTES, MAX_OUTPUT_BYTES, REQUEST_TIMEOUT, Secret, absolute,
    apply_patch_via_exec, capped_text, destroy_in_background, env_var, http_client, json,
    list_dir_via_exec, populate, read_bounded, send,
};
use crate::error::SandboxError;
use crate::workspace::client::SandboxClient;
use crate::workspace::manifest::Manifest;
use crate::workspace::session::SandboxSession;
use crate::workspace::types::{
    ClientCapabilities, CommandRequest, DirEntry, ExecOutput, SessionHandle, SnapshotId,
};

const PROVIDER: &str = "e2b";

/// The E2B control plane.
pub const DEFAULT_API_URL: &str = "https://api.e2b.app";

/// The domain sandboxes are reached under unless the API names another.
pub const DEFAULT_DOMAIN: &str = "e2b.app";

/// The template sandboxes start from unless [`E2bConfig::with_template`] names another.
pub const DEFAULT_TEMPLATE: &str = "base";

/// The port `envd` listens on inside every sandbox.
const ENVD_PORT: u16 = 49983;

/// Largest Connect envelope accepted from `envd`; output beyond the cap is drained, not kept.
const MAX_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;

/// Configuration of an [`E2bClient`].
///
/// # Example
///
/// ```rust
/// use adk_sandbox::E2bConfig;
/// use std::time::Duration;
///
/// let config = E2bConfig::new("e2b_test_key")
///     .with_template("base")
///     .with_timeout(Duration::from_secs(600))
///     .with_internet_access(false);
/// assert!(!format!("{config:?}").contains("e2b_test_key"));
/// ```
#[derive(Debug, Clone)]
pub struct E2bConfig {
    api_key: Secret,
    api_url: String,
    domain: String,
    envd_url: Option<String>,
    template: String,
    timeout: Duration,
    command_timeout: Duration,
    allow_internet_access: bool,
    env: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
    user: String,
    workdir: String,
}

impl E2bConfig {
    /// A configuration for the `base` template with a 15-minute lifetime, internet access,
    /// commands as `user` in `/home/user`, and a 120-second command timeout.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: Secret::new(api_key),
            api_url: DEFAULT_API_URL.to_string(),
            domain: DEFAULT_DOMAIN.to_string(),
            envd_url: None,
            template: DEFAULT_TEMPLATE.to_string(),
            timeout: Duration::from_secs(15 * 60),
            command_timeout: Duration::from_secs(120),
            allow_internet_access: true,
            env: BTreeMap::new(),
            metadata: BTreeMap::new(),
            user: "user".to_string(),
            workdir: "/home/user".to_string(),
        }
    }

    /// Reads the key from `E2B_API_KEY`, and the domain from `E2B_DOMAIN` when set.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::BackendUnavailable` when `E2B_API_KEY` is unset or blank.
    pub fn from_env() -> Result<Self, SandboxError> {
        let config = Self::new(env_var("E2B_API_KEY", "E2B")?);
        Ok(match std::env::var("E2B_DOMAIN") {
            Ok(domain) if !domain.trim().is_empty() => config.with_domain(domain.trim()),
            _ => config,
        })
    }

    /// The control-plane origin, for a self-hosted deployment or a test server.
    #[must_use]
    pub fn with_api_url(mut self, url: impl Into<String>) -> Self {
        self.api_url = url.into().trim_end_matches('/').to_string();
        self
    }

    /// The domain sandboxes are reached under when the API does not name one.
    #[must_use]
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = domain.into();
        self
    }

    /// One `envd` origin for every sandbox instead of `https://49983-{id}.{domain}`, for a
    /// proxy or a test server.
    #[must_use]
    pub fn with_envd_url(mut self, url: impl Into<String>) -> Self {
        self.envd_url = Some(url.into().trim_end_matches('/').to_string());
        self
    }

    /// The template (or snapshot) new sandboxes start from.
    #[must_use]
    pub fn with_template(mut self, template: impl Into<String>) -> Self {
        self.template = template.into();
        self
    }

    /// How long a sandbox lives before E2B kills it, whatever this process does
    /// (`timeout` on create; at least one second).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.max(Duration::from_secs(1));
        self
    }

    /// The timeout of a command that does not set its own.
    #[must_use]
    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Whether sandboxes may reach the internet (`allow_internet_access`).
    #[must_use]
    pub fn with_internet_access(mut self, allow: bool) -> Self {
        self.allow_internet_access = allow;
        self
    }

    /// An environment variable every sandbox starts with.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Metadata attached to every sandbox, which the E2B list API filters on.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// The user commands run as and files are owned by.
    #[must_use]
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// The workspace root inside the sandbox, which relative paths resolve against.
    #[must_use]
    pub fn with_workdir(mut self, workdir: impl Into<String>) -> Self {
        self.workdir = workdir.into();
        self
    }
}

/// `POST /v2/sandboxes` request (`NewSandboxV2`).
#[derive(Serialize)]
struct NewSandbox<'a> {
    #[serde(rename = "templateID")]
    template_id: &'a str,
    timeout: u64,
    #[serde(rename = "allow_internet_access")]
    allow_internet_access: bool,
    #[serde(rename = "envVars", skip_serializing_if = "BTreeMap::is_empty")]
    env_vars: &'a BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    metadata: &'a BTreeMap<String, String>,
}

/// The `Sandbox` schema the create and connect operations answer with.
#[derive(Deserialize)]
struct Sandbox {
    #[serde(rename = "sandboxID")]
    sandbox_id: String,
    #[serde(rename = "envdAccessToken", default)]
    envd_access_token: Option<String>,
    #[serde(default)]
    domain: Option<String>,
}

/// The `SnapshotInfo` schema.
#[derive(Deserialize)]
struct SnapshotInfo {
    #[serde(rename = "snapshotID")]
    snapshot_id: String,
}

/// How to reach one sandbox's `envd`.
#[derive(Clone, Debug)]
struct Connection {
    sandbox_id: String,
    envd_url: String,
    access_token: Option<Secret>,
    /// Created by this client, so dropping the client destroys it.
    owned: bool,
}

struct Inner {
    http: reqwest::Client,
    config: E2bConfig,
}

impl Inner {
    fn api(&self, path: &str) -> String {
        format!("{}{path}", self.config.api_url)
    }

    fn connection(&self, sandbox: Sandbox, owned: bool) -> Connection {
        let envd_url = match &self.config.envd_url {
            Some(url) => url.clone(),
            None => format!(
                "https://{ENVD_PORT}-{}.{}",
                sandbox.sandbox_id,
                sandbox.domain.as_deref().unwrap_or(&self.config.domain)
            ),
        };
        Connection {
            sandbox_id: sandbox.sandbox_id,
            envd_url,
            access_token: sandbox.envd_access_token.map(Secret::new),
            owned,
        }
    }

    async fn create(&self, template: &str) -> Result<Connection, SandboxError> {
        let body = NewSandbox {
            template_id: template,
            timeout: self.config.timeout.as_secs(),
            allow_internet_access: self.config.allow_internet_access,
            env_vars: &self.config.env,
            metadata: &self.config.metadata,
        };
        let request = self
            .http
            .post(self.api("/v2/sandboxes"))
            .header("X-API-Key", self.config.api_key.expose())
            .timeout(REQUEST_TIMEOUT)
            .json(&body);
        let response = send(request, PROVIDER, "create sandbox").await?;
        let sandbox: Sandbox = json(response, PROVIDER, "create sandbox").await?;
        Ok(self.connection(sandbox, true))
    }

    async fn kill(&self, sandbox_id: &str) -> Result<(), SandboxError> {
        let request = self
            .http
            .delete(self.api(&format!("/sandboxes/{sandbox_id}")))
            .header("X-API-Key", self.config.api_key.expose())
            .timeout(REQUEST_TIMEOUT);
        match send(request, PROVIDER, "kill sandbox").await {
            Ok(_) => Ok(()),
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                Err(SandboxError::SessionNotFound { handle: sandbox_id.to_string() })
            }
            Err(error) => Err(error),
        }
    }
}

/// A [`SandboxClient`] over E2B sandboxes: each provisioned workspace is a Firecracker
/// microVM created from [`E2bConfig`]'s template with a provider-side lifetime.
///
/// | Operation | E2B API |
/// |-----------|---------|
/// | `provision` | `POST /v2/sandboxes`, then the manifest through `envd` |
/// | `start` | the sandbox this client created, or `POST /v2/sandboxes/{id}/connect` for another, which resumes a paused one |
/// | `exec` | `envd` `process.Process/Start`; a timeout sends `SIGKILL` through `process.Process/SendSignal` |
/// | `read_file`, `write_file` | `envd` `GET`/`POST /files` |
/// | `pause` | `POST /sandboxes/{id}/pause` |
/// | `snapshot` | `POST /sandboxes/{id}/snapshots`; `resume` creates a sandbox from the snapshot |
/// | `stop` | `DELETE /sandboxes/{id}` |
///
/// Dropping the client kills every sandbox it created and did not stop.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_sandbox::workspace::{CommandRequest, Manifest, SandboxClient};
/// use adk_sandbox::{E2bClient, E2bConfig};
///
/// # async fn run() -> Result<(), adk_sandbox::SandboxError> {
/// let client = E2bClient::new(E2bConfig::from_env()?)?;
/// let handle = client.provision(&Manifest::new(vec![])).await?;
/// let session = client.start(&handle).await?;
/// let output = session.exec(CommandRequest::new("uname -a")).await?;
/// println!("{}", output.stdout);
/// client.stop(&handle).await?;
/// # Ok(())
/// # }
/// ```
pub struct E2bClient {
    inner: Arc<Inner>,
    live: Arc<Mutex<HashMap<String, Connection>>>,
}

impl E2bClient {
    /// A client for `config`.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::Remote` when the HTTP client cannot be built.
    pub fn new(config: E2bConfig) -> Result<Self, SandboxError> {
        Ok(Self {
            inner: Arc::new(Inner { http: http_client(PROVIDER)?, config }),
            live: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn known(&self, id: &str) -> Option<Connection> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    fn session(&self, connection: Connection) -> E2bSession {
        E2bSession { inner: self.inner.clone(), connection }
    }

    async fn provision_from(
        &self,
        template: &str,
        manifest: &Manifest,
    ) -> Result<SessionHandle, SandboxError> {
        let connection = self.inner.create(template).await?;
        let id = connection.sandbox_id.clone();
        self.live.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), connection.clone());
        if let Err(error) = populate(&self.session(connection), manifest).await {
            self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            if let Err(kill) = self.inner.kill(&id).await {
                tracing::warn!(sandbox.id = %id, error = %kill, "could not kill a sandbox whose manifest failed");
            }
            return Err(error);
        }
        tracing::info!(sandbox.provider = PROVIDER, sandbox.id = %id, template, "created a sandbox");
        Ok(SessionHandle::new(id))
    }
}

impl std::fmt::Debug for E2bClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2bClient").field("config", &self.inner.config).finish_non_exhaustive()
    }
}

impl Drop for E2bClient {
    fn drop(&mut self) {
        let owned: Vec<String> = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .filter(|(_, connection)| connection.owned)
            .map(|(id, _)| id)
            .collect();
        let inner = self.inner.clone();
        destroy_in_background(PROVIDER, owned, move |id| {
            let inner = inner.clone();
            async move { inner.kill(&id).await }
        });
    }
}

#[async_trait]
impl SandboxClient for E2bClient {
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        self.provision_from(&self.inner.config.template, manifest).await
    }

    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError> {
        if let Some(connection) = self.known(handle.as_str()) {
            return Ok(Box::new(self.session(connection)));
        }
        let request = self
            .inner
            .http
            .post(self.inner.api(&format!("/v2/sandboxes/{}/connect", handle.as_str())))
            .header("X-API-Key", self.inner.config.api_key.expose())
            .timeout(REQUEST_TIMEOUT)
            .json(&json!({ "timeout": self.inner.config.timeout.as_secs() }));
        let response = match send(request, PROVIDER, "connect sandbox").await {
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                return Err(SandboxError::SessionNotFound { handle: handle.as_str().to_string() });
            }
            other => other?,
        };
        let sandbox: Sandbox = json(response, PROVIDER, "connect sandbox").await?;
        let connection = self.inner.connection(sandbox, false);
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.as_str().to_string(), connection.clone());
        Ok(Box::new(self.session(connection)))
    }

    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(handle.as_str());
        self.inner.kill(handle.as_str()).await
    }

    async fn snapshot(&self, handle: &SessionHandle) -> Result<SnapshotId, SandboxError> {
        let request = self
            .inner
            .http
            .post(self.inner.api(&format!("/sandboxes/{}/snapshots", handle.as_str())))
            .header("X-API-Key", self.inner.config.api_key.expose())
            .timeout(REQUEST_TIMEOUT * 5)
            .json(&json!({}));
        let response = match send(request, PROVIDER, "create snapshot").await {
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                return Err(SandboxError::SessionNotFound { handle: handle.as_str().to_string() });
            }
            other => other?,
        };
        let info: SnapshotInfo = json(response, PROVIDER, "create snapshot").await?;
        Ok(SnapshotId::new(info.snapshot_id))
    }

    async fn resume(&self, snapshot_id: &SnapshotId) -> Result<SessionHandle, SandboxError> {
        match self.provision_from(snapshot_id.as_str(), &Manifest::new(Vec::new())).await {
            Err(SandboxError::Remote { status: Some(400 | 404), .. }) => {
                Err(SandboxError::SnapshotNotFound { id: snapshot_id.as_str().to_string() })
            }
            other => other,
        }
    }

    fn provider(&self) -> &str {
        PROVIDER
    }

    fn capabilities(&self) -> ClientCapabilities {
        ClientCapabilities::default()
            .with_snapshot(true)
            .with_pause(true)
            .with_remote(true)
            .with_network_blocked(!self.inner.config.allow_internet_access)
            .with_max_lifetime(self.inner.config.timeout)
    }

    async fn pause(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        let request = self
            .inner
            .http
            .post(self.inner.api(&format!("/sandboxes/{}/pause", handle.as_str())))
            .header("X-API-Key", self.inner.config.api_key.expose())
            .timeout(REQUEST_TIMEOUT * 2)
            .json(&json!({}));
        match send(request, PROVIDER, "pause sandbox").await {
            Ok(_) => Ok(()),
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                Err(SandboxError::SessionNotFound { handle: handle.as_str().to_string() })
            }
            Err(error) => Err(error),
        }
    }
}

/// A live E2B sandbox: commands and files through its `envd` daemon.
pub struct E2bSession {
    inner: Arc<Inner>,
    connection: Connection,
}

impl std::fmt::Debug for E2bSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2bSession")
            .field("sandbox_id", &self.connection.sandbox_id)
            .field("envd_url", &self.connection.envd_url)
            .finish_non_exhaustive()
    }
}

impl E2bSession {
    /// The E2B sandbox id.
    pub fn sandbox_id(&self) -> &str {
        &self.connection.sandbox_id
    }

    /// A request to `envd` carrying the sandbox's routing and access headers.
    fn envd(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .inner
            .http
            .request(method, format!("{}{path}", self.connection.envd_url))
            .header("E2b-Sandbox-Id", &self.connection.sandbox_id)
            .header("E2b-Sandbox-Port", ENVD_PORT.to_string());
        if let Some(token) = &self.connection.access_token {
            request = request.header("X-Access-Token", token.expose());
        }
        request
    }

    /// `envd` authenticates the process user with HTTP Basic and an empty password.
    fn user_auth(&self) -> String {
        format!("Basic {}", STANDARD.encode(format!("{}:", self.inner.config.user)))
    }

    /// Ends the process `pid` with `SIGKILL`; best effort, since the command already
    /// timed out.
    async fn kill(&self, pid: u64) {
        let request = self
            .envd(reqwest::Method::POST, "/process.Process/SendSignal")
            .header("Authorization", self.user_auth())
            .header("Connect-Protocol-Version", "1")
            .timeout(REQUEST_TIMEOUT)
            .json(&json!({ "process": { "pid": pid }, "signal": "SIGNAL_SIGKILL" }));
        if let Err(error) = send(request, PROVIDER, "signal process").await {
            tracing::warn!(sandbox.id = %self.connection.sandbox_id, pid, error = %error, "could not kill a timed-out command");
        }
    }
}

/// Splits a Connect streaming body into its envelopes: one flags byte, a big-endian `u32`
/// length, then the message.
#[derive(Default)]
struct Envelopes {
    buffer: Vec<u8>,
}

impl Envelopes {
    /// The end-of-stream flag of a Connect envelope.
    const END_STREAM: u8 = 0x02;

    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    fn pop(&mut self) -> Result<Option<(u8, Vec<u8>)>, SandboxError> {
        if self.buffer.len() < 5 {
            return Ok(None);
        }
        let length =
            u32::from_be_bytes([self.buffer[1], self.buffer[2], self.buffer[3], self.buffer[4]])
                as usize;
        if length > MAX_ENVELOPE_BYTES {
            return Err(remote_exec_error(format!(
                "a message of {length} bytes exceeds the {MAX_ENVELOPE_BYTES}-byte limit"
            )));
        }
        if self.buffer.len() < 5 + length {
            return Ok(None);
        }
        let flags = self.buffer[0];
        let message = self.buffer[5..5 + length].to_vec();
        self.buffer.drain(..5 + length);
        Ok(Some((flags, message)))
    }
}

fn remote_exec_error(message: impl Into<String>) -> SandboxError {
    SandboxError::Remote {
        provider: PROVIDER.to_string(),
        operation: "run command".to_string(),
        status: None,
        message: message.into(),
    }
}

/// A command's output as `envd` streams it.
#[derive(Default)]
struct Output {
    pid: Option<u64>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    discarded_stdout: bool,
    discarded_stderr: bool,
    exit_code: Option<i32>,
}

impl Output {
    fn append(
        buffer: &mut Vec<u8>,
        discarded: &mut bool,
        encoded: &str,
    ) -> Result<(), SandboxError> {
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|error| remote_exec_error(format!("undecodable output: {error}")))?;
        let room = MAX_OUTPUT_BYTES.saturating_sub(buffer.len());
        if bytes.len() > room {
            *discarded = true;
        }
        buffer.extend_from_slice(&bytes[..bytes.len().min(room)]);
        Ok(())
    }

    /// Applies one `StartResponse` message: `{"event": {"start" | "data" | "end" | "keepalive": ...}}`.
    fn apply(&mut self, message: &Value) -> Result<(), SandboxError> {
        let Some(event) = message.get("event") else {
            return Ok(());
        };
        if let Some(pid) = event.pointer("/start/pid").and_then(Value::as_u64) {
            self.pid = Some(pid);
        }
        if let Some(data) = event.get("data") {
            if let Some(stdout) = data.get("stdout").and_then(Value::as_str) {
                Self::append(&mut self.stdout, &mut self.discarded_stdout, stdout)?;
            }
            if let Some(stderr) = data.get("stderr").and_then(Value::as_str) {
                Self::append(&mut self.stderr, &mut self.discarded_stderr, stderr)?;
            }
        }
        if let Some(end) = event.get("end") {
            // Proto3 JSON omits zero values, so a missing exit code is 0.
            let code = end.get("exitCode").and_then(Value::as_i64).unwrap_or(0);
            self.exit_code = Some(i32::try_from(code).unwrap_or(-1));
            if let Some(error) = end.get("error").and_then(Value::as_str)
                && !error.is_empty()
            {
                if !self.stderr.is_empty() {
                    self.stderr.push(b'\n');
                }
                self.stderr.extend_from_slice(error.as_bytes());
            }
        }
        Ok(())
    }
}

#[async_trait]
impl SandboxSession for E2bSession {
    async fn exec_command(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Result<ExecOutput, SandboxError> {
        let mut request = CommandRequest::new(command);
        request.working_dir = working_dir.map(str::to_string);
        self.exec(request).await
    }

    async fn exec(&self, request: CommandRequest) -> Result<ExecOutput, SandboxError> {
        let workdir = &self.inner.config.workdir;
        let cwd = match request.working_dir.as_deref() {
            Some(dir) => absolute(workdir, dir)?,
            None => workdir.clone(),
        };
        for key in request.env.keys() {
            crate::workspace::session::validate_env_name(key)?;
        }
        let timeout = request.timeout.unwrap_or(self.inner.config.command_timeout);
        let message = json!({
            "process": {
                "cmd": "/bin/bash",
                "args": ["-l", "-c", request.command],
                "envs": request.env,
                "cwd": cwd,
            },
            "stdin": false,
        });
        let payload = serde_json::to_vec(&message)
            .map_err(|error| remote_exec_error(format!("cannot encode the request: {error}")))?;
        let mut body = Vec::with_capacity(payload.len() + 5);
        body.push(0);
        body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        body.extend_from_slice(&payload);
        let deadline = timeout + COMMAND_GRACE;

        let started = Instant::now();
        let http = self
            .envd(reqwest::Method::POST, "/process.Process/Start")
            .header("Content-Type", "application/connect+json")
            .header("Connect-Protocol-Version", "1")
            .header("Connect-Timeout-Ms", deadline.as_millis().to_string())
            .header("Keepalive-Ping-Interval", "50")
            .header("Authorization", self.user_auth())
            .timeout(deadline)
            .body(body);

        let mut output = Output::default();
        let streamed = tokio::time::timeout(timeout, async {
            let response = send(http, PROVIDER, "run command").await?;
            let mut stream = response.bytes_stream();
            let mut envelopes = Envelopes::default();
            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|error| remote_exec_error(error.without_url().to_string()))?;
                envelopes.push(&chunk);
                while let Some((flags, message)) = envelopes.pop()? {
                    let message: Value = serde_json::from_slice(&message).map_err(|error| {
                        remote_exec_error(format!("unexpected message: {error}"))
                    })?;
                    if flags & Envelopes::END_STREAM != 0 {
                        if let Some(error) = message.get("error") {
                            return Err(remote_exec_error(format!(
                                "{}: {}",
                                error.get("code").and_then(Value::as_str).unwrap_or("unknown"),
                                error.get("message").and_then(Value::as_str).unwrap_or_default()
                            )));
                        }
                        return Ok(());
                    }
                    output.apply(&message)?;
                }
            }
            Ok(())
        })
        .await;

        match streamed {
            Err(_elapsed) => {
                if let Some(pid) = output.pid {
                    self.kill(pid).await;
                }
                Ok(ExecOutput::new(
                    capped_text(output.stdout, output.discarded_stdout),
                    capped_text(output.stderr, output.discarded_stderr),
                    -1,
                    started.elapsed(),
                    true,
                ))
            }
            Ok(Err(error)) => Err(error),
            Ok(Ok(())) => {
                let exit_code = output
                    .exit_code
                    .ok_or_else(|| remote_exec_error("the stream ended before the command did"))?;
                Ok(ExecOutput::new(
                    capped_text(output.stdout, output.discarded_stdout),
                    capped_text(output.stderr, output.discarded_stderr),
                    exit_code,
                    started.elapsed(),
                    false,
                ))
            }
        }
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        let full = absolute(&self.inner.config.workdir, path)?;
        let request = self
            .envd(reqwest::Method::GET, "/files")
            .query(&[("path", full.as_str()), ("username", self.inner.config.user.as_str())])
            .timeout(REQUEST_TIMEOUT);
        let response = match send(request, PROVIDER, "read file").await {
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                return Err(SandboxError::ExecutionFailed(format!("file not found: {path}")));
            }
            other => other?,
        };
        read_bounded(response, MAX_FILE_BYTES, PROVIDER, "read file", false).await
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), SandboxError> {
        let full = absolute(&self.inner.config.workdir, path)?;
        if content.len() > MAX_FILE_BYTES {
            return Err(SandboxError::InvalidRequest(format!(
                "{path} is {} bytes; the limit is {MAX_FILE_BYTES}",
                content.len()
            )));
        }
        let request = self
            .envd(reqwest::Method::POST, "/files")
            .query(&[("path", full.as_str()), ("username", self.inner.config.user.as_str())])
            .header("Content-Type", "application/octet-stream")
            .timeout(REQUEST_TIMEOUT)
            .body(content.to_vec());
        send(request, PROVIDER, "write file").await.map(drop)
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        list_dir_via_exec(self, path).await
    }

    async fn apply_patch(&self, patch: &str) -> Result<(), SandboxError> {
        apply_patch_via_exec(self, patch).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(flags: u8, message: &Value) -> Vec<u8> {
        let payload = serde_json::to_vec(message).unwrap();
        let mut out = vec![flags];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
        out
    }

    #[test]
    fn envelopes_split_across_chunks() {
        let mut bytes = frame(0, &json!({"event": {"start": {"pid": 7}}}));
        bytes.extend(frame(2, &json!({})));
        let mut envelopes = Envelopes::default();
        envelopes.push(&bytes[..3]);
        assert!(envelopes.pop().unwrap().is_none());
        envelopes.push(&bytes[3..]);
        let (flags, first) = envelopes.pop().unwrap().unwrap();
        assert_eq!(flags, 0);
        assert_eq!(serde_json::from_slice::<Value>(&first).unwrap()["event"]["start"]["pid"], 7);
        let (flags, _) = envelopes.pop().unwrap().unwrap();
        assert_eq!(flags & Envelopes::END_STREAM, Envelopes::END_STREAM);
        assert!(envelopes.pop().unwrap().is_none());
    }

    #[test]
    fn output_decodes_events_and_defaults_a_zero_exit_code() {
        let mut output = Output::default();
        output.apply(&json!({"event": {"start": {"pid": 12}}})).unwrap();
        output.apply(&json!({"event": {"data": {"stdout": STANDARD.encode("hi\n")}}})).unwrap();
        output.apply(&json!({"event": {"data": {"stderr": STANDARD.encode("warn\n")}}})).unwrap();
        output.apply(&json!({"event": {"keepalive": {}}})).unwrap();
        output
            .apply(&json!({"event": {"end": {"exited": true, "status": "exit status 0"}}}))
            .unwrap();
        assert_eq!(output.pid, Some(12));
        assert_eq!(output.stdout, b"hi\n");
        assert_eq!(output.stderr, b"warn\n");
        assert_eq!(output.exit_code, Some(0));
    }

    #[test]
    fn output_is_capped() {
        let mut output = Output::default();
        let chunk = STANDARD.encode(vec![b'x'; MAX_OUTPUT_BYTES]);
        output.apply(&json!({"event": {"data": {"stdout": chunk}}})).unwrap();
        output.apply(&json!({"event": {"data": {"stdout": STANDARD.encode("more")}}})).unwrap();
        assert_eq!(output.stdout.len(), MAX_OUTPUT_BYTES);
        assert!(output.discarded_stdout);
    }
}
