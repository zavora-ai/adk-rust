//! Core trait and handle types for the managed agent runtime.
//!
//! The [`ManagedAgentRuntime`] trait defines the full lifecycle interface for
//! managed agents: create agents from declarative definitions, start sessions,
//! send/receive events, pause/resume/interrupt, and archive.
//!
//! # Architecture
//!
//! The runtime is a **library**, not a service. The platform hosts it.
//! This trait is provider-agnostic — it behaves identically for Gemini, OpenAI,
//! Anthropic, Ollama, and OpenAI-compatible providers.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_managed::runtime::{ManagedAgentRuntime, AgentHandle, SessionHandle};
//! use adk_managed::types::ManagedAgentDef;
//!
//! async fn example(runtime: &dyn ManagedAgentRuntime) {
//!     let def = ManagedAgentDef::default();
//!     let agent = runtime.create(def).await.unwrap();
//!     let session = runtime.start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None).await.unwrap();
//!     let status = runtime.status(&session).await.unwrap();
//!     println!("Session status: {status:?}");
//! }
//! ```

use std::collections::HashMap;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::types::{ManagedAgentDef, RuntimeError, SessionEvent, SessionStatus, UserEvent};

// ─── Handle Types ────────────────────────────────────────────────────────────

/// Opaque agent handle.
///
/// The platform assigns the user-facing `agt_` prefixed ID; the runtime uses
/// this internal handle for lookups. The inner string is an implementation detail.
///
/// # Example
///
/// ```
/// use adk_managed::runtime::AgentHandle;
///
/// let handle = AgentHandle("agent_abc123".to_string());
/// assert_eq!(handle.0, "agent_abc123");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentHandle(pub String);

/// Opaque session handle.
///
/// Identifies an active or archived session within the runtime. The inner string
/// is an implementation detail assigned by the runtime on session creation.
///
/// # Example
///
/// ```
/// use adk_managed::runtime::SessionHandle;
///
/// let handle = SessionHandle("session_xyz789".to_string());
/// assert_eq!(handle.0, "session_xyz789");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionHandle(pub String);

/// Who a managed session belongs to.
///
/// Every managed session was previously persisted under the constants `managed` /
/// `managed_user`, so all sessions shared one logical namespace: session lookup, memory, and
/// deletion could not be scoped to a caller, and audit could not attribute a session to
/// anyone. The runtime needs this at creation because the underlying `SessionService` is
/// addressed by app and user, and substituting constants to satisfy that contract is what
/// erased the caller.
///
/// # Example
///
/// ```rust
/// use adk_managed::ManagedOwner;
///
/// let owner = ManagedOwner::new("support-console", "user-42")?;
/// assert_eq!(owner.app_name(), "support-console");
///
/// // Blank components are rejected, since they would silently re-create a shared namespace.
/// assert!(ManagedOwner::new("", "user-42").is_err());
/// # Ok::<(), adk_managed::types::RuntimeError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ManagedOwner {
    app_name: String,
    user_id: String,
}

impl ManagedOwner {
    /// Validates and builds an owner identity.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidRequest`] when either component is empty or whitespace.
    pub fn new(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Result<Self, RuntimeError> {
        let app_name = app_name.into();
        let user_id = user_id.into();

        if app_name.trim().is_empty() {
            return Err(RuntimeError::InvalidRequest {
                message: "a managed session needs a non-empty app name to be addressable and \
                          auditable"
                    .to_string(),
                param: Some("app_name".to_string()),
            });
        }
        if user_id.trim().is_empty() {
            return Err(RuntimeError::InvalidRequest {
                message: "a managed session needs a non-empty user id; without one, sessions \
                          share a namespace and cannot be scoped to a caller"
                    .to_string(),
                param: Some("user_id".to_string()),
            });
        }

        Ok(Self { app_name, user_id })
    }

    /// The app this session belongs to.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// The user this session belongs to.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}

// ─── EnvironmentConfig ───────────────────────────────────────────────────────

/// Optional environment configuration for a session.
///
/// Provides environment variables and working directory context that the agent
/// session can use during execution (e.g., for sandbox or tool execution).
///
/// # Example
///
/// ```
/// use adk_managed::runtime::EnvironmentConfig;
///
/// let env = EnvironmentConfig {
///     env_vars: [("API_KEY".to_string(), "secret".to_string())].into(),
///     working_dir: Some("/workspace".to_string()),
/// };
/// assert_eq!(env.env_vars.get("API_KEY").unwrap(), "secret");
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvironmentConfig {
    /// Environment variables available to the agent.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env_vars: HashMap<String, String>,

    /// Optional working directory for the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

// ─── ManagedAgentRuntime Trait ───────────────────────────────────────────────

/// The central async trait defining the managed agent lifecycle.
///
/// Implementations of this trait encapsulate the full agent lifecycle:
/// creating agents from declarative definitions, starting durable sessions,
/// sending/receiving events, pausing/resuming, interrupting, and archiving.
///
/// The trait is provider-agnostic: it takes a [`ManagedAgentDef`] with a
/// [`ModelRef`](crate::types::ModelRef) and behaves identically regardless
/// of which LLM provider powers the agent.
///
/// # Implementors
///
/// - `DefaultManagedAgentRuntime` — the default implementation
///   composed from `Runner` + pluggable `SessionService` + optional sandbox/memory.
///
/// # Design Notes
///
/// - All methods return `Result<_, RuntimeError>` for structured error handling.
/// - `stream_events` returns a `BoxStream` for SSE-compatible event delivery.
/// - `from_seq` on `stream_events` enables `Last-Event-ID` reconnection.
/// - The runtime is `Send + Sync` for use across async task boundaries.
#[async_trait]
pub trait ManagedAgentRuntime: Send + Sync {
    /// Create a managed agent from a declarative definition.
    ///
    /// Resolves the [`ModelRef`](crate::types::ModelRef), builds a runnable
    /// agent, and stores it in the internal registry. Returns an opaque handle
    /// for use with `start_session`.
    async fn create(&self, def: ManagedAgentDef) -> Result<AgentHandle, RuntimeError>;

    /// Start a new session for the given agent.
    ///
    /// Creates a session in `Queued` status, initializes the session loop,
    /// and returns a handle for event interaction. The optional
    /// [`EnvironmentConfig`] provides env vars and working directory.
    async fn start_session(
        &self,
        agent: &AgentHandle,
        owner: &ManagedOwner,
        env: Option<EnvironmentConfig>,
    ) -> Result<SessionHandle, RuntimeError>;

    /// Send an event from the client to the agent session.
    ///
    /// Dispatches the [`UserEvent`] to the session loop. The event type
    /// determines behavior:
    /// - `user.message` — enqueues a message for processing
    /// - `user.interrupt` — signals the session to stop at next boundary
    /// - `user.custom_tool_result` — delivers a result to a parked tool call
    /// - `user.tool_confirmation` — approves or denies a pending tool use
    async fn send_event(
        &self,
        session: &SessionHandle,
        event: UserEvent,
    ) -> Result<(), RuntimeError>;

    /// Subscribe to the session's event stream.
    ///
    /// Returns a stream of [`SessionEvent`]s. If `from_seq` is provided,
    /// replays all events with `seq > from_seq` before attaching to the
    /// live broadcast (enabling SSE `Last-Event-ID` reconnection).
    async fn stream_events(
        &self,
        session: &SessionHandle,
        from_seq: Option<u64>,
    ) -> Result<BoxStream<'static, SessionEvent>, RuntimeError>;

    /// Interrupt the session at the next safe boundary.
    ///
    /// Signals the session loop's cancellation token. The loop will stop
    /// processing at the next inter-event boundary and emit `status.idle`.
    async fn interrupt(&self, session: &SessionHandle) -> Result<(), RuntimeError>;

    /// Pause the session, checkpointing current state.
    ///
    /// Stops consuming new input and persists the current run-state.
    /// The session transitions to `Paused` status.
    async fn pause(&self, session: &SessionHandle) -> Result<(), RuntimeError>;

    /// Resume a paused session from its last checkpoint.
    ///
    /// Clears the pause flag, rehydrates state if needed, and returns
    /// the session to active processing.
    async fn resume(&self, session: &SessionHandle) -> Result<(), RuntimeError>;

    /// Query the current status of a session.
    async fn status(&self, session: &SessionHandle) -> Result<SessionStatus, RuntimeError>;

    /// Archive a session (terminal state).
    ///
    /// Sets the session to `Archived` status and stops the session loop.
    /// Archived sessions retain their event log for read access.
    async fn archive(&self, session: &SessionHandle) -> Result<(), RuntimeError>;

    /// Delete a session and its associated data.
    ///
    /// Archives the session (if not already terminal) and removes all
    /// persisted data including events and checkpoints.
    async fn delete_session(&self, session: &SessionHandle) -> Result<(), RuntimeError>;
}
