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
//! // First prompt — the agent reads the project structure
//! let r1 = session.prompt("List the files in src/").await?;
//! println!("{}", r1.text);
//!
//! // Second prompt — the agent already has context from the first
//! let r2 = session.prompt("Now explain what main.rs does").await?;
//! println!("{} ({:?})", r2.text, r2.stop_reason);
//!
//! // Clean shutdown
//! session.close().await?;
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::Content;
use agent_client_protocol::v1::schema::{
    AvailableCommand, CancelNotification, CloseSessionRequest, ContentBlock, ContentChunk,
    PromptRequest, SessionConfigId, SessionConfigOption, SessionConfigOptionValue, SessionId,
    SessionModeId, SessionModeState, SessionStatus, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionModeRequest, SetSessionTitleRequest, StopReason, Usage,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::client::{AcpAgentInfo, AcpConnection, RoutedUpdate};
use crate::connection::AcpAgentConfig;
use crate::content::content_to_blocks;
use crate::error::{AcpError, Result};
use crate::permissions::PermissionPolicy;
use crate::streaming::{OutputChunk, map_routed_update};

/// Result of a prompt sent to a persistent session.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PromptResult {
    /// The text response from the agent.
    pub text: String,
    /// Wall-clock duration of this prompt.
    pub duration: Duration,
    /// Number of prompts sent in this session so far (including this one).
    pub prompt_count: u32,
    /// Why the agent ended the turn.
    pub stop_reason: StopReason,
    /// The tokens the turn used, when the agent reported them.
    pub usage: Option<Usage>,
}

/// What the client knows about a session's controls, kept current from the
/// agent's responses and `session/update` notifications.
#[derive(Debug, Default, Clone)]
struct SessionView {
    modes: Option<SessionModeState>,
    config_options: Option<Vec<SessionConfigOption>>,
    available_commands: Vec<AvailableCommand>,
    title: Option<String>,
}

impl SessionView {
    fn observe(&mut self, update: &SessionUpdate) {
        match update {
            SessionUpdate::CurrentModeUpdate(update) => {
                if let Some(modes) = &mut self.modes {
                    modes.current_mode_id = update.current_mode_id.clone();
                }
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                self.config_options = Some(update.config_options.clone());
            }
            SessionUpdate::AvailableCommandsUpdate(update) => {
                self.available_commands = update.available_commands.clone();
            }
            SessionUpdate::SessionInfoUpdate(update) => {
                if let Some(title) = update.title.value() {
                    self.title = Some(title.clone());
                } else if update.title.is_null() {
                    self.title = None;
                }
            }
            _ => {}
        }
    }
}

/// A persistent connection to an ACP agent with session reuse.
///
/// The agent process stays alive between prompts, preserving conversation
/// context and reducing spawn overhead. Use this when you need multiple
/// interactions with the same agent in sequence.
///
/// A session opened by [`AcpSession::start`] owns its agent process. One
/// opened from an [`AcpConnection`] shares that connection's process with the
/// connection's other sessions.
pub struct AcpSession {
    connection: AcpConnection,
    owns_connection: bool,
    session_id: SessionId,
    updates: mpsc::UnboundedReceiver<RoutedUpdate>,
    view: Arc<Mutex<SessionView>>,
    prompt_count: u32,
    started_at: Instant,
    closed: bool,
}

/// A cloneable handle that can cancel a prompt while another task awaits it.
///
/// Create the handle before calling [`AcpSession::prompt`], then move it into a
/// UI event handler, timeout task, or shutdown task. It sends ACP's official
/// `session/cancel` notification and leaves the session available for another
/// prompt after the agent acknowledges cancellation.
#[derive(Clone)]
pub struct AcpCancellationHandle {
    connection: AcpConnection,
    session_id: SessionId,
}

impl AcpCancellationHandle {
    /// Ask the external ACP agent to cancel its current prompt.
    pub async fn cancel(&self) -> Result<()> {
        self.connection
            .inner
            .connection
            .send_notification(CancelNotification::new(self.session_id.clone()))
            .map_err(|error| AcpError::ConnectionLost(error.to_string()))
    }
}

impl AcpSession {
    /// Start a new persistent session with an ACP agent.
    ///
    /// Spawns the agent process, performs the ACP handshake, and creates a
    /// session. The session owns the agent process, which stops when
    /// [`close()`](Self::close) is called or the session is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent cannot be started, the handshake fails, or
    /// the agent refuses the session.
    pub async fn start(config: AcpAgentConfig, policy: Arc<PermissionPolicy>) -> Result<Self> {
        info!(command = %config.command, cwd = %config.working_dir.display(), "starting persistent ACP session");
        let connection = AcpConnection::connect(config, policy).await?;
        match connection.new_session().await {
            Ok(mut session) => {
                session.owns_connection = true;
                Ok(session)
            }
            Err(error) => {
                let _ = connection.close().await;
                Err(error)
            }
        }
    }

    pub(crate) fn from_parts(
        connection: AcpConnection,
        session_id: SessionId,
        updates: mpsc::UnboundedReceiver<RoutedUpdate>,
        modes: Option<SessionModeState>,
        config_options: Option<Vec<SessionConfigOption>>,
    ) -> Self {
        Self {
            connection,
            owns_connection: false,
            session_id,
            updates,
            view: Arc::new(Mutex::new(SessionView {
                modes,
                config_options,
                ..SessionView::default()
            })),
            prompt_count: 0,
            started_at: Instant::now(),
            closed: false,
        }
    }

    /// Send a prompt to the agent within the existing session.
    ///
    /// The agent retains context from previous prompts in this session,
    /// so you don't need to re-explain project structure or repeat instructions.
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::ConnectionLost`] if the prompt was cancelled or the
    /// agent exited, and [`AcpError::Protocol`] if the agent answered the
    /// prompt with an error.
    pub async fn prompt(&mut self, text: &str) -> Result<PromptResult> {
        self.prompt_blocks(vec![ContentBlock::from(text.to_string())], |_| {}).await
    }

    /// Send a multimodal prompt built from ADK [`Content`]: text, images,
    /// audio and embedded resources each become their ACP content block.
    pub async fn prompt_content(&mut self, content: &Content) -> Result<PromptResult> {
        let blocks = content_to_blocks(content);
        if blocks.is_empty() {
            return Err(AcpError::InvalidConfig(
                "prompt content contained no transmittable blocks".into(),
            ));
        }
        self.prompt_blocks(blocks, |_| {}).await
    }

    /// Send a prompt made of ACP content blocks and observe every update of the
    /// turn as an [`OutputChunk`] while it runs.
    ///
    /// `on_chunk` sees text, thoughts, tool calls and updates, plans, usage,
    /// notices, compactions, mode and config changes and subagent activity, in
    /// the order the agent sent them. The returned result carries the
    /// collected message text, the stop reason and the turn's usage.
    pub async fn prompt_blocks(
        &mut self,
        blocks: Vec<ContentBlock>,
        mut on_chunk: impl FnMut(OutputChunk) + Send,
    ) -> Result<PromptResult> {
        self.ensure_open()?;
        let start = Instant::now();
        self.prompt_count += 1;
        debug!(
            prompt_count = self.prompt_count,
            blocks = blocks.len(),
            "sending prompt to ACP session"
        );

        // Updates that arrived between turns, such as available commands or a
        // late chunk of a cancelled turn, update the session view only.
        while let Ok(update) = self.updates.try_recv() {
            self.observe(&update);
        }

        let connection = self.connection.inner.connection.clone();
        let prompt = connection
            .send_request(PromptRequest::new(self.session_id.clone(), blocks))
            .block_task();
        tokio::pin!(prompt);

        let mut text = String::new();
        let response = loop {
            tokio::select! {
                biased;
                Some(update) = self.updates.recv() => {
                    self.consume(update, &mut text, &mut on_chunk);
                }
                response = &mut prompt => break response,
            }
        };
        // The agent sends a turn's updates before its response, and the
        // connection dispatches them in order, so every update of the turn is
        // already routed: drain what the select did not reach.
        while let Ok(update) = self.updates.try_recv() {
            self.consume(update, &mut text, &mut on_chunk);
        }

        let response = response.map_err(|error| self.connection.error(error))?;
        if response.stop_reason == StopReason::Cancelled {
            return Err(AcpError::ConnectionLost("prompt cancelled".into()));
        }
        Ok(PromptResult {
            text,
            duration: start.elapsed(),
            prompt_count: self.prompt_count,
            stop_reason: response.stop_reason,
            usage: response.usage,
        })
    }

    /// The updates the agent sent outside a prompt turn and not yet read, such
    /// as the history replayed by [`AcpConnection::load_session`].
    pub fn pending_updates(&mut self) -> Vec<OutputChunk> {
        let mut chunks = Vec::new();
        while let Ok(update) = self.updates.try_recv() {
            self.observe(&update);
            chunks.extend(map_routed_update(&self.session_id, update));
        }
        chunks
    }

    fn consume(
        &self,
        update: RoutedUpdate,
        text: &mut String,
        on_chunk: &mut impl FnMut(OutputChunk),
    ) {
        self.observe(&update);
        if update.session_id == self.session_id
            && let SessionUpdate::AgentMessageChunk(ContentChunk {
                content: ContentBlock::Text(chunk),
                ..
            }) = &update.update
        {
            text.push_str(&chunk.text);
        }
        if let Some(chunk) = map_routed_update(&self.session_id, update) {
            on_chunk(chunk);
        }
    }

    fn observe(&self, update: &RoutedUpdate) {
        if update.session_id == self.session_id
            && let Ok(mut view) = self.view.lock()
        {
            view.observe(&update.update);
        }
    }

    /// Close the session and, for a session started with [`start`](Self::start),
    /// stop the agent process.
    ///
    /// Sends `session/close` when the agent advertises it; otherwise cancels
    /// any running turn and releases the session locally.
    pub async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.connection.inner.routes.remove(&self.session_id);
        let result = if self.connection.is_closed() {
            Ok(())
        } else if self.connection.capabilities().session_capabilities.close.is_some() {
            self.connection
                .request(CloseSessionRequest::new(self.session_id.clone()))
                .await
                .map(drop)
        } else {
            self.connection
                .inner
                .connection
                .send_notification(CancelNotification::new(self.session_id.clone()))
                .map_err(|error| AcpError::ConnectionLost(error.to_string()))
        };
        if self.owns_connection {
            self.connection.close().await?;
        }
        info!(
            prompt_count = self.prompt_count,
            uptime = ?self.started_at.elapsed(),
            "ACP session closed"
        );
        result
    }

    /// Cancel the currently running prompt (`session/cancel`).
    ///
    /// Because this method borrows the session, use
    /// [`cancellation_handle`](Self::cancellation_handle) when a different task
    /// must cancel a prompt currently being awaited.
    pub async fn cancel(&mut self) -> Result<()> {
        info!("cancelling in-progress ACP prompt");
        self.cancellation_handle()?.cancel().await
    }

    /// Return a handle that can cancel an in-flight prompt from another task.
    pub fn cancellation_handle(&self) -> Result<AcpCancellationHandle> {
        self.ensure_open()?;
        Ok(AcpCancellationHandle {
            connection: self.connection.clone(),
            session_id: self.session_id.clone(),
        })
    }

    /// Switch the session's mode (`session/set_mode`).
    pub async fn set_mode(&mut self, mode_id: impl Into<SessionModeId>) -> Result<()> {
        self.ensure_open()?;
        let mode_id = mode_id.into();
        let advertised = self
            .modes()
            .is_some_and(|modes| modes.available_modes.iter().any(|mode| mode.id == mode_id));
        if !advertised {
            return Err(AcpError::Unsupported(format!(
                "the session does not offer the mode '{mode_id}'"
            )));
        }
        self.connection
            .request(SetSessionModeRequest::new(self.session_id.clone(), mode_id.clone()))
            .await?;
        if let Ok(mut view) = self.view.lock()
            && let Some(modes) = &mut view.modes
        {
            modes.current_mode_id = mode_id;
        }
        Ok(())
    }

    /// Set a configuration option (`session/set_config_option`) and return the
    /// full option set the agent reports afterwards.
    pub async fn set_config_option(
        &mut self,
        config_id: impl Into<SessionConfigId>,
        value: impl Into<SessionConfigOptionValue>,
    ) -> Result<Vec<SessionConfigOption>> {
        self.ensure_open()?;
        let response = self
            .connection
            .request(SetSessionConfigOptionRequest::new(self.session_id.clone(), config_id, value))
            .await?;
        if let Ok(mut view) = self.view.lock() {
            view.config_options = Some(response.config_options.clone());
        }
        Ok(response.config_options)
    }

    /// Rename the session (`session/set_title`) and return the title the agent
    /// stored.
    pub async fn set_title(&mut self, title: impl Into<String>) -> Result<String> {
        self.ensure_open()?;
        self.connection.require(
            self.connection.capabilities().session_capabilities.title.is_some(),
            "session/set_title",
        )?;
        let response = self
            .connection
            .request(SetSessionTitleRequest::new(self.session_id.clone(), title))
            .await?;
        if let Ok(mut view) = self.view.lock() {
            view.title = Some(response.title.clone());
        }
        Ok(response.title)
    }

    /// Whether the agent holds this session open or only saved (`session/status`).
    pub async fn status(&self) -> Result<SessionStatus> {
        self.connection.session_status(self.session_id.clone()).await
    }

    /// Fork this session into a new one on the same connection (`session/fork`).
    pub async fn fork(&self) -> Result<AcpSession> {
        self.ensure_open()?;
        self.connection.fork_session(self.session_id.clone()).await
    }

    /// Archive this session (`session/archive`).
    pub async fn archive(&self) -> Result<()> {
        self.connection.archive_session(self.session_id.clone()).await
    }

    /// Restore this session from the archive (`session/unarchive`).
    pub async fn unarchive(&self) -> Result<()> {
        self.connection.unarchive_session(self.session_id.clone()).await
    }

    /// The session's id.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// The connection the session runs on.
    pub fn connection(&self) -> &AcpConnection {
        &self.connection
    }

    /// What the agent reported in `initialize`, including the negotiated
    /// protocol version.
    pub fn agent_info(&self) -> &AcpAgentInfo {
        self.connection.agent_info()
    }

    /// The session's modes and the current one, when the agent offers modes.
    pub fn modes(&self) -> Option<SessionModeState> {
        self.view.lock().ok().and_then(|view| view.modes.clone())
    }

    /// The session's configuration options with their current values.
    pub fn config_options(&self) -> Option<Vec<SessionConfigOption>> {
        self.view.lock().ok().and_then(|view| view.config_options.clone())
    }

    /// The slash commands the agent last advertised for the session.
    pub fn available_commands(&self) -> Vec<AvailableCommand> {
        self.view.lock().map(|view| view.available_commands.clone()).unwrap_or_default()
    }

    /// The session's title, from `session/set_title` or the agent's last
    /// session-info update.
    pub fn title(&self) -> Option<String> {
        self.view.lock().ok().and_then(|view| view.title.clone())
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
        !self.closed && !self.connection.is_closed()
    }

    /// Get the working directory for this session.
    pub fn working_dir(&self) -> &PathBuf {
        &self.connection.config().working_dir
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed {
            Err(AcpError::ConnectionLost("session already closed".into()))
        } else if self.connection.is_closed() {
            Err(AcpError::ConnectionLost("agent process exited".into()))
        } else {
            Ok(())
        }
    }
}

impl Drop for AcpSession {
    fn drop(&mut self) {
        if !self.closed {
            self.connection.inner.routes.remove(&self.session_id);
            if self.owns_connection {
                warn!("AcpSession dropped without explicit close — stopping the agent process");
            }
        }
    }
}
