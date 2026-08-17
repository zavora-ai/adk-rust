//! Turnkey entrypoint for Agent Engine BYOC containers.
//!
//! [`serve_agent_engine`] is the whole `main` of a deployable engine: it
//! builds the dispatch app around an agent and serves it on `0.0.0.0:$PORT`.
//! [`build_agent_engine_app`] exposes the same app as a plain [`Router`] for
//! callers that manage their own listener or need to test without binding.

use super::{AgentEngineState, agent_engine_router};
use adk_core::{AdkError, Agent, ErrorCategory, ErrorComponent, Result};
use axum::{Router, routing::get};
use std::sync::Arc;
use tracing::{info, warn};

/// The port the platform assigns to the container.
const ENV_PORT: &str = "PORT";
/// GCP project of the deployment (set by the platform).
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
/// GCP location of the deployment (set by the platform).
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";
/// The bare numeric engine ID (set by the platform inside deployed
/// containers; not a full resource name).
const ENV_GOOGLE_CLOUD_AGENT_ENGINE_ID: &str = "GOOGLE_CLOUD_AGENT_ENGINE_ID";

/// The default port when neither `PORT` nor a port override is set.
const DEFAULT_PORT: u16 = 8080;

/// Options for [`serve_agent_engine`] and [`build_agent_engine_app`].
///
/// Every field is optional: the zero-configuration default serves the agent
/// with an in-memory session service — enough to answer platform queries,
/// but sessions do not survive a container restart. Production deployments
/// configure managed backends:
///
/// - `session_service` — `VertexAiSessionService` (feature `vertex-session`
///   in the consumer) picks up the platform's managed Sessions via
///   `VertexAiSessionConfig::from_env()`, which reads the same
///   `GOOGLE_CLOUD_*` variables the platform sets.
/// - `artifact_service` — `GcsArtifactService` (feature `gcs` on
///   `adk-artifact`) stores artifacts where the Gemini Enterprise console
///   renders them; take the bucket from an environment variable or a flag.
/// - `memory_service` — enables the `async_add_session_to_memory` /
///   `async_search_memory` class methods; without one they return an
///   `Unsupported` error.
///
/// # Example
///
/// ```rust,no_run
/// use adk_server::agent_engine::AgentEngineOptions;
///
/// let opts = AgentEngineOptions::new().with_app_name("weather-agent");
/// ```
#[derive(Default)]
pub struct AgentEngineOptions {
    session_service: Option<Arc<dyn adk_session::SessionService>>,
    memory_service: Option<Arc<dyn adk_memory::MemoryService>>,
    artifact_service: Option<Arc<dyn adk_artifact::ArtifactService>>,
    app_name: Option<String>,
    port_override: Option<u16>,
}

impl AgentEngineOptions {
    /// Creates empty options: in-memory sessions, no memory or artifact
    /// service, app name from the agent, port from `$PORT`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the session persistence backend. Defaults to
    /// `InMemorySessionService` when unset.
    pub fn with_session_service(
        mut self,
        session_service: Arc<dyn adk_session::SessionService>,
    ) -> Self {
        self.session_service = Some(session_service);
        self
    }

    /// Sets the memory service backing the memory class methods.
    pub fn with_memory_service(
        mut self,
        memory_service: Arc<dyn adk_memory::MemoryService>,
    ) -> Self {
        self.memory_service = Some(memory_service);
        self
    }

    /// Sets the artifact storage backend, wired into both the runner and the
    /// dispatch state.
    pub fn with_artifact_service(
        mut self,
        artifact_service: Arc<dyn adk_artifact::ArtifactService>,
    ) -> Self {
        self.artifact_service = Some(artifact_service);
        self
    }

    /// Overrides the application name used for session scoping. Defaults to
    /// the agent's name.
    pub fn with_app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = Some(app_name.into());
        self
    }

    /// Overrides the serving port, taking precedence over `$PORT`.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port_override = Some(port);
        self
    }
}

/// The platform-reserved environment, read once at startup for logging and
/// configuration hints.
struct PlatformEnv {
    project: Option<String>,
    location: Option<String>,
    engine_id: Option<String>,
}

impl PlatformEnv {
    fn read() -> Self {
        let read = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
            project: read(ENV_GOOGLE_CLOUD_PROJECT),
            location: read(ENV_GOOGLE_CLOUD_LOCATION),
            engine_id: read(ENV_GOOGLE_CLOUD_AGENT_ENGINE_ID),
        }
    }

    /// Whether the container appears to be running as a deployed engine.
    fn is_managed(&self) -> bool {
        self.engine_id.is_some()
    }
}

/// Resolves the serving port: explicit override, then `$PORT`, then 8080.
///
/// # Errors
///
/// Returns an invalid-input error when `$PORT` is set but not a valid port
/// number — a misconfigured port means the platform cannot reach the
/// container, so failing at startup beats serving on the wrong port.
fn resolve_port(port_override: Option<u16>) -> Result<u16> {
    if let Some(port) = port_override {
        return Ok(port);
    }
    match std::env::var(ENV_PORT) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(DEFAULT_PORT);
            }
            trimmed.parse().map_err(|_| {
                AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::InvalidInput,
                    "agent_engine.invalid_port",
                    format!("PORT must be a port number in 1-65535, got '{trimmed}'"),
                )
            })
        }
        Err(_) => Ok(DEFAULT_PORT),
    }
}

/// Builds the complete Agent Engine app for an agent: the dispatch router
/// plus a `GET /health` liveness route.
///
/// Use this instead of [`serve_agent_engine`] when you manage the listener
/// yourself (custom TLS, tests, graceful shutdown).
///
/// # Errors
///
/// Returns an error when the runner cannot be constructed — in practice,
/// when the resolved app name fails identity validation.
///
/// # Example
///
/// ```rust,no_run
/// use adk_server::agent_engine::{AgentEngineOptions, build_agent_engine_app};
/// use std::sync::Arc;
///
/// # async fn serve(agent: Arc<dyn adk_core::Agent>) -> anyhow::Result<()> {
/// let app = build_agent_engine_app(agent, AgentEngineOptions::new())?;
/// let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
/// axum::serve(listener, app).await?;
/// # Ok(())
/// # }
/// ```
pub fn build_agent_engine_app(agent: Arc<dyn Agent>, opts: AgentEngineOptions) -> Result<Router> {
    let app_name = opts.app_name.unwrap_or_else(|| agent.name().to_string());
    let env = PlatformEnv::read();
    info!(
        app.name = %app_name,
        gcp.project = env.project.as_deref().unwrap_or(""),
        gcp.location = env.location.as_deref().unwrap_or(""),
        gcp.engine_id = env.engine_id.as_deref().unwrap_or(""),
        "building agent engine app"
    );

    let session_service = match opts.session_service {
        Some(session_service) => session_service,
        None => {
            if env.is_managed() {
                // The platform sets GOOGLE_CLOUD_AGENT_ENGINE_ID inside deployed
                // containers; an in-memory session store there loses every
                // conversation on restart or scale-out.
                warn!(
                    "running as a deployed engine with in-memory sessions; conversations will \
                     not survive restarts. Configure a managed backend, e.g. \
                     VertexAiSessionService with VertexAiSessionConfig::from_env() (feature \
                     `vertex-session`), via AgentEngineOptions::with_session_service"
                );
            }
            Arc::new(adk_session::InMemorySessionService::new())
        }
    };

    let mut runner_builder = adk_runner::Runner::builder()
        .app_name(&app_name)
        .agent(agent)
        .session_service(session_service);
    if let Some(artifact_service) = &opts.artifact_service {
        runner_builder = runner_builder.artifact_service(artifact_service.clone());
    }
    let runner = Arc::new(runner_builder.build()?);

    let mut state = AgentEngineState::new(runner);
    if let Some(memory_service) = opts.memory_service {
        state = state.with_memory_service(memory_service);
    }
    if let Some(artifact_service) = opts.artifact_service {
        state = state.with_artifact_service(artifact_service);
    }

    Ok(agent_engine_router(state).route("/health", get(health)))
}

/// Liveness probe target for the platform's container health checks.
async fn health() -> &'static str {
    "ok"
}

/// Serves an agent as a complete Agent Engine container.
///
/// Binds `0.0.0.0:$PORT` (or [`AgentEngineOptions::with_port`], falling back
/// to `8080`), installs the workspace crypto provider, and serves the
/// dispatch endpoints until the process is stopped. This function is the
/// whole `main` of a BYOC engine:
///
/// ```rust,no_run
/// use adk_server::agent_engine::{AgentEngineOptions, serve_agent_engine};
/// use std::sync::Arc;
///
/// # async fn run(agent: Arc<dyn adk_core::Agent>) -> adk_core::Result<()> {
/// serve_agent_engine(agent, AgentEngineOptions::new()).await
/// # }
/// ```
///
/// # Errors
///
/// Returns an error when `$PORT` is invalid, the runner cannot be built, the
/// listener cannot bind, or serving fails.
pub async fn serve_agent_engine(agent: Arc<dyn Agent>, opts: AgentEngineOptions) -> Result<()> {
    adk_core::ensure_crypto_provider();
    let port = resolve_port(opts.port_override)?;
    let app = build_agent_engine_app(agent, opts)?;

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|err| {
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Unavailable,
            "agent_engine.bind_failed",
            format!("failed to bind {addr}: {err}"),
        )
    })?;
    info!(server.address = %addr, "agent engine serving");
    axum::serve(listener, app).await.map_err(|err| {
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Internal,
            "agent_engine.serve_failed",
            format!("server terminated abnormally: {err}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests are safe under nextest (one process per test); they would
    // race under plain `cargo test` threads.

    #[test]
    fn port_override_beats_env() {
        unsafe { std::env::set_var(ENV_PORT, "9999") };
        assert_eq!(resolve_port(Some(1234)).unwrap(), 1234);
    }

    #[test]
    fn port_env_is_used_when_no_override() {
        unsafe { std::env::set_var(ENV_PORT, "9042") };
        assert_eq!(resolve_port(None).unwrap(), 9042);
    }

    #[test]
    fn port_defaults_to_8080() {
        unsafe { std::env::remove_var(ENV_PORT) };
        assert_eq!(resolve_port(None).unwrap(), DEFAULT_PORT);
    }

    #[test]
    fn blank_port_env_falls_back() {
        unsafe { std::env::set_var(ENV_PORT, "  ") };
        assert_eq!(resolve_port(None).unwrap(), DEFAULT_PORT);
    }

    #[test]
    fn invalid_port_env_is_an_error() {
        unsafe { std::env::set_var(ENV_PORT, "not-a-port") };
        let err = resolve_port(None).unwrap_err();
        assert_eq!(err.http_status_code(), 400);
    }

    #[test]
    fn platform_env_detects_managed_deployments() {
        unsafe { std::env::set_var(ENV_GOOGLE_CLOUD_AGENT_ENGINE_ID, "12345") };
        assert!(PlatformEnv::read().is_managed());
        unsafe { std::env::remove_var(ENV_GOOGLE_CLOUD_AGENT_ENGINE_ID) };
        assert!(!PlatformEnv::read().is_managed());
    }
}
