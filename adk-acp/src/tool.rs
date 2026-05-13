//! ACP agent wrapped as an ADK Tool.
//!
//! [`AcpAgentTool`] allows an ADK agent to delegate tasks to an external ACP agent
//! (Claude Code, Codex, etc.) by sending prompts and receiving responses.

use std::sync::Arc;

use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::debug;

use crate::connection::{AcpAgentConfig, prompt_agent};

/// An external ACP agent exposed as an ADK Tool.
///
/// When invoked, spawns the ACP agent process, sends the `prompt` field from
/// the tool arguments, and returns the agent's text response.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::AcpAgentTool;
/// use adk_agent::LlmAgentBuilder;
/// use std::sync::Arc;
///
/// let claude = AcpAgentTool::new("claude-code")
///     .description("Delegate complex coding tasks to Claude Code");
///
/// let agent = LlmAgentBuilder::new("orchestrator")
///     .tool(Arc::new(claude))
///     .build()?;
/// ```
pub struct AcpAgentTool {
    name: String,
    description: String,
    config: AcpAgentConfig,
}

impl AcpAgentTool {
    /// Create a new ACP agent tool from a command string.
    ///
    /// The command is used to spawn the ACP agent process on each invocation.
    pub fn new(command: impl Into<String>) -> Self {
        let command = command.into();
        let name = command.split_whitespace().next().unwrap_or("acp-agent").to_string();

        Self {
            name: name.clone(),
            description: format!("Delegate tasks to the {name} ACP agent"),
            config: AcpAgentConfig::new(&command),
        }
    }

    /// Set a custom tool name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the tool description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Set the working directory for the ACP agent.
    pub fn working_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.config.working_dir = path.into();
        self
    }

    /// Set whether to auto-approve permission requests from the agent.
    pub fn auto_approve(mut self, approve: bool) -> Self {
        self.config.auto_approve = approve;
        self
    }
}

impl std::fmt::Debug for AcpAgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpAgentTool")
            .field("name", &self.name)
            .field("command", &self.config.command)
            .finish()
    }
}

#[async_trait]
impl Tool for AcpAgentTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task or question to send to the ACP agent"
                }
            },
            "required": ["prompt"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::tool("AcpAgentTool requires a 'prompt' string field"))?;

        debug!(tool = %self.name, prompt_len = prompt.len(), "invoking ACP agent");

        let response = prompt_agent(&self.config, prompt)
            .await
            .map_err(|e| AdkError::tool(format!("ACP agent error: {e}")))?;

        Ok(json!({ "response": response }))
    }
}
