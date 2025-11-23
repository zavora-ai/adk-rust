use adk_core::{Agent, EventStream, InvocationContext, Result};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::Arc;

/// Parallel agent executes sub-agents concurrently
pub struct ParallelAgent {
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
}

impl ParallelAgent {
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            sub_agents,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }
}

#[async_trait]
impl Agent for ParallelAgent {
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
        let sub_agents = self.sub_agents.clone();
        
        let s = stream! {
            use futures::stream::{FuturesUnordered, StreamExt};
            
            let mut futures = FuturesUnordered::new();
            
            for agent in sub_agents {
                let ctx = ctx.clone();
                futures.push(async move {
                    agent.run(ctx).await
                });
            }
            
            while let Some(result) = futures.next().await {
                match result {
                    Ok(mut stream) => {
                        while let Some(event_result) = stream.next().await {
                            yield event_result;
                        }
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }
}
