//! The control plane's listeners (spec 3.2 §7.11).

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::RequestContext;
use adk_server::approvals::{ApprovalEndpoints, approvals_router};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::routes::{ControlBackend, OPERATOR_SCOPE, routes};
use super::{ControlPlaneConfig, ControlTransport, paths};
use crate::error::{KernelError, Result};
use crate::kernel::Kernel;

/// How long [`ControlPlane::shutdown`] lets requests in flight finish; an event stream
/// never does, so it is cut after this.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// The running listeners of a kernel's control plane (spec 3.2 §7.11): the Unix socket, or
/// on Windows the named pipe [`ControlPlaneConfig::pipe_name`] names (§10 W2), and, when
/// [`ControlPlaneConfig::http`](super::ControlPlaneConfig::http) is set, an HTTP bind, all
/// serving [`router`](super::router) under `/api`, with the approvals inbox beside it when
/// the kernel has an approval service.
///
/// The socket is created owner-only and admits only peers running as its owner, each as
/// the OS user with the `operator` scope. The pipe carries a protected DACL that admits its
/// owner, the user the kernel runs as, alone (§10 W4), and admits a connection only when the
/// token the client opened it with is that user's. HTTP requests carry the configured
/// extractor's subject, or, on a loopback bind without one, loopback peers as the OS user
/// (K-I16). A non-loopback bind without an extractor is refused (K-I18). On a platform with
/// neither, `start` with a socket configured answers [`KernelError::ControlPlane`].
/// [`Kernel::start`] starts it and [`Kernel::stop`] shuts it down.
///
/// # Example
///
/// ```rust,no_run
/// use adk_kernel::control::ControlPlane;
///
/// # async fn example(kernel: adk_kernel::Kernel) -> adk_kernel::Result<()> {
/// let plane = ControlPlane::start(&kernel).await?;
/// println!("listening on {:?}", plane.socket_path());
/// plane.shutdown().await?;
/// # Ok(())
/// # }
/// ```
pub struct ControlPlane {
    socket: Option<PathBuf>,
    pipe: Option<String>,
    http: Option<SocketAddr>,
    tasks: Vec<JoinHandle<()>>,
    stop: CancellationToken,
}

impl fmt::Debug for ControlPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlPlane")
            .field("socket", &self.socket)
            .field("pipe", &self.pipe)
            .field("http", &self.http)
            .finish_non_exhaustive()
    }
}

impl ControlPlane {
    /// Serves `kernel`'s control plane as its configuration says.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for a refused configuration (K-I18) and
    /// [`KernelError::ControlPlane`] when the socket path, pipe or bind is taken, or when a
    /// socket is configured on a platform with neither Unix sockets nor named pipes.
    pub async fn start(kernel: &Kernel) -> Result<Self> {
        let config = kernel.config();
        Self::serve(Arc::new(kernel.clone()), &config.control, &config.home).await
    }

    /// Serves `backend` as `config` says, the socket path relative to `home`.
    pub(crate) async fn serve(
        backend: Arc<dyn ControlBackend>,
        config: &ControlPlaneConfig,
        home: &Path,
    ) -> Result<Self> {
        config.validate()?;
        let mut plane = Self {
            socket: None,
            pipe: None,
            http: None,
            tasks: Vec::new(),
            stop: CancellationToken::new(),
        };
        let api = api(&backend);
        #[cfg(windows)]
        if let Some(name) = config.pipe_name(home) {
            plane.tasks.push(super::pipe::serve(&name, api.clone(), plane.stop.clone()).await?);
            plane.pipe = Some(name);
        }
        #[cfg(not(windows))]
        if let Some(path) = config.socket_path(home) {
            // A failure drops `plane`, which stops whatever already listens.
            plane.tasks.push(serve_socket(&path, api.clone(), plane.stop.clone()).await?);
            plane.socket = Some(path);
        }
        if let Some(addr) = config.http {
            let (local, task) =
                serve_http(addr, api, &backend, config.extractor.clone(), plane.stop.clone())
                    .await?;
            plane.tasks.push(task);
            plane.http = Some(local);
        }
        tracing::info!(
            control.socket = ?plane.socket,
            control.pipe = ?plane.pipe,
            control.http = ?plane.http,
            "kernel control plane listening"
        );
        Ok(plane)
    }

    /// The socket's path, when one is served.
    pub fn socket_path(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// The named pipe's name, when one is served (Windows; spec 3.2 §10 W2).
    pub fn pipe_name(&self) -> Option<&str> {
        self.pipe.as_deref()
    }

    /// The HTTP address, when one is served: the bound address, so a configured port 0
    /// reads as the port the system chose.
    pub fn http_addr(&self) -> Option<SocketAddr> {
        self.http
    }

    /// Stops accepting, finishes the requests in flight and removes the socket.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Io`] when the socket cannot be removed.
    pub async fn shutdown(mut self) -> Result<()> {
        self.stop.cancel();
        for task in std::mem::take(&mut self.tasks) {
            let abort = task.abort_handle();
            if tokio::time::timeout(SHUTDOWN_GRACE, task).await.is_err() {
                abort.abort();
            }
        }
        match self.socket.take() {
            Some(path) => remove_socket(&path),
            None => Ok(()),
        }
    }
}

impl Drop for ControlPlane {
    fn drop(&mut self) {
        self.stop.cancel();
        for task in &self.tasks {
            task.abort();
        }
        if let Some(path) = self.socket.take()
            && let Err(error) = remove_socket(&path)
        {
            tracing::warn!(error = %error, socket.path = %path.display(), "could not remove the kernel socket");
        }
    }
}

fn remove_socket(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The kernel routes under `/api`, with the approvals inbox when the backend has an
/// approval service, its decisions checked by the kernel's decide guard.
pub(super) fn api(backend: &Arc<dyn ControlBackend>) -> Router {
    let mut routes = routes(backend.clone());
    if let Some(service) = backend.approvals() {
        // Each decision is answered under spec 3.3 §7.2 for the surface this transport is.
        let guard = Arc::new(super::decide::KernelDecideGuard::new(backend.clone()));
        let endpoints = ApprovalEndpoints::new(service.clone()).with_decide_guard(guard);
        routes = routes.merge(approvals_router(endpoints));
    }
    Router::new().nest(paths::API_PREFIX, routes)
}

/// The OS user the kernel runs as, the subject of the socket and of loopback peers.
pub(crate) fn os_user() -> String {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}

pub(super) fn operator(user_id: &str) -> RequestContext {
    RequestContext {
        user_id: user_id.to_string(),
        scopes: vec![OPERATOR_SCOPE.to_string()],
        metadata: HashMap::new(),
    }
}

/// Marks a request authenticated as `context` over `transport`, as the server's auth
/// middleware marks one, and runs it.
pub(super) async fn admit(
    mut request: Request<Body>,
    next: Next,
    context: RequestContext,
    transport: ControlTransport,
) -> Response {
    request.extensions_mut().insert(Some(context));
    request.extensions_mut().insert(transport);
    next.run(request).await
}

/// The uid of a socket peer, and its process when the OS says, from its credentials.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct PeerUid(Option<u32>, Option<u32>);

/// The process that sent a request on the socket, when the OS says (spec 3.3 §5.2): the
/// command routes record it.
#[derive(Debug, Clone, Copy)]
pub(super) struct PeerProcess(pub(super) u32);

#[cfg(unix)]
impl
    axum::extract::connect_info::Connected<
        axum::serve::IncomingStream<'_, tokio::net::UnixListener>,
    > for PeerUid
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        let credentials = stream.io().peer_cred().ok();
        let pid = credentials.and_then(|credentials| credentials.pid());
        Self(
            credentials.map(|credentials| credentials.uid()),
            pid.and_then(|pid| u32::try_from(pid).ok()),
        )
    }
}

/// The subject of a socket peer of uid `peer` on a socket `owner` owns: the OS user `user`
/// with the `operator` scope when the uids match, else refused.
#[cfg(unix)]
pub(crate) fn socket_subject(peer: Option<u32>, owner: u32, user: &str) -> Result<RequestContext> {
    match peer {
        Some(uid) if uid == owner => Ok(operator(user)),
        _ => Err(KernelError::Unauthorized {
            reason: "the kernel socket admits only the OS user that runs the kernel".into(),
        }),
    }
}

/// Serves `api` on a socket at `path`, created owner-only, admitting peers of its owner's
/// uid as the OS user.
#[cfg(unix)]
async fn serve_socket(path: &Path, api: Router, stop: CancellationToken) -> Result<JoinHandle<()>> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    prepare_socket_path(path).await?;
    let listener =
        tokio::net::UnixListener::bind(path).map_err(|error| KernelError::ControlPlane {
            reason: format!(
                "could not listen on {}: {error}; a socket path is limited to about 100 bytes, \
                 so choose a shorter kernel home if it is longer",
                path.display()
            ),
        })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    // The kernel created the socket, so its owner is the uid the kernel runs as.
    let owner = std::fs::metadata(path)?.uid();
    let user = os_user();
    let app = api.layer(middleware::from_fn(move |mut request: Request<Body>, next: Next| {
        let user = user.clone();
        async move {
            let (peer, pid) = request
                .extensions()
                .get::<ConnectInfo<PeerUid>>()
                .map_or((None, None), |ConnectInfo(PeerUid(uid, pid))| (*uid, *pid));
            if let Some(pid) = pid {
                request.extensions_mut().insert(PeerProcess(pid));
            }
            match socket_subject(peer, owner, &user) {
                Ok(context) => admit(request, next, context, ControlTransport::Socket).await,
                Err(error) => error.into_response(),
            }
        }
    }));
    let service = app.into_make_service_with_connect_info::<PeerUid>();
    Ok(tokio::spawn(async move {
        if let Err(error) =
            axum::serve(listener, service).with_graceful_shutdown(stop.cancelled_owned()).await
        {
            tracing::error!(error = %error, "the kernel control socket stopped");
        }
    }))
}

/// Clears a stale socket left by a kernel that did not stop cleanly, and refuses a path
/// another kernel answers on or that is not a socket.
#[cfg(unix)]
async fn prepare_socket_path(path: &Path) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_socket() {
        return Err(KernelError::ControlPlane {
            reason: format!(
                "{} exists and is not a socket; move it away, or give the kernel another home",
                path.display()
            ),
        });
    }
    match tokio::net::UnixStream::connect(path).await {
        Ok(_) => Err(KernelError::ControlPlane {
            reason: format!(
                "a kernel already listens on {}: attach to it with `adk-rust attach`, or stop \
                 it before starting another with the same home",
                path.display()
            ),
        }),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            tracing::info!(socket.path = %path.display(), "removing a stale kernel socket");
            remove_socket(path)
        }
        Err(error) => Err(KernelError::ControlPlane {
            reason: format!("could not check the kernel socket {}: {error}", path.display()),
        }),
    }
}

#[cfg(not(any(unix, windows)))]
async fn serve_socket(
    path: &Path,
    _api: Router,
    _stop: CancellationToken,
) -> Result<JoinHandle<()>> {
    Err(KernelError::ControlPlane {
        reason: format!(
            "the local control socket {} needs Unix sockets or Windows named pipes. Serve the \
             control plane over HTTP without a socket (ControlPlaneConfig::without_socket and \
             with_http, or `adk-rust up --bind`)",
            path.display()
        ),
    })
}

/// Serves `api` over HTTP on `addr`, authenticating with `extractor`, or admitting loopback
/// peers as the OS user when there is none (a non-loopback bind without one never gets
/// here: `ControlPlaneConfig::validate` refuses it). A request carrying a paired device's
/// token is that device whatever else would admit it (spec 3.3 §10 K2), and the pairing
/// claim route needs no caller.
async fn serve_http(
    addr: SocketAddr,
    api: Router,
    backend: &Arc<dyn ControlBackend>,
    extractor: Option<Arc<dyn RequestContextExtractor>>,
    stop: CancellationToken,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|error| {
        KernelError::ControlPlane { reason: format!("could not listen on {addr}: {error}") }
    })?;
    let local = listener.local_addr()?;
    let user = os_user();
    let devices = backend.clone();
    let app = api.layer(middleware::from_fn(move |request: Request<Body>, next: Next| {
        let extractor = extractor.clone();
        let user = user.clone();
        let backend = devices.clone();
        async move {
            let (parts, body) = request.into_parts();
            if let Some(pairing) = backend.pairing()
                && let Some(token) = crate::pairing::device_token(&parts.headers)
            {
                let (token, path) = (token.to_string(), parts.uri.path().to_string());
                let (request, answer) =
                    match super::pairing::read_answer(Request::from_parts(parts, body)).await {
                        Ok(read) => read,
                        Err(response) => return response,
                    };
                let (parts, body) = request.into_parts();
                return match super::pairing::admit_device(
                    backend.as_ref(),
                    &pairing,
                    &token,
                    &parts.method,
                    &path,
                    answer.as_deref(),
                )
                .await
                {
                    Ok(context) => {
                        let request = Request::from_parts(parts, body);
                        admit(request, next, context, ControlTransport::Http).await
                    }
                    Err(response) => response,
                };
            }
            let context = match &extractor {
                Some(extractor) => match extractor.extract(&parts).await {
                    Ok(context) => context,
                    Err(error @ RequestContextError::ExtractionFailed(_)) => {
                        return KernelError::ControlPlane { reason: error.to_string() }
                            .into_response();
                    }
                    Err(error) => {
                        return KernelError::Unauthorized { reason: error.to_string() }
                            .into_response();
                    }
                },
                None => {
                    let loopback = parts
                        .extensions
                        .get::<ConnectInfo<SocketAddr>>()
                        .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback());
                    if !loopback {
                        return KernelError::Unauthorized {
                            reason: "only loopback peers are admitted without a bearer token \
                                     or SSO"
                                .into(),
                        }
                        .into_response();
                    }
                    operator(&user)
                }
            };
            admit(Request::from_parts(parts, body), next, context, ControlTransport::Http).await
        }
    }));
    let app = match backend.pairing() {
        Some(_) => app.merge(super::pairing::claim_routes(backend.clone())),
        None => app,
    };
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        if let Err(error) =
            axum::serve(listener, service).with_graceful_shutdown(stop.cancelled_owned()).await
        {
            tracing::error!(error = %error, "the kernel control plane's HTTP listener stopped");
        }
    });
    Ok((local, task))
}
