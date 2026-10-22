//! The control plane's listeners (spec 3.2 §7.11).

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use tokio::task::JoinHandle;

use crate::error::{KernelError, Result};
use crate::kernel::Kernel;

/// The running listeners of a kernel's control plane (spec 3.2 §7.11): the Unix socket
/// and, when [`ControlPlaneConfig::http`](super::ControlPlaneConfig::http) is set, an HTTP
/// bind, both serving [`router`](super::router) under `/api`.
///
/// The socket is created owner-only and every request on it carries the OS user as an
/// authenticated subject; HTTP requests carry the configured extractor's subject (K-I16).
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
    http: Option<SocketAddr>,
    // Unimplemented: awaited by `shutdown` once feat/kernel-cli lands (spec 3.2 §7.11).
    #[allow(dead_code)]
    tasks: Vec<JoinHandle<()>>,
}

impl fmt::Debug for ControlPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlPlane")
            .field("socket", &self.socket)
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
    /// [`KernelError::ControlPlane`] when the socket path or bind is taken.
    pub async fn start(_kernel: &Kernel) -> Result<Self> {
        Err(KernelError::Unimplemented { item: "control::ControlPlane::start" })
    }

    /// The socket's path, when one is served.
    pub fn socket_path(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// The HTTP address, when one is served.
    pub fn http_addr(&self) -> Option<SocketAddr> {
        self.http
    }

    /// Stops accepting, finishes the requests in flight and removes the socket.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Io`] when the socket cannot be removed.
    pub async fn shutdown(self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "control::ControlPlane::shutdown" })
    }
}
