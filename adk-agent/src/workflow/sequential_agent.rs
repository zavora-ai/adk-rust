#[cfg(feature = "skills")]
use crate::skill_shim::{SelectionPolicy, SkillIndex};
use crate::workflow::LoopAgent;
use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, EventStream, InvocationContext, Result,
};
use async_trait::async_trait;
use std::sync::Arc;

/// Sequential agent executes sub-agents once in order
pub struct SequentialAgent {
    loop_agent: LoopAgent,
}

impl SequentialAgent {
    /// Create a new sequential agent with the given name and sub-agents.
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self { loop_agent: LoopAgent::new(name, sub_agents).with_max_iterations(1) }
    }

    /// Set the agent description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.loop_agent = self.loop_agent.with_description(desc);
        self
    }

    /// Add a before-agent callback.
    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.loop_agent = self.loop_agent.before_callback(callback);
        self
    }

    /// Add an after-agent callback.
    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.loop_agent = self.loop_agent.after_callback(callback);
        self
    }

    /// Set a preloaded skills index for this agent.
    #[cfg(feature = "skills")]
    pub fn with_skills(mut self, index: SkillIndex) -> Self {
        self.loop_agent = self.loop_agent.with_skills(index);
        self
    }

    /// Auto-load skills from `.skills/` in the current working directory.
    #[cfg(feature = "skills")]
    pub fn with_auto_skills(self) -> Result<Self> {
        self.with_skills_from_root(".")
    }

    /// Auto-load skills from `.skills/` under a custom root directory.
    #[cfg(feature = "skills")]
    pub fn with_skills_from_root(mut self, root: impl AsRef<std::path::Path>) -> Result<Self> {
        self.loop_agent = self.loop_agent.with_skills_from_root(root)?;
        Ok(self)
    }

    /// Customize skill selection behavior.
    #[cfg(feature = "skills")]
    pub fn with_skill_policy(mut self, policy: SelectionPolicy) -> Self {
        self.loop_agent = self.loop_agent.with_skill_policy(policy);
        self
    }

    /// Limit injected skill content length.
    #[cfg(feature = "skills")]
    pub fn with_skill_budget(mut self, max_chars: usize) -> Self {
        self.loop_agent = self.loop_agent.with_skill_budget(max_chars);
        self
    }
}

#[async_trait]
impl Agent for SequentialAgent {
    fn name(&self) -> &str {
        self.loop_agent.name()
    }

    fn description(&self) -> &str {
        self.loop_agent.description()
    }

    fn profile(&self) -> adk_core::AgentProfile {
        adk_core::AgentProfile { kind: adk_core::AgentKind::Sequential, ..Default::default() }
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        self.loop_agent.sub_agents()
    }

    fn supports_agent_transfer(&self) -> bool {
        // Deterministic workflow agent: on cross-turn resumption the runner
        // must restart from this root so every sub-agent runs again in order,
        // rather than resuming a single sub-agent that responded last.
        false
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        self.loop_agent.run(ctx).await
    }
}
