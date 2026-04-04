use adk_core::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, AfterToolCallbackFull, Agent,
    BeforeAgentCallback, BeforeModelCallback, BeforeModelResult, BeforeToolCallback,
    CallbackContext, Content, Event, EventActions, FunctionResponseData, GlobalInstructionProvider,
    InstructionProvider, InvocationContext, Llm, LlmRequest, LlmResponse, MemoryEntry,
    OnToolErrorCallback, Part, ReadonlyContext, Result, RetryBudget, Tool, ToolCallbackContext,
    ToolConfirmationDecision, ToolConfirmationPolicy, ToolConfirmationRequest, ToolContext,
    ToolExecutionStrategy, ToolOutcome, Toolset,
};
use adk_skill::{SelectionPolicy, SkillIndex, load_skill_index, select_skill_prompt_block};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tracing::Instrument;

use crate::{
    guardrails::{GuardrailSet, enforce_guardrails},
    tool_call_markup::normalize_option_content,
    workflow::with_user_content_override,
};

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
    disallow_transfer_to_parent: bool,
    disallow_transfer_to_peers: bool,
    include_contents: adk_core::IncludeContents,
    tools: Vec<Arc<dyn Tool>>,
    #[allow(dead_code)] // Used in runtime toolset resolution (task 2.2)
    toolsets: Vec<Arc<dyn Toolset>>,
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
    on_tool_error_callbacks: Arc<Vec<OnToolErrorCallback>>,
    /// Rich after-tool callbacks that receive tool, args, and response.
    after_tool_callbacks_full: Arc<Vec<AfterToolCallbackFull>>,
    /// Default retry budget applied to all tools without a per-tool override.
    default_retry_budget: Option<RetryBudget>,
    /// Per-tool retry budget overrides, keyed by tool name.
    tool_retry_budgets: std::collections::HashMap<String, RetryBudget>,
    /// Circuit breaker failure threshold. When set, tools are temporarily disabled
    /// after this many consecutive failures within a single invocation.
    circuit_breaker_threshold: Option<u32>,
    tool_confirmation_policy: ToolConfirmationPolicy,
    /// Per-agent tool execution strategy override. When `Some`, overrides the
    /// `RunConfig` strategy for this agent's dispatch loop.
    tool_execution_strategy: Option<ToolExecutionStrategy>,
    input_guardrails: Arc<GuardrailSet>,
    output_guardrails: Arc<GuardrailSet>,
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

impl LlmAgent {
    async fn apply_input_guardrails(
        ctx: Arc<dyn InvocationContext>,
        input_guardrails: Arc<GuardrailSet>,
    ) -> Result<Arc<dyn InvocationContext>> {
        let content =
            enforce_guardrails(input_guardrails.as_ref(), ctx.user_content(), "input").await?;
        if content.role != ctx.user_content().role || content.parts != ctx.user_content().parts {
            Ok(with_user_content_override(ctx, content))
        } else {
            Ok(ctx)
        }
    }

    async fn apply_output_guardrails(
        output_guardrails: &GuardrailSet,
        content: Content,
    ) -> Result<Content> {
        enforce_guardrails(output_guardrails, &content, "output").await
    }

    fn history_parts_from_provider_metadata(
        provider_metadata: Option<&serde_json::Value>,
    ) -> Vec<Part> {
        let Some(provider_metadata) = provider_metadata else {
            return Vec::new();
        };

        let history_parts = provider_metadata
            .get("conversation_history_parts")
            .or_else(|| {
                provider_metadata
                    .get("openai")
                    .and_then(|openai| openai.get("conversation_history_parts"))
            })
            .and_then(serde_json::Value::as_array);

        history_parts
            .into_iter()
            .flatten()
            .filter_map(|value| serde_json::from_value::<Part>(value.clone()).ok())
            .collect()
    }

    fn augment_content_for_history(
        content: &Content,
        provider_metadata: Option<&serde_json::Value>,
    ) -> Content {
        let mut augmented = content.clone();
        augmented.parts.extend(Self::history_parts_from_provider_metadata(provider_metadata));
        augmented
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
    toolsets: Vec<Arc<dyn Toolset>>,
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
    on_tool_error_callbacks: Vec<OnToolErrorCallback>,
    after_tool_callbacks_full: Vec<AfterToolCallbackFull>,
    default_retry_budget: Option<RetryBudget>,
    tool_retry_budgets: std::collections::HashMap<String, RetryBudget>,
    circuit_breaker_threshold: Option<u32>,
    tool_confirmation_policy: ToolConfirmationPolicy,
    tool_execution_strategy: Option<ToolExecutionStrategy>,
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
            toolsets: Vec::new(),
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
            on_tool_error_callbacks: Vec::new(),
            after_tool_callbacks_full: Vec::new(),
            default_retry_budget: None,
            tool_retry_budgets: std::collections::HashMap::new(),
            circuit_breaker_threshold: None,
            tool_confirmation_policy: ToolConfirmationPolicy::Never,
            tool_execution_strategy: None,
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
        let index = load_skill_index(root).map_err(|e| adk_core::AdkError::agent(e.to_string()))?;
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

    /// Register a dynamic toolset for per-invocation tool resolution.
    ///
    /// Toolsets are resolved at the start of each `run()` call using the
    /// invocation's `ReadonlyContext`. This enables context-dependent tools
    /// like per-user browser sessions from a pool.
    pub fn toolset(mut self, toolset: Arc<dyn Toolset>) -> Self {
        self.toolsets.push(toolset);
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

    /// Register a rich after-tool callback that receives the tool, arguments,
    /// and response value.
    ///
    /// This is the V2 callback surface aligned with the Python/Go ADK model
    /// where `after_tool_callback` receives the full tool execution context.
    /// Unlike [`after_tool_callback`](Self::after_tool_callback) (which only
    /// receives `CallbackContext`), this callback can inspect and modify tool
    /// results directly.
    ///
    /// Return `Ok(None)` to keep the original response, or `Ok(Some(value))`
    /// to replace the function response sent to the LLM.
    ///
    /// These callbacks run after the legacy `after_tool_callback` chain.
    /// `ToolOutcome` is available via `ctx.tool_outcome()`.
    pub fn after_tool_callback_full(mut self, callback: AfterToolCallbackFull) -> Self {
        self.after_tool_callbacks_full.push(callback);
        self
    }

    /// Register a callback invoked when a tool execution fails
    /// (after retries are exhausted).
    ///
    /// If the callback returns `Ok(Some(value))`, the value is used as a
    /// fallback function response to the LLM. If it returns `Ok(None)`,
    /// the next callback in the chain is tried. If no callback provides a
    /// fallback, the original error is reported to the LLM.
    pub fn on_tool_error(mut self, callback: OnToolErrorCallback) -> Self {
        self.on_tool_error_callbacks.push(callback);
        self
    }

    /// Set a default retry budget applied to all tools that do not have
    /// a per-tool override.
    ///
    /// When a tool execution fails and a retry budget applies, the agent
    /// retries up to `budget.max_retries` times with the configured delay
    /// between attempts.
    pub fn default_retry_budget(mut self, budget: RetryBudget) -> Self {
        self.default_retry_budget = Some(budget);
        self
    }

    /// Set a per-tool retry budget that overrides the default for the
    /// named tool.
    ///
    /// Per-tool budgets take precedence over the default retry budget.
    pub fn tool_retry_budget(mut self, tool_name: impl Into<String>, budget: RetryBudget) -> Self {
        self.tool_retry_budgets.insert(tool_name.into(), budget);
        self
    }

    /// Configure a circuit breaker that temporarily disables tools after
    /// `threshold` consecutive failures within a single invocation.
    ///
    /// When a tool's consecutive failure count reaches the threshold, subsequent
    /// calls to that tool are short-circuited with an immediate error response
    /// until the next invocation (which resets the state).
    pub fn circuit_breaker_threshold(mut self, threshold: u32) -> Self {
        self.circuit_breaker_threshold = Some(threshold);
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

    /// Set the tool execution strategy for this agent.
    ///
    /// When set, this overrides the `RunConfig`'s `tool_execution_strategy`
    /// for this agent's dispatch loop. When `None` (the default), the
    /// `RunConfig` value is used.
    pub fn tool_execution_strategy(mut self, strategy: ToolExecutionStrategy) -> Self {
        self.tool_execution_strategy = Some(strategy);
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
        let model = self.model.ok_or_else(|| adk_core::AdkError::agent("Model is required"))?;

        let mut seen_names = std::collections::HashSet::new();
        for agent in &self.sub_agents {
            if !seen_names.insert(agent.name()) {
                return Err(adk_core::AdkError::agent(format!(
                    "Duplicate sub-agent name: {}",
                    agent.name()
                )));
            }
        }

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
            toolsets: self.toolsets,
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
            on_tool_error_callbacks: Arc::new(self.on_tool_error_callbacks),
            after_tool_callbacks_full: Arc::new(self.after_tool_callbacks_full),
            default_retry_budget: self.default_retry_budget,
            tool_retry_budgets: self.tool_retry_budgets,
            circuit_breaker_threshold: self.circuit_breaker_threshold,
            tool_confirmation_policy: self.tool_confirmation_policy,
            tool_execution_strategy: self.tool_execution_strategy,
            input_guardrails: Arc::new(self.input_guardrails),
            output_guardrails: Arc::new(self.output_guardrails),
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

    fn actions_guard(&self) -> std::sync::MutexGuard<'_, EventActions> {
        self.actions.lock().unwrap_or_else(|e| e.into_inner())
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
        self.actions_guard().clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions_guard() = actions;
    }

    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>> {
        // ✅ Delegate to parent's memory if available
        if let Some(memory) = self.parent_ctx.memory() {
            memory.search(query).await
        } else {
            Ok(vec![])
        }
    }

    fn user_scopes(&self) -> Vec<String> {
        self.parent_ctx.user_scopes()
    }
}

/// Wrapper that adds ToolOutcome to an existing CallbackContext.
/// Used only during after-tool callback invocation so callbacks
/// can inspect structured metadata about the completed tool execution.
struct ToolOutcomeCallbackContext {
    inner: Arc<dyn CallbackContext>,
    outcome: ToolOutcome,
}

#[async_trait]
impl ReadonlyContext for ToolOutcomeCallbackContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.inner.agent_name()
    }

    fn user_id(&self) -> &str {
        self.inner.user_id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn session_id(&self) -> &str {
        self.inner.session_id()
    }

    fn branch(&self) -> &str {
        self.inner.branch()
    }

    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl CallbackContext for ToolOutcomeCallbackContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        self.inner.artifacts()
    }

    fn tool_outcome(&self) -> Option<ToolOutcome> {
        Some(self.outcome.clone())
    }
}

/// Per-invocation circuit breaker state.
///
/// Tracks consecutive failures per tool name within a single agent
/// invocation. When a tool's consecutive failure count reaches the
/// configured threshold the breaker "opens" and subsequent calls to
/// that tool are short-circuited with an immediate error response.
///
/// The state is created fresh at the start of each `run()` call so
/// it automatically resets between invocations.
struct CircuitBreakerState {
    threshold: u32,
    /// tool_name → consecutive failure count
    failures: std::collections::HashMap<String, u32>,
}

impl CircuitBreakerState {
    fn new(threshold: u32) -> Self {
        Self { threshold, failures: std::collections::HashMap::new() }
    }

    /// Returns `true` if the tool is currently tripped (open state).
    fn is_open(&self, tool_name: &str) -> bool {
        self.failures.get(tool_name).copied().unwrap_or(0) >= self.threshold
    }

    /// Record a tool outcome. Resets count on success, increments on failure.
    fn record(&mut self, outcome: &ToolOutcome) {
        if outcome.success {
            self.failures.remove(&outcome.tool_name);
        } else {
            let count = self.failures.entry(outcome.tool_name.clone()).or_insert(0);
            *count += 1;
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
        let ctx = Self::apply_input_guardrails(ctx, self.input_guardrails.clone()).await?;

        let agent_name = self.name.clone();
        let invocation_id = ctx.invocation_id().to_string();
        let model = self.model.clone();
        let tools = self.tools.clone();
        let toolsets = self.toolsets.clone();
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
        let on_tool_error_callbacks = self.on_tool_error_callbacks.clone();
        let after_tool_callbacks_full = self.after_tool_callbacks_full.clone();
        let default_retry_budget = self.default_retry_budget.clone();
        let tool_retry_budgets = self.tool_retry_budgets.clone();
        let circuit_breaker_threshold = self.circuit_breaker_threshold;
        let tool_confirmation_policy = self.tool_confirmation_policy.clone();
        let disallow_transfer_to_parent = self.disallow_transfer_to_parent;
        let disallow_transfer_to_peers = self.disallow_transfer_to_peers;
        let output_guardrails = self.output_guardrails.clone();
        let agent_tool_execution_strategy = self.tool_execution_strategy;

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
            let mut prompt_preamble = Vec::new();

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
                    prompt_preamble.push(Content {
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
                    prompt_preamble.push(Content {
                        role: "user".to_string(),
                        parts: vec![Part::Text { text: global_inst }],
                    });
                }
            } else if let Some(ref template) = global_instruction {
                // Static global instruction with template injection
                let processed = adk_core::inject_session_state(ctx.as_ref(), template).await?;
                if !processed.is_empty() {
                    prompt_preamble.push(Content {
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
                    prompt_preamble.push(Content {
                        role: "user".to_string(),
                        parts: vec![Part::Text { text: inst }],
                    });
                }
            } else if let Some(ref template) = instruction {
                // Static instruction with template injection
                let processed = adk_core::inject_session_state(ctx.as_ref(), template).await?;
                if !processed.is_empty() {
                    prompt_preamble.push(Content {
                        role: "user".to_string(),
                        parts: vec![Part::Text { text: processed }],
                    });
                }
            }

            // ===== LOAD SESSION HISTORY =====
            // Load previous conversation turns from the session
            // NOTE: Session history already includes the current user message (added by Runner before agent runs)
            // When transfer_targets is set, this agent was invoked via transfer — filter out
            // other agents' events so the LLM doesn't see the parent's tool calls as its own.
            let session_history = if !ctx.run_config().transfer_targets.is_empty() {
                ctx.session().conversation_history_for_agent(&agent_name)
            } else {
                ctx.session().conversation_history()
            };
            let mut session_history = session_history;
            let current_user_content = ctx.user_content().clone();
            if let Some(index) = session_history.iter().rposition(|content| content.role == "user") {
                session_history[index] = current_user_content.clone();
            } else {
                session_history.push(current_user_content.clone());
            }

            // ===== APPLY INCLUDE_CONTENTS FILTERING =====
            // Control what conversation history the agent sees
            let mut conversation_history = match include_contents {
                adk_core::IncludeContents::None => {
                    let mut filtered = prompt_preamble.clone();
                    filtered.push(current_user_content);
                    filtered
                }
                adk_core::IncludeContents::Default => {
                    let mut full_history = prompt_preamble;
                    full_history.extend(session_history);
                    full_history
                }
            };

            // ===== RESOLVE TOOLSETS =====
            // Start with static tools, then merge in toolset-provided tools
            let mut resolved_tools: Vec<Arc<dyn Tool>> = tools.clone();
            let static_tool_names: std::collections::HashSet<String> =
                tools.iter().map(|t| t.name().to_string()).collect();

            // Track which toolset provided each tool for deterministic error messages
            let mut toolset_source: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            for toolset in &toolsets {
                let toolset_tools = match toolset
                    .tools(ctx.clone() as Arc<dyn ReadonlyContext>)
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                for tool in &toolset_tools {
                    let name = tool.name().to_string();
                    // Check static-vs-toolset conflict
                    if static_tool_names.contains(&name) {
                        yield Err(adk_core::AdkError::agent(format!(
                            "Duplicate tool name '{name}': conflict between static tool and toolset '{}'",
                            toolset.name()
                        )));
                        return;
                    }
                    // Check toolset-vs-toolset conflict
                    if let Some(other_toolset_name) = toolset_source.get(&name) {
                        yield Err(adk_core::AdkError::agent(format!(
                            "Duplicate tool name '{name}': conflict between toolset '{}' and toolset '{}'",
                            other_toolset_name,
                            toolset.name()
                        )));
                        return;
                    }
                    toolset_source.insert(name, toolset.name().to_string());
                    resolved_tools.push(tool.clone());
                }
            }

            // Build tool lookup map for O(1) access from merged resolved_tools
            let tool_map: std::collections::HashMap<String, Arc<dyn Tool>> = resolved_tools
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
            // Uses Tool::declaration() so provider-native built-ins can attach
            // adapter-specific metadata while regular function tools retain the
            // standard name/description/schema shape.
            let mut tool_declarations = std::collections::HashMap::new();
            for tool in &resolved_tools {
                tool_declarations.insert(tool.name().to_string(), tool.declaration());
            }

            // Build the list of valid transfer targets.
            // Sources: sub_agents (always) + transfer_targets from RunConfig
            // (set by the runner to include parent/peers for transferred agents).
            // Apply disallow_transfer_to_parent / disallow_transfer_to_peers filtering.
            let mut valid_transfer_targets: Vec<String> = sub_agents
                .iter()
                .map(|a| a.name().to_string())
                .collect();

            // Merge in runner-provided targets (parent, peers) from RunConfig
            let run_config_targets = &ctx.run_config().transfer_targets;
            let parent_agent_name = ctx.run_config().parent_agent.clone();
            let sub_agent_names: std::collections::HashSet<&str> = sub_agents
                .iter()
                .map(|a| a.name())
                .collect();

            for target in run_config_targets {
                // Skip if already in the list (from sub_agents)
                if sub_agent_names.contains(target.as_str()) {
                    continue;
                }

                // Apply disallow flags
                let is_parent = parent_agent_name.as_deref() == Some(target.as_str());
                if is_parent && disallow_transfer_to_parent {
                    continue;
                }
                if !is_parent && disallow_transfer_to_peers {
                    continue;
                }

                valid_transfer_targets.push(target.clone());
            }

            // Inject transfer_to_agent tool if there are valid targets
            if !valid_transfer_targets.is_empty() {
                let transfer_tool_name = "transfer_to_agent";
                let transfer_tool_decl = serde_json::json!({
                    "name": transfer_tool_name,
                    "description": format!(
                        "Transfer execution to another agent. Valid targets: {}",
                        valid_transfer_targets.join(", ")
                    ),
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "agent_name": {
                                "type": "string",
                                "description": "The name of the agent to transfer to.",
                                "enum": valid_transfer_targets
                            }
                        },
                        "required": ["agent_name"]
                    }
                });
                tool_declarations.insert(transfer_tool_name.to_string(), transfer_tool_decl);
            }


            // ===== CIRCUIT BREAKER STATE =====
            // Created fresh per invocation so it resets between runs.
            let mut circuit_breaker_state = circuit_breaker_threshold.map(CircuitBreakerState::new);

            // Multi-turn loop with max iterations
            let mut iteration = 0;

            loop {
                iteration += 1;
                if iteration > max_iterations {
                    yield Err(adk_core::AdkError::agent(
                        format!("Max iterations ({max_iterations}) exceeded")
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
                let mut final_provider_metadata: Option<serde_json::Value> = None;

                if let Some(cached_response) = model_response_override {
                    // Use callback-provided response (e.g., from cache)
                    // Yield it as an event
                    accumulated_content = cached_response.content.clone();
                    final_provider_metadata = cached_response.provider_metadata.clone();
                    normalize_option_content(&mut accumulated_content);
                    if let Some(content) = accumulated_content.take() {
                        let has_function_calls = content
                            .parts
                            .iter()
                            .any(|part| matches!(part, Part::FunctionCall { .. }));
                        let content = if has_function_calls {
                            content
                        } else {
                            Self::apply_output_guardrails(output_guardrails.as_ref(), content).await?
                        };
                        accumulated_content = Some(content);
                    }

                    let mut cached_event = Event::new(&invocation_id);
                    cached_event.author = agent_name.clone();
                    cached_event.llm_response.content = accumulated_content.clone();
                    cached_event.llm_response.provider_metadata = cached_response.provider_metadata.clone();
                    cached_event.llm_request = Some(serde_json::to_string(&request).unwrap_or_default());
                    cached_event.provider_metadata.insert("gcp.vertex.agent.llm_request".to_string(), serde_json::to_string(&request).unwrap_or_default());
                    cached_event.provider_metadata.insert("gcp.vertex.agent.llm_response".to_string(), serde_json::to_string(&cached_response).unwrap_or_default());

                    // Populate long_running_tool_ids for function calls from long-running tools
                    if let Some(ref content) = accumulated_content {
                        cached_event.long_running_tool_ids = collect_long_running_ids(content);
                    }

                    yield Ok(cached_event);
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
                    let should_stream_to_client = matches!(streaming_mode, StreamingMode::SSE | StreamingMode::Bidi)
                        && output_guardrails.is_empty();

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

                        normalize_option_content(&mut chunk.content);

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
                            partial_event.llm_response.provider_metadata = chunk.provider_metadata.clone();

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
                        if let Some(content) = accumulated_content.take() {
                            let has_function_calls = content
                                .parts
                                .iter()
                                .any(|part| matches!(part, Part::FunctionCall { .. }));
                            let content = if has_function_calls {
                                content
                            } else {
                                Self::apply_output_guardrails(output_guardrails.as_ref(), content).await?
                            };
                            accumulated_content = Some(content);
                        }

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
                            final_event.llm_response.provider_metadata = last.provider_metadata.clone();
                            final_provider_metadata = last.provider_metadata.clone();
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
                    conversation_history.push(Self::augment_content_for_history(
                        content,
                        final_provider_metadata.as_ref(),
                    ));

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
                    // ===== RESOLVE TOOL EXECUTION STRATEGY =====
                    // Per-agent override; defaults to Sequential if not set.
                    let strategy = agent_tool_execution_strategy
                        .unwrap_or(ToolExecutionStrategy::Sequential);

                    // Collect function call parts with original indices for
                    // order-preserving reassembly in parallel/auto modes.
                    // Tuple: (index, name, args, id, function_call_id)
                    let mut fc_parts: Vec<(usize, String, serde_json::Value, Option<String>, String)> = Vec::new();
                    {
                        let mut tci = 0usize;
                        for part in &content.parts {
                            if let Part::FunctionCall { name, args, id, .. } = part {
                                let fallback = format!("{}_{}_{}", invocation_id, name, tci);
                                let fcid = id.clone().unwrap_or(fallback);
                                fc_parts.push((tci, name.clone(), args.clone(), id.clone(), fcid));
                                tci += 1;
                            }
                        }
                    }

                    // ===== HANDLE transfer_to_agent BEFORE DISPATCH =====
                    // Transfer calls cause an immediate return from the stream,
                    // so they must be handled inline regardless of strategy.
                    let mut transfer_handled = false;
                    for (_, fc_name, fc_args, fc_id, _) in &fc_parts {
                        if fc_name == "transfer_to_agent" {
                            let target_agent = fc_args.get("agent_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();

                            let valid_target = valid_transfer_targets.iter().any(|n| n == &target_agent);
                            if !valid_target {
                                let error_content = Content {
                                    role: "function".to_string(),
                                    parts: vec![Part::FunctionResponse {
                                        function_response: FunctionResponseData {
                                            name: fc_name.clone(),
                                            response: serde_json::json!({
                                                "error": format!(
                                                    "Agent '{}' not found. Available agents: {:?}",
                                                    target_agent, valid_transfer_targets
                                                )
                                            }),
                                        },
                                        id: fc_id.clone(),
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
                            transfer_handled = true;
                            break;
                        }
                    }
                    if transfer_handled {
                        return;
                    }

                    // Filter out transfer_to_agent and built-in tools
                    let fc_parts: Vec<_> = fc_parts.into_iter().filter(|(_, fc_name, _, _, _)| {
                        if fc_name == "transfer_to_agent" {
                            return false;
                        }
                        if let Some(tool) = tool_map.get(fc_name) {
                            if tool.is_builtin() {
                                adk_telemetry::debug!(tool.name = %fc_name, "skipping built-in tool execution");
                                return false;
                            }
                        }
                        true
                    }).collect();

                    // ===== TOOL CONFIRMATION PRE-CHECK =====
                    // Tool confirmation interrupts cause an immediate return,
                    // so check before parallel dispatch.
                    let mut confirmation_interrupted = false;
                    for (_, fc_name, fc_args, _, fc_call_id) in &fc_parts {
                        if tool_confirmation_policy.requires_confirmation(fc_name)
                            && ctx.run_config().tool_confirmation_decisions.get(fc_name).copied().is_none()
                        {
                                let mut ce = Event::new(&invocation_id);
                                ce.author = agent_name.clone();
                                ce.llm_response.interrupted = true;
                                ce.llm_response.turn_complete = true;
                                ce.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::Text {
                                        text: format!(
                                            "Tool confirmation required for '{}'. Provide approve/deny decision to continue.",
                                            fc_name
                                        ),
                                    }],
                                });
                                ce.actions.tool_confirmation = Some(ToolConfirmationRequest {
                                    tool_name: fc_name.clone(),
                                    function_call_id: Some(fc_call_id.clone()),
                                    args: fc_args.clone(),
                                });
                                yield Ok(ce);
                                confirmation_interrupted = true;
                                break;
                        }
                    }
                    if confirmation_interrupted {
                        return;
                    }

                    // Wrap circuit breaker in Mutex for shared access across parallel futures.
                    let cb_mutex = std::sync::Mutex::new(circuit_breaker_state.take());

                    // Per-tool execution async block. Returns (index, Content, EventActions, escalate_or_skip).
                    // Each tool retains its own retry budget, circuit breaker, tracing span,
                    // before/after callbacks, and error handling. Errors are captured as
                    // { "error": "..." } JSON — failed tools do not abort the batch.
                    let execute_one_tool = |idx: usize, name: String, args: serde_json::Value,
                                            id: Option<String>, function_call_id: String| {
                        let ctx = ctx.clone();
                        let tool_map = &tool_map;
                        let tool_retry_budgets = &tool_retry_budgets;
                        let default_retry_budget = &default_retry_budget;
                        let before_tool_callbacks = &before_tool_callbacks;
                        let after_tool_callbacks = &after_tool_callbacks;
                        let after_tool_callbacks_full = &after_tool_callbacks_full;
                        let on_tool_error_callbacks = &on_tool_error_callbacks;
                        let tool_confirmation_policy = &tool_confirmation_policy;
                        let cb_mutex = &cb_mutex;
                        let invocation_id = &invocation_id;
                        async move {
                            let mut tool_actions = EventActions::default();
                            let mut response_content: Option<Content> = None;
                            let mut run_after_tool_callbacks = true;
                            let mut tool_outcome_for_callback: Option<ToolOutcome> = None;
                            let mut executed_tool: Option<Arc<dyn Tool>> = None;
                            let mut executed_tool_response: Option<serde_json::Value> = None;

                            // Tool confirmation (deny case; None handled by pre-check)
                            if tool_confirmation_policy.requires_confirmation(&name) {
                                match ctx.run_config().tool_confirmation_decisions.get(&name).copied() {
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
                                                        "error": format!("Tool '{}' execution denied by confirmation policy", name)
                                                    }),
                                                },
                                                id: id.clone(),
                                            }],
                                        });
                                        run_after_tool_callbacks = false;
                                    }
                                    None => {
                                        response_content = Some(Content {
                                            role: "function".to_string(),
                                            parts: vec![Part::FunctionResponse {
                                                function_response: FunctionResponseData {
                                                    name: name.clone(),
                                                    response: serde_json::json!({
                                                        "error": format!("Tool '{}' requires confirmation", name)
                                                    }),
                                                },
                                                id: id.clone(),
                                            }],
                                        });
                                        run_after_tool_callbacks = false;
                                    }
                                }
                            }

                            // Before-tool callbacks
                            if response_content.is_none() {
                                let tool_ctx = Arc::new(ToolCallbackContext::new(
                                    ctx.clone(),
                                    name.clone(),
                                    args.clone(),
                                ));
                                for callback in before_tool_callbacks.as_ref() {
                                    match callback(tool_ctx.clone() as Arc<dyn CallbackContext>).await {
                                        Ok(Some(c)) => { response_content = Some(c); break; }
                                        Ok(None) => continue,
                                        Err(e) => {
                                            response_content = Some(Content {
                                                role: "function".to_string(),
                                                parts: vec![Part::FunctionResponse {
                                                    function_response: FunctionResponseData {
                                                        name: name.clone(),
                                                        response: serde_json::json!({ "error": e.to_string() }),
                                                    },
                                                    id: id.clone(),
                                                }],
                                            });
                                            run_after_tool_callbacks = false;
                                            break;
                                        }
                                    }
                                }
                            }

                            // Circuit breaker check
                            if response_content.is_none() {
                                let guard = cb_mutex.lock().unwrap_or_else(|e| e.into_inner());
                                if let Some(ref cb_state) = *guard {
                                    if cb_state.is_open(&name) {
                                        let msg = format!(
                                            "Tool '{}' is temporarily disabled after {} consecutive failures",
                                            name, cb_state.threshold
                                        );
                                        tracing::warn!(tool.name = %name, "circuit breaker open, skipping tool execution");
                                        response_content = Some(Content {
                                            role: "function".to_string(),
                                            parts: vec![Part::FunctionResponse {
                                                function_response: FunctionResponseData {
                                                    name: name.clone(),
                                                    response: serde_json::json!({ "error": msg }),
                                                },
                                                id: id.clone(),
                                            }],
                                        });
                                        run_after_tool_callbacks = false;
                                    }
                                }
                                drop(guard);
                            }

                            // Execute tool with retry budget and tracing
                            if response_content.is_none() {
                                if let Some(tool) = tool_map.get(&name) {
                                    let tool_ctx: Arc<dyn ToolContext> = Arc::new(
                                        AgentToolContext::new(ctx.clone(), function_call_id.clone()),
                                    );
                                    let span_name = format!("execute_tool {name}");
                                    let tool_span = tracing::info_span!(
                                        "",
                                        otel.name = %span_name,
                                        tool.name = %name,
                                        "gcp.vertex.agent.event_id" = %format!("{}_{}", invocation_id, name),
                                        "gcp.vertex.agent.invocation_id" = %invocation_id,
                                        "gcp.vertex.agent.session_id" = %ctx.session_id(),
                                        "gen_ai.conversation.id" = %ctx.session_id()
                                    );

                                    let budget = tool_retry_budgets.get(&name)
                                        .or(default_retry_budget.as_ref());
                                    let max_attempts = budget.map(|b| b.max_retries + 1).unwrap_or(1);
                                    let retry_delay = budget.map(|b| b.delay).unwrap_or_default();

                                    let tool_clone = tool.clone();
                                    let tool_start = std::time::Instant::now();
                                    let mut last_error = String::new();
                                    let mut final_attempt: u32 = 0;
                                    let mut retry_result: Option<serde_json::Value> = None;

                                    for attempt in 0..max_attempts {
                                        final_attempt = attempt;
                                        if attempt > 0 {
                                            tokio::time::sleep(retry_delay).await;
                                        }
                                        match async {
                                            tracing::info!(tool.name = %name, tool.args = %args, attempt = attempt, "tool_call");
                                            let exec_future = tool_clone.execute(tool_ctx.clone(), args.clone());
                                            tokio::time::timeout(tool_timeout, exec_future).await
                                        }.instrument(tool_span.clone()).await {
                                            Ok(Ok(value)) => {
                                                tracing::info!(tool.name = %name, tool.result = %value, "tool_result");
                                                retry_result = Some(value);
                                                break;
                                            }
                                            Ok(Err(e)) => {
                                                last_error = e.to_string();
                                                if attempt + 1 < max_attempts {
                                                    tracing::warn!(tool.name = %name, attempt = attempt, error = %last_error, "tool execution failed, retrying");
                                                } else {
                                                    tracing::warn!(tool.name = %name, error = %last_error, "tool_error");
                                                }
                                            }
                                            Err(_) => {
                                                last_error = format!(
                                                    "Tool '{}' timed out after {} seconds",
                                                    name, tool_timeout.as_secs()
                                                );
                                                if attempt + 1 < max_attempts {
                                                    tracing::warn!(tool.name = %name, attempt = attempt, timeout_secs = tool_timeout.as_secs(), "tool timed out, retrying");
                                                } else {
                                                    tracing::warn!(tool.name = %name, timeout_secs = tool_timeout.as_secs(), "tool_timeout");
                                                }
                                            }
                                        }
                                    }

                                    let tool_duration = tool_start.elapsed();
                                    let (tool_success, tool_error_message, function_response) = match retry_result {
                                        Some(value) => (true, None, value),
                                        None => (false, Some(last_error.clone()), serde_json::json!({ "error": last_error })),
                                    };

                                    let outcome = ToolOutcome {
                                        tool_name: name.clone(),
                                        tool_args: args.clone(),
                                        success: tool_success,
                                        duration: tool_duration,
                                        error_message: tool_error_message.clone(),
                                        attempt: final_attempt,
                                    };
                                    tool_outcome_for_callback = Some(outcome);

                                    // Circuit breaker recording
                                    {
                                        let mut guard = cb_mutex.lock().unwrap_or_else(|e| e.into_inner());
                                        if let Some(ref mut cb_state) = *guard {
                                            cb_state.record(tool_outcome_for_callback.as_ref().unwrap());
                                        }
                                    }

                                    // On-tool-error callbacks
                                    let final_function_response = if !tool_success {
                                        let mut fallback_result = None;
                                        let error_msg = tool_error_message.clone().unwrap_or_default();
                                        for callback in on_tool_error_callbacks.as_ref() {
                                            match callback(
                                                ctx.clone() as Arc<dyn CallbackContext>,
                                                tool.clone(),
                                                args.clone(),
                                                error_msg.clone(),
                                            ).await {
                                                Ok(Some(result)) => { fallback_result = Some(result); break; }
                                                Ok(None) => continue,
                                                Err(e) => { tracing::warn!(error = %e, "on_tool_error callback failed"); break; }
                                            }
                                        }
                                        fallback_result.unwrap_or(function_response)
                                    } else {
                                        function_response
                                    };

                                    let confirmation_decision = tool_actions.tool_confirmation_decision;
                                    tool_actions = tool_ctx.actions();
                                    if tool_actions.tool_confirmation_decision.is_none() {
                                        tool_actions.tool_confirmation_decision = confirmation_decision;
                                    }
                                    executed_tool = Some(tool.clone());
                                    executed_tool_response = Some(final_function_response.clone());
                                    response_content = Some(Content {
                                        role: "function".to_string(),
                                        parts: vec![Part::FunctionResponse {
                                            function_response: FunctionResponseData {
                                                name: name.clone(),
                                                response: final_function_response,
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

                            // After-tool callbacks
                            let mut response_content = response_content.expect("tool response content is set");
                            if run_after_tool_callbacks {
                                let outcome_ctx: Arc<dyn CallbackContext> = match tool_outcome_for_callback {
                                    Some(outcome) => Arc::new(ToolOutcomeCallbackContext {
                                        inner: ctx.clone() as Arc<dyn CallbackContext>,
                                        outcome,
                                    }),
                                    None => ctx.clone() as Arc<dyn CallbackContext>,
                                };
                                let cb_ctx: Arc<dyn CallbackContext> = Arc::new(ToolCallbackContext::new(
                                    outcome_ctx,
                                    name.clone(),
                                    args.clone(),
                                ));
                                for callback in after_tool_callbacks.as_ref() {
                                    match callback(cb_ctx.clone()).await {
                                        Ok(Some(modified)) => { response_content = modified; break; }
                                        Ok(None) => continue,
                                        Err(e) => {
                                            response_content = Content {
                                                role: "function".to_string(),
                                                parts: vec![Part::FunctionResponse {
                                                    function_response: FunctionResponseData {
                                                        name: name.clone(),
                                                        response: serde_json::json!({ "error": e.to_string() }),
                                                    },
                                                    id: id.clone(),
                                                }],
                                            };
                                            break;
                                        }
                                    }
                                }
                                if let (Some(tool_ref), Some(tool_resp)) = (&executed_tool, executed_tool_response) {
                                    for callback in after_tool_callbacks_full.as_ref() {
                                        match callback(
                                            cb_ctx.clone(), tool_ref.clone(), args.clone(), tool_resp.clone(),
                                        ).await {
                                            Ok(Some(modified_value)) => {
                                                response_content = Content {
                                                    role: "function".to_string(),
                                                    parts: vec![Part::FunctionResponse {
                                                        function_response: FunctionResponseData {
                                                            name: name.clone(),
                                                            response: modified_value,
                                                        },
                                                        id: id.clone(),
                                                    }],
                                                };
                                                break;
                                            }
                                            Ok(None) => continue,
                                            Err(e) => {
                                                response_content = Content {
                                                    role: "function".to_string(),
                                                    parts: vec![Part::FunctionResponse {
                                                        function_response: FunctionResponseData {
                                                            name: name.clone(),
                                                            response: serde_json::json!({ "error": e.to_string() }),
                                                        },
                                                        id: id.clone(),
                                                    }],
                                                };
                                                break;
                                            }
                                        }
                                    }
                                }
                            }

                            let escalate_or_skip = tool_actions.escalate || tool_actions.skip_summarization;
                            (idx, response_content, tool_actions, escalate_or_skip)
                        }
                    };

                    // ===== DISPATCH BASED ON STRATEGY =====
                    let results: Vec<(usize, Content, EventActions, bool)> = match strategy {
                        ToolExecutionStrategy::Sequential => {
                            let mut results = Vec::with_capacity(fc_parts.len());
                            for (idx, name, args, id, fcid) in fc_parts {
                                results.push(execute_one_tool(idx, name, args, id, fcid).await);
                            }
                            results
                        }
                        ToolExecutionStrategy::Parallel => {
                            let futs: Vec<_> = fc_parts.into_iter()
                                .map(|(idx, name, args, id, fcid)| execute_one_tool(idx, name, args, id, fcid))
                                .collect();
                            futures::future::join_all(futs).await
                        }
                        ToolExecutionStrategy::Auto => {
                            // Partition by is_read_only()
                            let mut read_only_fcs = Vec::new();
                            let mut mutable_fcs = Vec::new();
                            for fc in fc_parts {
                                let is_ro = tool_map.get(&fc.1)
                                    .map(|t| t.is_read_only())
                                    .unwrap_or(false);
                                if is_ro { read_only_fcs.push(fc); } else { mutable_fcs.push(fc); }
                            }
                            let mut all_results = Vec::new();
                            // Execute read-only tools concurrently first
                            if !read_only_fcs.is_empty() {
                                let ro_futs: Vec<_> = read_only_fcs.into_iter()
                                    .map(|(idx, name, args, id, fcid)| execute_one_tool(idx, name, args, id, fcid))
                                    .collect();
                                all_results.extend(futures::future::join_all(ro_futs).await);
                            }
                            // Then execute mutable tools sequentially
                            for (idx, name, args, id, fcid) in mutable_fcs {
                                all_results.push(execute_one_tool(idx, name, args, id, fcid).await);
                            }
                            // Sort by original index to preserve LLM-returned order
                            all_results.sort_by_key(|r| r.0);
                            all_results
                        }
                    };

                    // Restore circuit breaker state from the mutex
                    circuit_breaker_state = cb_mutex.into_inner().unwrap_or_else(|e| e.into_inner());

                    // Yield results in original order
                    for (_, response_content, tool_actions, escalate_or_skip) in results {
                        let mut tool_event = Event::new(&invocation_id);
                        tool_event.author = agent_name.clone();
                        tool_event.actions = tool_actions;
                        tool_event.llm_response.content = Some(response_content.clone());
                        yield Ok(tool_event);

                        if escalate_or_skip {
                            return;
                        }

                        conversation_history.push(response_content);
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
