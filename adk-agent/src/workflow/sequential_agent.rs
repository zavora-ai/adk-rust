use crate::workflow::LoopAgent;
use adk_core::{AfterAgentCallback, Agent, BeforeAgentCallback, EventStream, InvocationContext, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// Sequential agent executes sub-agents once in order
pub struct SequentialAgent {
    loop_agent: LoopAgent,
}

impl SequentialAgent {
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self {
            loop_agent: LoopAgent::new(name, sub_agents).with_max_iterations(1),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.loop_agent = self.loop_agent.with_description(desc);
        self
    }

    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.loop_agent = self.loop_agent.before_callback(callback);
        self
    }

    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.loop_agent = self.loop_agent.after_callback(callback);
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

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        self.loop_agent.sub_agents()
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        self.loop_agent.run(ctx).await
    }
}
