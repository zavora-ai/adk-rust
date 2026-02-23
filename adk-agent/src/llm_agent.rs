use adk_core::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, Agent, BeforeAgentCallback,
    BeforeModelCallback, BeforeModelResult, BeforeToolCallback, CallbackContext, Content, Event,
    EventActions, FunctionResponseData, GlobalInstructionProvider, InstructionProvider,
    InvocationContext, Llm, LlmRequest, LlmResponse, MemoryEntry, Part, ReadonlyContext, Result,
    Tool, ToolConfirmationDecision, ToolConfirmationPolicy, ToolConfirmationRequest, ToolContext,
};
use adk_skill::{SelectionPolicy, SkillIndex, load_skill_index, select_skill_prompt_block};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tracing::Instrument;

use crate::guardrails::GuardrailSet;

/// Default maximum number of LLM round-trips (iterations) before the agent stops.
pub const DEFAULT_MAX_ITERATIONS: u32 = 100;

/// Default tool execution timeout (5 minutes).
pub const DEFAULT_TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub struct LlmAgent {
    name: String,
    description: String,
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
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
    /// Default generation config (temperature, top_p, etc.) applied to every LLM request.
    generate_content_config: Option<adk_core::GenerateContentConfig>,
    /// Maximum number of LLM round-trips before stopping
    max_iterations: u32,
    /// Timeout for individual tool executions
    tool_timeout: std::time::Duration,
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
    before_model_callbacks: Arc<Vec<BeforeModelCallback>>,
    after_model_callbacks: Arc<Vec<AfterModelCallback>>,
    before_tool_callbacks: Arc<Vec<BeforeToolCallback>>,
    after_tool_callbacks: Arc<Vec<AfterToolCallback>>,
    tool_confirmation_policy: ToolConfirmationPolicy,
    #[allow(dead_code)] // Used when guardrails feature is enabled
    input_guardrails: GuardrailSet,
    #[allow(dead_code)] // Used when guardrails feature is enabled
    output_guardrails: GuardrailSet,
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
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    input_schema: Option<serde_json::Value>,
    output_schema: Option<serde_json::Value>,
    disallow_transfer_to_parent: bool,
    disallow_transfer_to_peers: bool,
    include_contents: adk_core::IncludeContents,
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,
    output_key: Option<String>,
    generate_content_config: Option<adk_core::GenerateContentConfig>,
    max_iterations: u32,
    tool_timeout: std::time::Duration,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
    before_model_callbacks: Vec<BeforeModelCallback>,
    after_model_callbacks: Vec<AfterModelCallback>,
    before_tool_callbacks: Vec<BeforeToolCallback>,
    after_tool_callbacks: Vec<AfterToolCallback>,
    tool_confirmation_policy: ToolConfirmationPolicy,
    input_guardrails: GuardrailSet,
    output_guardrails: GuardrailSet,
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
            skills_index: None,
            skill_policy: SelectionPolicy::default(),
            max_skill_chars: 2000,
            input_schema: None,
            output_schema: None,
            disallow_transfer_to_parent: false,
            disallow_transfer_to_peers: false,
            include_contents: adk_core::IncludeContents::Default,
            tools: Vec::new(),
            sub_agents: Vec::new(),
            output_key: None,
            generate_content_config: None,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
            before_model_callbacks: Vec::new(),
            after_model_callbacks: Vec::new(),
            before_tool_callbacks: Vec::new(),
            after_tool_callbacks: Vec::new(),
            tool_confirmation_policy: ToolConfirmationPolicy::Never,
            input_guardrails: GuardrailSet::new(),
            output_guardrails: GuardrailSet::new(),
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

    /// Set a preloaded skills index for this agent.
    pub fn with_skills(mut self, index: SkillIndex) -> Self {
        self.skills_index = Some(Arc::new(index));
        self
    }

    /// Auto-load skills from `.skills/` in the current working directory.
    pub fn with_auto_skills(self) -> Result<Self> {
        self.with_skills_from_root(".")
    }

    /// Auto-load skills from `.skills/` under a custom root directory.
    pub fn with_skills_from_root(mut self, root: impl AsRef<std::path::Path>) -> Result<Self> {
        let index = load_skill_index(root).map_err(|e| adk_core::AdkError::Agent(e.to_string()))?;
        self.skills_index = Some(Arc::new(index));
        Ok(self)
    }

    /// Customize skill selection behavior.
    pub fn with_skill_policy(mut self, policy: SelectionPolicy) -> Self {
        self.skill_policy = policy;
        self
    }

    /// Limit injected skill content length.
    pub fn with_skill_budget(mut self, max_chars: usize) -> Self {
        self.max_skill_chars = max_chars;
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

    /// Set default generation parameters (temperature, top_p, top_k, max_output_tokens)
    /// applied to every LLM request made by this agent.
    ///
    /// These defaults are merged with any per-request config. If `output_schema` is also
    /// set, the schema is preserved alongside these generation parameters.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_core::GenerateContentConfig;
    ///
    /// let agent = LlmAgentBuilder::new("my-agent")
    ///     .model(model)
    ///     .generate_content_config(GenerateContentConfig {
    ///         temperature: Some(0.7),
    ///         max_output_tokens: Some(2048),
    ///         ..Default::default()
    ///     })
    ///     .build()?;
    /// ```
    pub fn generate_content_config(mut self, config: adk_core::GenerateContentConfig) -> Self {
        self.generate_content_config = Some(config);
        self
    }

    /// Set the default temperature for LLM requests.
    /// Shorthand for setting just temperature without a full `GenerateContentConfig`.
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.generate_content_config
            .get_or_insert(adk_core::GenerateContentConfig::default())
            .temperature = Some(temperature);
        self
    }

    /// Set the default top_p for LLM requests.
    pub fn top_p(mut self, top_p: f32) -> Self {
        self.generate_content_config
            .get_or_insert(adk_core::GenerateContentConfig::default())
            .top_p = Some(top_p);
        self
    }

    /// Set the default top_k for LLM requests.
    pub fn top_k(mut self, top_k: i32) -> Self {
        self.generate_content_config
            .get_or_insert(adk_core::GenerateContentConfig::default())
            .top_k = Some(top_k);
        self
    }

    /// Set the default max output tokens for LLM requests.
    pub fn max_output_tokens(mut self, max_tokens: i32) -> Self {
        self.generate_content_config
            .get_or_insert(adk_core::GenerateContentConfig::default())
            .max_output_tokens = Some(max_tokens);
        self
    }

    /// Set the maximum number of LLM round-trips (iterations) before the agent stops.
    /// Default is 100.
    pub fn max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set the timeout for individual tool executions.
    /// Default is 5 minutes. Tools that exceed this timeout will return an error.
    pub fn tool_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.tool_timeout = timeout;
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

    /// Configure tool confirmation requirements for this agent.
    pub fn tool_confirmation_policy(mut self, policy: ToolConfirmationPolicy) -> Self {
        self.tool_confirmation_policy = policy;
        self
    }

    /// Require confirmation for a specific tool name.
    pub fn require_tool_confirmation(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_confirmation_policy = self.tool_confirmation_policy.with_tool(tool_name);
        self
    }

    /// Require confirmation for all tool calls.
    pub fn require_tool_confirmation_for_all(mut self) -> Self {
        self.tool_confirmation_policy = ToolConfirmationPolicy::Always;
        self
    }

    /// Set input guardrails to validate user input before processing.
    ///
    /// Input guardrails run before the agent processes the request and can:
    /// - Block harmful or off-topic content
    /// - Redact PII from user input
    /// - Enforce input length limits
    ///
    /// Requires the `guardrails` feature.
    pub fn input_guardrails(mut self, guardrails: GuardrailSet) -> Self {
        self.input_guardrails = guardrails;
        self
    }

    /// Set output guardrails to validate agent responses.
    ///
    /// Output guardrails run after the agent generates a response and can:
    /// - Enforce JSON schema compliance
    /// - Redact PII from responses
    /// - Block harmful content in responses
    ///
    /// Requires the `guardrails` feature.
    pub fn output_guardrails(mut self, guardrails: GuardrailSet) -> Self {
        self.output_guardrails = guardrails;
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
            skills_index: self.skills_index,
            skill_policy: self.skill_policy,
            max_skill_chars: self.max_skill_chars,
            input_schema: self.input_schema,
            output_schema: self.output_schema,
            disallow_transfer_to_parent: self.disallow_transfer_to_parent,
            disallow_transfer_to_peers: self.disallow_transfer_to_peers,
            include_contents: self.include_contents,
            tools: self.tools,
            sub_agents: self.sub_agents,
            output_key: self.output_key,
            generate_content_config: self.generate_content_config,
            max_iterations: self.max_iterations,
            tool_timeout: self.tool_timeout,
            before_callbacks: Arc::new(self.before_callbacks),
            after_callbacks: Arc::new(self.after_callbacks),
            before_model_callbacks: Arc::new(self.before_model_callbacks),
            after_model_callbacks: Arc::new(self.after_model_callbacks),
            before_tool_callbacks: Arc::new(self.before_tool_callbacks),
            after_tool_callbacks: Arc::new(self.after_tool_callbacks),
            tool_confirmation_policy: self.tool_confirmation_policy,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
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
        let skills_index = self.skills_index.clone();
        let skill_policy = self.skill_policy.clone();
        let max_skill_chars = self.max_skill_chars;
        let output_key = self.output_key.clone();
        let output_schema = self.output_schema.clone();
        let generate_content_config = self.generate_content_config.clone();
        let include_contents = self.include_contents;
        let max_iterations = self.max_iterations;
        let tool_timeout = self.tool_timeout;
        // Clone Arc references (cheap)
        let before_agent_callbacks = self.before_callbacks.clone();
        let after_agent_callbacks = self.after_callbacks.clone();
        let before_model_callbacks = self.before_model_callbacks.clone();
        let after_model_callbacks = self.after_model_callbacks.clone();
        let before_tool_callbacks = self.before_tool_callbacks.clone();
        let after_tool_callbacks = self.after_tool_callbacks.clone();
        let tool_confirmation_policy = self.tool_confirmation_policy.clone();

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

            // ===== PROCESS SKILL CONTEXT =====
            // If skills are configured, select the most relevant skill from user input
            // and inject it as a compact instruction block before other prompts.
            if let Some(index) = &skills_index {
                let user_query = ctx
                    .user_content()
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if let Some((_matched, skill_block)) = select_skill_prompt_block(
                    index.as_ref(),
                    &user_query,
                    &skill_policy,
                    max_skill_chars,
                ) {
                    conversation_history.push(Content {
                        role: "user".to_string(),
                        parts: vec![Part::Text { text: skill_block }],
                    });
                }
            }

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
            // NOTE: Session history already includes the current user message (added by Runner before agent runs)
            let session_history = ctx.session().conversation_history();
            conversation_history.extend(session_history);

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

            // Build tool lookup map for O(1) access (instead of linear scan)
            let tool_map: std::collections::HashMap<String, Arc<dyn Tool>> = tools
                .iter()
                .map(|t| (t.name().to_string(), t.clone()))
                .collect();

            // Helper: extract long-running tool IDs from content
            let collect_long_running_ids = |content: &Content| -> Vec<String> {
                content.parts.iter()
                    .filter_map(|p| {
                        if let Part::FunctionCall { name, .. } = p {
                            if let Some(tool) = tool_map.get(name) {
                                if tool.is_long_running() {
                                    return Some(name.clone());
                                }
                            }
                        }
                        None
                    })
                    .collect()
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
                // Merge agent-level generate_content_config with output_schema.
                // Agent-level config provides defaults (temperature, top_p, etc.),
                // output_schema is layered on top as response_schema.
                // If the runner set a cached_content name (via automatic cache lifecycle),
                // merge it into the config so the provider can reuse cached content.
                let config = match (&generate_content_config, &output_schema) {
                    (Some(base), Some(schema)) => {
                        let mut merged = base.clone();
                        merged.response_schema = Some(schema.clone());
                        Some(merged)
                    }
                    (Some(base), None) => Some(base.clone()),
                    (None, Some(schema)) => Some(adk_core::GenerateContentConfig {
                        response_schema: Some(schema.clone()),
                        ..Default::default()
                    }),
                    (None, None) => None,
                };

                // Layer cached_content from RunConfig onto the request config.
                let config = if let Some(ref cached) = ctx.run_config().cached_content {
                    let mut cfg = config.unwrap_or_default();
                    // Only set if the agent hasn't already specified one
                    if cfg.cached_content.is_none() {
                        cfg.cached_content = Some(cached.clone());
                    }
                    Some(cfg)
                } else {
                    config
                };

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

                if let Some(cached_response) = model_response_override {
                    // Use callback-provided response (e.g., from cache)
                    // Yield it as an event
                    let mut cached_event = Event::new(&invocation_id);
                    cached_event.author = agent_name.clone();
                    cached_event.llm_response.content = cached_response.content.clone();
                    cached_event.llm_request = Some(serde_json::to_string(&request).unwrap_or_default());
                    cached_event.provider_metadata.insert("gcp.vertex.agent.llm_request".to_string(), serde_json::to_string(&request).unwrap_or_default());
                    cached_event.provider_metadata.insert("gcp.vertex.agent.llm_response".to_string(), serde_json::to_string(&cached_response).unwrap_or_default());

                    // Populate long_running_tool_ids for function calls from long-running tools
                    if let Some(ref content) = cached_response.content {
                        cached_event.long_running_tool_ids = collect_long_running_ids(content);
                    }

                    yield Ok(cached_event);

                    accumulated_content = cached_response.content;
                } else {
                    // Record LLM request for tracing
                    let request_json = serde_json::to_string(&request).unwrap_or_default();

                    // Create call_llm span with GCP attributes (works for all model types)
                    let llm_ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos();
                    let llm_event_id = format!("{}_llm_{}", invocation_id, llm_ts);
                    let llm_span = tracing::info_span!(
                        "call_llm",
                        "gcp.vertex.agent.event_id" = %llm_event_id,
                        "gcp.vertex.agent.invocation_id" = %invocation_id,
                        "gcp.vertex.agent.session_id" = %ctx.session_id(),
                        "gen_ai.conversation.id" = %ctx.session_id(),
                        "gcp.vertex.agent.llm_request" = %request_json,
                        "gcp.vertex.agent.llm_response" = tracing::field::Empty  // Placeholder for later recording
                    );
                    let _llm_guard = llm_span.enter();

                    // Check streaming mode from run config
                    use adk_core::StreamingMode;
                    let streaming_mode = ctx.run_config().streaming_mode;
                    let should_stream_to_client = matches!(streaming_mode, StreamingMode::SSE | StreamingMode::Bidi);

                    // Always use streaming internally for LLM calls
                    let mut response_stream = model.generate_content(request, true).await?;

                    use futures::StreamExt;

                    // Track last chunk for final event metadata (used in None mode)
                    let mut last_chunk: Option<LlmResponse> = None;

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

                        // Accumulate content for conversation history (always needed)
                        if let Some(chunk_content) = chunk.content.clone() {
                            if let Some(ref mut acc) = accumulated_content {
                                acc.parts.extend(chunk_content.parts);
                            } else {
                                accumulated_content = Some(chunk_content);
                            }
                        }

                        // For SSE/Bidi mode: yield each chunk immediately with stable event ID
                        if should_stream_to_client {
                            let mut partial_event = Event::with_id(&llm_event_id, &invocation_id);
                            partial_event.author = agent_name.clone();
                            partial_event.llm_request = Some(request_json.clone());
                            partial_event.provider_metadata.insert("gcp.vertex.agent.llm_request".to_string(), request_json.clone());
                            partial_event.provider_metadata.insert("gcp.vertex.agent.llm_response".to_string(), serde_json::to_string(&chunk).unwrap_or_default());
                            partial_event.llm_response.partial = chunk.partial;
                            partial_event.llm_response.turn_complete = chunk.turn_complete;
                            partial_event.llm_response.finish_reason = chunk.finish_reason;
                            partial_event.llm_response.usage_metadata = chunk.usage_metadata.clone();
                            partial_event.llm_response.content = chunk.content.clone();

                            // Populate long_running_tool_ids
                            if let Some(ref content) = chunk.content {
                                partial_event.long_running_tool_ids = collect_long_running_ids(content);
                            }

                            yield Ok(partial_event);
                        }

                        // Store last chunk for final event metadata
                        last_chunk = Some(chunk.clone());

                        // Check if turn is complete
                        if chunk.turn_complete {
                            break;
                        }
                    }

                    // For None mode: yield single final event with accumulated content
                    if !should_stream_to_client {
                        let mut final_event = Event::with_id(&llm_event_id, &invocation_id);
                        final_event.author = agent_name.clone();
                        final_event.llm_request = Some(request_json.clone());
                        final_event.provider_metadata.insert("gcp.vertex.agent.llm_request".to_string(), request_json.clone());
                        final_event.llm_response.content = accumulated_content.clone();
                        final_event.llm_response.partial = false;
                        final_event.llm_response.turn_complete = true;

                        // Copy metadata from last chunk
                        if let Some(ref last) = last_chunk {
                            final_event.llm_response.finish_reason = last.finish_reason;
                            final_event.llm_response.usage_metadata = last.usage_metadata.clone();
                            final_event.provider_metadata.insert("gcp.vertex.agent.llm_response".to_string(), serde_json::to_string(last).unwrap_or_default());
                        }

                        // Populate long_running_tool_ids
                        if let Some(ref content) = accumulated_content {
                            final_event.long_running_tool_ids = collect_long_running_ids(content);
                        }

                        yield Ok(final_event);
                    }

                    // Record LLM response to span before guard drops
                    if let Some(ref content) = accumulated_content {
                        let response_json = serde_json::to_string(content).unwrap_or_default();
                        llm_span.record("gcp.vertex.agent.llm_response", &response_json);
                    }
                }

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
                    tool_map.get(name)
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
                    // Record LLM response for tracing
                    if let Some(ref content) = accumulated_content {
                        let response_json = serde_json::to_string(content).unwrap_or_default();
                        tracing::Span::current().record("gcp.vertex.agent.llm_response", &response_json);
                    }

                    tracing::info!(agent.name = %agent_name, "Agent execution complete");
                    break;
                }

                // Execute function calls and add responses to history
                if let Some(content) = &accumulated_content {
                    let mut tool_call_index = 0usize;
                    for part in &content.parts {
                        if let Part::FunctionCall { name, args, id, .. } = part {
                            let fallback_call_id =
                                format!("{}_{}_{}", invocation_id, name, tool_call_index);
                            tool_call_index += 1;
                            let function_call_id = id.clone().unwrap_or(fallback_call_id);

                            // Handle transfer_to_agent specially
                            if name == "transfer_to_agent" {
                                let target_agent = args.get("agent_name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();

                                // Validate that the target agent exists in sub_agents
                                let valid_target = sub_agents.iter().any(|a| a.name() == target_agent);
                                if !valid_target {
                                    // Return error to LLM so it can try again
                                    let error_content = Content {
                                        role: "function".to_string(),
                                        parts: vec![Part::FunctionResponse {
                                            function_response: FunctionResponseData {
                                                name: name.clone(),
                                                response: serde_json::json!({
                                                    "error": format!(
                                                        "Agent '{}' not found. Available agents: {:?}",
                                                        target_agent,
                                                        sub_agents.iter().map(|a| a.name()).collect::<Vec<_>>()
                                                    )
                                                }),
                                            },
                                            id: id.clone(),
                                        }],
                                    };
                                    conversation_history.push(error_content.clone());

                                    let mut error_event = Event::new(&invocation_id);
                                    error_event.author = agent_name.clone();
                                    error_event.llm_response.content = Some(error_content);
                                    yield Ok(error_event);
                                    continue;
                                }

                                let mut transfer_event = Event::new(&invocation_id);
                                transfer_event.author = agent_name.clone();
                                transfer_event.actions.transfer_to_agent = Some(target_agent);

                                yield Ok(transfer_event);
                                return;
                            }

                            // ===== BEFORE TOOL CALLBACKS =====
                            // Allows policy checks or callback-provided short-circuit responses.
                            let mut tool_actions = EventActions::default();
                            let mut response_content: Option<Content> = None;
                            let mut run_after_tool_callbacks = true;

                            // ===== TOOL CONFIRMATION POLICY =====
                            // If configured and no decision is provided, emit an interrupt event
                            // before execution. Callers can resume by re-running with a decision
                            // in RunConfig.tool_confirmation_decisions.
                            if tool_confirmation_policy.requires_confirmation(name) {
                                match ctx.run_config().tool_confirmation_decisions.get(name).copied()
                                {
                                    Some(ToolConfirmationDecision::Approve) => {
                                        tool_actions.tool_confirmation_decision =
                                            Some(ToolConfirmationDecision::Approve);
                                    }
                                    Some(ToolConfirmationDecision::Deny) => {
                                        tool_actions.tool_confirmation_decision =
                                            Some(ToolConfirmationDecision::Deny);
                                        response_content = Some(Content {
                                            role: "function".to_string(),
                                            parts: vec![Part::FunctionResponse {
                                                function_response: FunctionResponseData {
                                                    name: name.clone(),
                                                    response: serde_json::json!({
                                                        "error": format!(
                                                            "Tool '{}' execution denied by confirmation policy",
                                                            name
                                                        ),
                                                    }),
                                                },
                                                id: id.clone(),
                                            }],
                                        });
                                        run_after_tool_callbacks = false;
                                    }
                                    None => {
                                        let mut confirmation_event = Event::new(&invocation_id);
                                        confirmation_event.author = agent_name.clone();
                                        confirmation_event.llm_response.interrupted = true;
                                        confirmation_event.llm_response.turn_complete = true;
                                        confirmation_event.llm_response.content = Some(Content {
                                            role: "model".to_string(),
                                            parts: vec![Part::Text {
                                                text: format!(
                                                    "Tool confirmation required for '{}'. Provide approve/deny decision to continue.",
                                                    name
                                                ),
                                            }],
                                        });
                                        confirmation_event.actions.tool_confirmation =
                                            Some(ToolConfirmationRequest {
                                                tool_name: name.clone(),
                                                function_call_id: Some(function_call_id),
                                                args: args.clone(),
                                            });
                                        yield Ok(confirmation_event);
                                        return;
                                    }
                                }
                            }

                            if response_content.is_none() {
                                for callback in before_tool_callbacks.as_ref() {
                                    match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                                        Ok(Some(content)) => {
                                            response_content = Some(content);
                                            break;
                                        }
                                        Ok(None) => continue,
                                        Err(e) => {
                                            yield Err(e);
                                            return;
                                        }
                                    }
                                }
                            }

                            // Find and execute tool unless callbacks already short-circuited.
                            if response_content.is_none() {
                                if let Some(tool) = tool_map.get(name) {
                                    // ✅ Use AgentToolContext that preserves parent context
                                    let tool_ctx: Arc<dyn ToolContext> = Arc::new(AgentToolContext::new(
                                        ctx.clone(),
                                        format!("{}_{}", invocation_id, name),
                                    ));

                                    // Create span name following adk-go pattern: "execute_tool {name}"
                                    let span_name = format!("execute_tool {}", name);
                                    let tool_span = tracing::info_span!(
                                        "",
                                        otel.name = %span_name,
                                        tool.name = %name,
                                        "gcp.vertex.agent.event_id" = %format!("{}_{}", invocation_id, name),
                                        "gcp.vertex.agent.invocation_id" = %invocation_id,
                                        "gcp.vertex.agent.session_id" = %ctx.session_id(),
                                        "gen_ai.conversation.id" = %ctx.session_id()
                                    );

                                    // Use instrument() for proper async span handling, with timeout
                                    let tool_clone = tool.clone();
                                    let result = async {
                                        tracing::info!(tool.name = %name, tool.args = %args, "tool_call");
                                        let exec_future = tool_clone.execute(tool_ctx.clone(), args.clone());
                                        match tokio::time::timeout(tool_timeout, exec_future).await {
                                            Ok(Ok(result)) => {
                                                tracing::info!(tool.name = %name, tool.result = %result, "tool_result");
                                                result
                                            }
                                            Ok(Err(e)) => {
                                                tracing::warn!(tool.name = %name, error = %e, "tool_error");
                                                serde_json::json!({ "error": e.to_string() })
                                            }
                                            Err(_) => {
                                                tracing::warn!(tool.name = %name, timeout_secs = tool_timeout.as_secs(), "tool_timeout");
                                                serde_json::json!({
                                                    "error": format!(
                                                        "Tool '{}' timed out after {} seconds",
                                                        name, tool_timeout.as_secs()
                                                    )
                                                })
                                            }
                                        }
                                    }.instrument(tool_span).await;

                                    let confirmation_decision =
                                        tool_actions.tool_confirmation_decision;
                                    tool_actions = tool_ctx.actions();
                                    if tool_actions.tool_confirmation_decision.is_none() {
                                        tool_actions.tool_confirmation_decision =
                                            confirmation_decision;
                                    }
                                    response_content = Some(Content {
                                        role: "function".to_string(),
                                        parts: vec![Part::FunctionResponse {
                                            function_response: FunctionResponseData {
                                                name: name.clone(),
                                                response: result,
                                            },
                                            id: id.clone(),
                                        }],
                                    });
                                } else {
                                    response_content = Some(Content {
                                        role: "function".to_string(),
                                        parts: vec![Part::FunctionResponse {
                                            function_response: FunctionResponseData {
                                                name: name.clone(),
                                                response: serde_json::json!({
                                                    "error": format!("Tool {} not found", name)
                                                }),
                                            },
                                            id: id.clone(),
                                        }],
                                    });
                                }
                            }

                            // ===== AFTER TOOL CALLBACKS =====
                            // Allows post-processing of tool outputs or audit side effects.
                            let mut response_content = response_content.expect("tool response content is set");
                            if run_after_tool_callbacks {
                                for callback in after_tool_callbacks.as_ref() {
                                    match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                                        Ok(Some(modified_content)) => {
                                            response_content = modified_content;
                                            break;
                                        }
                                        Ok(None) => continue,
                                        Err(e) => {
                                            yield Err(e);
                                            return;
                                        }
                                    }
                                }
                            }

                            // Yield tool execution event
                            let mut tool_event = Event::new(&invocation_id);
                            tool_event.author = agent_name.clone();
                            tool_event.actions = tool_actions.clone();
                            tool_event.llm_response.content = Some(response_content.clone());
                            yield Ok(tool_event);

                            // Check if tool requested escalation or skip_summarization
                            if tool_actions.escalate || tool_actions.skip_summarization {
                                // Tool wants to terminate agent loop
                                return;
                            }

                            // Add function response to history
                            conversation_history.push(response_content);
                        }
                    }
                }

                // If all function calls were from long-running tools, we need ONE more model call
                // to let the model generate a user-friendly response about the pending task
                // But we mark this as the final iteration to prevent infinite loops
                if all_calls_are_long_running {
                    // Continue to next iteration for model to respond, but this will be the last
                    // The model will see the tool response and generate text like "Started task X..."
                    // On next iteration, there won't be function calls, so we'll break naturally
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
