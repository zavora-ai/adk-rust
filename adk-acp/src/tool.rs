//! ACP agent wrapped as an ADK Tool.
//!
//! [`AcpAgentTool`] allows an ADK agent to delegate tasks to an external ACP agent
//! by sending typed ACP prompts and receiving streamed responses.

use std::sync::Arc;
use std::time::Instant;

use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use crate::connection::{AcpAgentConfig, prompt_agent_with_policy};
use crate::permissions::PermissionPolicy;
use crate::usage::{AcpUsage, UsageTracker};

/// An external ACP agent exposed as an ADK Tool.
///
/// When invoked, spawns the ACP agent process, sends the `prompt` field from
/// the tool arguments, and returns the agent's text response.
///
/// # Features
///
/// - **Permission control**: Configure how tool permission requests are handled
/// - **Usage tracking**: Monitor invocation counts, response sizes, and latency
/// - **Telemetry**: All calls emit tracing spans for observability
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::{AcpAgentTool, PermissionPolicy, UsageTracker};
/// use adk_agent::LlmAgentBuilder;
/// use std::sync::Arc;
///
/// let tracker = UsageTracker::new();
///
/// let coding_agent = AcpAgentTool::new("my-coding-agent --acp")
///     .description("Delegate repository work to an ACP coding agent")
///     .permission_policy(PermissionPolicy::Custom(Box::new(|req| {
///         if req.title.contains("delete") {
///             adk_acp::PermissionDecision::deny()
///         } else {
///             adk_acp::PermissionDecision::allow_once()
///         }
///     })))
///     .usage_tracker(tracker.clone());
///
/// let agent = LlmAgentBuilder::new("orchestrator")
///     .tool(Arc::new(coding_agent))
///     .build()?;
///
/// // After some invocations:
/// let stats = tracker.stats();
/// println!("ACP calls: {}, avg latency: {:?}",
///     stats.total_calls,
///     stats.total_duration / stats.total_calls.max(1) as u32);
/// ```
pub struct AcpAgentTool {
    name: String,
    description: String,
    config: AcpAgentConfig,
    permission_policy: Arc<PermissionPolicy>,
    usage_tracker: Option<UsageTracker>,
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
            permission_policy: Arc::new(PermissionPolicy::DenyAll),
            usage_tracker: None,
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

    /// Set the permission policy for handling agent tool requests.
    ///
    /// Default is `PermissionPolicy::DenyAll`. Choose `AutoApprove` only for a
    /// trusted development environment, or use `Custom(...)` for fine-grained
    /// control.
    pub fn permission_policy(mut self, policy: PermissionPolicy) -> Self {
        self.permission_policy = Arc::new(policy);
        // Also update the config's auto_approve flag for the connection layer
        self.config.auto_approve = matches!(*self.permission_policy, PermissionPolicy::AutoApprove);
        self
    }

    /// Attach a usage tracker to record invocation metrics.
    pub fn usage_tracker(mut self, tracker: UsageTracker) -> Self {
        self.usage_tracker = Some(tracker);
        self
    }
}

impl std::fmt::Debug for AcpAgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpAgentTool")
            .field("name", &self.name)
            .field("command", &self.config.command)
            .field("permission_policy", &self.permission_policy)
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

        info!(
            tool = %self.name,
            prompt_len = prompt.len(),
            cwd = %self.config.working_dir.display(),
            "invoking ACP agent"
        );

        let start = Instant::now();

        let result =
            prompt_agent_with_policy(&self.config, prompt, self.permission_policy.clone()).await;
        let duration = start.elapsed();

        match &result {
            Ok(response) => {
                debug!(
                    tool = %self.name,
                    response_len = response.len(),
                    duration_ms = duration.as_millis() as u64,
                    "ACP agent responded"
                );

                if let Some(tracker) = &self.usage_tracker {
                    tracker.record(&AcpUsage {
                        tool_name: self.name.clone(),
                        prompt_chars: prompt.len(),
                        response_chars: response.len(),
                        duration,
                        success: true,
                        permission_requests: 0,
                        permissions_denied: 0,
                    });
                }

                Ok(json!({ "response": response }))
            }
            Err(e) => {
                warn!(
                    tool = %self.name,
                    error = %e,
                    duration_ms = duration.as_millis() as u64,
                    "ACP agent failed"
                );

                if let Some(tracker) = &self.usage_tracker {
                    tracker.record(&AcpUsage {
                        tool_name: self.name.clone(),
                        prompt_chars: prompt.len(),
                        response_chars: 0,
                        duration,
                        success: false,
                        permission_requests: 0,
                        permissions_denied: 0,
                    });
                }

                Err(AdkError::tool(format!("ACP agent error: {e}")))
            }
        }
    }
}
