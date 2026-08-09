//! Default implementation of the [`ManagedAgentRuntime`] trait.
//!
//! [`DefaultManagedAgentRuntime`] composes existing ADK crates (`Runner`,
//! `SessionService`, optional sandbox and memory) behind the unified lifecycle
//! trait. It manages active sessions as supervised background tasks with
//! durable checkpointing, event streaming, and custom tool parking.
//!
//! # Architecture
//!
//! The runtime is a library, not a service. The platform hosts it:
//!
//! - **Testable in isolation**: Zero HTTP/auth/billing dependencies
//! - **Embeddable**: Self-hosted deployments use the runtime trait directly
//! - **Swappable platform**: Different platforms can host the same runtime
//! - **Provider-neutral**: Identical event sequences regardless of model provider
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_managed::default_runtime::DefaultManagedAgentRuntime;
//! use adk_managed::resolver::DefaultModelResolver;
//! use adk_session::InMemorySessionService;
//!
//! let resolver = Arc::new(DefaultModelResolver::new());
//! let sessions = Arc::new(InMemorySessionService::new());
//!
//! let runtime = DefaultManagedAgentRuntime::new(resolver, sessions);
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::sync::{Mutex, Notify, RwLock, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use adk_core::Agent;
#[cfg(feature = "memory")]
use adk_core::Memory;
#[cfg(feature = "sandbox")]
use adk_sandbox::SandboxBackend;
use adk_session::service::{CreateRequest, SessionService};

use crate::agent_builder::{BuildError, build_agent};
use crate::checkpoint::CheckpointManager;
use crate::parking::ToolParkingLot;
use crate::replay::create_event_stream;
use crate::resolver::ModelResolver;
use crate::runtime::{
    AgentHandle, EnvironmentConfig, ManagedAgentRuntime, ManagedOwner, SessionHandle,
};
use crate::session_loop::SessionLoop;
use crate::types::{ManagedAgentDef, RuntimeError, SessionEvent, SessionStatus, UserEvent};

// ─── ActiveSession ───────────────────────────────────────────────────────────

/// The addressing a managed session's conversation was persisted under.
///
/// Deletion has to remove what creation wrote. Keeping the triple rather than rebuilding it
/// means a change to how sessions are addressed cannot leave orphaned conversation data
/// behind in the configured backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistedIdentity {
    /// The app name the session was created under.
    pub(crate) app_name: String,
    /// The user the session was created for.
    pub(crate) user_id: String,
    /// The session's own identifier.
    pub(crate) session_id: String,
}

/// Internal state for an active (or recently active) session.
///
/// Each session spawns a background task running the [`SessionLoop`](crate::session_loop::SessionLoop).
/// This struct holds the communication handles and control primitives needed
/// to interact with that background task from the runtime methods.
#[allow(dead_code)] // Fields are retained for the session lifecycle
pub(crate) struct ActiveSession {
    /// The built agent driving this session.
    pub(crate) agent: Arc<dyn Agent>,
    /// Sender for user events into the session loop.
    pub(crate) event_tx: mpsc::Sender<crate::types::UserEvent>,
    /// Broadcast sender for session events (fan-out to stream subscribers).
    pub(crate) broadcast_tx: broadcast::Sender<crate::types::SessionEvent>,
    /// Cancellation token for interrupt handling.
    pub(crate) cancel_token: CancellationToken,
    /// Pause flag — when true, the session loop parks until resumed.
    pub(crate) pause_flag: Arc<Mutex<bool>>,
    /// Notify used to wake the session loop after resume.
    pub(crate) pause_notify: Arc<Notify>,
    /// Current session status (shared with the session loop).
    pub(crate) status: Arc<RwLock<SessionStatus>>,
    /// The identity this session's conversation is persisted under.
    ///
    /// Recorded at creation so deletion removes exactly what creation wrote, rather than
    /// re-deriving an identity and risking a mismatch that silently leaves data behind.
    pub(crate) persisted_as: PersistedIdentity,
    /// Checkpoint manager for durable state.
    pub(crate) checkpoint: Arc<RwLock<CheckpointManager>>,
}

// ─── DefaultManagedAgentRuntime ──────────────────────────────────────────────

/// Default implementation of the managed agent runtime.
///
/// Composed from a [`ModelResolver`] + a pluggable [`SessionService`] +
/// optional sandbox factory and memory service. Has no platform dependencies —
/// the platform injects its own implementations of these traits.
///
/// # Fields
///
/// - `model_resolver` — resolves [`ModelRef`](crate::types::ModelRef) into `Arc<dyn Llm>`
/// - `session_service` — persistent session storage backend
/// - `sandbox_factory` — optional sandbox for built-in tool execution
/// - `memory` — optional cross-session memory service
/// - `sessions` — active session registry
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_managed::default_runtime::DefaultManagedAgentRuntime;
/// use adk_managed::resolver::DefaultModelResolver;
/// use adk_session::InMemorySessionService;
///
/// // Minimal runtime with defaults
/// let runtime = DefaultManagedAgentRuntime::new(
///     Arc::new(DefaultModelResolver::new()),
///     Arc::new(InMemorySessionService::new()),
/// );
///
/// // With sandbox and memory (feature-gated)
/// let runtime = DefaultManagedAgentRuntime::new(
///     Arc::new(DefaultModelResolver::new()),
///     Arc::new(InMemorySessionService::new()),
/// )
/// .with_sandbox(my_sandbox)
/// .with_memory(my_memory_service);
/// ```
pub struct DefaultManagedAgentRuntime {
    /// Resolves ModelRef → `Arc<dyn Llm>`.
    model_resolver: Arc<dyn ModelResolver>,
    /// Persistent session storage.
    session_service: Arc<dyn SessionService>,
    /// Optional sandbox backend for isolated built-in tool execution.
    ///
    /// When set, built-in tools (bash, code_execution, etc.) execute inside
    /// this sandbox. When `None`, built-in tools execute in-process.
    #[cfg(feature = "sandbox")]
    sandbox: Option<Arc<dyn SandboxBackend>>,
    /// Optional memory service for cross-session persistent memory.
    ///
    /// Passed to the Runner's `memory_service` field so agents can search
    /// and store semantic memories across sessions.
    #[cfg(feature = "memory")]
    memory: Option<Arc<dyn Memory>>,
    /// Registered agents keyed by agent handle ID.
    agents: Arc<RwLock<HashMap<String, RegisteredAgent>>>,
    /// Active session registry keyed by session ID.
    sessions: Arc<RwLock<HashMap<String, ActiveSession>>>,
}

/// Internal state for a registered agent.
#[allow(dead_code)] // `def` is retained for future session creation
struct RegisteredAgent {
    /// The built agent instance.
    agent: Arc<dyn Agent>,
    /// The original definition (retained for session creation).
    def: ManagedAgentDef,
}

impl DefaultManagedAgentRuntime {
    /// Create a new `DefaultManagedAgentRuntime` with injected services.
    ///
    /// # Arguments
    ///
    /// * `model_resolver` - Resolves `ModelRef` declarations into callable LLM instances.
    /// * `session_service` - Persistent storage backend for sessions and checkpoints.
    ///
    /// Use `.with_sandbox()` and `.with_memory()` builder methods to inject
    /// optional sandbox and memory services (feature-gated).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_managed::default_runtime::DefaultManagedAgentRuntime;
    /// use adk_managed::resolver::DefaultModelResolver;
    /// use adk_session::InMemorySessionService;
    ///
    /// let runtime = DefaultManagedAgentRuntime::new(
    ///     Arc::new(DefaultModelResolver::new()),
    ///     Arc::new(InMemorySessionService::new()),
    /// );
    /// ```
    pub fn new(
        model_resolver: Arc<dyn ModelResolver>,
        session_service: Arc<dyn SessionService>,
    ) -> Self {
        Self {
            model_resolver,
            session_service,
            #[cfg(feature = "sandbox")]
            sandbox: None,
            #[cfg(feature = "memory")]
            memory: None,
            agents: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set the sandbox backend for isolated built-in tool execution.
    #[cfg(feature = "sandbox")]
    pub fn with_sandbox(mut self, sandbox: Arc<dyn SandboxBackend>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Set the memory service for cross-session persistent memory.
    #[cfg(feature = "memory")]
    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Get a reference to the model resolver.
    pub fn model_resolver(&self) -> &Arc<dyn ModelResolver> {
        &self.model_resolver
    }

    /// Get a reference to the session service.
    pub fn session_service(&self) -> &Arc<dyn SessionService> {
        &self.session_service
    }

    /// Get a reference to the optional sandbox backend.
    #[cfg(feature = "sandbox")]
    pub fn sandbox(&self) -> Option<&Arc<dyn SandboxBackend>> {
        self.sandbox.as_ref()
    }

    /// Get a reference to the optional memory service.
    #[cfg(feature = "memory")]
    pub fn memory(&self) -> Option<&Arc<dyn Memory>> {
        self.memory.as_ref()
    }

    /// Get a reference to the active sessions map.
    #[cfg(test)]
    pub(crate) fn sessions(&self) -> &Arc<RwLock<HashMap<String, ActiveSession>>> {
        &self.sessions
    }
}

// ─── Channel and timeout defaults ────────────────────────────────────────────

/// Default capacity for the user event mpsc channel.
const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 64;

/// Default capacity for the session event broadcast channel.
const DEFAULT_BROADCAST_CHANNEL_CAPACITY: usize = 256;

/// Default timeout for custom tool parking (5 minutes).
const DEFAULT_PARKING_TIMEOUT: Duration = Duration::from_secs(300);

// ─── ManagedAgentRuntime implementation ──────────────────────────────────────

#[async_trait]
impl ManagedAgentRuntime for DefaultManagedAgentRuntime {
    /// Create a managed agent from a declarative definition.
    ///
    /// Resolves the `ModelRef` into an `Arc<dyn Llm>`, builds a runnable agent,
    /// stores it in the internal registry, and returns an opaque handle.
    async fn create(&self, def: ManagedAgentDef) -> Result<AgentHandle, RuntimeError> {
        // 1. Resolve model
        let model = self.model_resolver.resolve(&def.model).await.map_err(|e| {
            RuntimeError::ProviderError {
                provider: format!("{:?}", def.model),
                message: e.to_string(),
            }
        })?;

        // 2. Build agent from definition
        #[cfg(feature = "sandbox")]
        let agent = build_agent(&def, model, self.sandbox.clone()).map_err(|e| match e {
            BuildError::InvalidDef(msg) => RuntimeError::invalid_request(msg),
            BuildError::BuildFailed(msg) => RuntimeError::internal(msg),
        })?;
        #[cfg(not(feature = "sandbox"))]
        let agent = build_agent(&def, model).map_err(|e| match e {
            BuildError::InvalidDef(msg) => RuntimeError::invalid_request(msg),
            BuildError::BuildFailed(msg) => RuntimeError::internal(msg),
        })?;

        // 3. Generate handle ID
        let handle_id = uuid::Uuid::new_v4().to_string();

        info!(agent_handle = %handle_id, agent_name = %def.name, "agent created");

        // 4. Store in registry
        let registered = RegisteredAgent { agent, def };
        self.agents.write().await.insert(handle_id.clone(), registered);

        Ok(AgentHandle(handle_id))
    }

    /// Start a new session for the given agent.
    ///
    /// Creates internal communication channels, spawns the session loop as a
    /// background task, and stores the active session handle. Initial status
    /// is `Queued`.
    async fn start_session(
        &self,
        agent: &AgentHandle,
        owner: &ManagedOwner,
        env: Option<EnvironmentConfig>,
    ) -> Result<SessionHandle, RuntimeError> {
        // Environment configuration was accepted and discarded — the parameter was named
        // `_env`. Applying `env_vars` or `working_dir` to an in-process session loop would
        // mutate process-global state shared with every other session, so the runtime refuses
        // rather than pretending. A sandboxed execution boundary is what would make this
        // honourable.
        if let Some(env) = &env
            && (!env.env_vars.is_empty() || env.working_dir.is_some())
        {
            return Err(RuntimeError::InvalidRequest {
                message: "EnvironmentConfig cannot be honoured by this runtime: sessions run \
                          in-process, so per-session environment variables and working \
                          directories would have to mutate process-global state shared with \
                          other sessions. Pass `None`, or configure a sandboxed runtime."
                    .to_string(),
                param: Some("env".to_string()),
            });
        }

        // 1. Look up agent from registry
        let agents = self.agents.read().await;
        let registered = agents
            .get(&agent.0)
            .ok_or_else(|| RuntimeError::NotFound { session_id: agent.0.clone() })?;
        let agent_arc = Arc::clone(&registered.agent);
        drop(agents);

        // 2. Generate session ID
        let session_id = uuid::Uuid::new_v4().to_string();

        // 3. Create mpsc channel for user events
        let (event_tx, event_rx) = mpsc::channel(DEFAULT_EVENT_CHANNEL_CAPACITY);

        // 4. Create broadcast channel for session events
        let (broadcast_tx, _) = broadcast::channel(DEFAULT_BROADCAST_CHANNEL_CAPACITY);

        // 5. Create control primitives
        let cancel_token = CancellationToken::new();
        let pause_flag = Arc::new(Mutex::new(false));
        let pause_notify = Arc::new(Notify::new());

        // 6. Create ToolParkingLot and CheckpointManager
        let parking = Arc::new(ToolParkingLot::new(DEFAULT_PARKING_TIMEOUT));
        let checkpoint = Arc::new(RwLock::new(CheckpointManager::new(session_id.clone())));

        // 7. Seed the session in the SessionService.
        //    The Runner's run() calls session_service.get() which requires the
        //    session to exist. We create it here with the same triple
        //    (owner app, owner user, session_id) that
        //    build_runner/run_str use in the session loop.
        let persisted_as = PersistedIdentity {
            app_name: owner.app_name().to_string(),
            user_id: owner.user_id().to_string(),
            session_id: session_id.clone(),
        };

        self.session_service
            .create(CreateRequest {
                app_name: persisted_as.app_name.clone(),
                user_id: persisted_as.user_id.clone(),
                session_id: Some(session_id.clone()),
                state: std::collections::HashMap::new(),
            })
            .await
            .map_err(|e| RuntimeError::internal(format!("failed to seed session: {e}")))?;

        // 8. Spawn SessionLoop as background task, sharing the status the caller observes
        //    so normal queued → running → idle transitions are visible through
        //    `ManagedAgentRuntime::status`.
        let status = Arc::new(RwLock::new(SessionStatus::Queued));

        #[cfg(feature = "memory")]
        let session_loop = SessionLoop::with_pause_controls(
            session_id.clone(),
            event_rx,
            broadcast_tx.clone(),
            Arc::clone(&parking),
            cancel_token.clone(),
            Arc::clone(&pause_flag),
            Arc::clone(&pause_notify),
            Arc::clone(&checkpoint),
            Arc::clone(&agent_arc),
            Arc::clone(&self.session_service),
            self.memory.clone(),
        )
        .with_shared_status(Arc::clone(&status))
        .with_owner(owner.app_name(), owner.user_id());
        #[cfg(not(feature = "memory"))]
        let session_loop = SessionLoop::with_pause_controls(
            session_id.clone(),
            event_rx,
            broadcast_tx.clone(),
            Arc::clone(&parking),
            cancel_token.clone(),
            Arc::clone(&pause_flag),
            Arc::clone(&pause_notify),
            Arc::clone(&checkpoint),
            Arc::clone(&agent_arc),
            Arc::clone(&self.session_service),
        )
        .with_shared_status(Arc::clone(&status))
        .with_owner(owner.app_name(), owner.user_id());
        tokio::spawn(session_loop.run());

        // 10. Create and store ActiveSession
        let active_session = ActiveSession {
            agent: agent_arc,
            persisted_as,
            event_tx,
            broadcast_tx,
            cancel_token,
            pause_flag,
            pause_notify,
            status,
            checkpoint,
        };

        self.sessions.write().await.insert(session_id.clone(), active_session);

        info!(session_id = %session_id, "session started");

        Ok(SessionHandle(session_id))
    }

    /// Send a user event to the session.
    ///
    /// Dispatches the event to the session loop's input channel.
    async fn send_event(
        &self,
        session: &SessionHandle,
        event: UserEvent,
    ) -> Result<(), RuntimeError> {
        let sessions = self.sessions.read().await;
        let active = sessions
            .get(&session.0)
            .ok_or_else(|| RuntimeError::NotFound { session_id: session.0.clone() })?;

        active
            .event_tx
            .send(event)
            .await
            .map_err(|_| RuntimeError::conflict("session loop channel closed"))?;

        Ok(())
    }

    /// Subscribe to the session's event stream.
    ///
    /// If `from_seq` is provided, replays historical events first, then attaches
    /// to the live broadcast.
    async fn stream_events(
        &self,
        session: &SessionHandle,
        from_seq: Option<u64>,
    ) -> Result<BoxStream<'static, SessionEvent>, RuntimeError> {
        let sessions = self.sessions.read().await;
        let active = sessions
            .get(&session.0)
            .ok_or_else(|| RuntimeError::NotFound { session_id: session.0.clone() })?;

        // Subscribe to broadcast channel
        let broadcast_rx = active.broadcast_tx.subscribe();

        // Read checkpoint for replay
        let checkpoint = active.checkpoint.read().await;
        let stream = create_event_stream(&checkpoint, broadcast_rx, from_seq);

        Ok(stream)
    }

    /// Interrupt the session at the next safe boundary.
    async fn interrupt(&self, session: &SessionHandle) -> Result<(), RuntimeError> {
        let sessions = self.sessions.read().await;
        let active = sessions
            .get(&session.0)
            .ok_or_else(|| RuntimeError::NotFound { session_id: session.0.clone() })?;

        debug!(session_id = %session.0, "interrupting session");
        active.cancel_token.cancel();

        Ok(())
    }

    /// Pause the session, checkpointing current state.
    async fn pause(&self, session: &SessionHandle) -> Result<(), RuntimeError> {
        let sessions = self.sessions.read().await;
        let active = sessions
            .get(&session.0)
            .ok_or_else(|| RuntimeError::NotFound { session_id: session.0.clone() })?;

        debug!(session_id = %session.0, "pausing session");
        *active.pause_flag.lock().await = true;
        *active.status.write().await = SessionStatus::Paused;

        Ok(())
    }

    /// Resume a paused session.
    async fn resume(&self, session: &SessionHandle) -> Result<(), RuntimeError> {
        let sessions = self.sessions.read().await;
        let active = sessions
            .get(&session.0)
            .ok_or_else(|| RuntimeError::NotFound { session_id: session.0.clone() })?;

        debug!(session_id = %session.0, "resuming session");
        *active.pause_flag.lock().await = false;
        *active.status.write().await = SessionStatus::Running;
        active.pause_notify.notify_one();

        Ok(())
    }

    /// Query the current status of a session.
    async fn status(&self, session: &SessionHandle) -> Result<SessionStatus, RuntimeError> {
        let sessions = self.sessions.read().await;
        let active = sessions
            .get(&session.0)
            .ok_or_else(|| RuntimeError::NotFound { session_id: session.0.clone() })?;

        Ok(*active.status.read().await)
    }

    /// Archive a session (terminal state).
    async fn archive(&self, session: &SessionHandle) -> Result<(), RuntimeError> {
        let sessions = self.sessions.read().await;
        let active = sessions
            .get(&session.0)
            .ok_or_else(|| RuntimeError::NotFound { session_id: session.0.clone() })?;

        debug!(session_id = %session.0, "archiving session");
        *active.status.write().await = SessionStatus::Archived;
        active.cancel_token.cancel();

        Ok(())
    }

    /// Delete a session and its associated data.
    async fn delete_session(&self, session: &SessionHandle) -> Result<(), RuntimeError> {
        // First archive (set terminal state and cancel loop)
        {
            let sessions = self.sessions.read().await;
            if let Some(active) = sessions.get(&session.0) {
                *active.status.write().await = SessionStatus::Archived;
                active.cancel_token.cancel();
            }
        }

        // Remove from sessions map
        let removed = self.sessions.write().await.remove(&session.0);
        let Some(removed) = removed else {
            return Err(RuntimeError::NotFound { session_id: session.0.clone() });
        };

        // Deleting the handle is the control plane only. `start_session` seeded a persistent
        // session and the Runner appended every turn to it, so stopping here reported
        // deletion while the conversation stayed in the configured backend — surviving the
        // process that "deleted" it. Delete under the identity creation used.
        let identity = removed.persisted_as;
        self.session_service
            .delete(adk_session::DeleteRequest {
                app_name: identity.app_name.clone(),
                user_id: identity.user_id.clone(),
                session_id: identity.session_id.clone(),
            })
            .await
            .map_err(|e| {
                // The handle is already gone, so the caller must be told the data is not.
                RuntimeError::internal(format!(
                    "session {} was removed from the runtime but its persisted conversation \
                     could not be deleted: {e}. The data remains under app {} / user {} and \
                     needs manual cleanup.",
                    identity.session_id, identity.app_name, identity.user_id
                ))
            })?;

        debug!(
            session_id = %session.0,
            app_name = %identity.app_name,
            user_id = %identity.user_id,
            "session deleted, including persisted conversation"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::DefaultModelResolver;
    use crate::types::{ContentBlock, ModelRef};
    use adk_core::{Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream};
    use async_stream::stream;
    use futures::StreamExt;
    use std::time::Duration;

    /// A minimal in-memory session service for testing.
    /// Uses adk-session's InMemorySessionService.
    fn mock_session_service() -> Arc<dyn SessionService> {
        Arc::new(adk_session::InMemorySessionService::new())
    }

    /// Mock LLM for testing the full runtime lifecycle.
    struct MockLlm {
        name: String,
    }

    impl MockLlm {
        fn new(name: &str) -> Self {
            Self { name: name.to_string() }
        }
    }

    #[async_trait]
    impl Llm for MockLlm {
        fn name(&self) -> &str {
            &self.name
        }

        async fn generate_content(
            &self,
            _request: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<LlmResponseStream> {
            let s = stream! {
                yield Ok(LlmResponse {
                    content: Some(Content::new("model").with_text("Hello from mock")),
                    partial: false,
                    turn_complete: true,
                    finish_reason: Some(FinishReason::Stop),
                    ..Default::default()
                });
            };
            Ok(Box::pin(s))
        }
    }

    /// A mock resolver that returns a MockLlm for any model ref.
    struct MockResolver;

    #[async_trait]
    impl ModelResolver for MockResolver {
        async fn resolve(
            &self,
            _model_ref: &ModelRef,
        ) -> crate::resolver::ResolverResult<Arc<dyn Llm>> {
            Ok(Arc::new(MockLlm::new("mock-model")))
        }
    }

    fn test_owner() -> ManagedOwner {
        ManagedOwner::new("app", "user").expect("valid owner")
    }

    fn create_test_runtime() -> DefaultManagedAgentRuntime {
        let resolver: Arc<dyn ModelResolver> = Arc::new(MockResolver);
        let sessions = mock_session_service();
        DefaultManagedAgentRuntime::new(resolver, sessions)
    }

    #[test]
    fn test_new_with_minimal_config() {
        let resolver = Arc::new(DefaultModelResolver::new());
        let sessions = mock_session_service();

        let _runtime = DefaultManagedAgentRuntime::new(resolver, sessions);

        #[cfg(feature = "sandbox")]
        assert!(_runtime.sandbox().is_none());
        #[cfg(feature = "memory")]
        assert!(_runtime.memory().is_none());
    }

    #[cfg(all(feature = "sandbox", feature = "memory"))]
    #[test]
    fn test_new_with_sandbox_and_memory() {
        use adk_sandbox::{
            BackendCapabilities, EnforcedLimits, ExecRequest, ExecResult, Language, SandboxBackend,
            SandboxError,
        };

        struct FakeSandbox;

        #[async_trait]
        impl SandboxBackend for FakeSandbox {
            fn name(&self) -> &str {
                "fake"
            }
            fn capabilities(&self) -> BackendCapabilities {
                BackendCapabilities {
                    supported_languages: vec![Language::Python],
                    isolation_class: "fake".to_string(),
                    enforced_limits: EnforcedLimits {
                        timeout: true,
                        memory: false,
                        network_isolation: false,
                        // Read and write isolation are reported separately since #485.
                        filesystem_write_isolation: false,
                        filesystem_read_isolation: false,
                        environment_isolation: false,
                    },
                }
            }
            async fn execute(&self, _request: ExecRequest) -> Result<ExecResult, SandboxError> {
                Ok(ExecResult {
                    stdout: "ok".to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                    duration: std::time::Duration::from_millis(1),
                })
            }
        }

        struct FakeMemory;

        #[async_trait]
        impl adk_core::Memory for FakeMemory {
            async fn search(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
                Ok(vec![])
            }
        }

        let resolver = Arc::new(DefaultModelResolver::new());
        let sessions = mock_session_service();

        let runtime = DefaultManagedAgentRuntime::new(resolver, sessions)
            .with_sandbox(Arc::new(FakeSandbox))
            .with_memory(Arc::new(FakeMemory));

        assert!(runtime.sandbox().is_some());
        assert!(runtime.memory().is_some());
    }

    #[test]
    fn test_sessions_map_starts_empty() {
        let resolver = Arc::new(DefaultModelResolver::new());
        let sessions = mock_session_service();

        let runtime = DefaultManagedAgentRuntime::new(resolver, sessions);

        let sessions = runtime.sessions().try_read().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_accessors_return_injected_services() {
        let resolver: Arc<dyn ModelResolver> = Arc::new(DefaultModelResolver::new());
        let session_service = mock_session_service();

        let runtime =
            DefaultManagedAgentRuntime::new(Arc::clone(&resolver), Arc::clone(&session_service));

        // Verify we get references back (type-level verification)
        let _r: &Arc<dyn ModelResolver> = runtime.model_resolver();
        let _s: &Arc<dyn SessionService> = runtime.session_service();
    }

    // ─── Task 7.2: create() method tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_create_agent_returns_handle() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "test-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: Some("You are helpful.".to_string()),
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let handle = runtime.create(def).await.unwrap();
        assert!(!handle.0.is_empty());
    }

    #[tokio::test]
    async fn test_create_agent_stores_in_registry() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "stored-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let handle = runtime.create(def).await.unwrap();
        let agents = runtime.agents.read().await;
        assert!(agents.contains_key(&handle.0));
    }

    #[tokio::test]
    async fn test_create_multiple_agents() {
        let runtime = create_test_runtime();

        let make_def = |name: &str| ManagedAgentDef {
            name: name.to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let h1 = runtime.create(make_def("agent-1")).await.unwrap();
        let h2 = runtime.create(make_def("agent-2")).await.unwrap();

        assert_ne!(h1.0, h2.0);
        assert_eq!(runtime.agents.read().await.len(), 2);
    }

    // ─── Task 7.3: start_session() method tests ──────────────────────────────

    #[tokio::test]
    async fn test_start_session_returns_handle() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "session-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();
        assert!(!session.0.is_empty());
    }

    #[tokio::test]
    async fn test_start_session_initial_status_queued() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "status-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();

        let status = runtime.status(&session).await.unwrap();
        assert_eq!(status, SessionStatus::Queued);
    }

    #[tokio::test]
    async fn test_start_session_unknown_agent_returns_error() {
        let runtime = create_test_runtime();

        let fake_agent = AgentHandle("nonexistent".to_string());
        let result = runtime.start_session(&fake_agent, &test_owner(), None).await;
        assert!(result.is_err());
    }

    // ─── Task 7.4: send_event() method tests ─────────────────────────────────

    #[tokio::test]
    async fn test_send_event_message() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "event-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();

        let event =
            UserEvent::Message { content: vec![ContentBlock::Text { text: "Hello".to_string() }] };

        let result = runtime.send_event(&session, event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_event_unknown_session_returns_error() {
        let runtime = create_test_runtime();

        let fake_session = SessionHandle("nonexistent".to_string());
        let event =
            UserEvent::Message { content: vec![ContentBlock::Text { text: "Hello".to_string() }] };

        let result = runtime.send_event(&fake_session, event).await;
        assert!(result.is_err());
    }

    // ─── Task 7.5: stream_events() method tests ──────────────────────────────

    #[tokio::test]
    async fn test_stream_events_receives_broadcast() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "stream-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();

        // Subscribe to stream
        let mut stream = runtime.stream_events(&session, None).await.unwrap();

        // Send a message (the session loop will process it and emit events)
        let event =
            UserEvent::Message { content: vec![ContentBlock::Text { text: "Test".to_string() }] };
        runtime.send_event(&session, event).await.unwrap();

        // We should receive at least a StatusRunning event
        let first_event = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("timed out waiting for event")
            .expect("stream ended unexpectedly");

        match first_event {
            SessionEvent::StatusRunning { .. } => {}
            other => panic!("expected StatusRunning, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stream_events_unknown_session_returns_error() {
        let runtime = create_test_runtime();

        let fake_session = SessionHandle("nonexistent".to_string());
        let result = runtime.stream_events(&fake_session, None).await;
        assert!(result.is_err());
    }

    // ─── Task 7.6: interrupt/pause/resume/status/archive/delete tests ────────

    #[tokio::test]
    async fn test_interrupt_cancels_session() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "interrupt-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();

        let result = runtime.interrupt(&session).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_pause_sets_paused_status() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "pause-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();

        runtime.pause(&session).await.unwrap();
        let status = runtime.status(&session).await.unwrap();
        assert_eq!(status, SessionStatus::Paused);
    }

    #[tokio::test]
    async fn test_resume_clears_pause() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "resume-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();

        runtime.pause(&session).await.unwrap();
        assert_eq!(runtime.status(&session).await.unwrap(), SessionStatus::Paused);

        runtime.resume(&session).await.unwrap();
        assert_eq!(runtime.status(&session).await.unwrap(), SessionStatus::Running);
    }

    #[tokio::test]
    async fn test_archive_sets_archived_status() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "archive-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();

        runtime.archive(&session).await.unwrap();
        let status = runtime.status(&session).await.unwrap();
        assert_eq!(status, SessionStatus::Archived);
    }

    #[tokio::test]
    async fn test_delete_session_removes_from_registry() {
        let runtime = create_test_runtime();

        let def = ManagedAgentDef {
            name: "delete-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let agent = runtime.create(def).await.unwrap();
        let session = runtime
            .start_session(&agent, &ManagedOwner::new("app", "user").unwrap(), None)
            .await
            .unwrap();

        runtime.delete_session(&session).await.unwrap();

        // Session should no longer be accessible
        let result = runtime.status(&session).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_session_returns_error() {
        let runtime = create_test_runtime();

        let fake_session = SessionHandle("nonexistent".to_string());
        let result = runtime.delete_session(&fake_session).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_interrupt_nonexistent_session_returns_error() {
        let runtime = create_test_runtime();

        let fake_session = SessionHandle("nonexistent".to_string());
        let result = runtime.interrupt(&fake_session).await;
        assert!(result.is_err());
    }
}
