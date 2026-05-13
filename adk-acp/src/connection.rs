//! ACP connection management.
//!
//! Wraps the `agent-client-protocol` SDK to manage the lifecycle of a connection
//! to an external ACP agent process.

use std::path::PathBuf;
use std::str::FromStr;

use agent_client_protocol::schema::{
    InitializeRequest, ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome,
};
use agent_client_protocol::{Agent, Client, ConnectionTo};
use agent_client_protocol_tokio::AcpAgent;
use tracing::{debug, info};

use crate::error::{AcpError, Result};

/// Configuration for connecting to an ACP agent.
#[derive(Debug, Clone)]
pub struct AcpAgentConfig {
    /// Command to spawn the agent (e.g., "claude-code" or "codex --model o3").
    pub command: String,
    /// Working directory for the agent session.
    pub working_dir: PathBuf,
    /// Whether to auto-approve permission requests (YOLO mode).
    pub auto_approve: bool,
}

impl AcpAgentConfig {
    /// Create a new config with a command string.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = AcpAgentConfig::new("claude-code")
    ///     .working_dir("/path/to/project");
    /// ```
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            auto_approve: true,
        }
    }

    /// Set the working directory.
    pub fn working_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.working_dir = path.into();
        self
    }

    /// Set whether to auto-approve permission requests.
    ///
    /// Default is `true` (YOLO mode). Set to `false` to reject all permission requests.
    pub fn auto_approve(mut self, approve: bool) -> Self {
        self.auto_approve = approve;
        self
    }
}

/// Send a single prompt to an ACP agent and return the response text.
///
/// This spawns the agent process, initializes the connection, creates a session,
/// sends the prompt, and collects the streamed response. The agent process is
/// terminated when the connection completes.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::connection::{AcpAgentConfig, prompt_agent};
///
/// let config = AcpAgentConfig::new("claude-code")
///     .working_dir("/path/to/project");
///
/// let response = prompt_agent(&config, "Explain this function").await?;
/// println!("{response}");
/// ```
pub async fn prompt_agent(config: &AcpAgentConfig, prompt: &str) -> Result<String> {
    info!(command = %config.command, cwd = %config.working_dir.display(), "spawning ACP agent");

    let agent = AcpAgent::from_str(&config.command).map_err(|e| {
        AcpError::InvalidConfig(format!("invalid command '{}': {e}", config.command))
    })?;

    let prompt_text = prompt.to_string();
    let working_dir = config.working_dir.clone();
    let auto_approve = config.auto_approve;

    let result: std::result::Result<String, agent_client_protocol::Error> = Client
        .builder()
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx: ConnectionTo<Agent>| {
                if auto_approve {
                    debug!("auto-approving ACP permission request");
                    let option_id = request.options.first().map(|opt| opt.option_id.clone());
                    if let Some(id) = option_id {
                        responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
                        ))
                    } else {
                        responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Cancelled,
                        ))
                    }
                } else {
                    debug!("rejecting ACP permission request (auto_approve=false)");
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
            // Initialize
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;

            // Create session, send prompt, and collect response
            let response_text = connection
                .build_session(&working_dir)
                .block_task()
                .run_until(async |mut session| {
                    session.send_prompt(&prompt_text)?;
                    let text = session.read_to_string().await?;
                    Ok(text)
                })
                .await?;

            Ok(response_text)
        })
        .await;

    result.map_err(|e| AcpError::Protocol(e.to_string()))
}
