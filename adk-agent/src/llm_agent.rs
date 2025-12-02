use adk_core::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, Agent, BeforeAgentCallback,
    BeforeModelCallback, BeforeModelResult, BeforeToolCallback, CallbackContext, Content, Event,
    EventActions, GlobalInstructionProvider, InstructionProvider, InvocationContext, Llm,
    LlmRequest, MemoryEntry, Part, ReadonlyContext, Result, Tool, ToolContext,
};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

pub struct LlmAgent {
    name: String,
    description: String,
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,
    #[allow(dead_code)] // Part of public API via builder
    input_schema: Option<serde_json::Value>,
    output_schema: Option<serde_json::Value>,
    #[allow(dead_code)] // Part of public API via builder
    disallow_transfer_to_parent: bool,
    #[allow(dead_code)] // Part of public API via builder
    disallow_transfer_to_peers: bool,
    include_contents: adk_core::IncludeContents,
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,
    output_key: Option<String>,
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
    before_model_callbacks: Arc<Vec<BeforeModelCallback>>,
    after_model_callbacks: Arc<Vec<AfterModelCallback>>,
    before_tool_callbacks: Arc<Vec<BeforeToolCallback>>,
    after_tool_callbacks: Arc<Vec<AfterToolCallback>>,
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
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,
    input_schema: Option<serde_json::Value>,
    output_schema: Option<serde_json::Value>,
    disallow_transfer_to_parent: bool,
    disallow_transfer_to_peers: bool,
    include_contents: adk_core::IncludeContents,
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,
    output_key: Option<String>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
    before_model_callbacks: Vec<BeforeModelCallback>,
    after_model_callbacks: Vec<AfterModelCallback>,
    before_tool_callbacks: Vec<BeforeToolCallback>,
    after_tool_callbacks: Vec<AfterToolCallback>,
}

impl LlmAgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            model: None,
            instruction: None,
            instruction_provider: None,
            global_instruction: None,
            global_instruction_provider: None,
            input_schema: None,
            output_schema: None,
            disallow_transfer_to_parent: false,
            disallow_transfer_to_peers: false,
            include_contents: adk_core::IncludeContents::Default,
            tools: Vec::new(),
            sub_agents: Vec::new(),
            output_key: None,
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
            before_model_callbacks: Vec::new(),
            after_model_callbacks: Vec::new(),
            before_tool_callbacks: Vec::new(),
            after_tool_callbacks: Vec::new(),
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

    pub fn instruction_provider(mut self, provider: InstructionProvider) -> Self {
        self.instruction_provider = Some(Arc::new(provider));
        self
    }

    pub fn global_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.global_instruction = Some(instruction.into());
        self
    }

    pub fn global_instruction_provider(mut self, provider: GlobalInstructionProvider) -> Self {
        self.global_instruction_provider = Some(Arc::new(provider));
        self
    }

    pub fn input_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = Some(schema);
        self
    }

    pub fn output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn disallow_transfer_to_parent(mut self, disallow: bool) -> Self {
        self.disallow_transfer_to_parent = disallow;
        self
    }

    pub fn disallow_transfer_to_peers(mut self, disallow: bool) -> Self {
        self.disallow_transfer_to_peers = disallow;
        self
    }

    pub fn include_contents(mut self, include: adk_core::IncludeContents) -> Self {
        self.include_contents = include;
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

    pub fn before_model_callback(mut self, callback: BeforeModelCallback) -> Self {
        self.before_model_callbacks.push(callback);
        self
    }

    pub fn after_model_callback(mut self, callback: AfterModelCallback) -> Self {
        self.after_model_callbacks.push(callback);
        self
    }

    pub fn before_tool_callback(mut self, callback: BeforeToolCallback) -> Self {
        self.before_tool_callbacks.push(callback);
        self
    }

    pub fn after_tool_callback(mut self, callback: AfterToolCallback) -> Self {
        self.after_tool_callbacks.push(callback);
        self
    }

    pub fn build(self) -> Result<LlmAgent> {
        let model =
            self.model.ok_or_else(|| adk_core::AdkError::Agent("Model is required".to_string()))?;

        Ok(LlmAgent {
            name: self.name,
            description: self.description.unwrap_or_default(),
            model,
            instruction: self.instruction,
            instruction_provider: self.instruction_provider,
            global_instruction: self.global_instruction,
            global_instruction_provider: self.global_instruction_provider,
            input_schema: self.input_schema,
            output_schema: self.output_schema,
            disallow_transfer_to_parent: self.disallow_transfer_to_parent,
            disallow_transfer_to_peers: self.disallow_transfer_to_peers,
            include_contents: self.include_contents,
            tools: self.tools,
            sub_agents: self.sub_agents,
            output_key: self.output_key,
            before_callbacks: Arc::new(self.before_callbacks),
            after_callbacks: Arc::new(self.after_callbacks),
            before_model_callbacks: Arc::new(self.before_model_callbacks),
            after_model_callbacks: Arc::new(self.after_model_callbacks),
            before_tool_callbacks: Arc::new(self.before_tool_callbacks),
            after_tool_callbacks: Arc::new(self.after_tool_callbacks),
        })
    }
}

// AgentToolContext wraps the parent InvocationContext and preserves all context
// instead of throwing it away like SimpleToolContext did
struct AgentToolContext {
    parent_ctx: Arc<dyn InvocationContext>,
    function_call_id: String,
    actions: Mutex<EventActions>,
}

impl AgentToolContext {
    fn new(parent_ctx: Arc<dyn InvocationContext>, function_call_id: String) -> Self {
        Self { parent_ctx, function_call_id, actions: Mutex::new(EventActions::default()) }
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

    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
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

    #[adk_telemetry::instrument(
        skip(self, ctx),
        fields(
            agent.name = %self.name,
            agent.description = %self.description,
            invocation.id = %ctx.invocation_id(),
            user.id = %ctx.user_id(),
            session.id = %ctx.session_id()
        )
    )]
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
        adk_telemetry::info!("Starting agent execution");

        let agent_name = self.name.clone();
        let invocation_id = ctx.invocation_id().to_string();
        let model = self.model.clone();
        let tools = self.tools.clone();
        let sub_agents = self.sub_agents.clone();

        let instruction = self.instruction.clone();
        let instruction_provider = self.instruction_provider.clone();
        let global_instruction = self.global_instruction.clone();
        let global_instruction_provider = self.global_instruction_provider.clone();
        let output_key = self.output_key.clone();
        let output_schema = self.output_schema.clone();
        let include_contents = self.include_contents;
        // Clone Arc references (cheap)
        let before_agent_callbacks = self.before_callbacks.clone();
        let after_agent_callbacks = self.after_callbacks.clone();
        let before_model_callbacks = self.before_model_callbacks.clone();
        let after_model_callbacks = self.after_model_callbacks.clone();
        let _before_tool_callbacks = self.before_tool_callbacks.clone();
        let _after_tool_callbacks = self.after_tool_callbacks.clone();

        let s = stream! {
            // ===== BEFORE AGENT CALLBACKS =====
            // Execute before the agent starts running
            // If any returns content, skip agent execution
            for callback in before_agent_callbacks.as_ref() {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        // Callback returned content - yield it and skip agent execution
                        let mut early_event = Event::new(&invocation_id);
                        early_event.author = agent_name.clone();
                        early_event.llm_response.content = Some(content);
                        yield Ok(early_event);

                        // Skip rest of agent execution and go to after callbacks
                        for after_callback in after_agent_callbacks.as_ref() {
                            match after_callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                                Ok(Some(after_content)) => {
                                    let mut after_event = Event::new(&invocation_id);
                                    after_event.author = agent_name.clone();
                                    after_event.llm_response.content = Some(after_content);
                                    yield Ok(after_event);
                                    return;
                                }
                                Ok(None) => continue,
                                Err(e) => {
                                    yield Err(e);
                                    return;
                                }
                            }
                        }
                        return;
                    }
                    Ok(None) => {
                        // Continue to next callback
                        continue;
                    }
                    Err(e) => {
                        // Callback failed - propagate error
                        yield Err(e);
                        return;
                    }
                }
            }

            // ===== MAIN AGENT EXECUTION =====
            let mut conversation_history = Vec::new();

            // ===== PROCESS GLOBAL INSTRUCTION =====
            // GlobalInstruction provides tree-wide personality/identity
            if let Some(provider) = &global_instruction_provider {
                // Dynamic global instruction via provider
                let global_inst = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
                if !global_inst.is_empty() {
                    conversation_history.push(Content {
                        role: "user".to_string(),
                        parts: vec![Part::Text { text: global_inst }],
                    });
                }
            } else if let Some(ref template) = global_instruction {
                // Static global instruction with template injection
                let processed = adk_core::inject_session_state(ctx.as_ref(), template).await?;
                if !processed.is_empty() {
                    conversation_history.push(Content {
                        role: "user".to_string(),
                        parts: vec![Part::Text { text: processed }],
                    });
                }
            }

            // ===== PROCESS AGENT INSTRUCTION =====
            // Agent-specific instruction
            if let Some(provider) = &instruction_provider {
                // Dynamic instruction via provider
                let inst = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
                if !inst.is_empty() {
                    conversation_history.push(Content {
                        role: "user".to_string(),
                        parts: vec![Part::Text { text: inst }],
                    });
                }
            } else if let Some(ref template) = instruction {
                // Static instruction with template injection
                let processed = adk_core::inject_session_state(ctx.as_ref(), template).await?;
                if !processed.is_empty() {
                    conversation_history.push(Content {
                        role: "user".to_string(),
                        parts: vec![Part::Text { text: processed }],
                    });
                }
            }

            // ===== LOAD SESSION HISTORY =====
            // Load previous conversation turns from the session
            let session_history = ctx.session().conversation_history();
            conversation_history.extend(session_history);

            // Add user content (current turn)
            conversation_history.push(ctx.user_content().clone());

            // ===== APPLY INCLUDE_CONTENTS FILTERING =====
            // Control what conversation history the agent sees
            let mut conversation_history = match include_contents {
                adk_core::IncludeContents::None => {
                    // Agent operates solely on current turn - only keep the latest user input
                    // Remove all previous history except instructions and current user message
                    let mut filtered = Vec::new();

                    // Keep global and agent instructions (already added above)
                    let instruction_count = conversation_history.iter()
                        .take_while(|c| c.role == "user" && c.parts.iter().any(|p| {
                            if let Part::Text { text } = p {
                                // These are likely instructions, not user queries
                                !text.is_empty()
                            } else {
                                false
                            }
                        }))
                        .count();

                    // Take instructions
                    filtered.extend(conversation_history.iter().take(instruction_count).cloned());

                    // Take only the last user message (current turn)
                    if let Some(last) = conversation_history.last() {
                        if last.role == "user" {
                            filtered.push(last.clone());
                        }
                    }

                    filtered
                }
                adk_core::IncludeContents::Default => {
                    // Default behavior - keep full conversation history
                    conversation_history
                }
            };

            // Build tool declarations for Gemini
            // Uses enhanced_description() which includes NOTE for long-running tools
            let mut tool_declarations = std::collections::HashMap::new();
            for tool in &tools {
                // Build FunctionDeclaration JSON with enhanced description
                // For long-running tools, this includes a warning not to call again if pending
                let mut decl = serde_json::json!({
                    "name": tool.name(),
                    "description": tool.enhanced_description(),
                });

                if let Some(params) = tool.parameters_schema() {
                    decl["parameters"] = params;
                }

                if let Some(response) = tool.response_schema() {
                    decl["response"] = response;
                }

                tool_declarations.insert(tool.name().to_string(), decl);
            }

            // Inject transfer_to_agent tool if sub-agents exist
            if !sub_agents.is_empty() {
                let transfer_tool_name = "transfer_to_agent";
                let transfer_tool_decl = serde_json::json!({
                    "name": transfer_tool_name,
                    "description": "Transfer execution to another agent.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "agent_name": {
                                "type": "string",
                                "description": "The name of the agent to transfer to."
                            }
                        },
                        "required": ["agent_name"]
                    }
                });
                tool_declarations.insert(transfer_tool_name.to_string(), transfer_tool_decl);
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
                let config = output_schema.as_ref().map(|schema| {
                    adk_core::GenerateContentConfig {
                        temperature: None,
                        top_p: None,
                        top_k: None,
                        max_output_tokens: None,
                        response_schema: Some(schema.clone()),
                    }
                });

                let request = LlmRequest {
                    model: model.name().to_string(),
                    contents: conversation_history.clone(),
                    tools: tool_declarations.clone(),
                    config,
                };

                // ===== BEFORE MODEL CALLBACKS =====
                // These can modify the request or skip the model call by returning a response
                let mut current_request = request;
                let mut model_response_override = None;
                for callback in before_model_callbacks.as_ref() {
                    match callback(ctx.clone() as Arc<dyn CallbackContext>, current_request.clone()).await {
                        Ok(BeforeModelResult::Continue(modified_request)) => {
                            // Callback may have modified the request, continue with it
                            current_request = modified_request;
                        }
                        Ok(BeforeModelResult::Skip(response)) => {
                            // Callback returned a response - skip model call
                            model_response_override = Some(response);
                            break;
                        }
                        Err(e) => {
                            // Callback failed - propagate error
                            yield Err(e);
                            return;
                        }
                    }
                }
                let request = current_request;

                // Determine streaming source: cached response or real model
            let mut accumulated_content: Option<Content> = None;
            let mut terminate_agent = false;

                if let Some(cached_response) = model_response_override {
                    let mut cached_response = cached_response;
                    normalize_option_content(&mut cached_response.content);
                    // Use callback-provided response (e.g., from cache)
                    // Yield it as an event
                    let mut cached_event = Event::new(&invocation_id);
                    cached_event.author = agent_name.clone();
                    cached_event.llm_response.content = cached_response.content.clone();

                    // Populate long_running_tool_ids for function calls from long-running tools
                    if let Some(ref content) = cached_response.content {
                        let long_running_ids: Vec<String> = content.parts.iter()
                            .filter_map(|p| {
                                if let Part::FunctionCall { name, .. } = p {
                                    if let Some(tool) = tools.iter().find(|t| t.name() == name) {
                                        if tool.is_long_running() {
                                            return Some(name.clone());
                                        }
                                    }
                                }
                                None
                            })
                            .collect();
                        cached_event.long_running_tool_ids = long_running_ids;
                    }

                    yield Ok(cached_event);

                    accumulated_content = cached_response.content;
                } else {
                    // Call model with STREAMING ENABLED
                    let mut response_stream = model.generate_content(request, true).await?;

                    use futures::StreamExt;

                    // Stream and process chunks with AfterModel callbacks
                    while let Some(chunk_result) = response_stream.next().await {
                        let mut chunk = match chunk_result {
                            Ok(c) => c,
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        };

                        // ===== AFTER MODEL CALLBACKS (per chunk) =====
                        // Callbacks can modify each streaming chunk
                        for callback in after_model_callbacks.as_ref() {
                            match callback(ctx.clone() as Arc<dyn CallbackContext>, chunk.clone()).await {
                                Ok(Some(modified_chunk)) => {
                                    // Callback modified this chunk
                                    chunk = modified_chunk;
                                    break;
                                }
                                Ok(None) => {
                                    // Continue to next callback
                                    continue;
                                }
                                Err(e) => {
                                    // Callback failed - propagate error
                                    yield Err(e);
                                    return;
                                }
                            }
                        }

                        normalize_option_content(&mut chunk.content);

                        // Yield the (possibly modified) partial event
                        let mut partial_event = Event::new(&invocation_id);
                        partial_event.author = agent_name.clone();
                        partial_event.llm_response.content = chunk.content.clone();

                        // Populate long_running_tool_ids for function calls from long-running tools
                        if let Some(ref content) = chunk.content {
                            let long_running_ids: Vec<String> = content.parts.iter()
                                .filter_map(|p| {
                                    if let Part::FunctionCall { name, .. } = p {
                                        // Check if this tool is long-running
                                        if let Some(tool) = tools.iter().find(|t| t.name() == name) {
                                            if tool.is_long_running() {
                                                // Use tool name as ID (we don't have explicit call IDs)
                                                return Some(name.clone());
                                            }
                                        }
                                    }
                                    None
                                })
                                .collect();
                            partial_event.long_running_tool_ids = long_running_ids;
                        }

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
                            break;
                        }
                    }
                }

                normalize_option_content(&mut accumulated_content);

                // After streaming/caching completes, check for function calls in accumulated content
                let function_call_names: Vec<String> = accumulated_content.as_ref()
                    .map(|c| c.parts.iter()
                        .filter_map(|p| {
                            if let Part::FunctionCall { name, .. } = p {
                                Some(name.clone())
                            } else {
                                None
                            }
                        })
                        .collect())
                    .unwrap_or_default();

                let has_function_calls = !function_call_names.is_empty();

                // Check if ALL function calls are from long-running tools
                // If so, we should NOT continue the loop - the tool returned a pending status
                // and the agent/client will poll for completion later
                let all_calls_are_long_running = has_function_calls && function_call_names.iter().all(|name| {
                    tools.iter()
                        .find(|t| t.name() == name)
                        .map(|t| t.is_long_running())
                        .unwrap_or(false)
                });

                // Add final content to history
                if let Some(ref content) = accumulated_content {
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
                            // Handle transfer_to_agent specially
                            if name == "transfer_to_agent" {
                                let target_agent = args.get("agent_name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();

                                let mut transfer_event = Event::new(&invocation_id);
                                transfer_event.author = agent_name.clone();
                                transfer_event.actions.transfer_to_agent = Some(target_agent);

                                yield Ok(transfer_event);
                                return;
                            }


                            // Find and execute tool
                            let (tool_result, tool_actions) = if let Some(tool) = tools.iter().find(|t| t.name() == name) {
                                // ✅ Use AgentToolContext that preserves parent context
                                let tool_ctx: Arc<dyn ToolContext> = Arc::new(AgentToolContext::new(
                                    ctx.clone(),
                                    format!("{}_{}", invocation_id, name),
                                ));

                                let result = match tool.execute(tool_ctx.clone(), args.clone()).await {
                                    Ok(result) => result,
                                    Err(e) => serde_json::json!({ "error": e.to_string() }),
                                };

                                (result, tool_ctx.actions())
                            } else {
                                (serde_json::json!({ "error": format!("Tool {} not found", name) }), EventActions::default())
                            };

                            // Yield tool execution event
                            let mut tool_event = Event::new(&invocation_id);
                            tool_event.author = agent_name.clone();
                            tool_event.actions = tool_actions.clone();
                            tool_event.llm_response.content = Some(Content {
                                role: "function".to_string(),
                                parts: vec![Part::FunctionResponse {
                                    name: name.clone(),
                                    response: tool_result.clone(),
                                }],
                            });
                            yield Ok(tool_event);

                            if tool_actions.escalate || tool_actions.skip_summarization {
                                terminate_agent = true;
                            }

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

                if terminate_agent {
                    break;
                }

                // If all function calls were from long-running tools, treat as final response
                // The tools have been executed and returned pending status - don't continue the loop
                if all_calls_are_long_running {
                    break;
                }
            }

            // ===== AFTER AGENT CALLBACKS =====
            // Execute after the agent completes
            for callback in after_agent_callbacks.as_ref() {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        // Callback returned content - yield it
                        let mut after_event = Event::new(&invocation_id);
                        after_event.author = agent_name.clone();
                        after_event.llm_response.content = Some(content);
                        yield Ok(after_event);
                        break; // First callback that returns content wins
                    }
                    Ok(None) => {
                        // Continue to next callback
                        continue;
                    }
                    Err(e) => {
                        // Callback failed - propagate error
                        yield Err(e);
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }
}

fn normalize_option_content(content: &mut Option<Content>) {
    if let Some(ref mut content) = content {
        normalize_tool_call_markup(content);
    }
}

fn normalize_tool_call_markup(content: &mut Content) {
    let parts = std::mem::take(&mut content.parts);
    let mut normalized = Vec::new();

    for part in parts {
        match part {
            Part::Text { text } => {
                normalized.extend(convert_text_to_parts(text));
            }
            other => normalized.push(other),
        }
    }

    content.parts = normalized;
}

fn convert_text_to_parts(text: String) -> Vec<Part> {
    const TOOL_CALL_START: &str = "<tool_call>";
    const TOOL_CALL_END: &str = "</tool_call>";

    if !text.contains(TOOL_CALL_START) {
        return vec![Part::Text { text }];
    }

    let mut parts = Vec::new();
    let mut remainder = text.as_str();

    while let Some(start_idx) = remainder.find(TOOL_CALL_START) {
        let (before, after_start_tag) = remainder.split_at(start_idx);
        if !before.is_empty() {
            parts.push(Part::Text { text: before.to_string() });
        }

        let after_start = &after_start_tag[TOOL_CALL_START.len()..];
        if let Some(end_idx) = after_start.find(TOOL_CALL_END) {
            let block = &after_start[..end_idx];
            if let Some(call_part) = parse_tool_call_block(block) {
                parts.push(call_part);
            } else {
                parts.push(Part::Text {
                    text: format!("{}{}{}", TOOL_CALL_START, block, TOOL_CALL_END),
                });
            }
            remainder = &after_start[end_idx + TOOL_CALL_END.len()..];
        } else {
            parts.push(Part::Text { text: format!("{}{}", TOOL_CALL_START, after_start) });
            remainder = "";
            break;
        }
    }

    if !remainder.is_empty() {
        parts.push(Part::Text { text: remainder.to_string() });
    }

    if parts.is_empty() {
        vec![Part::Text { text }]
    } else {
        parts
    }
}

fn parse_tool_call_block(block: &str) -> Option<Part> {
    let trimmed = block.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut lines = trimmed.lines();
    let name_line = lines.next()?.trim();
    if name_line.is_empty() {
        return None;
    }

    let remainder = lines.collect::<Vec<_>>().join("\n");
    let mut slice = remainder.as_str();
    let mut args_map = serde_json::Map::new();
    let mut found_arg = false;

    loop {
        slice = slice.trim_start();
        if slice.is_empty() {
            break;
        }

        let rest = if let Some(rest) = slice.strip_prefix("<arg_key>") {
            rest
        } else {
            break;
        };

        let key_end = rest.find("</arg_key>")?;
        let key = rest[..key_end].trim().to_string();
        let mut after_key = &rest[key_end + "</arg_key>".len()..];

        after_key = after_key.trim_start();
        let rest = if let Some(rest) = after_key.strip_prefix("<arg_value>") {
            rest
        } else {
            break;
        };

        let value_end = rest.find("</arg_value>")?;
        let value_text = rest[..value_end].trim();
        let value = parse_arg_value(value_text);
        args_map.insert(key, value);
        slice = &rest[value_end + "</arg_value>".len()..];
        found_arg = true;
    }

    if !found_arg {
        return None;
    }

    Some(Part::FunctionCall {
        name: name_line.to_string(),
        args: serde_json::Value::Object(args_map),
    })
}

fn parse_arg_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return serde_json::Value::String(String::new());
    }

    serde_json::from_str(trimmed).unwrap_or_else(|_| serde_json::Value::String(trimmed.to_string()))
}
