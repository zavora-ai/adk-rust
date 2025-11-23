use adk_core::{
    Agent, AfterAgentCallback, BeforeAgentCallback, CallbackContext, Content, Event, EventActions, EventStream, InvocationContext, Llm,
    LlmRequest, MemoryEntry, Part, ReadonlyContext, Result, Tool, ToolContext,
};
use async_stream::stream;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

pub struct LlmAgent {
    name: String,
    description: String,
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,
    output_key: Option<String>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
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
    output_key: Option<String>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
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
            output_key: None,
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
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

    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = Some(key.into());
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

    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.before_callbacks.push(callback);
        self
    }

    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.after_callbacks.push(callback);
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
            output_key: self.output_key,
            before_callbacks: self.before_callbacks,
            after_callbacks: self.after_callbacks,
        })
    }
}

// Simple ToolContext implementation for tool execution
struct SimpleToolContext {
    invocation_id: String,
    agent_name: String,
    actions: EventActions,
    content: Content,
}

#[async_trait]
impl ReadonlyContext for SimpleToolContext {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }
    fn agent_name(&self) -> &str {
        &self.agent_name
    }
    fn user_id(&self) -> &str {
        ""
    }
    fn app_name(&self) -> &str {
        ""
    }
    fn session_id(&self) -> &str {
        ""
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for SimpleToolContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for SimpleToolContext {
    fn function_call_id(&self) -> &str {
        &self.invocation_id
    }
    fn actions(&self) -> &EventActions {
        &self.actions
    }
    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(vec![])
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
        let model = self.model.clone();
        let agent_name = self.name.clone();
        let invocation_id = ctx.invocation_id().to_string();
        let tools = self.tools.clone();
        let instruction = self.instruction.clone();
        let output_key = self.output_key.clone();

        let s = stream! {
            let mut conversation_history = Vec::new();

            // Add instruction if present
            if let Some(instr) = instruction {
                conversation_history.push(Content {
                    role: "user".to_string(),
                    parts: vec![Part::Text { text: instr }],
                });
            }

            // Add user content
            conversation_history.push(ctx.user_content().clone());

            // Build tool declarations for Gemini
            let mut tool_declarations = std::collections::HashMap::new();
            for tool in &tools {
                // Build FunctionDeclaration JSON
                let mut decl = serde_json::json!({
                    "name": tool.name(),
                    "description": tool.description(),
                });
                
                if let Some(params) = tool.parameters_schema() {
                    decl["parameters"] = params;
                }
                
                if let Some(response) = tool.response_schema() {
                    decl["response"] = response;
                }
                
                tool_declarations.insert(tool.name().to_string(), decl);
            }

            // Multi-turn loop with max iterations
            let max_iterations = 10;
            let mut iteration = 0;
            
            loop {
                iteration += 1;
                if iteration > max_iterations {
                    yield Err(adk_core::AdkError::Agent(
                        format!("Max iterations ({}) exceeded", max_iterations)
                    ));
                    return;
                }

                // Build request with conversation history
                let request = LlmRequest {
                    model: model.name().to_string(),
                    contents: conversation_history.clone(),
                    tools: tool_declarations.clone(),
                    config: None,
                };

                // Call model
                let mut response_stream = model.generate_content(request, false).await?;

                use futures::StreamExt;
                let response = match response_stream.next().await {
                    Some(Ok(resp)) => resp,
                    Some(Err(e)) => {
                        yield Err(e);
                        return;
                    }
                    None => return,
                };

                // Check if response has function calls
                let has_function_calls = response.content.as_ref()
                    .map(|c| c.parts.iter().any(|p| matches!(p, Part::FunctionCall { .. })))
                    .unwrap_or(false);

                // Add model response to history FIRST (before executing tools)
                if let Some(content) = response.content.clone() {
                    conversation_history.push(content);
                }

                // Yield model response event
                let mut event = Event::new(&invocation_id);
                event.author = agent_name.clone();
                event.content = response.content.clone();
                
                // Handle output_key: save agent output to state_delta
                if let Some(ref output_key) = output_key {
                    if let Some(ref content) = event.content {
                        let mut text_parts = String::new();
                        for part in &content.parts {
                            if let Part::Text { text } = part {
                                text_parts.push_str(text);
                            }
                        }
                        if !text_parts.is_empty() {
                            event.actions.state_delta.insert(
                                output_key.clone(),
                                serde_json::Value::String(text_parts),
                            );
                        }
                    }
                }
                
                yield Ok(event);

                if !has_function_calls {
                    // No function calls, we're done
                    break;
                }

                // Execute function calls and add responses to history
                if let Some(content) = &response.content {
                    for part in &content.parts {
                        if let Part::FunctionCall { name, args } = part {
                            // Find and execute tool
                            let tool_result = if let Some(tool) = tools.iter().find(|t| t.name() == name) {
                                let tool_ctx = Arc::new(SimpleToolContext {
                                    invocation_id: invocation_id.clone(),
                                    agent_name: agent_name.clone(),
                                    actions: EventActions::default(),
                                    content: Content::new("user"),
                                }) as Arc<dyn ToolContext>;
                                
                                match tool.execute(tool_ctx, args.clone()).await {
                                    Ok(result) => result,
                                    Err(e) => serde_json::json!({ "error": e.to_string() }),
                                }
                            } else {
                                serde_json::json!({ "error": format!("Tool {} not found", name) })
                            };

                            // Yield tool execution event
                            let mut tool_event = Event::new(&invocation_id);
                            tool_event.author = agent_name.clone();
                            tool_event.content = Some(Content {
                                role: "function".to_string(),
                                parts: vec![Part::FunctionResponse {
                                    name: name.clone(),
                                    response: tool_result.clone(),
                                }],
                            });
                            yield Ok(tool_event);

                            // Add function response to history
                            conversation_history.push(Content {
                                role: "function".to_string(),
                                parts: vec![Part::FunctionResponse {
                                    name: name.clone(),
                                    response: tool_result,
                                }],
                            });
                        }
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }
}
