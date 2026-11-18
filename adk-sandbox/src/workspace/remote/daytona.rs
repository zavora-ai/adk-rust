//! Daytona: workspaces in Daytona sandboxes (feature `remote-daytona`).
//!
//! The control plane (`https://app.daytona.io/api`, `Authorization: Bearer`) creates,
//! starts, pauses, snapshots and deletes sandboxes; its OpenAPI document is
//! <https://www.daytona.io/docs/openapi.json>. Commands and files go to the toolbox API at
//! `{toolboxProxyUrl}/{sandboxId}` with the same credentials
//! (<https://www.daytona.io/docs/toolbox-openapi.json>,
//! <https://www.daytona.io/docs/en/tools/api/>).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    COMMAND_GRACE, MAX_FILE_BYTES, REQUEST_TIMEOUT, Secret, absolute, apply_patch_via_exec,
    capped_text, destroy_in_background, env_var, http_client, json, list_dir_via_exec, populate,
    read_bounded, send,
};
use crate::error::SandboxError;
use crate::workspace::client::SandboxClient;
use crate::workspace::manifest::Manifest;
use crate::workspace::session::{SandboxSession, validate_env_name};
use crate::workspace::types::{
    ClientCapabilities, CommandRequest, DirEntry, ExecOutput, SessionHandle, SnapshotId,
};

const PROVIDER: &str = "daytona";

/// The Daytona control plane.
pub const DEFAULT_API_URL: &str = "https://app.daytona.io/api";

/// How often a sandbox's state is polled while it starts.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Configuration of a [`DaytonaClient`].
///
/// # Example
///
/// ```rust
/// use adk_sandbox::DaytonaConfig;
/// use std::time::Duration;
///
/// let config = DaytonaConfig::new("dtn_test_key")
///     .with_target("us")
///     .with_ttl(Duration::from_secs(30 * 60))
///     .with_network_blocked(true);
/// assert!(!format!("{config:?}").contains("dtn_test_key"));
/// ```
#[derive(Debug, Clone)]
pub struct DaytonaConfig {
    api_key: Secret,
    api_url: String,
    organization_id: Option<String>,
    toolbox_url: Option<String>,
    snapshot: Option<String>,
    target: Option<String>,
    env: BTreeMap<String, String>,
    labels: BTreeMap<String, String>,
    network_blocked: bool,
    auto_stop: Duration,
    ttl: Duration,
    start_timeout: Duration,
    command_timeout: Duration,
    workdir: String,
}

impl DaytonaConfig {
    /// A configuration for Daytona's default snapshot: a one-hour lifetime, stopped after 15
    /// idle minutes and deleted when stopped, network open, commands in `/home/daytona` with a
    /// 120-second timeout.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: Secret::new(api_key),
            api_url: DEFAULT_API_URL.to_string(),
            organization_id: None,
            toolbox_url: None,
            snapshot: None,
            target: None,
            env: BTreeMap::new(),
            labels: BTreeMap::new(),
            network_blocked: false,
            auto_stop: Duration::from_secs(15 * 60),
            ttl: Duration::from_secs(60 * 60),
            start_timeout: Duration::from_secs(120),
            command_timeout: Duration::from_secs(120),
            workdir: "/home/daytona".to_string(),
        }
    }

    /// Reads the key from `DAYTONA_API_KEY`, and `DAYTONA_API_URL`,
    /// `DAYTONA_ORGANIZATION_ID` and `DAYTONA_TARGET` when set, as the Daytona SDKs do.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::BackendUnavailable` when `DAYTONA_API_KEY` is unset or blank.
    pub fn from_env() -> Result<Self, SandboxError> {
        let mut config = Self::new(env_var("DAYTONA_API_KEY", "Daytona")?);
        let read = |key: &str| {
            std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        };
        if let Some(url) = read("DAYTONA_API_URL") {
            config = config.with_api_url(url);
        }
        config.organization_id = read("DAYTONA_ORGANIZATION_ID");
        config.target = read("DAYTONA_TARGET");
        Ok(config)
    }

    /// The control-plane origin, for a self-hosted deployment or a test server.
    #[must_use]
    pub fn with_api_url(mut self, url: impl Into<String>) -> Self {
        self.api_url = url.into().trim_end_matches('/').to_string();
        self
    }

    /// The organization requests act for (`X-Daytona-Organization-ID`).
    #[must_use]
    pub fn with_organization(mut self, organization_id: impl Into<String>) -> Self {
        self.organization_id = Some(organization_id.into());
        self
    }

    /// One toolbox origin for every sandbox instead of the sandbox's `toolboxProxyUrl`, for
    /// a test server. Requests go to `{url}/{sandboxId}/...`.
    #[must_use]
    pub fn with_toolbox_url(mut self, url: impl Into<String>) -> Self {
        self.toolbox_url = Some(url.into().trim_end_matches('/').to_string());
        self
    }

    /// The snapshot (image) new sandboxes start from; Daytona's default when unset.
    #[must_use]
    pub fn with_snapshot(mut self, snapshot: impl Into<String>) -> Self {
        self.snapshot = Some(snapshot.into());
        self
    }

    /// The region sandboxes are created in.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// An environment variable every sandbox starts with.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// A label attached to every sandbox.
    #[must_use]
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    /// Blocks all network access from sandboxes (`networkBlockAll`).
    #[must_use]
    pub fn with_network_blocked(mut self, blocked: bool) -> Self {
        self.network_blocked = blocked;
        self
    }

    /// How long an idle sandbox runs before Daytona stops it, and so deletes it
    /// (`autoStopInterval`, whole minutes, at least one).
    #[must_use]
    pub fn with_auto_stop(mut self, idle: Duration) -> Self {
        self.auto_stop = idle;
        self
    }

    /// How long a sandbox lives before Daytona destroys it, whatever its state
    /// (`ttlMinutes`, whole minutes, at least one).
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// How long `provision` waits for a new sandbox to reach `started`.
    #[must_use]
    pub fn with_start_timeout(mut self, timeout: Duration) -> Self {
        self.start_timeout = timeout;
        self
    }

    /// The timeout of a command that does not set its own.
    #[must_use]
    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// The workspace root inside the sandbox, which relative paths resolve against.
    #[must_use]
    pub fn with_workdir(mut self, workdir: impl Into<String>) -> Self {
        self.workdir = workdir.into();
        self
    }
}

/// Whole minutes of `duration`, rounded up, at least one.
fn minutes(duration: Duration) -> u64 {
    duration.as_secs().div_ceil(60).max(1)
}

/// `POST /sandbox` request (`CreateSandbox`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSandbox<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: &'a BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    labels: &'a BTreeMap<String, String>,
    network_block_all: bool,
    auto_stop_interval: u64,
    /// 0: deleted as soon as it stops, so an idle sandbox costs nothing afterwards.
    auto_delete_interval: i64,
    ttl_minutes: u64,
}

/// The `Sandbox` schema.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sandbox {
    id: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    toolbox_proxy_url: Option<String>,
    #[serde(default)]
    error_reason: Option<String>,
}

/// The `ExecuteResponse` schema of the toolbox `process/execute` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteResponse {
    // Some daemon versions name it `code`, which the Daytona SDKs also read.
    #[serde(default, alias = "code")]
    exit_code: Option<i32>,
    #[serde(default)]
    result: String,
}

#[derive(Clone, Debug)]
struct Connection {
    sandbox_id: String,
    toolbox: String,
    /// Created by this client, so dropping the client deletes it.
    owned: bool,
}

struct Inner {
    http: reqwest::Client,
    config: DaytonaConfig,
}

impl Inner {
    fn api(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.authorized(self.http.request(method, format!("{}{path}", self.config.api_url)))
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = request.bearer_auth(self.config.api_key.expose());
        match &self.config.organization_id {
            Some(organization) => request.header("X-Daytona-Organization-ID", organization),
            None => request,
        }
    }

    fn connection(&self, sandbox: &Sandbox, owned: bool) -> Result<Connection, SandboxError> {
        let base = match (&self.config.toolbox_url, &sandbox.toolbox_proxy_url) {
            (Some(url), _) => url.clone(),
            (None, Some(url)) => url.trim_end_matches('/').to_string(),
            (None, None) => {
                return Err(SandboxError::Remote {
                    provider: PROVIDER.to_string(),
                    operation: "create sandbox".to_string(),
                    status: None,
                    message: "the sandbox has no toolboxProxyUrl".to_string(),
                });
            }
        };
        Ok(Connection {
            sandbox_id: sandbox.id.clone(),
            toolbox: format!("{base}/{}", sandbox.id),
            owned,
        })
    }

    async fn get(&self, id: &str) -> Result<Sandbox, SandboxError> {
        let request =
            self.api(reqwest::Method::GET, &format!("/sandbox/{id}")).timeout(REQUEST_TIMEOUT);
        match send(request, PROVIDER, "get sandbox").await {
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                Err(SandboxError::SessionNotFound { handle: id.to_string() })
            }
            other => json(other?, PROVIDER, "get sandbox").await,
        }
    }

    /// Polls `sandbox` until it is `started`, for at most the start timeout.
    async fn wait_started(&self, mut sandbox: Sandbox) -> Result<Sandbox, SandboxError> {
        let deadline = Instant::now() + self.config.start_timeout;
        loop {
            match sandbox.state.as_deref() {
                Some("started") => return Ok(sandbox),
                Some(state @ ("error" | "build_failed" | "destroyed" | "destroying")) => {
                    return Err(SandboxError::ProvisionFailed {
                        resource: sandbox.id.clone(),
                        reason: format!(
                            "the sandbox is {state}{}",
                            sandbox.error_reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()
                        ),
                        suggestion: "Check the snapshot and the organization's quota in the Daytona dashboard.".to_string(),
                    });
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(SandboxError::ProvisionFailed {
                    resource: sandbox.id.clone(),
                    reason: format!(
                        "the sandbox did not start within {:?} (state {})",
                        self.config.start_timeout,
                        sandbox.state.as_deref().unwrap_or("unknown")
                    ),
                    suggestion: "Raise DaytonaConfig::with_start_timeout, or use a snapshot that is already built.".to_string(),
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
            sandbox = self.get(&sandbox.id).await?;
        }
    }

    async fn create(&self, snapshot: Option<&str>) -> Result<Connection, SandboxError> {
        let body = CreateSandbox {
            snapshot,
            target: self.config.target.as_deref(),
            env: &self.config.env,
            labels: &self.config.labels,
            network_block_all: self.config.network_blocked,
            auto_stop_interval: minutes(self.config.auto_stop),
            auto_delete_interval: 0,
            ttl_minutes: minutes(self.config.ttl),
        };
        let request =
            self.api(reqwest::Method::POST, "/sandbox").timeout(REQUEST_TIMEOUT).json(&body);
        let response = send(request, PROVIDER, "create sandbox").await?;
        let sandbox: Sandbox = json(response, PROVIDER, "create sandbox").await?;
        let id = sandbox.id.clone();
        match self.wait_started(sandbox).await {
            Ok(sandbox) => self.connection(&sandbox, true),
            Err(error) => {
                if let Err(delete) = self.delete(&id).await {
                    tracing::warn!(sandbox.id = %id, error = %delete, "could not delete a sandbox that failed to start");
                }
                Err(error)
            }
        }
    }

    async fn delete(&self, id: &str) -> Result<(), SandboxError> {
        let request =
            self.api(reqwest::Method::DELETE, &format!("/sandbox/{id}")).timeout(REQUEST_TIMEOUT);
        match send(request, PROVIDER, "delete sandbox").await {
            Ok(_) => Ok(()),
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                Err(SandboxError::SessionNotFound { handle: id.to_string() })
            }
            Err(error) => Err(error),
        }
    }
}

/// A [`SandboxClient`] over Daytona sandboxes: each provisioned workspace is a sandbox
/// created from [`DaytonaConfig`] with a time to live, an idle auto-stop and delete-on-stop.
///
/// | Operation | Daytona API |
/// |-----------|-------------|
/// | `provision` | `POST /sandbox`, polling `GET /sandbox/{id}` until `started`, then the manifest through the toolbox |
/// | `start` | the sandbox this client created, or `POST /sandbox/{id}/start` for another, which resumes a stopped or paused one |
/// | `exec` | toolbox `POST /process/execute` with `cwd`, `envs` and `timeout`; stdout and stderr arrive combined, as `stdout` |
/// | `read_file`, `write_file` | toolbox `GET /files/download`, `POST /files/upload-v2` |
/// | `pause` | `POST /sandbox/{id}/pause` (sandbox classes that support it) |
/// | `snapshot` | `POST /sandbox/{id}/snapshot`; `resume` creates a sandbox from the snapshot |
/// | `stop` | `DELETE /sandbox/{id}` |
///
/// Dropping the client deletes every sandbox it created and did not stop.
///
/// # Example
///
/// ```rust,no_run
/// use adk_sandbox::workspace::{CommandRequest, Manifest, SandboxClient};
/// use adk_sandbox::{DaytonaClient, DaytonaConfig};
///
/// # async fn run() -> Result<(), adk_sandbox::SandboxError> {
/// let client = DaytonaClient::new(DaytonaConfig::from_env()?)?;
/// let handle = client.provision(&Manifest::new(vec![])).await?;
/// let session = client.start(&handle).await?;
/// let output = session.exec(CommandRequest::new("python3 --version")).await?;
/// println!("{}", output.stdout);
/// client.stop(&handle).await?;
/// # Ok(())
/// # }
/// ```
pub struct DaytonaClient {
    inner: Arc<Inner>,
    live: Arc<Mutex<HashMap<String, Connection>>>,
}

impl DaytonaClient {
    /// A client for `config`.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::Remote` when the HTTP client cannot be built.
    pub fn new(config: DaytonaConfig) -> Result<Self, SandboxError> {
        Ok(Self {
            inner: Arc::new(Inner { http: http_client(PROVIDER)?, config }),
            live: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn known(&self, id: &str) -> Option<Connection> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    fn session(&self, connection: Connection) -> DaytonaSession {
        DaytonaSession { inner: self.inner.clone(), connection }
    }

    async fn provision_from(
        &self,
        snapshot: Option<&str>,
        manifest: &Manifest,
    ) -> Result<SessionHandle, SandboxError> {
        let connection = self.inner.create(snapshot).await?;
        let id = connection.sandbox_id.clone();
        self.live.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), connection.clone());
        if let Err(error) = populate(&self.session(connection), manifest).await {
            self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            if let Err(delete) = self.inner.delete(&id).await {
                tracing::warn!(sandbox.id = %id, error = %delete, "could not delete a sandbox whose manifest failed");
            }
            return Err(error);
        }
        tracing::info!(sandbox.provider = PROVIDER, sandbox.id = %id, "created a sandbox");
        Ok(SessionHandle::new(id))
    }
}

impl std::fmt::Debug for DaytonaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaytonaClient").field("config", &self.inner.config).finish_non_exhaustive()
    }
}

impl Drop for DaytonaClient {
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
            async move { inner.delete(&id).await }
        });
    }
}

#[async_trait]
impl SandboxClient for DaytonaClient {
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        let snapshot = self.inner.config.snapshot.clone();
        self.provision_from(snapshot.as_deref(), manifest).await
    }

    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError> {
        if let Some(connection) = self.known(handle.as_str()) {
            return Ok(Box::new(self.session(connection)));
        }
        let mut sandbox = self.inner.get(handle.as_str()).await?;
        if sandbox.state.as_deref() != Some("started") {
            let request = self
                .inner
                .api(reqwest::Method::POST, &format!("/sandbox/{}/start", handle.as_str()))
                .timeout(REQUEST_TIMEOUT);
            let response = send(request, PROVIDER, "start sandbox").await?;
            sandbox =
                self.inner.wait_started(json(response, PROVIDER, "start sandbox").await?).await?;
        }
        let connection = self.inner.connection(&sandbox, false)?;
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.as_str().to_string(), connection.clone());
        Ok(Box::new(self.session(connection)))
    }

    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(handle.as_str());
        self.inner.delete(handle.as_str()).await
    }

    async fn snapshot(&self, handle: &SessionHandle) -> Result<SnapshotId, SandboxError> {
        let name = format!("adk-{}-{}", handle.as_str(), uuid::Uuid::new_v4().simple());
        let request = self
            .inner
            .api(reqwest::Method::POST, &format!("/sandbox/{}/snapshot", handle.as_str()))
            .timeout(REQUEST_TIMEOUT)
            .json(&json!({ "name": name }));
        let response = match send(request, PROVIDER, "create snapshot").await {
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                return Err(SandboxError::SessionNotFound { handle: handle.as_str().to_string() });
            }
            other => other?,
        };
        // Snapshotting runs in the background; the sandbox is usable again once it is back
        // to `started`.
        let sandbox: Sandbox = json(response, PROVIDER, "create snapshot").await?;
        self.inner.wait_started(sandbox).await?;
        Ok(SnapshotId::new(name))
    }

    async fn resume(&self, snapshot_id: &SnapshotId) -> Result<SessionHandle, SandboxError> {
        match self.provision_from(Some(snapshot_id.as_str()), &Manifest::new(Vec::new())).await {
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
            .with_network_blocked(self.inner.config.network_blocked)
            .with_max_lifetime(Duration::from_secs(minutes(self.inner.config.ttl) * 60))
    }

    async fn pause(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        let request = self
            .inner
            .api(reqwest::Method::POST, &format!("/sandbox/{}/pause", handle.as_str()))
            .timeout(REQUEST_TIMEOUT);
        match send(request, PROVIDER, "pause sandbox").await {
            Ok(_) => Ok(()),
            Err(SandboxError::Remote { status: Some(404), .. }) => {
                Err(SandboxError::SessionNotFound { handle: handle.as_str().to_string() })
            }
            Err(error) => Err(error),
        }
    }
}

/// A live Daytona sandbox: commands and files through its toolbox API.
pub struct DaytonaSession {
    inner: Arc<Inner>,
    connection: Connection,
}

impl std::fmt::Debug for DaytonaSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaytonaSession")
            .field("sandbox_id", &self.connection.sandbox_id)
            .field("toolbox", &self.connection.toolbox)
            .finish_non_exhaustive()
    }
}

impl DaytonaSession {
    /// The Daytona sandbox id.
    pub fn sandbox_id(&self) -> &str {
        &self.connection.sandbox_id
    }

    fn toolbox(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.inner.authorized(
            self.inner.http.request(method, format!("{}{path}", self.connection.toolbox)),
        )
    }
}

#[async_trait]
impl SandboxSession for DaytonaSession {
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
            validate_env_name(key)?;
        }
        let timeout = request.timeout.unwrap_or(self.inner.config.command_timeout);
        // The toolbox takes whole seconds and defaults to 10 when none is sent.
        let seconds = (timeout.as_secs() + u64::from(timeout.subsec_nanos() > 0)).max(1);
        let body = json!({
            "command": request.command,
            "cwd": cwd,
            "envs": request.env,
            "timeout": seconds,
        });
        let started = Instant::now();
        let http = self
            .toolbox(reqwest::Method::POST, "/process/execute")
            .timeout(timeout + COMMAND_GRACE)
            .json(&body);
        let response = match tokio::time::timeout(
            timeout + COMMAND_GRACE,
            send(http, PROVIDER, "run command"),
        )
        .await
        {
            Err(_) | Ok(Err(SandboxError::Remote { status: Some(408), .. })) => {
                return Ok(ExecOutput::new("", "", -1, started.elapsed(), true));
            }
            Ok(Err(error)) => return Err(error),
            Ok(Ok(response)) => response,
        };
        let result: ExecuteResponse = json(response, PROVIDER, "run command").await?;
        Ok(ExecOutput::new(
            capped_text(result.result.into_bytes(), false),
            "",
            result.exit_code.unwrap_or(-1),
            started.elapsed(),
            false,
        ))
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        let full = absolute(&self.inner.config.workdir, path)?;
        let request = self
            .toolbox(reqwest::Method::GET, "/files/download")
            .query(&[("path", full.as_str())])
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
        // The upload does not create missing parent directories on every daemon version.
        if let Some((parent, _)) = full.rsplit_once('/')
            && !parent.is_empty()
            && parent != self.inner.config.workdir
        {
            let output = self
                .exec(CommandRequest::new(format!(
                    "mkdir -p -- {}",
                    crate::workspace::session::shell_quote(parent)
                )))
                .await?;
            if output.exit_code != 0 {
                return Err(SandboxError::ExecutionFailed(format!(
                    "cannot create the directory of {path}: {}",
                    output.stdout.trim()
                )));
            }
        }
        let part = reqwest::multipart::Part::bytes(content.to_vec())
            .file_name(path.rsplit('/').next().unwrap_or(path).to_string())
            .mime_str("application/octet-stream")
            .map_err(|error| SandboxError::InvalidRequest(error.to_string()))?;
        let request = self
            .toolbox(reqwest::Method::POST, "/files/upload-v2")
            .query(&[("path", full.as_str())])
            .timeout(REQUEST_TIMEOUT)
            .multipart(reqwest::multipart::Form::new().part("file", part));
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

    #[test]
    fn minutes_round_up_and_never_reach_zero() {
        assert_eq!(minutes(Duration::from_secs(1)), 1);
        assert_eq!(minutes(Duration::from_secs(60)), 1);
        assert_eq!(minutes(Duration::from_secs(61)), 2);
        assert_eq!(minutes(Duration::ZERO), 1);
    }
}
