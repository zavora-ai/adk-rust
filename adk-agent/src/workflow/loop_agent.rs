use adk_core::{AfterAgentCallback, Agent, BeforeAgentCallback, EventStream, InvocationContext, Result};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::Arc;

/// Loop agent executes sub-agents repeatedly for N iterations or until escalation
pub struct LoopAgent {
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    max_iterations: Option<u32>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
}

impl LoopAgent {
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            sub_agents,
            max_iterations: None,
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = Some(max);
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
impl Agent for LoopAgent {
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
        let max_iterations = self.max_iterations;
        
        let s = stream! {
            use futures::StreamExt;
            
            let mut count = max_iterations;
            
            loop {
                let mut should_exit = false;
                
                for agent in &sub_agents {
                    let mut stream = agent.run(ctx.clone()).await?;
                    
                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(event) => {
                                if event.actions.escalate {
                                    should_exit = true;
                                }
                                yield Ok(event);
                            }
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        }
                    }
                    
                    if should_exit {
                        return;
                    }
                }
                
                if let Some(ref mut c) = count {
                    *c -= 1;
                    if *c == 0 {
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }
}
