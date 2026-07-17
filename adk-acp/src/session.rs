//! Persistent ACP session with connection reuse.
//!
//! Unlike [`prompt_agent`](crate::prompt_agent) which spawns a fresh process per call,
//! [`AcpSession`] keeps the agent process alive across multiple prompts — preserving
//! context, reducing latency, and enabling session-based workflows.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_acp::{AcpSession, AcpAgentConfig, PermissionPolicy};
//! use std::sync::Arc;
//!
//! let config = AcpAgentConfig::new("my-coding-agent --acp")
//!     .working_dir("/path/to/project");
//!
//! let mut session = AcpSession::start(config, Arc::new(PermissionPolicy::DenyAll)).await?;
//!
//! // First prompt — Kiro reads the project structure
//! let r1 = session.prompt("List the files in src/").await?;
//! println!("{}", r1.text);
//!
//! // Second prompt — Kiro already has context from the first
//! let r2 = session.prompt("Now explain what main.rs does").await?;
//! println!("{}", r2.text);
//!
//! // Clean shutdown
//! session.close().await?;
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, ContentBlock, ContentChunk, InitializeRequest,
    NewSessionRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionNotification, SessionUpdate,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{Agent, Client, ConnectionTo, Responder, SessionMessage};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::connection::AcpAgentConfig;
use crate::error::{AcpError, Result};
use crate::host::{AcpHostHandler, capabilities};
use crate::permissions::{PermissionPolicy, PermissionRequest, outcome_for};

/// Result of a prompt sent to a persistent session.
#[derive(Debug, Clone)]
pub struct PromptResult {
    /// The text response from the agent.
    pub text: String,
    /// Wall-clock duration of this prompt.
    pub duration: Duration,
    /// Number of prompts sent in this session so far (including this one).
    pub prompt_count: u32,
}

/// A persistent connection to an ACP agent with session reuse.
///
/// The agent process stays alive between prompts, preserving conversation
/// context and reducing spawn overhead. Use this when you need multiple
/// interactions with the same agent in sequence.
pub struct AcpSession {
    config: AcpAgentConfig,
    #[allow(dead_code)]
    policy: Arc<PermissionPolicy>,
    prompt_count: u32,
    started_at: Instant,
    /// Inner state — None after close()
    inner: Option<SessionInner>,
}

/// Holds the actual connection state.
/// We use a channel-based approach: the ACP connection runs in a background task,
/// and we send prompts to it via channels.
struct SessionInner {
    prompt_tx: tokio::sync::mpsc::Sender<SessionCommand>,
    result_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<SessionResult>>>,
}

/// A cloneable handle that can cancel a prompt while another task awaits it.
///
/// Create the handle before calling [`AcpSession::prompt`], then move it into a
/// UI event handler, timeout task, or shutdown task. It sends ACP's official
/// `session/cancel` notification and leaves the session available for another
/// prompt after the agent acknowledges cancellation.
#[derive(Clone)]
pub struct AcpCancellationHandle {
    prompt_tx: tokio::sync::mpsc::Sender<SessionCommand>,
}

impl AcpCancellationHandle {
    /// Ask the external ACP agent to cancel its current prompt.
    pub async fn cancel(&self) -> Result<()> {
        self.prompt_tx
            .send(SessionCommand::Cancel)
            .await
            .map_err(|_| AcpError::ConnectionLost("agent process exited".into()))
    }
}

enum SessionCommand {
    Prompt(String),
    Cancel,
    Close,
}

enum SessionResult {
    Response(String),
    Error(String),
    Cancelled,
    Closed,
}

impl AcpSession {
    /// Start a new persistent session with an ACP agent.
    ///
    /// Spawns the agent process, performs the ACP handshake, and creates a session.
    /// The connection stays alive until [`close()`](Self::close) is called or the
    /// session is dropped.
    pub async fn start(config: AcpAgentConfig, policy: Arc<PermissionPolicy>) -> Result<Self> {
        info!(command = %config.command, cwd = %config.working_dir.display(), "starting persistent ACP session");

        let agent = crate::connection::build_agent(&config)?;

        let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<SessionCommand>(1);
        let (result_tx, result_rx) = tokio::sync::mpsc::channel::<SessionResult>(1);

        let working_dir = config.working_dir.clone();
        let mcp_servers = config.mcp_servers.clone();
        let filesystem = config.filesystem.clone();
        let terminal = config.terminal.clone();
        let client_capabilities = capabilities(filesystem.as_ref(), terminal.as_ref());
        let policy_clone = policy.clone();

        // Spawn the ACP connection in a background task
        tokio::spawn(async move {
            let result_tx_err = result_tx.clone();
            let outcome = Client
                .builder()
                .on_receive_request(
                    {
                        let policy = policy_clone.clone();
                        move |request: RequestPermissionRequest,
                              responder: Responder<RequestPermissionResponse>,
                              cx: ConnectionTo<Agent>| {
                            let policy = policy.clone();
                            async move {
                            cx.spawn(async move {
                                let cancellation = responder.cancellation();
                                let perm_request = PermissionRequest::from_acp(&request);
                                let decision = tokio::select! {
                                    decision = policy.decide(&perm_request) => decision,
                                    _ = cancellation.cancelled() => {
                                        return responder.respond(RequestPermissionResponse::new(
                                            RequestPermissionOutcome::Cancelled,
                                        ));
                                    }
                                };
                                let outcome = outcome_for(&perm_request, &decision);
                                debug!(title = %perm_request.title, decision = %decision, "ACP permission policy evaluated");
                                responder.respond(RequestPermissionResponse::new(outcome))
                            })?;
                            Ok(())
                            }
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .with_handler(AcpHostHandler::new(filesystem, terminal))
                .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
                    // Initialize
                    let initialization = connection
                        .send_request(
                            InitializeRequest::new(ProtocolVersion::V1)
                                .client_capabilities(client_capabilities),
                        )
                        .block_task()
                        .await?;
                    crate::connection::validate_initialization(
                        &initialization,
                        &mcp_servers,
                    )?;

                    // Create session and enter the prompt loop
                    connection
                        .build_session_from(
                            NewSessionRequest::new(&working_dir).mcp_servers(mcp_servers),
                        )
                        .block_task()
                        .run_until(async |mut session| {
                            // Process commands from the main task
                            while let Some(cmd) = prompt_rx.recv().await {
                                match cmd {
                                    SessionCommand::Prompt(text) => {
                                        if let Err(error) = session.send_prompt(&text) {
                                            let _ = result_tx
                                                .send(SessionResult::Error(error.to_string()))
                                                .await;
                                            continue;
                                        }

                                        let connection = session.connection();
                                        let session_id = session.session_id().clone();
                                        let mut response = String::new();
                                        let mut cancellation_requested = false;

                                        loop {
                                            tokio::select! {
                                                update = session.read_update() => {
                                                    match update? {
                                                        SessionMessage::SessionMessage(dispatch) => {
                                                            MatchDispatch::new(dispatch)
                                                                .if_notification(async |notification: SessionNotification| {
                                                                    if let SessionUpdate::AgentMessageChunk(ContentChunk {
                                                                        content: ContentBlock::Text(text),
                                                                        ..
                                                                    }) = notification.update
                                                                    {
                                                                        response.push_str(&text.text);
                                                                    }
                                                                    Ok(())
                                                                })
                                                                .await
                                                                .otherwise_ignore()?;
                                                        }
                                                        SessionMessage::StopReason(stop_reason) => {
                                                            let result = if cancellation_requested
                                                                || stop_reason
                                                                    == agent_client_protocol::schema::v1::StopReason::Cancelled
                                                            {
                                                                SessionResult::Cancelled
                                                            } else {
                                                                SessionResult::Response(response)
                                                            };
                                                            let _ = result_tx.send(result).await;
                                                            break;
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                                command = prompt_rx.recv() => {
                                                    match command {
                                                        Some(SessionCommand::Cancel) => {
                                                            cancellation_requested = true;
                                                            connection.send_notification(
                                                                CancelNotification::new(session_id.clone()),
                                                            )?;
                                                        }
                                                        Some(SessionCommand::Close) => {
                                                            connection.send_notification(
                                                                CancelNotification::new(session_id.clone()),
                                                            )?;
                                                            let _ = result_tx.send(SessionResult::Closed).await;
                                                            return Ok(());
                                                        }
                                                        Some(SessionCommand::Prompt(_)) => {
                                                            let _ = result_tx
                                                                .send(SessionResult::Error(
                                                                    "a prompt is already running in this ACP session".into(),
                                                                ))
                                                                .await;
                                                        }
                                                        None => return Ok(()),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    SessionCommand::Cancel => {
                                        session.connection().send_notification(
                                            CancelNotification::new(session.session_id().clone()),
                                        )?;
                                        let _ = result_tx.send(SessionResult::Cancelled).await;
                                    }
                                    SessionCommand::Close => {
                                        session
                                            .connection()
                                            .send_request(CloseSessionRequest::new(
                                                session.session_id().clone(),
                                            ))
                                            .block_task()
                                            .await?;
                                        let _ = result_tx.send(SessionResult::Closed).await;
                                        break;
                                    }
                                }
                            }
                            Ok(())
                        })
                        .await?;

                    Ok(())
                })
                .await;

            if let Err(e) = outcome {
                warn!(error = %e, "ACP session background task ended with error");
                let _ = result_tx_err.send(SessionResult::Error(e.to_string())).await;
            }
        });

        Ok(Self {
            config,
            policy,
            prompt_count: 0,
            started_at: Instant::now(),
            inner: Some(SessionInner { prompt_tx, result_rx: Arc::new(Mutex::new(result_rx)) }),
        })
    }

    /// Send a prompt to the agent within the existing session.
    ///
    /// The agent retains context from previous prompts in this session,
    /// so you don't need to re-explain project structure or repeat instructions.
    pub async fn prompt(&mut self, text: &str) -> Result<PromptResult> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| AcpError::ConnectionLost("session already closed".into()))?;

        let start = Instant::now();
        self.prompt_count += 1;

        debug!(
            prompt_count = self.prompt_count,
            prompt_len = text.len(),
            "sending prompt to persistent session"
        );

        inner
            .prompt_tx
            .send(SessionCommand::Prompt(text.to_string()))
            .await
            .map_err(|_| AcpError::ConnectionLost("agent process exited".into()))?;

        let mut rx = inner.result_rx.lock().await;
        match rx.recv().await {
            Some(SessionResult::Response(text)) => Ok(PromptResult {
                text,
                duration: start.elapsed(),
                prompt_count: self.prompt_count,
            }),
            Some(SessionResult::Error(e)) => Err(AcpError::Protocol(e)),
            Some(SessionResult::Cancelled) => {
                Err(AcpError::ConnectionLost("prompt cancelled".into()))
            }
            Some(SessionResult::Closed) => Err(AcpError::ConnectionLost("session closed".into())),
            None => Err(AcpError::ConnectionLost("agent process exited".into())),
        }
    }

    /// Close the session and terminate the agent process.
    pub async fn close(&mut self) -> Result<()> {
        if let Some(inner) = self.inner.take() {
            inner
                .prompt_tx
                .send(SessionCommand::Close)
                .await
                .map_err(|_| AcpError::ConnectionLost("agent process exited".into()))?;
            let mut rx = inner.result_rx.lock().await;
            match rx.recv().await {
                Some(SessionResult::Closed) => {}
                Some(SessionResult::Error(error)) => return Err(AcpError::Protocol(error)),
                None => return Err(AcpError::ConnectionLost("agent process exited".into())),
                _ => return Err(AcpError::Protocol("unexpected ACP close response".into())),
            }
            info!(
                prompt_count = self.prompt_count,
                uptime = ?self.started_at.elapsed(),
                "ACP session closed"
            );
        }
        Ok(())
    }

    /// Cancel the currently running prompt.
    ///
    /// Because this method borrows the session, use
    /// [`cancellation_handle`](Self::cancellation_handle) when a different task
    /// must cancel a prompt currently being awaited.
    pub async fn cancel(&mut self) -> Result<()> {
        if let Some(inner) = &self.inner {
            info!("cancelling in-progress ACP prompt");
            let _ = inner.prompt_tx.send(SessionCommand::Cancel).await;
            // Drain the result channel
            let mut rx = inner.result_rx.lock().await;
            let _ = rx.recv().await;
        }
        Ok(())
    }

    /// Return a handle that can cancel an in-flight prompt from another task.
    pub fn cancellation_handle(&self) -> Result<AcpCancellationHandle> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| AcpError::ConnectionLost("session already closed".into()))?;
        Ok(AcpCancellationHandle { prompt_tx: inner.prompt_tx.clone() })
    }

    /// Number of prompts sent in this session.
    pub fn prompt_count(&self) -> u32 {
        self.prompt_count
    }

    /// How long this session has been alive.
    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Whether the session is still connected.
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    /// Get the working directory for this session.
    pub fn working_dir(&self) -> &PathBuf {
        &self.config.working_dir
    }
}

impl Drop for AcpSession {
    fn drop(&mut self) {
        if self.inner.is_some() {
            warn!("AcpSession dropped without explicit close — agent process may linger");
        }
    }
}
