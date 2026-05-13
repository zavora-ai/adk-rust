//! ACP toolset — multiple ACP agents as a single ADK Toolset.

use std::sync::Arc;

use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;

use crate::tool::AcpAgentTool;

/// A collection of ACP agents exposed as an ADK Toolset.
///
/// Each agent becomes a named tool that can be invoked by the parent ADK agent.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::{AcpToolset, AcpAgentTool};
///
/// let toolset = AcpToolset::new("coding-agents")
///     .add(AcpAgentTool::new("claude-code").description("Complex refactoring"))
///     .add(AcpAgentTool::new("codex").description("Quick code generation"));
///
/// let agent = LlmAgentBuilder::new("orchestrator")
///     .toolset(Arc::new(toolset))
///     .build()?;
/// ```
pub struct AcpToolset {
    name: String,
    agents: Vec<Arc<AcpAgentTool>>,
}

impl AcpToolset {
    /// Create a new empty ACP toolset.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), agents: Vec::new() }
    }

    /// Add an ACP agent tool to the toolset.
    pub fn with_agent(mut self, tool: AcpAgentTool) -> Self {
        self.agents.push(Arc::new(tool));
        self
    }
}

#[async_trait]
impl Toolset for AcpToolset {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(self.agents.iter().map(|a| a.clone() as Arc<dyn Tool>).collect())
    }
}
