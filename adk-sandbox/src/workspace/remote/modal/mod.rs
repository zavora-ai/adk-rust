//! Modal: workspaces in Modal Sandboxes (feature `remote-modal`).
//!
//! Modal's API is gRPC only: its Python SDK and its JavaScript and Go SDKs speak it to
//! `api.modal.com`. This client speaks the same protocol over `tonic`, with the subset of
//! Modal's published protobuf definitions it needs vendored in `adk-sandbox/proto/modal/`
//! (`modal_proto/api.proto` and `modal_proto/task_command_router.proto` from
//! <https://github.com/modal-labs/modal-client>, Apache-2.0) and the generated code committed.
//!
//! | Plane | Endpoint | Credentials |
//! |-------|----------|-------------|
//! | Control: apps, images, sandbox lifecycle | `https://api.modal.com` (`ModalClient` service) | `x-modal-token-id` and `x-modal-token-secret`, plus the short-lived `x-modal-auth-token` that `AuthTokenGet` issues |
//! | Commands, stdin, stdout and stderr, filesystem snapshots | The sandbox's task command router, at the URL `SandboxCreateV2` or `SandboxGetCommandRouterAccess` returns (`TaskCommandRouter` service) | `authorization: Bearer` with the sandbox-scoped JWT returned beside the URL; the token never goes there |
//!
//! Sandboxes are created as Modal's current SDKs create them by default (`SandboxCreateV2`);
//! `start` also attaches to a sandbox another client created through the older RPCs, chosen by
//! the shape of its id as the SDKs choose.

#[allow(dead_code, missing_docs, unreachable_pub, clippy::all, clippy::pedantic)]
mod proto {
    pub(crate) mod client {
        include!("proto/modal.client.rs");
    }
    pub(crate) mod task_command_router {
        include!("proto/modal.task_command_router.rs");
    }
}

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tonic::metadata::{AsciiMetadataValue, MetadataMap};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Status};

use self::proto::client as api;
use self::proto::task_command_router as tcr;
use super::{
    COMMAND_GRACE, ERROR_EXCERPT_BYTES, MAX_FILE_BYTES, MAX_OUTPUT_BYTES, REQUEST_TIMEOUT, Secret,
    absolute, apply_patch_via_exec, capped_text, destroy_in_background, list_dir_via_exec,
    populate,
};
use crate::error::SandboxError;
use crate::workspace::client::SandboxClient;
use crate::workspace::manifest::Manifest;
use crate::workspace::session::{SandboxSession, validate_env_name};
use crate::workspace::types::{
    ClientCapabilities, CommandRequest, DirEntry, ExecOutput, SessionHandle, SnapshotId,
};

type ControlClient = api::modal_client_client::ModalClientClient<Channel>;
type RouterClient = tcr::task_command_router_client::TaskCommandRouterClient<Channel>;

const PROVIDER: &str = "modal";

/// Modal's control plane.
pub const DEFAULT_SERVER_URL: &str = "https://api.modal.com";

/// The Modal app sandboxes belong to unless [`ModalConfig::with_app_name`] names another; it
/// is created on first use.
pub const DEFAULT_APP_NAME: &str = "adk-sandbox";

/// The registry image sandboxes start from unless `ModalConfig::with_image` or
/// `ModalConfig::with_image_id` names another. Its build adds `git` and `patch`.
pub const DEFAULT_IMAGE: &str = "python:3.13-slim";

/// The Dockerfile commands built on top of `DEFAULT_IMAGE`: `git` for manifest repositories
/// and `patch` for `apply_patch`, which the slim image lacks.
const DEFAULT_IMAGE_COMMANDS: &[&str] = &[
    "RUN apt-get update && apt-get install -y --no-install-recommends git patch ca-certificates \
     && rm -rf /var/lib/apt/lists/*",
];

/// `x-modal-client-type`: `CLIENT_TYPE_LIBMODAL`, the type api.proto gives clients of the
/// protocol the JavaScript and Go SDKs speak; `x-modal-libmodal-version` names this crate.
const CLIENT_TYPE: &str = "7";

/// `x-modal-client-version`: the Python SDK version whose protocol behaviour the client
/// follows, as the JavaScript and Go SDKs send it.
const CLIENT_VERSION: &str = "1.0.0";

const LIBMODAL_VERSION: &str = concat!("adk-sandbox/", env!("CARGO_PKG_VERSION"));

/// Largest gRPC message sent or accepted, as the Modal SDKs set it.
const MAX_MESSAGE_BYTES: usize = 100 * 1024 * 1024;

/// HTTP/2 flow-control windows, as the Modal SDKs set them.
const WINDOW_BYTES: u32 = 64 * 1024 * 1024;

/// stdin is written in chunks of this size.
const STDIN_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// How long `ImageJoinStreaming` and `TaskExecWait` hold a request open on the server.
const LONG_POLL: Duration = Duration::from_secs(55);

/// How long stdout and stderr may keep streaming after the command exits, for output a
/// background process still holds open.
const STDIO_DRAIN: Duration = Duration::from_secs(5);

/// How often a sandbox is polled for its task while it is scheduled.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Retries of a control-plane call that failed with a transient status.
const CONTROL_RETRIES: u32 = 3;

/// Refresh the control-plane auth token this long before it expires.
const AUTH_REFRESH_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Runs the command with `bash` where the image has it, `sh` otherwise; `$1` is the command.
const RUN_SCRIPT: &str =
    "if command -v bash >/dev/null 2>&1; then exec bash -c \"$1\"; else exec sh -c \"$1\"; fi";

/// Writes stdin to `$1`, creating its directory.
const WRITE_SCRIPT: &str = "mkdir -p -- \"$(dirname -- \"$1\")\" && cat > \"$1\"";

/// Copies `$1` to stdout.
const READ_SCRIPT: &str = "cat -- \"$1\"";

/// Configuration of a [`ModalClient`].
///
/// # Example
///
/// ```rust
/// use adk_sandbox::ModalConfig;
/// use std::time::Duration;
///
/// let config = ModalConfig::new("ak-test-id", "as-test-secret")
///     .with_app_name("agents")
///     .with_timeout(Duration::from_secs(600))
///     .with_network_blocked(true)
///     .with_tag("team", "research");
/// let debug = format!("{config:?}");
/// assert!(!debug.contains("ak-test-id") && !debug.contains("as-test-secret"));
/// ```
#[derive(Debug, Clone)]
pub struct ModalConfig {
    token_id: Secret,
    token_secret: Secret,
    server_url: String,
    environment: Option<String>,
    app_name: String,
    image: String,
    image_commands: Vec<String>,
    image_id: Option<String>,
    timeout: Duration,
    idle_timeout: Option<Duration>,
    command_timeout: Duration,
    start_timeout: Duration,
    network_blocked: bool,
    cpu: Option<f64>,
    memory_mib: Option<u32>,
    env: BTreeMap<String, String>,
    tags: BTreeMap<String, String>,
    name_prefix: Option<String>,
    workdir: String,
    snapshot_ttl: Duration,
}

impl ModalConfig {
    /// A configuration for a Modal token (`ak-…` id, `as-…` secret): sandboxes in the app
    /// `adk-sandbox` of the token's default environment, from `python:3.13-slim` with `git`
    /// and `patch` added, with a 15-minute lifetime, network open, commands as `root` in
    /// `/root` with a 120-second timeout.
    pub fn new(token_id: impl Into<String>, token_secret: impl Into<String>) -> Self {
        Self {
            token_id: Secret::new(token_id),
            token_secret: Secret::new(token_secret),
            server_url: DEFAULT_SERVER_URL.to_string(),
            environment: None,
            app_name: DEFAULT_APP_NAME.to_string(),
            image: DEFAULT_IMAGE.to_string(),
            image_commands: DEFAULT_IMAGE_COMMANDS.iter().map(|c| (*c).to_string()).collect(),
            image_id: None,
            timeout: Duration::from_secs(15 * 60),
            idle_timeout: None,
            command_timeout: Duration::from_secs(120),
            start_timeout: Duration::from_secs(300),
            network_blocked: false,
            cpu: None,
            memory_mib: None,
            env: BTreeMap::new(),
            tags: BTreeMap::new(),
            name_prefix: None,
            workdir: "/root".to_string(),
            snapshot_ttl: Duration::from_secs(30 * 24 * 60 * 60),
        }
    }

    /// Reads the token from `MODAL_TOKEN_ID` and `MODAL_TOKEN_SECRET`, and `MODAL_ENVIRONMENT`
    /// and `MODAL_SERVER_URL` when set, as the Modal SDKs do.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::BackendUnavailable` when either token variable is unset or blank.
    pub fn from_env() -> Result<Self, SandboxError> {
        let read = |key: &str| {
            std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        };
        let required = |key: &str| {
            read(key).ok_or_else(|| {
                SandboxError::BackendUnavailable(format!(
                    "{key} is not set; set MODAL_TOKEN_ID and MODAL_TOKEN_SECRET to a Modal token \
                     (`modal token new` creates one), or build the config with ModalConfig::new"
                ))
            })
        };
        let mut config = Self::new(required("MODAL_TOKEN_ID")?, required("MODAL_TOKEN_SECRET")?);
        config.environment = read("MODAL_ENVIRONMENT");
        // The SDKs accept a comma-separated list of fallbacks; the first is the primary.
        if let Some(url) = read("MODAL_SERVER_URL") {
            let primary = url.split(',').next().unwrap_or_default().trim().to_string();
            config = config.with_server_url(primary);
        }
        Ok(config)
    }

    /// The control-plane origin, for a test server.
    #[must_use]
    pub fn with_server_url(mut self, url: impl Into<String>) -> Self {
        self.server_url = url.into().trim_end_matches('/').to_string();
        self
    }

    /// The Modal environment apps and images are looked up in; the token's default when unset.
    #[must_use]
    pub fn with_environment(mut self, environment: impl Into<String>) -> Self {
        self.environment = Some(environment.into());
        self
    }

    /// The app sandboxes belong to, created on first use.
    #[must_use]
    pub fn with_app_name(mut self, name: impl Into<String>) -> Self {
        self.app_name = name.into();
        self
    }

    /// The registry image sandboxes start from (`FROM {tag}`), built by Modal on first use.
    /// Replaces the default `python:3.13-slim` and the `git` and `patch` it adds.
    #[must_use]
    pub fn with_image(mut self, tag: impl Into<String>) -> Self {
        self.image = tag.into();
        self.image_commands.clear();
        self
    }

    /// A Dockerfile command built on top of the image, such as `RUN pip install numpy`.
    #[must_use]
    pub fn with_image_command(mut self, command: impl Into<String>) -> Self {
        self.image_commands.push(command.into());
        self
    }

    /// An image Modal already built (`im-…`), such as a filesystem snapshot; nothing is built.
    #[must_use]
    pub fn with_image_id(mut self, image_id: impl Into<String>) -> Self {
        self.image_id = Some(image_id.into());
        self
    }

    /// How long a sandbox lives before Modal terminates it, whatever this process does
    /// (`timeout_secs`, whole seconds from one second to Modal's 24-hour limit).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.clamp(Duration::from_secs(1), Duration::from_secs(24 * 60 * 60));
        self
    }

    /// How long a sandbox may sit idle before Modal terminates it (`idle_timeout_secs`).
    #[must_use]
    pub fn with_idle_timeout(mut self, idle: Duration) -> Self {
        self.idle_timeout = Some(idle.max(Duration::from_secs(1)));
        self
    }

    /// The timeout of a command that does not set its own.
    #[must_use]
    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// How long `provision` waits for the image to build and for the sandbox to be scheduled.
    #[must_use]
    pub fn with_start_timeout(mut self, timeout: Duration) -> Self {
        self.start_timeout = timeout;
        self
    }

    /// Blocks all network access from sandboxes (`NetworkAccess::BLOCKED`).
    #[must_use]
    pub fn with_network_blocked(mut self, blocked: bool) -> Self {
        self.network_blocked = blocked;
        self
    }

    /// The CPU cores each sandbox requests, in fractions of a physical core.
    #[must_use]
    pub fn with_cpu(mut self, cores: f64) -> Self {
        self.cpu = Some(cores);
        self
    }

    /// The memory each sandbox requests, in MiB.
    #[must_use]
    pub fn with_memory_mib(mut self, mib: u32) -> Self {
        self.memory_mib = Some(mib);
        self
    }

    /// An environment variable every command starts with; a command's own variables win.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// A tag on every sandbox, which Modal's sandbox list filters on.
    #[must_use]
    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }

    /// Names each sandbox `{prefix}-{12 hex digits}`, unique within the app. Unnamed when
    /// unset.
    #[must_use]
    pub fn with_name_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_prefix = Some(prefix.into());
        self
    }

    /// The workspace root inside the sandbox, which relative paths resolve against.
    #[must_use]
    pub fn with_workdir(mut self, workdir: impl Into<String>) -> Self {
        self.workdir = workdir.into();
        self
    }

    /// How long Modal keeps a filesystem snapshot (`ttl_seconds`; 30 days unless set).
    #[must_use]
    pub fn with_snapshot_ttl(mut self, ttl: Duration) -> Self {
        self.snapshot_ttl = ttl.max(Duration::from_secs(1));
        self
    }

    /// The sandbox lifetime in whole seconds, rounded up.
    fn timeout_secs(&self) -> u32 {
        whole_seconds(self.timeout)
    }

    fn sandbox_name(&self) -> Result<Option<String>, SandboxError> {
        let Some(prefix) = &self.name_prefix else {
            return Ok(None);
        };
        let valid = !prefix.is_empty()
            && prefix.len() <= 50
            && prefix.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !valid {
            return Err(SandboxError::InvalidRequest(format!(
                "sandbox name prefix {prefix:?} must be 1 to 50 letters, digits, dashes, \
                 periods or underscores"
            )));
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        Ok(Some(format!("{prefix}-{}", &id[..12])))
    }
}

/// Whole seconds of `duration`, rounded up, at least one, saturating at `u32::MAX`.
fn whole_seconds(duration: Duration) -> u32 {
    let seconds = duration.as_secs() + u64::from(duration.subsec_nanos() > 0);
    u32::try_from(seconds.max(1)).unwrap_or(u32::MAX)
}

/// Modal's V1 sandbox ids are `sb-` and 22 base-62 characters; the SDKs route every other
/// shape to the V2 RPCs.
fn is_v1_sandbox_id(id: &str) -> bool {
    id.strip_prefix("sb-")
        .is_some_and(|rest| rest.len() == 22 && rest.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// The HTTP status a gRPC code corresponds to (the `google.rpc.Code` mapping), so a Modal
/// error lands in the same `AdkError` category as the REST providers' errors.
fn http_status(code: Code) -> Option<u16> {
    match code {
        Code::Ok | Code::Cancelled => None,
        Code::InvalidArgument | Code::FailedPrecondition | Code::OutOfRange => Some(400),
        Code::Unauthenticated => Some(401),
        Code::PermissionDenied => Some(403),
        Code::NotFound => Some(404),
        Code::AlreadyExists | Code::Aborted => Some(409),
        Code::ResourceExhausted => Some(429),
        Code::Unknown | Code::Internal | Code::DataLoss => Some(500),
        Code::Unimplemented => Some(501),
        Code::Unavailable => Some(503),
        Code::DeadlineExceeded => Some(504),
    }
}

/// A gRPC status as `SandboxError::Remote`, the message cut to a bounded excerpt.
fn remote_error(operation: &str, status: &Status) -> SandboxError {
    let mut message = format!("{:?}: {}", status.code(), status.message());
    if message.len() > ERROR_EXCERPT_BYTES {
        let mut cut = ERROR_EXCERPT_BYTES;
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        message.truncate(cut);
    }
    SandboxError::Remote {
        provider: PROVIDER.to_string(),
        operation: operation.to_string(),
        status: http_status(status.code()),
        message,
    }
}

fn remote_message(operation: &str, message: impl Into<String>) -> SandboxError {
    SandboxError::Remote {
        provider: PROVIDER.to_string(),
        operation: operation.to_string(),
        status: None,
        message: message.into(),
    }
}

/// The message of a call that ran out of time, which `exec` reports as a timed-out command.
const TIMED_OUT: &str = "the request timed out";

fn timed_out(operation: &str) -> SandboxError {
    remote_message(operation, TIMED_OUT)
}

fn is_timed_out(error: &SandboxError) -> bool {
    matches!(error, SandboxError::Remote { status: None, message, .. } if message == TIMED_OUT)
}

/// Statuses a call is retried on, as the Modal SDKs retry them.
fn transient(code: Code) -> bool {
    matches!(code, Code::Unavailable | Code::Internal | Code::Unknown)
}

/// `SessionNotFound` for a status that says the sandbox does not exist.
fn not_found(error: SandboxError, sandbox_id: &str) -> SandboxError {
    match error {
        SandboxError::Remote { status: Some(400 | 404), .. } => {
            SandboxError::SessionNotFound { handle: sandbox_id.to_string() }
        }
        other => other,
    }
}

fn ascii(value: &str, what: &str) -> Result<AsciiMetadataValue, SandboxError> {
    AsciiMetadataValue::try_from(value).map_err(|_| {
        SandboxError::InvalidRequest(format!("the {what} is not a valid gRPC header value"))
    })
}

/// Seconds since the Unix epoch at which `jwt` expires, from its `exp` claim.
fn jwt_expiry(jwt: &str) -> Option<u64> {
    let payload = jwt.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<serde_json::Value>(&decoded).ok()?.get("exp")?.as_u64()
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// A channel to `url` that connects on first use: TLS with the WebPKI roots for `https`.
fn endpoint(url: &str, operation: &str) -> Result<Endpoint, SandboxError> {
    let endpoint = Endpoint::from_shared(url.to_string())
        .map_err(|error| remote_message(operation, format!("invalid URL: {error}")))?
        .connect_timeout(Duration::from_secs(10))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10))
        .keep_alive_while_idle(true)
        .initial_stream_window_size(WINDOW_BYTES)
        .initial_connection_window_size(WINDOW_BYTES)
        .user_agent(LIBMODAL_VERSION)
        .map_err(|error| remote_message(operation, error.to_string()))?;
    if url.starts_with("https://") {
        endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(|error| remote_message(operation, format!("TLS setup failed: {error}")))
    } else {
        Ok(endpoint)
    }
}

/// The control-plane auth token and when to refresh it.
struct AuthToken {
    token: Secret,
    refresh_at: Instant,
}

/// A sandbox's task command router.
struct Router {
    client: RouterClient,
    jwt: RwLock<Secret>,
}

/// How to reach one sandbox.
#[derive(Clone)]
struct Connection {
    sandbox_id: String,
    task_id: String,
    router: Arc<Router>,
    /// Created by this client, so dropping the client terminates it.
    owned: bool,
}

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    Code(i32),
    Signal(i32),
}

/// A bounded copy of one output stream; bytes past the limit are drained and dropped.
struct Capture {
    bytes: Vec<u8>,
    max: usize,
    overflowed: bool,
}

impl Capture {
    fn new(max: usize) -> Self {
        Self { bytes: Vec::new(), max, overflowed: false }
    }

    fn push(&mut self, data: &[u8]) {
        let room = self.max.saturating_sub(self.bytes.len());
        if data.len() > room {
            self.overflowed = true;
        }
        self.bytes.extend_from_slice(&data[..data.len().min(room)]);
    }

    fn text(self) -> String {
        capped_text(self.bytes, self.overflowed)
    }
}

struct Inner {
    config: ModalConfig,
    host: String,
    control_endpoint: Endpoint,
    control: OnceLock<ControlClient>,
    auth: tokio::sync::Mutex<Option<AuthToken>>,
    app_id: tokio::sync::OnceCell<String>,
    image_id: tokio::sync::OnceCell<String>,
}

impl Inner {
    /// The control-plane client; the channel is made on first use, inside the runtime.
    fn control(&self) -> ControlClient {
        self.control
            .get_or_init(|| {
                ControlClient::new(self.control_endpoint.connect_lazy())
                    .max_decoding_message_size(MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_MESSAGE_BYTES)
            })
            .clone()
    }

    /// The metadata every control-plane call carries, as the Modal SDKs send it.
    fn identify(&self, metadata: &mut MetadataMap) -> Result<(), SandboxError> {
        metadata.insert("x-modal-client-type", AsciiMetadataValue::from_static(CLIENT_TYPE));
        metadata.insert("x-modal-client-version", AsciiMetadataValue::from_static(CLIENT_VERSION));
        metadata
            .insert("x-modal-libmodal-version", AsciiMetadataValue::from_static(LIBMODAL_VERSION));
        metadata.insert("x-modal-host", ascii(&self.host, "server host")?);
        metadata.insert("x-modal-token-id", ascii(self.config.token_id.expose(), "token id")?);
        metadata.insert(
            "x-modal-token-secret",
            ascii(self.config.token_secret.expose(), "token secret")?,
        );
        Ok(())
    }

    /// The control-plane auth token, fetched with `AuthTokenGet` and refreshed five minutes
    /// before its `exp`.
    async fn auth_token(&self) -> Result<Secret, SandboxError> {
        let mut cached = self.auth.lock().await;
        if let Some(token) = cached.as_ref()
            && Instant::now() < token.refresh_at
        {
            return Ok(token.token.clone());
        }
        let operation = "get auth token";
        let mut delay = Duration::from_millis(100);
        let mut attempt = 0;
        let token = loop {
            let mut request = tonic::Request::new(api::AuthTokenGetRequest {});
            request.set_timeout(REQUEST_TIMEOUT);
            self.identify(request.metadata_mut())?;
            let mut control = self.control();
            match tokio::time::timeout(REQUEST_TIMEOUT, control.auth_token_get(request)).await {
                Err(_) => return Err(timed_out(operation)),
                Ok(Ok(response)) => break response.into_inner().token,
                Ok(Err(status)) if transient(status.code()) && attempt < CONTROL_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(1));
                }
                Ok(Err(status)) => return Err(remote_error(operation, &status)),
            }
        };
        if token.is_empty() {
            return Err(remote_message(operation, "Modal returned no auth token"));
        }
        // A token without `exp` is kept for 20 minutes, as the SDKs keep it.
        let lifetime = jwt_expiry(&token)
            .map(|exp| Duration::from_secs(exp.saturating_sub(unix_now())))
            .unwrap_or(Duration::from_secs(20 * 60));
        let token = Secret::new(token);
        *cached = Some(AuthToken {
            token: token.clone(),
            refresh_at: Instant::now() + lifetime.saturating_sub(AUTH_REFRESH_WINDOW),
        });
        Ok(token)
    }

    /// One control-plane call, with its identity and auth metadata, an idempotency key kept
    /// across retries, and a deadline of `timeout` over all attempts.
    async fn unary<Req, Resp, F, Fut>(
        &self,
        operation: &str,
        message: Req,
        timeout: Duration,
        call: F,
    ) -> Result<Resp, SandboxError>
    where
        Req: Clone,
        F: Fn(ControlClient, tonic::Request<Req>) -> Fut,
        Fut: Future<Output = Result<tonic::Response<Resp>, Status>>,
    {
        let deadline = Instant::now() + timeout;
        let idempotency_key = uuid::Uuid::new_v4().to_string();
        let mut delay = Duration::from_millis(100);
        let mut attempt = 0;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(timed_out(operation));
            }
            let token = self.auth_token().await?;
            let mut request = tonic::Request::new(message.clone());
            request.set_timeout(left);
            let metadata = request.metadata_mut();
            self.identify(metadata)?;
            metadata.insert("x-modal-auth-token", ascii(token.expose(), "auth token")?);
            metadata.insert("x-idempotency-key", ascii(&idempotency_key, "idempotency key")?);
            metadata.insert("x-retry-attempt", ascii(&attempt.to_string(), "retry attempt")?);
            match tokio::time::timeout(left, call(self.control(), request)).await {
                Err(_) => return Err(timed_out(operation)),
                Ok(Ok(response)) => return Ok(response.into_inner()),
                Ok(Err(status))
                    if transient(status.code())
                        && attempt < CONTROL_RETRIES
                        && deadline.saturating_duration_since(Instant::now()) > delay =>
                {
                    attempt += 1;
                    tracing::debug!(sandbox.provider = PROVIDER, operation, code = ?status.code(), attempt, "retrying a Modal call");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(1));
                }
                Ok(Err(status)) => return Err(remote_error(operation, &status)),
            }
        }
    }

    fn environment_name(&self) -> String {
        self.config.environment.clone().unwrap_or_default()
    }

    /// The app sandboxes belong to, created if missing (`AppGetOrCreate`).
    async fn app_id(&self) -> Result<String, SandboxError> {
        self.app_id
            .get_or_try_init(|| async {
                let request = api::AppGetOrCreateRequest {
                    app_name: self.config.app_name.clone(),
                    environment_name: self.environment_name(),
                    object_creation_type: api::ObjectCreationType::CreateIfMissing as i32,
                };
                let response = self
                    .unary("look up app", request, REQUEST_TIMEOUT, |mut control, request| async move {
                        control.app_get_or_create(request).await
                    })
                    .await?;
                Ok(response.app_id)
            })
            .await
            .cloned()
    }

    /// The image sandboxes start from: the configured id, or the configured registry image,
    /// built once per client.
    async fn image_id(&self) -> Result<String, SandboxError> {
        if let Some(id) = &self.config.image_id {
            return Ok(id.clone());
        }
        self.image_id.get_or_try_init(|| self.build_image()).await.cloned()
    }

    /// `ImageGetOrCreate`, then `ImageJoinStreaming` until the build has a result. Modal
    /// caches builds by their Dockerfile commands, so a built image returns at once.
    async fn build_image(&self) -> Result<String, SandboxError> {
        let app_id = self.app_id().await?;
        let environment = self
            .unary(
                "look up environment",
                api::EnvironmentGetOrCreateRequest {
                    deployment_name: self.environment_name(),
                    object_creation_type: api::ObjectCreationType::Unspecified as i32,
                },
                REQUEST_TIMEOUT,
                |mut control, request| async move { control.environment_get_or_create(request).await },
            )
            .await?;
        let builder_version = environment
            .metadata
            .and_then(|metadata| metadata.settings)
            .map(|settings| settings.image_builder_version)
            .unwrap_or_default();
        let mut dockerfile_commands = vec![format!("FROM {}", self.config.image)];
        dockerfile_commands.extend(self.config.image_commands.iter().cloned());
        let created = self
            .unary(
                "build image",
                api::ImageGetOrCreateRequest {
                    image: Some(api::Image { dockerfile_commands }),
                    app_id,
                    force_build: false,
                    builder_version,
                },
                REQUEST_TIMEOUT,
                |mut control, request| async move { control.image_get_or_create(request).await },
            )
            .await?;
        let deadline = Instant::now() + self.config.start_timeout;
        let mut result = created.result.filter(|result| result.status != 0);
        let mut last_entry_id = String::new();
        let mut failures = 0;
        while result.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(SandboxError::ProvisionFailed {
                    resource: created.image_id.clone(),
                    reason: format!(
                        "the image {} did not build within {:?}",
                        self.config.image, self.config.start_timeout
                    ),
                    suggestion: "Raise ModalConfig::with_start_timeout, or build the image once \
                                 and pass its id to ModalConfig::with_image_id."
                        .to_string(),
                });
            }
            match self.join_image(&created.image_id, &mut last_entry_id, left).await {
                Ok(joined) => result = joined,
                Err(error) => {
                    failures += 1;
                    if failures > CONTROL_RETRIES {
                        return Err(error);
                    }
                }
            }
        }
        let result = result.unwrap_or_default();
        if result.status == api::generic_result::GenericStatus::Success as i32 {
            tracing::info!(sandbox.provider = PROVIDER, image.id = %created.image_id, image = %self.config.image, "built a sandbox image");
            return Ok(created.image_id);
        }
        let status = api::generic_result::GenericStatus::try_from(result.status)
            .map(|status| status.as_str_name().to_string())
            .unwrap_or_else(|_| result.status.to_string());
        Err(SandboxError::ProvisionFailed {
            resource: created.image_id,
            reason: format!("the image build ended with {status}: {}", result.exception.trim()),
            suggestion: "Check the image tag and Dockerfile commands; the build log is in the \
                         Modal dashboard."
                .to_string(),
        })
    }

    /// One `ImageJoinStreaming` long poll: the build's result if it arrives.
    async fn join_image(
        &self,
        image_id: &str,
        last_entry_id: &mut String,
        left: Duration,
    ) -> Result<Option<api::GenericResult>, SandboxError> {
        let operation = "wait for image build";
        let hold = LONG_POLL.min(left);
        let deadline = (hold + COMMAND_GRACE / 3).min(left);
        let token = self.auth_token().await?;
        let mut request = tonic::Request::new(api::ImageJoinStreamingRequest {
            image_id: image_id.to_string(),
            timeout: hold.as_secs_f32(),
            last_entry_id: last_entry_id.clone(),
        });
        request.set_timeout(deadline);
        self.identify(request.metadata_mut())?;
        request.metadata_mut().insert("x-modal-auth-token", ascii(token.expose(), "auth token")?);
        let mut control = self.control();
        let joined = tokio::time::timeout(deadline, async {
            let mut stream = control
                .image_join_streaming(request)
                .await
                .map_err(|status| remote_error(operation, &status))?
                .into_inner();
            while let Some(item) =
                stream.message().await.map_err(|status| remote_error(operation, &status))?
            {
                if !item.entry_id.is_empty() {
                    *last_entry_id = item.entry_id;
                }
                if let Some(result) = item.result.filter(|result| result.status != 0) {
                    return Ok(Some(result));
                }
            }
            Ok(None)
        })
        .await;
        // A poll that outlives its hold ends without a result; the caller polls again.
        joined.unwrap_or(Ok(None))
    }

    /// `SandboxCreateV2`; the response carries the task and its command router.
    async fn create(&self, image_id: &str) -> Result<Connection, SandboxError> {
        let app_id = self.app_id().await?;
        let config = &self.config;
        let resources =
            (config.cpu.is_some() || config.memory_mib.is_some()).then(|| api::Resources {
                memory_mb: config.memory_mib.unwrap_or_default(),
                milli_cpu: config
                    .cpu
                    .map(|cores| (cores * 1000.0).round() as u32)
                    .unwrap_or_default(),
            });
        let network_access_type = if config.network_blocked {
            api::network_access::NetworkAccessType::Blocked
        } else {
            api::network_access::NetworkAccessType::Open
        };
        let definition = api::Sandbox {
            entrypoint_args: Vec::new(),
            image_id: image_id.to_string(),
            resources,
            timeout_secs: config.timeout_secs(),
            workdir: Some(config.workdir.clone()),
            network_access: Some(api::NetworkAccess {
                network_access_type: network_access_type as i32,
                allowed_cidrs: Vec::new(),
            }),
            name: config.sandbox_name()?,
            idle_timeout_secs: config.idle_timeout.map(whole_seconds),
        };
        let request = api::SandboxCreateV2Request {
            app_id,
            definition: Some(definition),
            tags: config
                .tags
                .iter()
                .map(|(name, value)| api::SandboxTag {
                    tag_name: name.clone(),
                    tag_value: value.clone(),
                })
                .collect(),
            secret_sources: Vec::new(),
        };
        let created = self
            .unary("create sandbox", request, REQUEST_TIMEOUT, |mut control, request| async move {
                control.sandbox_create_v2(request).await
            })
            .await?;
        let api::SandboxCreateV2Response { sandbox_id, task_id, command_router_access } = created;
        let connected = async {
            let task_id =
                if task_id.is_empty() { self.wait_for_task(&sandbox_id).await? } else { task_id };
            let access = match command_router_access {
                Some(access) if !access.url.is_empty() => access,
                _ => self.router_access(&sandbox_id, &task_id).await?,
            };
            Ok::<_, SandboxError>(Connection {
                router: Arc::new(self.router(&access)?),
                sandbox_id: sandbox_id.clone(),
                task_id,
                owned: true,
            })
        }
        .await;
        if connected.is_err()
            && let Err(error) = self.terminate(&sandbox_id).await
        {
            tracing::warn!(sandbox.id = %sandbox_id, error = %error, "could not terminate a sandbox that failed to start");
        }
        connected
    }

    /// Polls `SandboxGetTaskId` until the sandbox runs a task, for at most the start timeout.
    async fn wait_for_task(&self, sandbox_id: &str) -> Result<String, SandboxError> {
        let deadline = Instant::now() + self.config.start_timeout;
        let v1 = is_v1_sandbox_id(sandbox_id);
        loop {
            let request = api::SandboxGetTaskIdRequest {
                sandbox_id: sandbox_id.to_string(),
                timeout: None,
                wait_until_ready: false,
            };
            let response = self
                .unary(
                    "get sandbox task",
                    request,
                    REQUEST_TIMEOUT,
                    |mut control, request| async move {
                        if v1 {
                            control.sandbox_get_task_id(request).await
                        } else {
                            control.sandbox_get_task_id_v2(request).await
                        }
                    },
                )
                .await
                .map_err(|error| not_found(error, sandbox_id))?;
            if let Some(task_id) = response.task_id.filter(|id| !id.is_empty()) {
                return Ok(task_id);
            }
            if let Some(result) = response.task_result {
                tracing::debug!(sandbox.id = %sandbox_id, status = result.status, "the sandbox has finished");
                return Err(SandboxError::SessionNotFound { handle: sandbox_id.to_string() });
            }
            if Instant::now() + POLL_INTERVAL >= deadline {
                return Err(SandboxError::ProvisionFailed {
                    resource: sandbox_id.to_string(),
                    reason: format!(
                        "the sandbox was not scheduled within {:?}",
                        self.config.start_timeout
                    ),
                    suggestion: "Modal may lack capacity for the requested resources; retry, or \
                                 raise ModalConfig::with_start_timeout."
                        .to_string(),
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// A fresh URL and JWT for the sandbox's command router.
    async fn router_access(
        &self,
        sandbox_id: &str,
        task_id: &str,
    ) -> Result<api::CommandRouterAccess, SandboxError> {
        let operation = "get command router access";
        if is_v1_sandbox_id(sandbox_id) {
            let request = api::TaskGetCommandRouterAccessRequest { task_id: task_id.to_string() };
            let response = self
                .unary(operation, request, REQUEST_TIMEOUT, |mut control, request| async move {
                    control.task_get_command_router_access(request).await
                })
                .await?;
            return Ok(api::CommandRouterAccess { jwt: response.jwt, url: response.url });
        }
        let request = api::SandboxGetCommandRouterAccessRequest {
            target: Some(api::sandbox_get_command_router_access_request::Target::SandboxId(
                sandbox_id.to_string(),
            )),
        };
        let response = self
            .unary(operation, request, REQUEST_TIMEOUT, |mut control, request| async move {
                control.sandbox_get_command_router_access(request).await
            })
            .await
            .map_err(|error| not_found(error, sandbox_id))?;
        Ok(api::CommandRouterAccess { jwt: response.jwt, url: response.url })
    }

    /// A client for the command router at `access.url`. It must be `https`, unless the
    /// control plane itself is a plaintext test server.
    fn router(&self, access: &api::CommandRouterAccess) -> Result<Router, SandboxError> {
        let operation = "connect to command router";
        let plaintext_allowed = self.config.server_url.starts_with("http://");
        if !access.url.starts_with("https://") && !plaintext_allowed {
            return Err(remote_message(operation, "the command router URL is not https"));
        }
        let client = RouterClient::new(endpoint(&access.url, operation)?.connect_lazy())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        Ok(Router { client, jwt: RwLock::new(Secret::new(access.jwt.clone())) })
    }

    /// `SandboxTerminateV2`, or `SandboxTerminate` for a V1 id.
    async fn terminate(&self, sandbox_id: &str) -> Result<(), SandboxError> {
        let v1 = is_v1_sandbox_id(sandbox_id);
        let request = api::SandboxTerminateRequest { sandbox_id: sandbox_id.to_string() };
        self.unary(
            "terminate sandbox",
            request,
            REQUEST_TIMEOUT,
            |mut control, request| async move {
                if v1 {
                    control.sandbox_terminate(request).await
                } else {
                    control.sandbox_terminate_v2(request).await
                }
            },
        )
        .await
        .map(drop)
        .map_err(|error| not_found(error, sandbox_id))
    }

    /// Attaches to a sandbox this client did not create.
    async fn attach(&self, sandbox_id: &str) -> Result<Connection, SandboxError> {
        let task_id = self.wait_for_task(sandbox_id).await?;
        let access = self.router_access(sandbox_id, &task_id).await?;
        Ok(Connection {
            sandbox_id: sandbox_id.to_string(),
            task_id,
            router: Arc::new(self.router(&access)?),
            owned: false,
        })
    }

    /// One command-router call with the sandbox's JWT, until `deadline`: a transient status
    /// or a call that outlives `per_call` is retried with the same request, which the
    /// router's ids and offsets make idempotent; `Unauthenticated` refreshes the JWT once.
    async fn router_call<Req, Resp, F, Fut>(
        &self,
        connection: &Connection,
        operation: &str,
        message: Req,
        per_call: Duration,
        deadline: Instant,
        call: F,
    ) -> Result<Resp, SandboxError>
    where
        Req: Clone,
        F: Fn(RouterClient, tonic::Request<Req>) -> Fut,
        Fut: Future<Output = Result<tonic::Response<Resp>, Status>>,
    {
        let mut delay = Duration::from_millis(10);
        let mut refreshed = false;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(timed_out(operation));
            }
            let limit = per_call.min(left);
            let request = self.router_request(connection, message.clone(), limit)?;
            let outcome =
                tokio::time::timeout(limit, call(connection.router.client.clone(), request)).await;
            let status = match outcome {
                Ok(Ok(response)) => return Ok(response.into_inner()),
                Ok(Err(status)) => status,
                Err(_) => Status::deadline_exceeded("the call outlived its deadline"),
            };
            match status.code() {
                Code::Unauthenticated if !refreshed => {
                    refreshed = true;
                    self.refresh_router(connection).await?;
                    continue;
                }
                code if transient(code) || code == Code::DeadlineExceeded => {}
                _ => return Err(remote_error(operation, &status)),
            }
            if deadline.saturating_duration_since(Instant::now()) <= delay {
                return Err(if status.code() == Code::DeadlineExceeded {
                    timed_out(operation)
                } else {
                    remote_error(operation, &status)
                });
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(1));
        }
    }

    fn router_request<Req>(
        &self,
        connection: &Connection,
        message: Req,
        timeout: Duration,
    ) -> Result<tonic::Request<Req>, SandboxError> {
        let mut request = tonic::Request::new(message);
        request.set_timeout(timeout);
        let jwt = connection.router.jwt.read().unwrap_or_else(|e| e.into_inner()).clone();
        request
            .metadata_mut()
            .insert("authorization", ascii(&format!("Bearer {}", jwt.expose()), "router token")?);
        Ok(request)
    }

    /// Replaces the router JWT after the router refused it.
    async fn refresh_router(&self, connection: &Connection) -> Result<(), SandboxError> {
        let access = self.router_access(&connection.sandbox_id, &connection.task_id).await?;
        *connection.router.jwt.write().unwrap_or_else(|e| e.into_inner()) = Secret::new(access.jwt);
        Ok(())
    }

    /// `TaskExecStart` of `args`: stdout and stderr piped, the router killing the command
    /// after `timeout`, `env` passed as an inline secret source as the SDKs pass it.
    async fn start_exec(
        &self,
        connection: &Connection,
        args: Vec<String>,
        workdir: String,
        env: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<String, SandboxError> {
        let exec_id = uuid::Uuid::new_v4().to_string();
        let secret_sources = if env.is_empty() {
            Vec::new()
        } else {
            vec![api::SecretSource {
                source: Some(api::secret_source::Source::Env(api::StringMap {
                    contents: env.into_iter().collect::<HashMap<_, _>>(),
                })),
            }]
        };
        let request = tcr::TaskExecStartRequest {
            task_id: connection.task_id.clone(),
            exec_id: exec_id.clone(),
            command_args: args,
            stdout_config: tcr::TaskExecStdoutConfig::Pipe as i32,
            stderr_config: tcr::TaskExecStderrConfig::Pipe as i32,
            timeout_secs: Some(whole_seconds(timeout)),
            workdir: Some(workdir),
            secret_sources,
        };
        self.router_call(
            connection,
            "run command",
            request,
            REQUEST_TIMEOUT,
            Instant::now() + REQUEST_TIMEOUT,
            |mut router, request| async move { router.task_exec_start(request).await },
        )
        .await
        .map_err(|error| not_found(error, &connection.sandbox_id))?;
        Ok(exec_id)
    }

    /// Writes `data` to the command's stdin and closes it.
    async fn write_stdin(
        &self,
        connection: &Connection,
        exec_id: &str,
        data: &[u8],
    ) -> Result<(), SandboxError> {
        let mut offset = 0u64;
        let mut chunks: Vec<&[u8]> = data.chunks(STDIN_CHUNK_BYTES).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        let last = chunks.len() - 1;
        for (index, chunk) in chunks.into_iter().enumerate() {
            let request = tcr::TaskExecStdinWriteRequest {
                task_id: connection.task_id.clone(),
                exec_id: exec_id.to_string(),
                offset,
                data: chunk.to_vec(),
                eof: index == last,
            };
            self.router_call(
                connection,
                "write command input",
                request,
                REQUEST_TIMEOUT,
                Instant::now() + REQUEST_TIMEOUT,
                |mut router, request| async move { router.task_exec_stdin_write(request).await },
            )
            .await?;
            offset += chunk.len() as u64;
        }
        Ok(())
    }

    /// Waits for the command to exit, stdout and stderr streaming into the captures meanwhile,
    /// for at most `deadline` plus the drain allowance.
    async fn collect(
        &self,
        connection: &Connection,
        exec_id: &str,
        deadline: Instant,
        stdout: &mut Capture,
        stderr: &mut Capture,
    ) -> Result<Exit, SandboxError> {
        let (exited, watch) = tokio::sync::watch::channel(false);
        let wait = async {
            let exit = self.wait_exec(connection, exec_id, deadline).await;
            let _ = exited.send(true);
            exit
        };
        let joined = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline + STDIO_DRAIN),
            async {
                tokio::join!(
                    wait,
                    self.read_stdio(
                        connection,
                        exec_id,
                        tcr::TaskExecStdioFileDescriptor::Stdout,
                        stdout,
                        deadline,
                        watch.clone()
                    ),
                    self.read_stdio(
                        connection,
                        exec_id,
                        tcr::TaskExecStdioFileDescriptor::Stderr,
                        stderr,
                        deadline,
                        watch
                    ),
                )
            },
        )
        .await;
        let Ok((exit, out, err)) = joined else {
            return Err(timed_out("wait for command"));
        };
        let exit = exit?;
        out?;
        err?;
        Ok(exit)
    }

    /// `TaskExecWait`, re-polled until the command exits or `deadline` passes.
    async fn wait_exec(
        &self,
        connection: &Connection,
        exec_id: &str,
        deadline: Instant,
    ) -> Result<Exit, SandboxError> {
        let request = tcr::TaskExecWaitRequest {
            task_id: connection.task_id.clone(),
            exec_id: exec_id.to_string(),
        };
        let response = self
            .router_call(
                connection,
                "wait for command",
                request,
                LONG_POLL + COMMAND_GRACE / 6,
                deadline,
                |mut router, request| async move { router.task_exec_wait(request).await },
            )
            .await?;
        match response.exit_status {
            Some(tcr::task_exec_wait_response::ExitStatus::Code(code)) => Ok(Exit::Code(code)),
            Some(tcr::task_exec_wait_response::ExitStatus::Signal(signal)) => {
                Ok(Exit::Signal(signal))
            }
            None => Err(remote_message("wait for command", "the command ended without a status")),
        }
    }

    /// Streams one of the command's outputs into `capture` until it ends, resuming at the
    /// last offset after a transient failure. It stops [`STDIO_DRAIN`] after the command
    /// exits, for output a background process holds open.
    async fn read_stdio(
        &self,
        connection: &Connection,
        exec_id: &str,
        fd: tcr::TaskExecStdioFileDescriptor,
        capture: &mut Capture,
        deadline: Instant,
        mut exited: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), SandboxError> {
        let operation = "read command output";
        let drained = async move {
            let _ = exited.wait_for(|exited| *exited).await;
            tokio::time::sleep(STDIO_DRAIN).await;
        };
        tokio::pin!(drained);
        let mut offset = 0u64;
        let mut retries = 0;
        let mut refreshed = false;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            let request = self.router_request(
                connection,
                tcr::TaskExecStdioReadRequest {
                    task_id: connection.task_id.clone(),
                    exec_id: exec_id.to_string(),
                    offset,
                    file_descriptor: fd as i32,
                },
                left,
            )?;
            let mut router = connection.router.client.clone();
            let opened = tokio::select! {
                opened = router.task_exec_stdio_read(request) => opened,
                () = &mut drained => return Ok(()),
            };
            let failure = match opened {
                Ok(response) => {
                    let mut stream = response.into_inner();
                    loop {
                        let item = tokio::select! {
                            item = stream.message() => item,
                            () = &mut drained => return Ok(()),
                        };
                        match item {
                            Ok(Some(chunk)) => {
                                offset += chunk.data.len() as u64;
                                capture.push(&chunk.data);
                                retries = 0;
                            }
                            Ok(None) => return Ok(()),
                            Err(status) => break status,
                        }
                    }
                }
                Err(status) => status,
            };
            match failure.code() {
                Code::Unauthenticated if !refreshed => {
                    refreshed = true;
                    self.refresh_router(connection).await?;
                }
                code if (transient(code) || code == Code::DeadlineExceeded) && retries < 10 => {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(10) * 2u32.pow(retries.min(7))).await;
                }
                _ => return Err(remote_error(operation, &failure)),
            }
        }
    }

    /// Runs `args` with stdin `input`, returning its exit and both outputs, each capped at
    /// `max` bytes.
    async fn run_raw(
        &self,
        connection: &Connection,
        operation: &str,
        args: Vec<String>,
        input: Option<&[u8]>,
        max: usize,
    ) -> Result<(Exit, Capture, Capture), SandboxError> {
        let exec_id = self
            .start_exec(
                connection,
                args,
                self.config.workdir.clone(),
                BTreeMap::new(),
                REQUEST_TIMEOUT,
            )
            .await?;
        if let Some(input) = input {
            self.write_stdin(connection, &exec_id, input).await?;
        }
        let mut stdout = Capture::new(max);
        let mut stderr = Capture::new(MAX_OUTPUT_BYTES);
        let deadline = Instant::now() + REQUEST_TIMEOUT + COMMAND_GRACE;
        match self.collect(connection, &exec_id, deadline, &mut stdout, &mut stderr).await {
            Ok(exit) => Ok((exit, stdout, stderr)),
            Err(error) if is_timed_out(&error) => Err(timed_out(operation)),
            Err(error) => Err(error),
        }
    }
}

/// A [`SandboxClient`] over Modal Sandboxes: each provisioned workspace is a sandbox in the
/// configured app, created from [`ModalConfig`]'s image with a provider-side lifetime.
///
/// | Operation | Modal RPC |
/// |-----------|-----------|
/// | `provision` | `AppGetOrCreate` and `ImageGetOrCreate` (once per client), `SandboxCreateV2`, then the manifest through the command router |
/// | `start` | the sandbox this client created, or `SandboxGetTaskIdV2` and `SandboxGetCommandRouterAccess` for another |
/// | `exec` | router `TaskExecStart` with `timeout_secs`, after which Modal kills the command; `TaskExecStdioRead` and `TaskExecWait` |
/// | `read_file`, `write_file` | `cat` through the router, the content over stdin and stdout |
/// | `snapshot` | router `TaskSnapshotFilesystem`, an image; `resume` creates a sandbox from it |
/// | `stop` | `SandboxTerminateV2` |
///
/// Dropping the client terminates every sandbox it created and did not stop.
///
/// # Example
///
/// ```rust,no_run
/// use adk_sandbox::workspace::{CommandRequest, Manifest, SandboxClient};
/// use adk_sandbox::{ModalClient, ModalConfig};
///
/// # async fn run() -> Result<(), adk_sandbox::SandboxError> {
/// let client = ModalClient::new(ModalConfig::from_env()?)?;
/// let handle = client.provision(&Manifest::new(vec![])).await?;
/// let session = client.start(&handle).await?;
/// let output = session.exec(CommandRequest::new("python3 --version")).await?;
/// println!("{}", output.stdout);
/// client.stop(&handle).await?;
/// # Ok(())
/// # }
/// ```
pub struct ModalClient {
    inner: Arc<Inner>,
    live: Arc<Mutex<HashMap<String, Connection>>>,
}

impl ModalClient {
    /// A client for `config`. No connection is made until the first call.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::Remote` when the server URL is not a valid URL.
    pub fn new(config: ModalConfig) -> Result<Self, SandboxError> {
        let control_endpoint = endpoint(&config.server_url, "connect to Modal")?;
        let host = control_endpoint.uri().host().unwrap_or_default().to_string();
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                host,
                control_endpoint,
                control: OnceLock::new(),
                auth: tokio::sync::Mutex::new(None),
                app_id: tokio::sync::OnceCell::new(),
                image_id: tokio::sync::OnceCell::new(),
            }),
            live: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The ids of the sandboxes running in the client's app, newest first, whoever created
    /// them, for cleanup and audits.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::Remote` when Modal refuses or fails a request.
    pub async fn running_sandboxes(&self) -> Result<Vec<String>, SandboxError> {
        let app_id = self.inner.app_id().await?;
        let mut ids = Vec::new();
        for v1 in [false, true] {
            let mut before = 0.0;
            loop {
                let request = api::SandboxListRequest {
                    app_id: app_id.clone(),
                    before_timestamp: before,
                    environment_name: String::new(),
                    include_finished: false,
                    tags: Vec::new(),
                };
                let page = self
                    .inner
                    .unary(
                        "list sandboxes",
                        request,
                        REQUEST_TIMEOUT,
                        |mut control, request| async move {
                            if v1 {
                                control.sandbox_list(request).await
                            } else {
                                control.sandbox_list_v2(request).await
                            }
                        },
                    )
                    .await?;
                let Some(oldest) = page.sandboxes.last() else {
                    break;
                };
                before = oldest.created_at;
                ids.extend(page.sandboxes.into_iter().map(|sandbox| sandbox.id));
            }
        }
        Ok(ids)
    }

    fn known(&self, id: &str) -> Option<Connection> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    fn session(&self, connection: Connection) -> ModalSession {
        ModalSession { inner: self.inner.clone(), connection }
    }

    /// The connection of `handle`, attaching to a sandbox another client created.
    async fn connection(&self, handle: &SessionHandle) -> Result<Connection, SandboxError> {
        if let Some(connection) = self.known(handle.as_str()) {
            return Ok(connection);
        }
        let connection = self.inner.attach(handle.as_str()).await?;
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.as_str().to_string(), connection.clone());
        Ok(connection)
    }

    async fn provision_from(
        &self,
        image_id: Option<&str>,
        manifest: &Manifest,
    ) -> Result<SessionHandle, SandboxError> {
        let image_id = match image_id {
            Some(id) => id.to_string(),
            None => self.inner.image_id().await?,
        };
        let connection = self.inner.create(&image_id).await?;
        let id = connection.sandbox_id.clone();
        self.live.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), connection.clone());
        if let Err(error) = populate(&self.session(connection), manifest).await {
            self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            if let Err(terminate) = self.inner.terminate(&id).await {
                tracing::warn!(sandbox.id = %id, error = %terminate, "could not terminate a sandbox whose manifest failed");
            }
            return Err(error);
        }
        tracing::info!(sandbox.provider = PROVIDER, sandbox.id = %id, image.id = %image_id, "created a sandbox");
        Ok(SessionHandle::new(id))
    }
}

impl std::fmt::Debug for ModalClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModalClient").field("config", &self.inner.config).finish_non_exhaustive()
    }
}

impl Drop for ModalClient {
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
            async move { inner.terminate(&id).await }
        });
    }
}

#[async_trait]
impl SandboxClient for ModalClient {
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        self.provision_from(None, manifest).await
    }

    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError> {
        Ok(Box::new(self.session(self.connection(handle).await?)))
    }

    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(handle.as_str());
        self.inner.terminate(handle.as_str()).await
    }

    async fn snapshot(&self, handle: &SessionHandle) -> Result<SnapshotId, SandboxError> {
        let connection = self.connection(handle).await?;
        let ttl = i64::try_from(self.inner.config.snapshot_ttl.as_secs()).unwrap_or(i64::MAX);
        let request = tcr::TaskSnapshotFilesystemRequest {
            task_id: connection.task_id.clone(),
            snapshot_id: uuid::Uuid::new_v4().to_string(),
            ttl_seconds: Some(ttl),
        };
        let response = self
            .inner
            .router_call(
                &connection,
                "snapshot filesystem",
                request,
                REQUEST_TIMEOUT * 2,
                Instant::now() + REQUEST_TIMEOUT * 2,
                |mut router, request| async move { router.task_snapshot_filesystem(request).await },
            )
            .await
            .map_err(|error| not_found(error, handle.as_str()))?;
        if response.image_id.is_empty() {
            return Err(remote_message("snapshot filesystem", "Modal returned no image id"));
        }
        Ok(SnapshotId::new(response.image_id))
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
            .with_remote(true)
            .with_network_blocked(self.inner.config.network_blocked)
            .with_max_lifetime(Duration::from_secs(u64::from(self.inner.config.timeout_secs())))
    }
}

/// A live Modal sandbox: commands and files through its task command router.
pub struct ModalSession {
    inner: Arc<Inner>,
    connection: Connection,
}

impl std::fmt::Debug for ModalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModalSession")
            .field("sandbox_id", &self.connection.sandbox_id)
            .field("task_id", &self.connection.task_id)
            .finish_non_exhaustive()
    }
}

impl ModalSession {
    /// The Modal sandbox id.
    pub fn sandbox_id(&self) -> &str {
        &self.connection.sandbox_id
    }
}

#[async_trait]
impl SandboxSession for ModalSession {
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
        let config = &self.inner.config;
        let cwd = match request.working_dir.as_deref() {
            Some(dir) => absolute(&config.workdir, dir)?,
            None => config.workdir.clone(),
        };
        for key in request.env.keys() {
            validate_env_name(key)?;
        }
        let timeout = request.timeout.unwrap_or(config.command_timeout);
        let mut env = config.env.clone();
        env.extend(request.env);
        let args = vec![
            "sh".to_string(),
            "-c".to_string(),
            RUN_SCRIPT.to_string(),
            "sh".to_string(),
            request.command,
        ];
        let started = Instant::now();
        let exec_id = self.inner.start_exec(&self.connection, args, cwd, env, timeout).await?;
        let mut stdout = Capture::new(MAX_OUTPUT_BYTES);
        let mut stderr = Capture::new(MAX_OUTPUT_BYTES);
        // Modal kills the command at `timeout`; the grace covers the exit's way back.
        let deadline = started + timeout + COMMAND_GRACE;
        let exit = self
            .inner
            .collect(&self.connection, &exec_id, deadline, &mut stdout, &mut stderr)
            .await;
        let exit = match exit {
            Ok(exit) => exit,
            Err(error) if is_timed_out(&error) => {
                return Ok(ExecOutput::new(
                    stdout.text(),
                    stderr.text(),
                    -1,
                    started.elapsed(),
                    true,
                ));
            }
            Err(error) => return Err(error),
        };
        // Modal reports a command it killed at its timeout as SIGKILL, by signal or as 137.
        let killed = match exit {
            Exit::Signal(_) => true,
            Exit::Code(code) => code > 128,
        };
        let elapsed = started.elapsed();
        if killed && elapsed >= timeout {
            return Ok(ExecOutput::new(stdout.text(), stderr.text(), -1, elapsed, true));
        }
        let code = match exit {
            Exit::Code(code) => code,
            Exit::Signal(signal) => 128 + signal,
        };
        Ok(ExecOutput::new(stdout.text(), stderr.text(), code, elapsed, false))
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        let full = absolute(&self.inner.config.workdir, path)?;
        let args = vec!["sh".into(), "-c".into(), READ_SCRIPT.into(), "sh".into(), full];
        let (exit, stdout, stderr) =
            self.inner.run_raw(&self.connection, "read file", args, None, MAX_FILE_BYTES).await?;
        if exit != Exit::Code(0) {
            let stderr = stderr.text();
            if stderr.contains("No such file or directory") {
                return Err(SandboxError::ExecutionFailed(format!("file not found: {path}")));
            }
            return Err(SandboxError::ExecutionFailed(format!(
                "cannot read {path}: {}",
                stderr.trim()
            )));
        }
        if stdout.overflowed {
            return Err(remote_message(
                "read file",
                format!("{path} exceeds the {MAX_FILE_BYTES}-byte limit"),
            ));
        }
        Ok(stdout.bytes)
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), SandboxError> {
        let full = absolute(&self.inner.config.workdir, path)?;
        if content.len() > MAX_FILE_BYTES {
            return Err(SandboxError::InvalidRequest(format!(
                "{path} is {} bytes; the limit is {MAX_FILE_BYTES}",
                content.len()
            )));
        }
        let args = vec!["sh".into(), "-c".into(), WRITE_SCRIPT.into(), "sh".into(), full];
        let (exit, _, stderr) = self
            .inner
            .run_raw(&self.connection, "write file", args, Some(content), MAX_OUTPUT_BYTES)
            .await?;
        if exit != Exit::Code(0) {
            return Err(SandboxError::ExecutionFailed(format!(
                "cannot write {path}: {}",
                stderr.text().trim()
            )));
        }
        Ok(())
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
    fn v1_ids_are_recognised_by_shape() {
        assert!(is_v1_sandbox_id("sb-0123456789ABCDEFGHIJKL"));
        assert!(!is_v1_sandbox_id("sb-01M7PH9H24HPDRTS5K7CKWVBHE"));
        assert!(!is_v1_sandbox_id("sb-short"));
        assert!(!is_v1_sandbox_id("ta-0123456789ABCDEFGHIJKL"));
    }

    #[test]
    fn grpc_codes_map_to_their_http_equivalents() {
        assert_eq!(http_status(Code::Unauthenticated), Some(401));
        assert_eq!(http_status(Code::PermissionDenied), Some(403));
        assert_eq!(http_status(Code::NotFound), Some(404));
        assert_eq!(http_status(Code::ResourceExhausted), Some(429));
        assert_eq!(http_status(Code::DeadlineExceeded), Some(504));
        assert_eq!(http_status(Code::Unavailable), Some(503));
        assert_eq!(http_status(Code::Cancelled), None);
    }

    #[test]
    fn whole_seconds_round_up() {
        assert_eq!(whole_seconds(Duration::from_millis(1)), 1);
        assert_eq!(whole_seconds(Duration::from_secs(5)), 5);
        assert_eq!(whole_seconds(Duration::from_millis(5001)), 6);
        assert_eq!(whole_seconds(Duration::ZERO), 1);
    }

    #[test]
    fn jwt_expiry_reads_exp() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":1900000000,"sub":"x"}"#);
        assert_eq!(jwt_expiry(&format!("h.{payload}.s")), Some(1_900_000_000));
        assert_eq!(jwt_expiry("not-a-jwt"), None);
    }

    #[test]
    fn errors_are_cut_and_keep_the_code() {
        let status = Status::not_found("x".repeat(2 * ERROR_EXCERPT_BYTES));
        let SandboxError::Remote { status, message, .. } = remote_error("op", &status) else {
            panic!("not a remote error");
        };
        assert_eq!(status, Some(404));
        assert!(message.starts_with("NotFound: "));
        assert_eq!(message.len(), ERROR_EXCERPT_BYTES);
    }

    #[test]
    fn name_prefixes_are_validated() {
        let config = ModalConfig::new("id", "secret").with_name_prefix("agent");
        let name = config.sandbox_name().unwrap().unwrap();
        assert!(name.starts_with("agent-") && name.len() == "agent-".len() + 12);
        assert!(
            ModalConfig::new("id", "secret").with_name_prefix("bad name").sandbox_name().is_err()
        );
        assert_eq!(ModalConfig::new("id", "secret").sandbox_name().unwrap(), None);
    }

    #[test]
    fn capture_keeps_the_limit_and_notes_overflow() {
        let mut capture = Capture::new(4);
        capture.push(b"abc");
        capture.push(b"def");
        assert_eq!(capture.bytes, b"abcd");
        assert!(capture.overflowed);
    }
}
