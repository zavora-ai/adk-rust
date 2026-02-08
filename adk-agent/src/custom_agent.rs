use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, CallbackContext, Event, EventStream,
    InvocationContext, Result,
};
use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
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
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
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
        let handler = &self.handler;
        let before_callbacks = self.before_callbacks.clone();
        let after_callbacks = self.after_callbacks.clone();
        let agent_name = self.name.clone();

        // Execute before callbacks — if any returns content, short-circuit
        for callback in before_callbacks.as_ref() {
            match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                Ok(Some(content)) => {
                    let invocation_id = ctx.invocation_id().to_string();
                    let s = stream! {
                        let mut early_event = Event::new(&invocation_id);
                        early_event.author = agent_name.clone();
                        early_event.llm_response.content = Some(content);
                        yield Ok(early_event);

                        for after_cb in after_callbacks.as_ref() {
                            match after_cb(ctx.clone() as Arc<dyn CallbackContext>).await {
                                Ok(Some(after_content)) => {
                                    let mut after_event = Event::new(&invocation_id);
                                    after_event.author = agent_name.clone();
                                    after_event.llm_response.content = Some(after_content);
                                    yield Ok(after_event);
                                    return;
                                }
                                Ok(None) => continue,
                                Err(e) => { yield Err(e); return; }
                            }
                        }
                    };
                    return Ok(Box::pin(s));
                }
                Ok(None) => continue,
                Err(e) => return Err(e),
            }
        }

        // Run the actual handler
        let mut inner_stream = (handler)(ctx.clone()).await?;

        let s = stream! {
            while let Some(result) = inner_stream.next().await {
                yield result;
            }

            // Execute after callbacks
            for callback in after_callbacks.as_ref() {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut after_event = Event::new(ctx.invocation_id());
                        after_event.author = agent_name.clone();
                        after_event.llm_response.content = Some(content);
                        yield Ok(after_event);
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => { yield Err(e); return; }
                }
            }
        };

        Ok(Box::pin(s))
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
            before_callbacks: Arc::new(self.before_callbacks),
            after_callbacks: Arc::new(self.after_callbacks),
            handler,
        })
    }
}
