//! ACP connection management.
//!
//! Wraps the `agent-client-protocol` SDK to manage the lifecycle of a connection
//! to an external ACP agent process.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use adk_core::Content;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, McpServer,
    NewSessionRequest, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionNotification, SessionUpdate,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo, Responder, SessionMessage};
use tracing::{debug, info};

use crate::content::content_to_blocks;
use crate::error::{AcpError, Result};
use crate::host::{AcpFileSystem, AcpHostHandler, AcpTerminal, capabilities};
use crate::permissions::{PermissionPolicy, PermissionRequest, outcome_for};

/// Configuration for connecting to an ACP agent.
#[derive(Clone)]
pub struct AcpAgentConfig {
    /// Command to spawn the agent (e.g., "claude-code" or "codex --model o3").
    pub command: String,
    /// Working directory for the agent session.
    pub working_dir: PathBuf,
    /// Whether to auto-approve permission requests (YOLO mode).
    /// Used by `prompt_agent()`. For fine-grained control, use `prompt_agent_with_policy()`.
    pub auto_approve: bool,
    /// Environment variables to inject when spawning the agent process.
    /// These are merged with the current process environment (these take precedence).
    pub env: std::collections::HashMap<String, String>,
    /// MCP servers made available to the external agent for each new session.
    pub mcp_servers: Vec<McpServer>,
    /// Optional file host exposed to the external ACP agent.
    pub filesystem: Option<Arc<dyn AcpFileSystem>>,
    /// Optional terminal host exposed to the external ACP agent.
    pub terminal: Option<Arc<dyn AcpTerminal>>,
}

impl std::fmt::Debug for AcpAgentConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let environment_keys: Vec<&str> = self.env.keys().map(String::as_str).collect();
        let mcp_servers: Vec<(&str, &'static str)> = self
            .mcp_servers
            .iter()
            .map(|server| match server {
                McpServer::Stdio(config) => (config.name.as_str(), "stdio"),
                McpServer::Http(config) => (config.name.as_str(), "http"),
                McpServer::Sse(config) => (config.name.as_str(), "sse"),
                _ => ("unknown", "unsupported"),
            })
            .collect();
        formatter
            .debug_struct("AcpAgentConfig")
            .field("command", &self.command)
            .field("working_dir", &self.working_dir)
            .field("auto_approve", &self.auto_approve)
            .field("environment_keys", &environment_keys)
            .field("mcp_servers", &mcp_servers)
            .field("filesystem_enabled", &self.filesystem.is_some())
            .field("terminal_enabled", &self.terminal.is_some())
            .finish()
    }
}

impl AcpAgentConfig {
    /// Create a new config with a command string.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            auto_approve: false,
            env: std::collections::HashMap::new(),
            mcp_servers: Vec::new(),
            filesystem: None,
            terminal: None,
        }
    }

    /// Set the working directory.
    pub fn working_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.working_dir = path.into();
        self
    }

    /// Set whether to auto-approve permission requests.
    pub fn auto_approve(mut self, approve: bool) -> Self {
        self.auto_approve = approve;
        self
    }

    /// Add an environment variable to inject when spawning the agent.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set multiple environment variables at once.
    pub fn envs(
        mut self,
        vars: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        for (k, v) in vars {
            self.env.insert(k.into(), v.into());
        }
        self
    }

    /// Add an MCP server to every session created with this configuration.
    ///
    /// ACP v1 requires agents to support stdio MCP servers. HTTP and SSE
    /// entries should only be used when the remote agent advertises them.
    pub fn mcp_server(mut self, server: McpServer) -> Self {
        self.mcp_servers.push(server);
        self
    }

    /// Set all MCP servers supplied during ACP session creation.
    pub fn mcp_servers(mut self, servers: Vec<McpServer>) -> Self {
        self.mcp_servers = servers;
        self
    }

    /// Expose application-controlled file callbacks to the ACP agent.
    pub fn filesystem(mut self, filesystem: Arc<dyn AcpFileSystem>) -> Self {
        self.filesystem = Some(filesystem);
        self
    }

    /// Expose a complete application-controlled terminal lifecycle to the ACP agent.
    pub fn terminal(mut self, terminal: Arc<dyn AcpTerminal>) -> Self {
        self.terminal = Some(terminal);
        self
    }
}

/// Send a single prompt to an ACP agent and return the response text.
///
/// Uses simple auto-approve/deny based on `config.auto_approve`.
/// For fine-grained permission control, use [`prompt_agent_with_policy`].
pub async fn prompt_agent(config: &AcpAgentConfig, prompt: &str) -> Result<String> {
    let policy =
        if config.auto_approve { PermissionPolicy::AutoApprove } else { PermissionPolicy::DenyAll };
    prompt_agent_with_policy(config, prompt, Arc::new(policy)).await
}

/// Send a single prompt to an ACP agent with a custom permission policy.
///
/// The policy is invoked for each `session/request_permission` message from the agent.
/// This enables HITL (human-in-the-loop) control over sensitive operations.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::{AcpAgentConfig, PermissionPolicy, PermissionDecision};
/// use adk_acp::connection::prompt_agent_with_policy;
/// use std::sync::Arc;
///
/// let config = AcpAgentConfig::new("kiro-cli acp");
/// let policy = Arc::new(PermissionPolicy::Custom(Box::new(|req| {
///     if req.title.contains("delete") {
///         PermissionDecision::deny()
///     } else {
///         PermissionDecision::allow_once()
///     }
/// })));
///
/// let response = prompt_agent_with_policy(&config, "Refactor main.rs", policy).await?;
/// ```
pub async fn prompt_agent_with_policy(
    config: &AcpAgentConfig,
    prompt: &str,
    policy: Arc<PermissionPolicy>,
) -> Result<String> {
    info!(command = %config.command, cwd = %config.working_dir.display(), "spawning ACP agent");

    let agent = build_agent(config)?;

    let prompt_text = prompt.to_string();
    let working_dir = config.working_dir.clone();
    let mcp_servers = config.mcp_servers.clone();
    let filesystem = config.filesystem.clone();
    let terminal = config.terminal.clone();
    let client_capabilities = capabilities(filesystem.as_ref(), terminal.as_ref());
    let policy_clone = policy.clone();

    let result: std::result::Result<String, agent_client_protocol::Error> = Client
        .builder()
        .on_receive_request(
            move |request: RequestPermissionRequest,
                  responder: Responder<RequestPermissionResponse>,
                  cx: ConnectionTo<Agent>| {
                let policy = policy_clone.clone();
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
            validate_initialization(&initialization, &mcp_servers)?;

            // Create session, send prompt, and collect response
            let response_text = connection
                .build_session_from(
                    NewSessionRequest::new(&working_dir).mcp_servers(mcp_servers),
                )
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

/// Send a single multimodal prompt to an ACP agent and return the response text.
///
/// Unlike [`prompt_agent_with_policy`], which sends a single text block, this
/// transmits every representable [`Part`](adk_core::Part) of the supplied
/// [`Content`] as its matching ACP [`ContentBlock`] (text, image, audio, and
/// embedded resources) via [`content_to_blocks`]. Parts with no ACP
/// representation are skipped rather than dropping the whole prompt, and text is
/// always preserved.
///
/// The agent's streamed text response is collected and returned, mirroring
/// [`prompt_agent_with_policy`]'s return contract.
///
/// # Errors
///
/// Returns [`AcpError`] if the content maps to no transmittable blocks, if the
/// handshake fails, or if the agent reports a protocol error.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::{AcpAgentConfig, PermissionPolicy};
/// use adk_acp::connection::prompt_agent_content_with_policy;
/// use adk_core::{Content, Part};
/// use std::sync::Arc;
///
/// let mut content = Content::new("user");
/// content.parts.push(Part::Text { text: "What is in this image?".into() });
/// content.parts.push(Part::inline_data("image/png", png_bytes));
///
/// let config = AcpAgentConfig::new("my-coding-agent --acp");
/// let response = prompt_agent_content_with_policy(
///     &config,
///     &content,
///     Arc::new(PermissionPolicy::DenyAll),
/// ).await?;
/// ```
pub async fn prompt_agent_content_with_policy(
    config: &AcpAgentConfig,
    content: &Content,
    policy: Arc<PermissionPolicy>,
) -> Result<String> {
    let blocks = content_to_blocks(content);
    if blocks.is_empty() {
        return Err(AcpError::InvalidConfig(
            "prompt content contained no transmittable blocks".into(),
        ));
    }

    info!(
        command = %config.command,
        cwd = %config.working_dir.display(),
        block_count = blocks.len(),
        "spawning ACP agent for multimodal prompt"
    );

    let agent = build_agent(config)?;

    let working_dir = config.working_dir.clone();
    let mcp_servers = config.mcp_servers.clone();
    let filesystem = config.filesystem.clone();
    let terminal = config.terminal.clone();
    let client_capabilities = capabilities(filesystem.as_ref(), terminal.as_ref());
    let policy_clone = policy.clone();

    let result: std::result::Result<String, agent_client_protocol::Error> = Client
        .builder()
        .on_receive_request(
            move |request: RequestPermissionRequest,
                  responder: Responder<RequestPermissionResponse>,
                  cx: ConnectionTo<Agent>| {
                let policy = policy_clone.clone();
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
            },
            agent_client_protocol::on_receive_request!(),
        )
        .with_handler(AcpHostHandler::new(filesystem, terminal))
        .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
            let initialization = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(client_capabilities),
                )
                .block_task()
                .await?;
            validate_initialization(&initialization, &mcp_servers)?;

            let response_text = connection
                .build_session_from(
                    NewSessionRequest::new(&working_dir).mcp_servers(mcp_servers),
                )
                .block_task()
                .run_until(async move |mut session| collect_prompt_blocks(&mut session, blocks).await)
                .await?;

            Ok(response_text)
        })
        .await;

    result.map_err(|e| AcpError::Protocol(e.to_string()))
}

/// Send a prompt built from explicit content blocks and collect the agent's
/// streamed text response.
///
/// The SDK's [`send_prompt`](agent_client_protocol::ActiveSession::send_prompt)
/// convenience only carries a single text block, so multimodal prompts send the
/// [`PromptRequest`] directly over the session connection. Agent message chunks
/// are drained from the session update channel (a biased select keeps the
/// channel ahead of the prompt future so no buffered chunk is missed) until the
/// prompt turn completes.
async fn collect_prompt_blocks(
    session: &mut agent_client_protocol::ActiveSession<'_, Agent>,
    blocks: Vec<ContentBlock>,
) -> std::result::Result<String, agent_client_protocol::Error> {
    let connection = session.connection();
    let session_id = session.session_id().clone();
    let prompt_future =
        connection.send_request(PromptRequest::new(session_id, blocks)).block_task();
    tokio::pin!(prompt_future);

    let mut text = String::new();
    loop {
        tokio::select! {
            biased;
            update = session.read_update() => {
                match update? {
                    SessionMessage::SessionMessage(dispatch) => {
                        MatchDispatch::new(dispatch)
                            .if_notification(async |notification: SessionNotification| {
                                if let SessionUpdate::AgentMessageChunk(ContentChunk {
                                    content: ContentBlock::Text(text_content),
                                    ..
                                }) = notification.update
                                {
                                    text.push_str(&text_content.text);
                                }
                                Ok(())
                            })
                            .await
                            .otherwise_ignore()?;
                    }
                    SessionMessage::StopReason(_) => break,
                    _ => {}
                }
            }
            result = &mut prompt_future => {
                result?;
                break;
            }
        }
    }
    Ok(text)
}

pub(crate) fn validate_initialization(
    response: &InitializeResponse,
    mcp_servers: &[McpServer],
) -> std::result::Result<(), agent_client_protocol::Error> {
    if response.protocol_version != ProtocolVersion::V1 {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("the ACP agent did not negotiate protocol version 1"));
    }
    for server in mcp_servers {
        match server {
            McpServer::Stdio(_) => {}
            McpServer::Http(_) if response.agent_capabilities.mcp_capabilities.http => {}
            McpServer::Sse(_) if response.agent_capabilities.mcp_capabilities.sse => {}
            McpServer::Http(_) => {
                return Err(agent_client_protocol::Error::invalid_params()
                    .data("the ACP agent did not advertise HTTP MCP support"));
            }
            McpServer::Sse(_) => {
                return Err(agent_client_protocol::Error::invalid_params()
                    .data("the ACP agent did not advertise SSE MCP support"));
            }
            _ => {
                return Err(agent_client_protocol::Error::invalid_params()
                    .data("the ACP agent did not advertise this MCP transport"));
            }
        }
    }
    Ok(())
}

/// Build an SDK process component while preserving environment values exactly.
///
/// `AcpAgentConfig` here is this crate's type; the SDK has an unrelated type of the same name.
pub(crate) fn build_agent(config: &AcpAgentConfig) -> Result<AcpAgent> {
    let parsed = AcpAgent::from_str(&config.command).map_err(|error| {
        AcpError::InvalidConfig(format!("invalid command '{}': {error}", config.command))
    })?;
    // Applied after parsing so a configured variable overrides one from the command string.
    let spawn = parsed
        .into_config()
        .envs(config.env.iter().map(|(name, value)| (name.clone(), value.clone())));
    Ok(AcpAgent::new(spawn))
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{EnvVariable, McpServerStdio};

    use super::*;

    #[test]
    fn debug_output_redacts_environment_values() {
        let config = AcpAgentConfig::new("agent --acp")
            .env("API_TOKEN", "top-secret-agent-token")
            .mcp_server(McpServer::Stdio(
                McpServerStdio::new("private-tools", "/bin/echo")
                    .env(vec![EnvVariable::new("MCP_SECRET", "top-secret-mcp-token")]),
            ));

        let debug = format!("{config:?}");
        assert!(debug.contains("API_TOKEN"));
        assert!(debug.contains("private-tools"));
        assert!(!debug.contains("top-secret-agent-token"));
        assert!(!debug.contains("top-secret-mcp-token"));
    }
}
