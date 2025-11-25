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


// AgentToolContext wraps the parent InvocationContext and preserves all context
// instead of throwing it away like SimpleToolContext did
struct AgentToolContext {
    parent_ctx: Arc<dyn InvocationContext>,
    function_call_id: String,
    actions: EventActions,
}

impl AgentToolContext {
    fn new(parent_ctx: Arc<dyn InvocationContext>, function_call_id: String) -> Self {
        Self {
            parent_ctx,
            function_call_id,
            actions: EventActions::default(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for AgentToolContext {
    fn invocation_id(&self) -> &str {
        self.parent_ctx.invocation_id()
    }
    
    fn agent_name(&self) -> &str {
        self.parent_ctx.agent_name()
    }
    
    fn user_id(&self) -> &str {
        // ✅ Delegate to parent - now tools get the real user_id!
        self.parent_ctx.user_id()
    }
    
    fn app_name(&self) -> &str {
        // ✅ Delegate to parent - now tools get the real app_name!
        self.parent_ctx.app_name()
    }
    
    fn session_id(&self) -> &str {
        // ✅ Delegate to parent - now tools get the real session_id!
        self.parent_ctx.session_id()
    }
    
    fn branch(&self) -> &str {
        self.parent_ctx.branch()
    }
    
    fn user_content(&self) -> &Content {
        self.parent_ctx.user_content()
    }
}

#[async_trait]
impl CallbackContext for AgentToolContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        // ✅ Delegate to parent - tools can now access artifacts!
        self.parent_ctx.artifacts()
    }
}

#[async_trait]
impl ToolContext for AgentToolContext {
    fn function_call_id(&self) -> &str {
        &self.function_call_id
    }
    
    fn actions(&self) -> &EventActions {
        &self.actions
    }
    
    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>> {
        // ✅ Delegate to parent's memory if available
        if let Some(memory) = self.parent_ctx.memory() {
            memory.search(query).await
        } else {
            Ok(vec![])
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

                // Call model with STREAMING ENABLED
                let mut response_stream = model.generate_content(request, true).await?;

                use futures::StreamExt;
                
                // Accumulate chunks as they arrive
                let mut accumulated_content: Option<Content> = None;
                let mut has_function_calls = false;
                let mut turn_complete = false;
                
                // Stream and yield chunks immediately
                while let Some(chunk_result) = response_stream.next().await {
                    let chunk = match chunk_result {
                        Ok(c) => c,
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    };
                    
                    // Yield partial event immediately for user feedback
                    let mut partial_event = Event::new(&invocation_id);
                    partial_event.author = agent_name.clone();
                    partial_event.content = chunk.content.clone();
                    yield Ok(partial_event);
                    
                    // Accumulate content for history
                    if let Some(chunk_content) = chunk.content {
                        if let Some(ref mut acc) = accumulated_content {
                            // Merge parts from this chunk into accumulated content
                            acc.parts.extend(chunk_content.parts);
                        } else {
                            // First chunk - initialize accumulator
                            accumulated_content = Some(chunk_content);
                        }
                    }
                    
                    // Check if turn is complete
                    if chunk.turn_complete {
                        turn_complete = true;
                        break;
                    }
                }
                
                // After streaming completes, check for function calls in accumulated content
                if let Some(ref content) = accumulated_content {
                    has_function_calls = content.parts.iter().any(|p| matches!(p, Part::FunctionCall { .. }));
                    
                    // Add accumulated response to history
                    conversation_history.push(content.clone());
                    
                    // Handle output_key: save final agent output to state_delta
                    if let Some(ref output_key) = output_key {
                        if !has_function_calls {  // Only save if not calling tools
                            let mut text_parts = String::new();
                            for part in &content.parts {
                                if let Part::Text { text } = part {
                                    text_parts.push_str(text);
                                }
                            }
                            if !text_parts.is_empty() {
                                // Yield a final state update event
                                let mut state_event = Event::new(&invocation_id);
                                state_event.author = agent_name.clone();
                                state_event.actions.state_delta.insert(
                                    output_key.clone(),
                                    serde_json::Value::String(text_parts),
                                );
                                yield Ok(state_event);
                            }
                        }
                    }
                }

                if !has_function_calls {
                    // No function calls, we're done
                    break;
                }

                // Execute function calls and add responses to history
                if let Some(content) = &accumulated_content {
                    for part in &content.parts {
                        if let Part::FunctionCall { name, args } = part {
                            // Find and execute tool
                            let tool_result = if let Some(tool) = tools.iter().find(|t| t.name() == name) {
                                // ✅ Use AgentToolContext that preserves parent context
                                let tool_ctx = Arc::new(AgentToolContext::new(
                                    ctx.clone(),
                                    format!("{}_{}", invocation_id, name),
                                )) as Arc<dyn ToolContext>;
                                
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
