//! ACP session handler: manages sessions and routes prompts to the ADK Runner.
//!
//! The [`AcpSessionHandler`] is the core component that bridges ACP protocol
//! messages to ADK agent execution.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{Agent, Content, SessionId, UserId};
use adk_runner::{Runner, RunnerConfig};
use adk_session::SessionService;
use futures::StreamExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::config::AcpServerConfig;
use super::error::AcpServerError;
use super::streamer::{ResponseStreamer, SessionNotification};

/// State for a single active ACP session.
#[derive(Debug)]
struct SessionState {
    /// The ADK session ID.
    #[allow(dead_code)]
    session_id: String,
    /// User ID for this session.
    user_id: String,
    /// Cancellation token for in-progress execution.
    execution_token: Option<CancellationToken>,
    /// Whether this session is currently executing a prompt.
    is_executing: bool,
}

/// Manages ACP sessions and routes prompts to the ADK Runner.
///
/// Handles session lifecycle (create, close, drain) and prompt execution.
/// Each session maps 1:1 to an ADK session managed by the configured
/// [`SessionService`].
pub struct AcpSessionHandler {
    agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
    app_name: String,
    max_sessions: usize,
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
    shutdown_token: CancellationToken,
}

impl AcpSessionHandler {
    /// Create a new session handler from the server config.
    pub fn new(
        config: &AcpServerConfig,
        shutdown_token: CancellationToken,
    ) -> Result<Self, AcpServerError> {
        Ok(Self {
            agent: config.agent.clone(),
            session_service: config.session_service.clone(),
            app_name: config.agent_name.clone(),
            max_sessions: config.max_sessions,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            shutdown_token,
        })
    }

    /// Create a new ACP session.
    ///
    /// Creates a corresponding ADK session via the configured SessionService
    /// and returns a unique session identifier.
    ///
    /// # Errors
    ///
    /// Returns `MaxSessionsReached` if the session limit is hit.
    /// Returns `ShuttingDown` if the server is shutting down.
    pub async fn create_session(&self) -> Result<String, AcpServerError> {
        if self.shutdown_token.is_cancelled() {
            return Err(AcpServerError::ShuttingDown);
        }

        let mut sessions = self.sessions.lock().await;

        if sessions.len() >= self.max_sessions {
            return Err(AcpServerError::MaxSessionsReached(self.max_sessions));
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let user_id = format!("acp-user-{}", &session_id[..8]);

        // Create the ADK session via SessionService
        self.session_service
            .create(adk_session::CreateRequest {
                app_name: self.app_name.clone(),
                user_id: user_id.clone(),
                session_id: Some(session_id.clone()),
                state: HashMap::new(),
            })
            .await
            .map_err(|e| AcpServerError::Internal(format!("failed to create session: {e}")))?;

        sessions.insert(
            session_id.clone(),
            SessionState {
                session_id: session_id.clone(),
                user_id,
                execution_token: None,
                is_executing: false,
            },
        );

        info!(session_id = %session_id, "created ACP session");
        Ok(session_id)
    }

    /// Close an ACP session and release resources.
    ///
    /// Cancels any in-progress execution and removes the session from the registry.
    ///
    /// # Errors
    ///
    /// Returns `SessionNotFound` if the session doesn't exist.
    pub async fn close_session(&self, session_id: &str) -> Result<(), AcpServerError> {
        let mut sessions = self.sessions.lock().await;

        let state = sessions
            .remove(session_id)
            .ok_or_else(|| AcpServerError::SessionNotFound(session_id.to_string()))?;

        // Cancel in-progress execution if any
        if let Some(token) = &state.execution_token {
            token.cancel();
        }

        info!(session_id = %session_id, "closed ACP session");
        Ok(())
    }

    /// Handle a prompt within an active session.
    ///
    /// Validates the session exists, runs the agent, and returns a vector
    /// of notifications produced during execution.
    ///
    /// # Errors
    ///
    /// Returns `SessionNotFound` if the session doesn't exist.
    /// Returns `ShuttingDown` if the server is shutting down.
    pub async fn handle_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<Vec<SessionNotification>, AcpServerError> {
        if self.shutdown_token.is_cancelled() {
            return Err(AcpServerError::ShuttingDown);
        }

        // Validate session and mark as executing
        let (user_id, exec_token) = {
            let mut sessions = self.sessions.lock().await;
            let state = sessions
                .get_mut(session_id)
                .ok_or_else(|| AcpServerError::SessionNotFound(session_id.to_string()))?;

            let exec_token = CancellationToken::new();
            state.execution_token = Some(exec_token.clone());
            state.is_executing = true;
            (state.user_id.clone(), exec_token)
        };

        // Build runner config and execute
        let result = self.execute_prompt(session_id, &user_id, prompt, exec_token.clone()).await;

        // Mark session as no longer executing
        {
            let mut sessions = self.sessions.lock().await;
            if let Some(state) = sessions.get_mut(session_id) {
                state.is_executing = false;
                state.execution_token = None;
            }
        }

        result
    }

    /// Execute a prompt using the ADK Runner.
    async fn execute_prompt(
        &self,
        session_id: &str,
        user_id: &str,
        prompt: &str,
        cancellation_token: CancellationToken,
    ) -> Result<Vec<SessionNotification>, AcpServerError> {
        let runner_config = RunnerConfig {
            app_name: self.app_name.clone(),
            agent: self.agent.clone(),
            session_service: self.session_service.clone(),
            memory_service: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
            request_context: None,
            cancellation_token: Some(cancellation_token),
            intra_compaction_config: None,
            intra_compaction_summarizer: None,
        };

        let runner = Runner::new(runner_config)
            .map_err(|e| AcpServerError::Execution(format!("failed to create runner: {e}")))?;

        let content = Content::new("user").with_text(prompt);

        let mut event_stream = runner
            .run(
                UserId::new(user_id.to_string())
                    .map_err(|e| AcpServerError::Execution(e.to_string()))?,
                SessionId::new(session_id.to_string())
                    .map_err(|e| AcpServerError::Execution(e.to_string()))?,
                content,
            )
            .await
            .map_err(|e| AcpServerError::Execution(format!("runner.run failed: {e}")))?;

        let mut notifications = Vec::new();

        while let Some(result) = event_stream.next().await {
            match result {
                Ok(event) => {
                    let mapped = ResponseStreamer::map_event(&event);
                    notifications.extend(mapped);
                }
                Err(e) => {
                    warn!(error = %e, session_id = %session_id, "event stream error");
                    notifications
                        .push(ResponseStreamer::make_error("execution_error", &e.to_string()));
                    break;
                }
            }
        }

        notifications.push(ResponseStreamer::make_completion());
        Ok(notifications)
    }

    /// Get the number of active sessions.
    pub async fn active_session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Drain all sessions during shutdown.
    ///
    /// Waits for in-progress executions to complete or cancels them
    /// after the given timeout.
    pub async fn drain_sessions(&self, timeout: std::time::Duration) {
        info!("draining sessions (timeout: {timeout:?})");

        // Cancel all in-progress executions
        let sessions = self.sessions.lock().await;
        for (id, state) in sessions.iter() {
            if let Some(token) = &state.execution_token {
                info!(session_id = %id, "cancelling in-progress execution");
                token.cancel();
            }
        }
        drop(sessions);

        // Wait briefly for executions to finish
        tokio::time::sleep(timeout.min(std::time::Duration::from_secs(1))).await;

        // Clear all sessions
        let mut sessions = self.sessions.lock().await;
        sessions.clear();
        info!("all sessions drained");
    }
}
