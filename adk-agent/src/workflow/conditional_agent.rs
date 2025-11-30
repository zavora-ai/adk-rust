use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, EventStream, InvocationContext, Result,
};
use async_trait::async_trait;
use std::sync::Arc;

type ConditionFn = Box<dyn Fn(&dyn InvocationContext) -> bool + Send + Sync>;

/// Conditional agent runs different sub-agents based on a condition
pub struct ConditionalAgent {
    name: String,
    description: String,
    condition: ConditionFn,
    if_agent: Arc<dyn Agent>,
    else_agent: Option<Arc<dyn Agent>>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
}

impl ConditionalAgent {
    pub fn new<F>(name: impl Into<String>, condition: F, if_agent: Arc<dyn Agent>) -> Self
    where
        F: Fn(&dyn InvocationContext) -> bool + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            description: String::new(),
            condition: Box::new(condition),
            if_agent,
            else_agent: None,
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_else(mut self, else_agent: Arc<dyn Agent>) -> Self {
        self.else_agent = Some(else_agent);
        self
    }

    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.before_callbacks.push(callback);
        self
    }

    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.after_callbacks.push(callback);
        self
    }
}

#[async_trait]
impl Agent for ConditionalAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let agent = if (self.condition)(ctx.as_ref()) {
            self.if_agent.clone()
        } else if let Some(else_agent) = &self.else_agent {
            else_agent.clone()
        } else {
            return Ok(Box::pin(futures::stream::empty()));
        };

        agent.run(ctx).await
    }
}
