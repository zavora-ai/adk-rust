//! The control-plane client the CLI and other surfaces use (spec 3.2 §7.11, §8.1).

use std::path::{Path, PathBuf};

use futures::stream::BoxStream;

use super::api::{AttachRequest, AttachResponse, FreezeResponse, ProcessesQuery};
use crate::bus::EventRecord;
use crate::error::{KernelError, Result};
use crate::kernel::KernelHealth;
use crate::process::Process;
use crate::sched::QueueClass;

/// A client of a running kernel's control plane (spec 3.2 §7.11), over its Unix socket or
/// HTTP. Surfaces hold no state (K-I17): every call reads the kernel.
///
/// # Example
///
/// ```rust,no_run
/// use adk_kernel::control::ControlClient;
///
/// # async fn example(home: &std::path::Path) -> adk_kernel::Result<()> {
/// let client = match ControlClient::connect_local(home).await? {
///     Some(client) => client,
///     None => ControlClient::http("https://kernel.example.com").with_bearer_token("token"),
/// };
/// for process in client.processes(&Default::default()).await? {
///     println!("{} {:?}", process.meta.pid, process.run.state);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ControlClient {
    transport: Transport,
    // Unimplemented: sent by every call once feat/kernel-cli lands (spec 3.2 §7.11).
    #[allow(dead_code)]
    token: Option<String>,
}

#[derive(Debug, Clone)]
enum Transport {
    Socket(PathBuf),
    Http(String),
}

impl ControlClient {
    /// A client of the kernel listening on the Unix socket at `path`.
    pub fn socket(path: impl Into<PathBuf>) -> Self {
        Self { transport: Transport::Socket(path.into()), token: None }
    }

    /// A client of the kernel serving HTTP at `base_url`, the routes under its `/api`.
    pub fn http(base_url: impl Into<String>) -> Self {
        Self { transport: Transport::Http(base_url.into()), token: None }
    }

    /// Sends `token` as a bearer token.
    #[must_use]
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Where the client connects: `unix:<path>` or the base URL.
    pub fn endpoint(&self) -> String {
        match &self.transport {
            Transport::Socket(path) => format!("unix:{}", path.display()),
            Transport::Http(base) => base.clone(),
        }
    }

    /// A client of the kernel running at `home`, or `None` when none answers on its socket.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the socket exists but cannot be read.
    pub async fn connect_local(_home: &Path) -> Result<Option<Self>> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::connect_local" })
    }

    /// `GET /kernel/health`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn health(&self) -> Result<KernelHealth> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::health" })
    }

    /// `GET /kernel/processes`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn processes(&self, _query: &ProcessesQuery) -> Result<Vec<Process>> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::processes" })
    }

    /// `POST /kernel/processes/{pid}/kill`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn kill(&self, _pid: &str, _reason: Option<&str>) -> Result<Process> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::kill" })
    }

    /// `POST /kernel/processes/{pid}/nice`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn nice(&self, _pid: &str, _queue: QueueClass) -> Result<Process> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::nice" })
    }

    /// `POST /kernel/processes/{pid}/suspend`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn suspend(&self, _pid: &str) -> Result<Process> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::suspend" })
    }

    /// `POST /kernel/processes/{pid}/resume`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn resume(&self, _pid: &str) -> Result<Process> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::resume" })
    }

    /// `POST /kernel/freeze`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn freeze(&self, _reason: Option<&str>) -> Result<FreezeResponse> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::freeze" })
    }

    /// `POST /kernel/unfreeze`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn unfreeze(&self) -> Result<FreezeResponse> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::unfreeze" })
    }

    /// `GET /kernel/events`: the event bus from just after `after`, then live.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached; a broken
    /// stream ends with the error as its last item.
    pub async fn events(
        &self,
        _after: Option<u64>,
    ) -> Result<BoxStream<'static, Result<EventRecord>>> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::events" })
    }

    /// `POST /kernel/attach/{session}`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn attach(
        &self,
        _session_id: &str,
        _request: &AttachRequest,
    ) -> Result<AttachResponse> {
        Err(KernelError::Unimplemented { item: "control::ControlClient::attach" })
    }
}
