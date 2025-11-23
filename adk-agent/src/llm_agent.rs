use adk_core::{
    Agent, Content, Event, EventStream, InvocationContext, Llm, LlmRequest, Part, Result, Tool,
};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::Arc;

pub struct LlmAgent {
    name: String,
    description: String,
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,
}

impl std::fmt::Debug for LlmAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmAgent")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("model", &self.model.name())
            .field("instruction", &self.instruction)
            .field("tools_count", &self.tools.len())
            .field("sub_agents_count", &self.sub_agents.len())
            .finish()
    }
}

pub struct LlmAgentBuilder {
    name: String,
    description: Option<String>,
    model: Option<Arc<dyn Llm>>,
    instruction: Option<String>,
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,
}

impl LlmAgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            model: None,
            instruction: None,
            tools: Vec::new(),
            sub_agents: Vec::new(),
        }
    }

    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn sub_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.sub_agents.push(agent);
        self
    }

    pub fn build(self) -> Result<LlmAgent> {
        let model = self
            .model
            .ok_or_else(|| adk_core::AdkError::Agent("Model is required".to_string()))?;

        Ok(LlmAgent {
            name: self.name,
            description: self.description.unwrap_or_default(),
            model,
            instruction: self.instruction,
            tools: self.tools,
            sub_agents: self.sub_agents,
        })
    }
}

impl LlmAgent {
    fn build_request(&self, ctx: &Arc<dyn InvocationContext>) -> LlmRequest {
        let mut contents = Vec::new();

        // Add instruction as system message if present
        if let Some(instruction) = &self.instruction {
            contents.push(Content {
                role: "user".to_string(),
                parts: vec![Part::Text {
                    text: instruction.clone(),
                }],
            });
        }

        // Add user content
        let user_content = ctx.user_content();
        contents.push(user_content.clone());

        LlmRequest {
            model: self.model.name().to_string(),
            contents,
            tools: Default::default(),
            config: None,
        }
    }
}

#[async_trait]
impl Agent for LlmAgent {
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
        let request = self.build_request(&ctx);
        let model = self.model.clone();
        let agent_name = self.name.clone();
        let invocation_id = ctx.invocation_id().to_string();

        let s = stream! {
            // Call model (non-streaming for now)
            let mut response_stream = model.generate_content(request, false).await?;

            // Get first (and only) response
            use futures::StreamExt;
            if let Some(result) = response_stream.next().await {
                let response = result?;
                
                // Create event with response content
                let mut event = Event::new(&invocation_id);
                event.author = agent_name.clone();
                event.content = response.content.clone();
                
                yield Ok(event);
            }
        };

        Ok(Box::pin(s))
    }
}
