//! Per-session sandbox lifecycle management — adk-python
//! `AgentEngineSandboxCodeExecutor` parity.

use super::client::VertexSandboxClient;
use super::types::{
    CodeExecutionEnvironment, CreateSandboxRequest, InputFile, SandboxEnvironmentSpec,
    SandboxExecutionResult, SandboxState,
};
use super::{DEFAULT_SANDBOX_DISPLAY_NAME, DEFAULT_SANDBOX_TTL, errors};
use adk_core::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

/// Where the executor runs code.
#[derive(Debug, Clone)]
enum ExecutorTarget {
    /// A fixed, pre-provisioned sandbox used for every session.
    Sandbox(String),
    /// A reasoning engine under which sandboxes are created per session.
    Engine(String),
}

/// Executes code in Agent Engine sandboxes with per-session lifecycle
/// management, matching adk-python's `AgentEngineSandboxCodeExecutor`.
///
/// Two modes:
///
/// - **Fixed sandbox** ([`for_sandbox`](Self::for_sandbox)) — every session
///   uses the given sandbox; execution fails when it is not running.
/// - **Engine-managed** ([`for_engine`](Self::for_engine)) — a sandbox is
///   created lazily per session (display name `default_sandbox`, TTL one
///   year, code-execution spec with service defaults) and cached. On each
///   execution the sandbox is re-fetched and recreated when it is missing
///   or no longer `STATE_RUNNING`.
///
/// The session→sandbox map is guarded by an async lock; sandbox creation
/// is serialized across sessions so concurrent first calls cannot create
/// duplicates.
///
/// # Example
///
/// ```rust,no_run
/// use adk_code::vertex_sandbox::{
///     SandboxCodeExecutor, VertexSandboxClient, VertexSandboxConfig,
/// };
/// use std::sync::Arc;
///
/// # async fn run() -> adk_core::Result<()> {
/// let client = Arc::new(VertexSandboxClient::new_with_adc(VertexSandboxConfig::new(
///     "my-project",
///     "us-central1",
/// ))?);
/// let executor = SandboxCodeExecutor::for_engine(client, "4242");
/// let result = executor.execute_for_session("session-1", "print(1 + 1)", &[]).await?;
/// assert_eq!(result.stdout, "2\n");
/// # Ok(())
/// # }
/// ```
pub struct SandboxCodeExecutor {
    client: Arc<VertexSandboxClient>,
    target: ExecutorTarget,
    sessions: RwLock<HashMap<String, String>>,
}

impl SandboxCodeExecutor {
    /// Uses one pre-provisioned sandbox for every session.
    ///
    /// When both a sandbox and an engine are available, prefer this
    /// constructor — the sandbox wins in adk-python's executor too.
    pub fn for_sandbox(client: Arc<VertexSandboxClient>, sandbox_name: impl Into<String>) -> Self {
        Self {
            client,
            target: ExecutorTarget::Sandbox(sandbox_name.into()),
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Creates sandboxes lazily per session under the given reasoning
    /// engine (full resource name or bare numeric ID).
    pub fn for_engine(client: Arc<VertexSandboxClient>, engine: impl Into<String>) -> Self {
        Self {
            client,
            target: ExecutorTarget::Engine(engine.into()),
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Executes code in the sandbox associated with `session_key`,
    /// creating or recreating it first when necessary.
    ///
    /// # Errors
    ///
    /// Returns an error when the sandbox cannot be resolved, created, or
    /// executed against, or when the outputs cannot be decoded.
    pub async fn execute_for_session(
        &self,
        session_key: &str,
        code: &str,
        files: &[InputFile],
    ) -> Result<SandboxExecutionResult> {
        let sandbox = self.ensure_sandbox(session_key).await?;
        self.client.execute_code(&sandbox, code, files).await
    }

    /// Resolves the sandbox for a session, applying the lazy
    /// create/recreate semantics.
    async fn ensure_sandbox(&self, session_key: &str) -> Result<String> {
        match &self.target {
            ExecutorTarget::Sandbox(name) => {
                let sandbox = self.client.get_sandbox(name).await?;
                if sandbox.state == Some(SandboxState::Running) {
                    return Ok(name.clone());
                }
                Err(errors().unavailable(format!(
                    "vertex sandbox '{name}' is not running (state {:?}); wait for provisioning to finish or provision a new sandbox",
                    sandbox.state,
                )))
            }
            ExecutorTarget::Engine(engine) => {
                // The write lock is held across the check and the create so
                // concurrent calls cannot race a duplicate sandbox into
                // existence for the same session.
                let mut sessions = self.sessions.write().await;
                if let Some(cached) = sessions.get(session_key) {
                    match self.client.get_sandbox(cached).await {
                        Ok(sandbox) if sandbox.state == Some(SandboxState::Running) => {
                            return Ok(cached.clone());
                        }
                        Ok(sandbox) => {
                            debug!(
                                sandbox.name = cached.as_str(),
                                sandbox.state = ?sandbox.state,
                                "cached sandbox is not running; recreating",
                            );
                        }
                        Err(error) if error.is_not_found() => {
                            debug!(
                                sandbox.name = cached.as_str(),
                                "cached sandbox no longer exists; recreating",
                            );
                        }
                        Err(error) => return Err(error),
                    }
                }
                let request = CreateSandboxRequest::new(DEFAULT_SANDBOX_DISPLAY_NAME)
                    .with_ttl(DEFAULT_SANDBOX_TTL)
                    .with_spec(SandboxEnvironmentSpec::code_execution(
                        CodeExecutionEnvironment::default(),
                    ));
                let created = self.client.create_sandbox(engine, request).await?;
                let name = created.name.ok_or_else(|| {
                    errors().invalid_response(
                        "vertex sandbox create returned a sandbox without a name; inspect the sandbox in Google Cloud",
                    )
                })?;
                sessions.insert(session_key.to_string(), name.clone());
                Ok(name)
            }
        }
    }
}
