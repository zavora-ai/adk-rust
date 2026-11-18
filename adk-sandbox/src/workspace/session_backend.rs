//! [`SessionBackend`]: a [`SandboxBackend`] that runs every request in one environment of a
//! [`SandboxClient`], so [`SandboxTool`](crate::SandboxTool) and the `adk-code` executors can
//! run code in a container or a remote microVM as well as in a local process.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::client::SandboxClient;
use super::manifest::Manifest;
use super::release::release_in_background;
use super::session::{SandboxSession, shell_quote};
use super::types::{CommandRequest, SessionHandle};
use crate::backend::{BackendCapabilities, EnforcedLimits, SandboxBackend};
use crate::error::SandboxError;
use crate::types::{ExecRequest, ExecResult, Language};

/// The directory, relative to the workspace root, that holds each request's program and stdin
/// while it runs.
const EXEC_DIR: &str = ".adk-exec";

/// A provisioned environment and its live session.
#[derive(Clone)]
struct Live {
    handle: SessionHandle,
    session: Arc<dyn SandboxSession>,
}

/// A [`SandboxBackend`] over a [`SandboxClient`]: the first request provisions an
/// environment from the manifest, every request runs in it, and the environment is
/// destroyed by [`close`](Self::close), when the backend drops, or when a request is
/// cancelled mid-run, since a cancelled remote command cannot be stopped any other way.
///
/// Each request's program is written to `.adk-exec/` in the workspace and removed when it
/// ends. The default languages are Python (`python3`), JavaScript (`node`) and shell
/// commands (`sh`); [`with_languages`](Self::with_languages) adds Rust (`rustc`) for an
/// image that has it.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_sandbox::workspace::{LocalUnixClient, SessionBackend};
/// use adk_sandbox::{ExecRequest, Language, SandboxBackend, SandboxTool};
///
/// # async fn run() -> Result<(), adk_sandbox::SandboxError> {
/// let client = Arc::new(LocalUnixClient::new(None, std::env::temp_dir().join("snapshots")));
/// let backend = Arc::new(SessionBackend::new(client));
/// let result = backend
///     .execute(ExecRequest {
///         language: Language::Python,
///         code: "print(6 * 7)".to_string(),
///         stdin: None,
///         timeout: std::time::Duration::from_secs(10),
///         memory_limit_mb: None,
///         env: Default::default(),
///     })
///     .await?;
/// assert_eq!(result.stdout.trim(), "42");
/// let tool = SandboxTool::new(backend.clone());
/// # let _ = tool;
/// backend.close().await?;
/// # Ok(())
/// # }
/// ```
pub struct SessionBackend {
    client: Arc<dyn SandboxClient>,
    manifest: Manifest,
    languages: Vec<Language>,
    live: Slot,
}

impl SessionBackend {
    /// A backend whose environment `client` provisions from an empty manifest.
    pub fn new(client: Arc<dyn SandboxClient>) -> Self {
        Self {
            client,
            manifest: Manifest::new(Vec::new()),
            languages: vec![Language::Python, Language::JavaScript, Language::Command],
            live: Arc::new(Mutex::new(None)),
        }
    }

    /// Provisions the environment from `manifest` instead of an empty one.
    #[must_use]
    pub fn with_manifest(mut self, manifest: Manifest) -> Self {
        self.manifest = manifest;
        self
    }

    /// The languages requests may use. `Wasm` and `TypeScript` are refused whatever this
    /// says: no interpreter for them is assumed in an environment.
    #[must_use]
    pub fn with_languages(mut self, languages: Vec<Language>) -> Self {
        self.languages = languages;
        self
    }

    /// The client environments come from.
    pub fn client(&self) -> &Arc<dyn SandboxClient> {
        &self.client
    }

    /// The handle of the live environment, if one is provisioned.
    pub async fn handle(&self) -> Option<SessionHandle> {
        self.live.lock().await.as_ref().map(|live| live.handle.clone())
    }

    /// Destroys the live environment, if any; the next request provisions a new one.
    ///
    /// # Errors
    ///
    /// Returns what the client's `stop` returns.
    pub async fn close(&self) -> Result<(), SandboxError> {
        let live = self.live.lock().await.take();
        match live {
            Some(live) => self.client.stop(&live.handle).await,
            None => Ok(()),
        }
    }

    /// The live environment, provisioned on first use.
    async fn session(&self) -> Result<Live, SandboxError> {
        let mut slot = self.live.lock().await;
        if let Some(live) = slot.as_ref() {
            return Ok(live.clone());
        }
        let handle = self.client.provision(&self.manifest).await?;
        let session = match self.client.start(&handle).await {
            Ok(session) => Arc::from(session),
            Err(error) => {
                if let Err(stop) = self.client.stop(&handle).await {
                    tracing::warn!(error = %stop, "could not stop an environment that failed to start");
                }
                return Err(error);
            }
        };
        tracing::info!(
            sandbox.provider = self.client.provider(),
            sandbox.handle = handle.as_str(),
            "provisioned a sandbox environment"
        );
        let live = Live { handle, session };
        *slot = Some(live.clone());
        Ok(live)
    }
}

impl std::fmt::Debug for SessionBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionBackend")
            .field("provider", &self.client.provider())
            .field("languages", &self.languages)
            .finish_non_exhaustive()
    }
}

impl Drop for SessionBackend {
    fn drop(&mut self) {
        let live = match self.live.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        if let Some(live) = live {
            release_in_background(self.client.clone(), live.handle, "backend dropped");
        }
    }
}

/// The live environment of a backend, shared with its cancel guards.
type Slot = Arc<Mutex<Option<Live>>>;

/// The environment a running request uses, released if the request is dropped.
struct Armed {
    client: Arc<dyn SandboxClient>,
    slot: Slot,
    handle: SessionHandle,
}

/// Destroys the environment when a request is dropped before its command returned.
struct CancelGuard {
    armed: Option<Armed>,
}

impl CancelGuard {
    fn disarm(mut self) {
        self.armed = None;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        let Some(Armed { client, slot, handle }) = self.armed.take() else {
            return;
        };
        // The slot is cleared first so no later request reuses an environment whose stop is
        // already under way.
        if let Ok(mut slot) = slot.try_lock()
            && slot.as_ref().is_some_and(|live| live.handle == handle)
        {
            *slot = None;
        }
        release_in_background(client, handle, "request cancelled mid-command");
    }
}

/// The program file extension and the command that runs it, for `language`.
fn interpreter(language: Language) -> Option<(&'static str, &'static str)> {
    match language {
        Language::Python => Some(("py", "python3")),
        Language::JavaScript => Some(("js", "node")),
        Language::Command => Some(("sh", "sh")),
        Language::Rust => Some(("rs", "rustc")),
        Language::TypeScript | Language::Wasm => None,
    }
}

#[async_trait]
impl SandboxBackend for SessionBackend {
    fn name(&self) -> &str {
        self.client.provider()
    }

    fn capabilities(&self) -> BackendCapabilities {
        let remote = self.client.capabilities().remote;
        BackendCapabilities {
            supported_languages: self.languages.clone(),
            isolation_class: if remote { "remote" } else { "session" }.to_string(),
            enforced_limits: EnforcedLimits {
                timeout: true,
                memory: false,
                network_isolation: false,
                // A remote environment cannot reach the host's files or variables at all.
                filesystem_write_isolation: remote,
                filesystem_read_isolation: remote,
                environment_isolation: remote,
            },
        }
    }

    async fn execute(&self, request: ExecRequest) -> Result<ExecResult, SandboxError> {
        let (extension, program) = interpreter(request.language)
            .filter(|_| self.languages.contains(&request.language))
            .ok_or_else(|| {
                SandboxError::InvalidRequest(format!(
                    "{} cannot run {} code; it runs {}",
                    self.client.provider(),
                    request.language,
                    self.languages.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
                ))
            })?;
        let live = self.session().await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let source = format!("{EXEC_DIR}/{id}.{extension}");
        live.session.write_file(&source, request.code.as_bytes()).await?;
        let stdin = match &request.stdin {
            Some(input) => {
                let path = format!("{EXEC_DIR}/{id}.stdin");
                live.session.write_file(&path, input.as_bytes()).await?;
                shell_quote(&path)
            }
            None => "/dev/null".to_string(),
        };
        let run = match request.language {
            Language::Rust => {
                let binary = shell_quote(&format!("{EXEC_DIR}/{id}.bin"));
                format!("rustc -O -o {binary} {} && {binary}", shell_quote(&source))
            }
            _ => format!("{program} {}", shell_quote(&source)),
        };
        let mut command = CommandRequest::new(format!(
            "{{ {run}; }} < {stdin}; adk_status=$?; rm -f {}.*; exit $adk_status",
            shell_quote(&format!("{EXEC_DIR}/{id}"))
        ))
        .timeout(request.timeout);
        command.env = request.env.into_iter().collect();

        let guard = CancelGuard {
            armed: Some(Armed {
                client: self.client.clone(),
                slot: self.live.clone(),
                handle: live.handle.clone(),
            }),
        };
        let output = live.session.exec(command).await;
        guard.disarm();
        let output = output?;
        if output.timed_out {
            return Err(SandboxError::Timeout { timeout: request.timeout });
        }
        Ok(ExecResult {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
            duration: output.duration,
        })
    }
}
