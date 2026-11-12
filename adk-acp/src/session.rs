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
    AvailableCommand, CloseSessionRequest, ContentBlock, ContentChunk, PromptRequest,
    SessionConfigId, SessionConfigOption, SessionConfigOptionValue, SessionId, SessionModeId,
    SessionModeState, SessionStatus, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionModeRequest, SetSessionTitleRequest, StopReason, Usage,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::client::{AcpAgentInfo, AcpConnection, RoutedUpdate, SdkConnection};
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
    /// The session's protocol v2 foreground-state updates, which end its turns.
    #[cfg(feature = "unstable-protocol-v2")]
    states: Option<mpsc::UnboundedReceiver<agent_client_protocol::v2::schema::StateUpdate>>,
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
        self.connection.send_cancel(&self.session_id)
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
            #[cfg(feature = "unstable-protocol-v2")]
            states: None,
        }
    }

    /// Route the session's protocol v2 foreground-state updates to it.
    #[cfg(feature = "unstable-protocol-v2")]
    pub(crate) fn with_states(
        mut self,
        states: mpsc::UnboundedReceiver<agent_client_protocol::v2::schema::StateUpdate>,
    ) -> Self {
        self.states = Some(states);
        self
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

        let connection = match &self.connection.inner.connection {
            SdkConnection::V1(connection) => connection.clone(),
            #[cfg(feature = "unstable-protocol-v2")]
            SdkConnection::V2(connection) => {
                let connection = connection.clone();
                return self.prompt_v2(connection, blocks, &mut on_chunk, start).await;
            }
        };
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
        let result = if self.connection.is_closed() { Ok(()) } else { self.close_on_agent().await };
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

    /// Close the session on the agent: `session/close` where the agent
    /// supports it, otherwise a `session/cancel` of its running work.
    async fn close_on_agent(&self) -> Result<()> {
        #[cfg(feature = "unstable-protocol-v2")]
        if let SdkConnection::V2(connection) = &self.connection.inner.connection {
            return crate::client_v2::close_session(&self.connection, connection, &self.session_id)
                .await;
        }
        if self.connection.capabilities().session_capabilities.close.is_some() {
            self.connection
                .request(CloseSessionRequest::new(self.session_id.clone()))
                .await
                .map(drop)
        } else {
            self.connection.send_cancel(&self.session_id)
        }
    }

    /// One protocol v2 turn. The prompt's response only acknowledges that the
    /// agent inserted it; the turn ends with the `idle` state update that
    /// follows the `running` one, which carries the stop reason and usage.
    #[cfg(feature = "unstable-protocol-v2")]
    async fn prompt_v2(
        &mut self,
        connection: agent_client_protocol::v2::ConnectionTo<agent_client_protocol::Agent>,
        blocks: Vec<ContentBlock>,
        on_chunk: &mut (impl FnMut(OutputChunk) + Send),
        start: Instant,
    ) -> Result<PromptResult> {
        use agent_client_protocol::v2::schema as v2;

        let Some(prompt) = crate::protocol_v2::blocks_to_v2(&blocks) else {
            return Err(AcpError::InvalidConfig(
                "prompt content has no ACP protocol v2 form".into(),
            ));
        };
        let mut states = self.states.take().ok_or_else(|| {
            AcpError::Protocol("the session has no protocol v2 state route".into())
        })?;
        // States queued before this prompt describe earlier work.
        while states.try_recv().is_ok() {}

        let sent = connection
            .send_request(v2::PromptRequest::new(self.session_id.0.to_string(), prompt))
            .block_task();
        tokio::pin!(sent);
        let mut text = String::new();
        let mut accepted = false;
        let mut started = false;
        let mut idle: Option<v2::IdleStateUpdate> = None;
        let outcome = loop {
            if accepted && idle.is_some() {
                break Ok(());
            }
            tokio::select! {
                biased;
                Some(update) = self.updates.recv() => {
                    self.consume(update, &mut text, on_chunk);
                }
                Some(state) = states.recv() => match state {
                    // An idle snapshot without a stop reason that precedes this
                    // turn's work, such as a new session's, ends nothing.
                    v2::StateUpdate::Idle(update) if started || update.stop_reason.is_some() => {
                        idle = Some(update);
                    }
                    v2::StateUpdate::Idle(_) => {}
                    // Running, requires-action and unknown states are all busy.
                    _ => started = true,
                },
                response = &mut sent, if !accepted => match response {
                    Ok(_) => accepted = true,
                    Err(error) => break Err(self.connection.error(error)),
                },
                else => break Err(AcpError::ConnectionLost(
                    "the agent closed the connection during the prompt".into(),
                )),
            }
        };
        self.states = Some(states);
        outcome?;
        // The idle update follows every update of the turn, so the updates still
        // queued belong to it.
        while let Ok(update) = self.updates.try_recv() {
            self.consume(update, &mut text, on_chunk);
        }

        let idle = idle.unwrap_or_default();
        let stop_reason = match &idle.stop_reason {
            Some(v2::StopReason::Error(failure)) => {
                let message = failure
                    .error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_else(|| "the agent reported an error".into());
                return Err(AcpError::Protocol(message));
            }
            Some(v2::StopReason::Cancelled) => {
                return Err(AcpError::ConnectionLost("prompt cancelled".into()));
            }
            Some(reason) => {
                crate::protocol_v2::stop_reason_to_v1(reason).unwrap_or(StopReason::EndTurn)
            }
            None => StopReason::EndTurn,
        };
        Ok(PromptResult {
            text,
            duration: start.elapsed(),
            prompt_count: self.prompt_count,
            stop_reason,
            usage: idle.usage.as_deref().map(crate::protocol_v2::usage_to_v1),
        })
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
        #[cfg(feature = "unstable-protocol-v2")]
        if let SdkConnection::V2(connection) = &self.connection.inner.connection {
            let options = crate::client_v2::set_config_option(
                &self.connection,
                connection,
                &self.session_id,
                config_id.into(),
                value.into(),
            )
            .await?;
            if let Ok(mut view) = self.view.lock() {
                view.config_options = Some(options.clone());
            }
            return Ok(options);
        }
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
