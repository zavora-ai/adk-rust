//! ACP connection management.
//!
//! Wraps the `agent-client-protocol` SDK to manage the lifecycle of a connection
//! to an external ACP agent process.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use adk_core::Content;
use agent_client_protocol::AcpAgent;
use agent_client_protocol::v1::schema::{
    ContentBlock, Implementation, InitialSessionConfigOption, McpServer, SessionConfigId,
    SessionConfigOptionValue, SystemPrompt, SystemPromptMode,
};
use tracing::info;

use crate::client::AcpConnection;
use crate::content::content_to_blocks;
use crate::elicitation::AcpElicitation;
use crate::error::{AcpError, Result};
use crate::host::{AcpFileSystem, AcpTerminal};
use crate::permissions::PermissionPolicy;

/// Configuration for connecting to an ACP agent.
#[derive(Clone)]
#[non_exhaustive]
pub struct AcpAgentConfig {
    /// Command to spawn the agent (e.g., "claude-code" or "codex --model o3").
    ///
    /// The string is split into words with POSIX shell rules, so a backslash escapes the next
    /// character. Single-quote an argument that contains backslashes, such as a Windows path:
    /// `python3 'C:\agents\agent.py'`.
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
    /// Directories outside [`working_dir`](Self::working_dir) that sessions may
    /// use, sent as `additionalDirectories` when the agent advertises them.
    pub additional_directories: Vec<PathBuf>,
    /// System-level instructions sent with each session request.
    ///
    /// ACP lets a client send one only when the agent advertises
    /// `sessionCapabilities.systemPrompt`, and `override` only when it
    /// advertises that mode; otherwise starting a session fails with
    /// [`AcpError::Unsupported`].
    pub system_prompt: Option<SystemPrompt>,
    /// Initial configuration values sent with `session/new`, `session/load`
    /// and `session/resume` when the agent advertises `session/options`.
    pub initial_config_options: Vec<InitialSessionConfigOption>,
    /// Authentication method to use right after `initialize`, from the
    /// agent's advertised `authMethods`.
    pub auth_method: Option<String>,
    /// Handler for the agent's `elicitation/create` requests. Elicitation is
    /// advertised only when one is set.
    pub elicitation: Option<Arc<dyn AcpElicitation>>,
    /// The client name and version sent as `clientInfo`.
    pub client_info: Implementation,
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
            .field("additional_directories", &self.additional_directories)
            .field("system_prompt", &self.system_prompt.as_ref().map(|prompt| &prompt.mode))
            .field("initial_config_options", &self.initial_config_options.len())
            .field("auth_method", &self.auth_method)
            .field("elicitation_enabled", &self.elicitation.is_some())
            .field("client_info", &self.client_info)
            .finish()
    }
}

impl AcpAgentConfig {
    /// Create a new config with a command string, split as described on [`Self::command`].
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            auto_approve: false,
            env: std::collections::HashMap::new(),
            mcp_servers: Vec::new(),
            filesystem: None,
            terminal: None,
            additional_directories: Vec::new(),
            system_prompt: None,
            initial_config_options: Vec::new(),
            auth_method: None,
            elicitation: None,
            client_info: Implementation::new("adk-rust", env!("CARGO_PKG_VERSION"))
                .title("ADK-Rust".to_string()),
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

    /// Let sessions use a directory outside the working directory.
    pub fn additional_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.additional_directories.push(path.into());
        self
    }

    /// Add `text` to the agent's own system instructions for every session
    /// (`systemPrompt` in `append` mode).
    pub fn append_system_prompt(mut self, text: impl Into<String>) -> Self {
        self.system_prompt = Some(SystemPrompt::new(vec![ContentBlock::from(text.into())]));
        self
    }

    /// Replace the agent's own system instructions with `text` for every
    /// session (`systemPrompt` in `override` mode).
    ///
    /// The client takes over any tool guidance or safety instructions the
    /// agent's defaults provided.
    pub fn override_system_prompt(mut self, text: impl Into<String>) -> Self {
        self.system_prompt = Some(
            SystemPrompt::new(vec![ContentBlock::from(text.into())])
                .mode(SystemPromptMode::Override),
        );
        self
    }

    /// Send a fully built system prompt with every session request.
    pub fn system_prompt(mut self, prompt: SystemPrompt) -> Self {
        self.system_prompt = Some(prompt);
        self
    }

    /// Start every session with a configuration option set to `value`.
    pub fn initial_config_option(
        mut self,
        config_id: impl Into<SessionConfigId>,
        value: impl Into<SessionConfigOptionValue>,
    ) -> Self {
        self.initial_config_options.push(InitialSessionConfigOption::new(config_id, value));
        self
    }

    /// Authenticate with this method from the agent's `authMethods` right
    /// after `initialize`.
    pub fn auth_method(mut self, method_id: impl Into<String>) -> Self {
        self.auth_method = Some(method_id.into());
        self
    }

    /// Answer the agent's `elicitation/create` requests with `handler`.
    pub fn elicitation(mut self, handler: Arc<dyn AcpElicitation>) -> Self {
        self.elicitation = Some(handler);
        self
    }

    /// Set the client name and version sent as `clientInfo`.
    pub fn client_info(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.client_info = Implementation::new(name, version);
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
    one_shot(config, policy, vec![ContentBlock::from(prompt.to_string())]).await
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
    one_shot(config, policy, blocks).await
}

/// Start the agent, prompt one new session, and stop the agent.
async fn one_shot(
    config: &AcpAgentConfig,
    policy: Arc<PermissionPolicy>,
    blocks: Vec<ContentBlock>,
) -> Result<String> {
    let connection = AcpConnection::connect(config.clone(), policy).await?;
    let result = async {
        let mut session = connection.new_session().await?;
        let result = session.prompt_blocks(blocks, |_| {}).await;
        session.close().await?;
        result
    }
    .await;
    let closed = connection.close().await;
    let reply = result?;
    closed?;
    Ok(reply.text)
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
    use agent_client_protocol::v1::schema::{EnvVariable, McpServerStdio};

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

    #[test]
    fn a_single_quoted_windows_path_keeps_its_backslashes() {
        let agent = build_agent(&AcpAgentConfig::new(r"python3 'D:\a\agent.py' normal")).unwrap();
        assert_eq!(agent.config().arguments(), [r"D:\a\agent.py", "normal"]);
    }
}
