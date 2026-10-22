//! The control plane (spec 3.2 §7.11).
//!
//! One API, two transports in phase 1: a Unix domain socket at the kernel home (macOS,
//! Linux), which trusts the OS user, and HTTP on loopback or, with authentication, on the
//! network. The named pipe on Windows is §10 W2 (phase 4). Surfaces hold no state (K-I17);
//! every control action carries an authenticated subject and is audited as `kernel.control`
//! (K-I16).
//!
//! [`router`] is the API. `adk-rust up` merges it into `ServerBuilder` with
//! `add_api_routes`, so the server's auth middleware and `RequestContextExtractor` apply and
//! the routes are served under `/api`; [`ControlPlane`] serves the same router, under the
//! same `/api` prefix, on the socket and on [`ControlPlaneConfig::http`] when set.
//!
//! | Route ([`paths`], under `/api`) | Purpose |
//! |---|---|
//! | `GET /kernel/health` | [`KernelHealth`] |
//! | `GET /kernel/processes` | the process table, each process with its [`ProcessView`](crate::ProcessView) ([`api::ProcessesQuery`]) |
//! | `POST /kernel/processes/{pid}/{kill,nice,suspend,resume}` | [`ProcessTable`](crate::ProcessTable) actions |
//! | `GET /kernel/events` | the event bus as server-sent events, from a cursor ([`api::EventsQuery`]) |
//! | `POST /kernel/attach/{session}` | a surface attaches to a session: transcript cursor, pending holds, process |
//! | `POST /kernel/freeze`, `POST /kernel/unfreeze` | [`Kernel::freeze`] and [`Kernel::unfreeze`] |
//!
//! The approvals inbox, `/api/runs`, `/api/cron`, `/api/admin/freeze` and `/hooks/{name}`
//! are the existing server routes, unchanged; `POST /api/admin/freeze` delegates to
//! [`Kernel::freeze`] through the kernel's `FreezeDelegate` (§10 S4). Agenda, driver,
//! partition and `/proc` routes arrive with their phases.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::AdkError;
use adk_server::auth_bridge::RequestContextExtractor;
use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use crate::error::{KernelError, Result};
use crate::kernel::{Kernel, KernelHealth};

pub mod api;
mod client;
mod server;

pub use client::ControlClient;
pub use server::ControlPlane;

/// The control plane's route paths, relative to the `/api` prefix they are served under
/// (spec 3.2 §7.11).
pub mod paths {
    /// The prefix every route is served under, on the socket and through
    /// `ServerBuilder::add_api_routes`.
    pub const API_PREFIX: &str = "/api";
    /// `GET`: the kernel's health.
    pub const HEALTH: &str = "/kernel/health";
    /// `GET`: the process table.
    pub const PROCESSES: &str = "/kernel/processes";
    /// `POST`: kill a process.
    pub const KILL: &str = "/kernel/processes/{pid}/kill";
    /// `POST`: move a process to another queue.
    pub const NICE: &str = "/kernel/processes/{pid}/nice";
    /// `POST`: suspend a process.
    pub const SUSPEND: &str = "/kernel/processes/{pid}/suspend";
    /// `POST`: resume a process.
    pub const RESUME: &str = "/kernel/processes/{pid}/resume";
    /// `GET`: the event bus as server-sent events.
    pub const EVENTS: &str = "/kernel/events";
    /// `POST`: attach to a session.
    pub const ATTACH: &str = "/kernel/attach/{session}";
    /// `POST`: freeze execution.
    pub const FREEZE: &str = "/kernel/freeze";
    /// `POST`: lift the freeze.
    pub const UNFREEZE: &str = "/kernel/unfreeze";
}

/// Where the control plane listens and how it authenticates HTTP callers (spec 3.2 §7.11).
///
/// The socket trusts the OS user. HTTP authenticates with the extractor the caller passes:
/// adk-cli passes `LoopbackOperatorExtractor` on loopback and `BearerTokenExtractor`
/// otherwise, with the `operator` scope the approvals inbox routes to; SSO extractors from
/// `adk-auth` serve teams. A non-loopback bind without an extractor is refused (K-I18).
///
/// # Example
///
/// ```rust
/// use std::path::Path;
/// use adk_kernel::control::ControlPlaneConfig;
///
/// let config = ControlPlaneConfig::default();
/// assert_eq!(config.socket_path(Path::new("/home/a/.config/adk-rust")).unwrap(), Path::new("/home/a/.config/adk-rust/kernel.sock"));
/// assert!(config.validate().is_ok());
///
/// let exposed = ControlPlaneConfig::default().with_http("0.0.0.0:8080".parse().unwrap());
/// assert!(exposed.validate().is_err());
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct ControlPlaneConfig {
    /// The Unix socket, relative to the kernel home unless absolute; `None` serves no
    /// socket. Defaults to [`DEFAULT_SOCKET`](Self::DEFAULT_SOCKET).
    pub socket: Option<PathBuf>,
    /// An HTTP bind the kernel serves itself; `None` when the routes are merged into a
    /// `ServerBuilder` instead. Defaults to `None`.
    pub http: Option<SocketAddr>,
    /// Authenticates HTTP callers; required on a non-loopback bind (K-I18).
    pub extractor: Option<Arc<dyn RequestContextExtractor>>,
}

impl ControlPlaneConfig {
    /// The socket's file name under the kernel home.
    pub const DEFAULT_SOCKET: &'static str = "kernel.sock";

    /// Serves the socket at `path`, relative to the kernel home unless absolute.
    #[must_use]
    pub fn with_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket = Some(path.into());
        self
    }

    /// Serves no socket.
    #[must_use]
    pub fn without_socket(mut self) -> Self {
        self.socket = None;
        self
    }

    /// Serves HTTP on `addr`.
    #[must_use]
    pub fn with_http(mut self, addr: SocketAddr) -> Self {
        self.http = Some(addr);
        self
    }

    /// Authenticates HTTP callers with `extractor`.
    #[must_use]
    pub fn with_extractor(mut self, extractor: Arc<dyn RequestContextExtractor>) -> Self {
        self.extractor = Some(extractor);
        self
    }

    /// The socket's path under `home`, or `None` when no socket is served.
    pub fn socket_path(&self, home: &Path) -> Option<PathBuf> {
        self.socket.as_ref().map(|socket| home.join(socket))
    }

    /// Refuses a non-loopback HTTP bind without an extractor (K-I18).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] naming the bind.
    pub fn validate(&self) -> Result<()> {
        match self.http {
            Some(addr) if !addr.ip().is_loopback() && self.extractor.is_none() => {
                Err(KernelError::Config {
                    reason: format!(
                        "serving the control plane on {addr} needs authentication: pass a \
                         bearer token or SSO extractor, or bind 127.0.0.1"
                    ),
                })
            }
            _ => Ok(()),
        }
    }
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self { socket: Some(PathBuf::from(Self::DEFAULT_SOCKET)), http: None, extractor: None }
    }
}

impl fmt::Debug for ControlPlaneConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlPlaneConfig")
            .field("socket", &self.socket)
            .field("http", &self.http)
            .field("extractor", &self.extractor.is_some())
            .finish()
    }
}

/// The transport a control action arrived on, as the `kernel.control` audit record names it
/// (spec 3.2 §9.3).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::ControlTransport;
///
/// assert_eq!(serde_json::to_value(ControlTransport::Socket).unwrap(), "socket");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ControlTransport {
    /// The Unix socket.
    Socket,
    /// HTTP.
    Http,
}

/// The control plane's routes over `kernel` (spec 3.2 §7.11), relative to the `/api`
/// prefix: merge them with `ServerBuilder::add_api_routes`, whose auth middleware puts the
/// caller's `RequestContext` in the request extensions.
///
/// # Example
///
/// ```rust,no_run
/// # fn example(kernel: adk_kernel::Kernel, builder: adk_server::ServerBuilder) {
/// let builder = builder.add_api_routes(adk_kernel::control::router(kernel));
/// # let _ = builder;
/// # }
/// ```
pub fn router(kernel: Kernel) -> Router {
    Router::new()
        .route(paths::HEALTH, get(health))
        .route(paths::PROCESSES, get(processes))
        .route(paths::KILL, post(kill))
        .route(paths::NICE, post(nice))
        .route(paths::SUSPEND, post(suspend))
        .route(paths::RESUME, post(resume))
        .route(paths::EVENTS, get(events))
        .route(paths::ATTACH, post(attach))
        .route(paths::FREEZE, post(freeze))
        .route(paths::UNFREEZE, post(unfreeze))
        .with_state(kernel)
}

impl IntoResponse for KernelError {
    fn into_response(self) -> Response {
        let error = AdkError::from(self);
        let status = StatusCode::from_u16(error.http_status_code())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(error.to_problem_json())).into_response()
    }
}

async fn health(State(kernel): State<Kernel>) -> Json<KernelHealth> {
    Json(kernel.health())
}

async fn processes(
    State(_kernel): State<Kernel>,
    Query(_query): Query<api::ProcessesQuery>,
) -> Result<Json<api::ProcessesResponse>> {
    Err(KernelError::Unimplemented { item: "control::processes" })
}

async fn kill(
    State(_kernel): State<Kernel>,
    UrlPath(_pid): UrlPath<String>,
    Json(_request): Json<api::KillRequest>,
) -> Result<Json<api::ProcessResponse>> {
    Err(KernelError::Unimplemented { item: "control::kill" })
}

async fn nice(
    State(_kernel): State<Kernel>,
    UrlPath(_pid): UrlPath<String>,
    Json(_request): Json<api::NiceRequest>,
) -> Result<Json<api::ProcessResponse>> {
    Err(KernelError::Unimplemented { item: "control::nice" })
}

async fn suspend(
    State(_kernel): State<Kernel>,
    UrlPath(_pid): UrlPath<String>,
) -> Result<Json<api::ProcessResponse>> {
    Err(KernelError::Unimplemented { item: "control::suspend" })
}

async fn resume(
    State(_kernel): State<Kernel>,
    UrlPath(_pid): UrlPath<String>,
) -> Result<Json<api::ProcessResponse>> {
    Err(KernelError::Unimplemented { item: "control::resume" })
}

async fn events(
    State(_kernel): State<Kernel>,
    Query(_query): Query<api::EventsQuery>,
) -> Result<Response> {
    Err(KernelError::Unimplemented { item: "control::events" })
}

async fn attach(
    State(_kernel): State<Kernel>,
    UrlPath(_session): UrlPath<String>,
    Json(_request): Json<api::AttachRequest>,
) -> Result<Json<api::AttachResponse>> {
    Err(KernelError::Unimplemented { item: "control::attach" })
}

async fn freeze(
    State(_kernel): State<Kernel>,
    Json(_request): Json<api::FreezeRequest>,
) -> Result<Json<api::FreezeResponse>> {
    Err(KernelError::Unimplemented { item: "control::freeze" })
}

async fn unfreeze(State(_kernel): State<Kernel>) -> Result<Json<api::FreezeResponse>> {
    Err(KernelError::Unimplemented { item: "control::unfreeze" })
}
