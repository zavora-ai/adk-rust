use adk_core::{Agent, Event, EventStream, InvocationContext, Result};
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
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        (self.handler)(ctx).await
    }
}

pub struct CustomAgentBuilder {
    name: String,
    description: String,
    handler: Option<RunHandler>,
}

impl CustomAgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            handler: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
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

        Ok(CustomAgent {
            name: self.name,
            description: self.description,
            handler,
        })
    }
}
