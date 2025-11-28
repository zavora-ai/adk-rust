use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, EventStream, InvocationContext, Result,
};
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type RunHandler = Box<
    dyn Fn(Arc<dyn InvocationContext>) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send>>
        + Send
        + Sync,
>;

pub struct CustomAgent {
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    #[allow(dead_code)] // Part of public API, callbacks not yet implemented
    before_callbacks: Vec<BeforeAgentCallback>,
    #[allow(dead_code)] // Part of public API, callbacks not yet implemented  
    after_callbacks: Vec<AfterAgentCallback>,
    handler: RunHandler,
}

impl CustomAgent {
    pub fn builder(name: impl Into<String>) -> CustomAgentBuilder {
        CustomAgentBuilder::new(name)
    }
}

#[async_trait]
impl Agent for CustomAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        (self.handler)(ctx).await
    }
}

pub struct CustomAgentBuilder {
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
    handler: Option<RunHandler>,
}

impl CustomAgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            sub_agents: Vec::new(),
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
            handler: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn sub_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.sub_agents.push(agent);
        self
    }

    pub fn sub_agents(mut self, agents: Vec<Arc<dyn Agent>>) -> Self {
        self.sub_agents = agents;
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

    pub fn handler<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(Arc<dyn InvocationContext>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<EventStream>> + Send + 'static,
    {
        self.handler = Some(Box::new(move |ctx| Box::pin(handler(ctx))));
        self
    }

    pub fn build(self) -> Result<CustomAgent> {
        let handler = self.handler.ok_or_else(|| {
            adk_core::AdkError::Agent("CustomAgent requires a handler".to_string())
        })?;

        // Validate sub-agents have unique names
        let mut seen_names = std::collections::HashSet::new();
        for agent in &self.sub_agents {
            if !seen_names.insert(agent.name()) {
                return Err(adk_core::AdkError::Agent(format!(
                    "Duplicate sub-agent name: {}",
                    agent.name()
                )));
            }
        }

        Ok(CustomAgent {
            name: self.name,
            description: self.description,
            sub_agents: self.sub_agents,
            before_callbacks: self.before_callbacks,
            after_callbacks: self.after_callbacks,
            handler,
        })
    }
}
