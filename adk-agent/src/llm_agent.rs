use adk_core::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, AfterToolCallbackFull, Agent,
    BeforeAgentCallback, BeforeModelCallback, BeforeModelResult, BeforeToolCallback,
    CallbackContext, Content, Event, EventActions, FunctionResponseData, GlobalInstructionProvider,
    InstructionProvider, InvocationContext, Llm, LlmRequest, LlmResponse, MemoryEntry,
    OnToolErrorCallback, Part, ReadonlyContext, Result, RetryBudget, Tool, ToolCallbackContext,
    ToolConfirmationDecision, ToolConfirmationPolicy, ToolConfirmationRequest, ToolContext,
    ToolExecutionStrategy, ToolOutcome, Toolset,
};
use async_stream::stream;
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tracing::Instrument;

#[cfg(feature = "enhanced-plugins")]
use adk_plugin::{
    BeforeModelCallResult, BeforeToolCallResult, EnhancedPlugin, EnhancedPluginManager,
};

#[cfg(feature = "skills")]
use crate::skill_shim::load_skill_index;
use crate::{
    guardrails::{
        GuardrailSet, ToolGuardrailSet, ToolScreening, enforce_guardrails, screen_tool_call,
    },
    skill_shim::{SelectionPolicy, SkillIndex, apply_skill_injection},
    tool_call_markup::normalize_option_content,
    workflow::with_user_content_override,
};

/// Default maximum number of LLM round-trips (iterations) before the agent stops.
pub const DEFAULT_MAX_ITERATIONS: u32 = 100;

/// Default tool execution timeout (5 minutes).
pub const DEFAULT_TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

fn trace_json_payload<T: serde::Serialize>(
    value: &T,
    record_payloads: bool,
    max_bytes: usize,
) -> String {
    let json = serde_json::to_string(value).unwrap_or_default();
    if cfg!(feature = "record-payloads") && record_payloads {
        return json;
    }

    let max_bytes = max_bytes.max(32);
    if json.len() <= max_bytes {
        return json;
    }

    let mut end = max_bytes;
    while !json.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated {} bytes]", &json[..end], json.len() - end)
}

#[derive(Debug, Clone)]
struct PendingToolCall {
    index: usize,
    name: String,
    args: serde_json::Value,
    id: Option<String>,
    function_call_id: String,
    guardrail_denial: Option<String>,
}

fn build_generation_config(
    base: Option<&adk_core::GenerateContentConfig>,
    output_schema: Option<&serde_json::Value>,
    cached_content: Option<&str>,
) -> Option<adk_core::GenerateContentConfig> {
    let mut config = base.cloned().unwrap_or_default();
    if let Some(schema) = output_schema {
        config.response_schema = Some(schema.clone());
    }
    if config.cached_content.is_none()
        && let Some(cached_content) = cached_content
    {
        config.cached_content = Some(cached_content.to_string());
    }

    if base.is_some() || output_schema.is_some() || cached_content.is_some() {
        Some(config)
    } else {
        None
    }
}

fn collect_function_calls(content: &Content, invocation_id: &str) -> Vec<PendingToolCall> {
    content
        .parts
        .iter()
        .filter_map(|part| {
            if let Part::FunctionCall { name, args, id, .. } = part {
                Some((name, args, id))
            } else {
                None
            }
        })
        .enumerate()
        .map(|(index, (name, args, id))| PendingToolCall {
            index,
            name: name.clone(),
            args: args.clone(),
            id: id.clone(),
            function_call_id: id
                .clone()
                .unwrap_or_else(|| format!("{invocation_id}_{name}_{index}")),
            guardrail_denial: None,
        })
        .collect()
}

fn build_partial_llm_event(
    event_id: &str,
    invocation_id: &str,
    agent_name: &str,
    request_json: &str,
    chunk: &LlmResponse,
    long_running_tool_ids: Vec<String>,
) -> Event {
    let mut event = Event::with_id(event_id, invocation_id);
    event.author = agent_name.to_string();
    event.llm_request = Some(request_json.to_string());
    event
        .provider_metadata
        .insert("gcp.vertex.agent.llm_request".to_string(), request_json.to_string());
    event.provider_metadata.insert(
        "gcp.vertex.agent.llm_response".to_string(),
        serde_json::to_string(chunk).unwrap_or_default(),
    );
    event.llm_response.partial = chunk.partial;
    event.llm_response.turn_complete = chunk.turn_complete;
    event.llm_response.finish_reason = chunk.finish_reason;
    event.llm_response.usage_metadata = chunk.usage_metadata.clone();
    event.llm_response.content = chunk.content.clone();
    event.llm_response.provider_metadata = chunk.provider_metadata.clone();
    event.llm_response.interaction_id = chunk.interaction_id.clone();
    // Provider failures delivered as `Ok(LlmResponse { error_code, .. })` must
    // remain observable on the streamed event.
    event.llm_response.interrupted = chunk.interrupted;
    event.llm_response.error_code = chunk.error_code.clone();
    event.llm_response.error_message = chunk.error_message.clone();
    event.long_running_tool_ids = long_running_tool_ids;
    event
}

fn build_final_llm_event(
    event_id: &str,
    invocation_id: &str,
    agent_name: &str,
    request_json: &str,
    content: Option<&Content>,
    last_chunk: Option<&LlmResponse>,
    long_running_tool_ids: Vec<String>,
) -> Event {
    let mut event = Event::with_id(event_id, invocation_id);
    event.author = agent_name.to_string();
    event.llm_request = Some(request_json.to_string());
    event
        .provider_metadata
        .insert("gcp.vertex.agent.llm_request".to_string(), request_json.to_string());
    event.llm_response.content = content.cloned();
    event.llm_response.partial = false;
    event.llm_response.turn_complete = true;

    if let Some(last_chunk) = last_chunk {
        event.llm_response.finish_reason = last_chunk.finish_reason;
        event.llm_response.usage_metadata = last_chunk.usage_metadata.clone();
        event.llm_response.provider_metadata = last_chunk.provider_metadata.clone();
        event.llm_response.interaction_id = last_chunk.interaction_id.clone();
        event.llm_response.interrupted = last_chunk.interrupted;
        event.llm_response.error_code = last_chunk.error_code.clone();
        event.llm_response.error_message = last_chunk.error_message.clone();
        event.provider_metadata.insert(
            "gcp.vertex.agent.llm_response".to_string(),
            serde_json::to_string(last_chunk).unwrap_or_default(),
        );
    }

    event.long_running_tool_ids = long_running_tool_ids;
    event
}

/// An LLM-powered agent that orchestrates tool calls and sub-agent delegation.
///
/// `LlmAgent` is the primary agent type in ADK. It sends requests to an LLM,
/// executes tool calls from the response, and iterates until the model produces
/// a final text response or the iteration limit is reached.
///
/// Use [`LlmAgentBuilder`] (via `LlmAgent::builder()`) to construct instances.
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
    /// Maximum retry attempts for output schema validation (default: 3).
    output_max_retries: usize,
    disallow_transfer_to_parent: bool,
    disallow_transfer_to_peers: bool,
    include_contents: adk_core::IncludeContents,
    tools: Vec<Arc<dyn Tool>>,
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
    tool_guardrails: Arc<ToolGuardrailSet>,
    /// Enhanced plugin manager for fine-grained tool/model call interception.
    /// Only created when enhanced plugins are registered (zero overhead otherwise).
    #[cfg(feature = "enhanced-plugins")]
    enhanced_plugin_manager: Option<Arc<EnhancedPluginManager>>,
    /// Optional sandbox configuration for workspace lifecycle management.
    /// When present, the SandboxRunner uses this to provision and bind tools.
    /// The config does NOT add tools directly — that is SandboxRunner's responsibility.
    #[cfg(feature = "sandbox")]
    sandbox_config: Option<adk_sandbox::workspace::SandboxConfig>,
}

struct PromptConfig {
    instruction: Option<String>,
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    output_schema: Option<serde_json::Value>,
    include_contents: adk_core::IncludeContents,
}

impl PromptConfig {
    fn from_agent(agent: &LlmAgent) -> Self {
        Self {
            instruction: agent.instruction.clone(),
            instruction_provider: agent.instruction_provider.clone(),
            global_instruction: agent.global_instruction.clone(),
            global_instruction_provider: agent.global_instruction_provider.clone(),
            skills_index: agent.skills_index.clone(),
            skill_policy: agent.skill_policy.clone(),
            max_skill_chars: agent.max_skill_chars,
            output_schema: agent.output_schema.clone(),
            include_contents: agent.include_contents,
        }
    }

    async fn prepare_conversation(
        &self,
        ctx: &Arc<dyn InvocationContext>,
        agent_name: &str,
    ) -> Result<Vec<Content>> {
        let mut preamble = Vec::new();

        if let Some(provider) = &self.global_instruction_provider {
            let instruction = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            if !instruction.is_empty() {
                preamble.push(Content::new("user").with_text(instruction));
            }
        } else if let Some(template) = &self.global_instruction {
            let instruction = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            if !instruction.is_empty() {
                preamble.push(Content::new("user").with_text(instruction));
            }
        }

        if let Some(provider) = &self.instruction_provider {
            let instruction = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            if !instruction.is_empty() {
                preamble.push(Content::new("user").with_text(instruction));
            }
        } else if let Some(template) = &self.instruction {
            let instruction = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            if !instruction.is_empty() {
                preamble.push(Content::new("user").with_text(instruction));
            }
        }

        if let Some(schema) = &self.output_schema {
            preamble.push(Content::new("user").with_text(format!(
                "You MUST respond with valid JSON conforming to this schema: {schema}. Do not include any text outside the JSON object."
            )));
        }

        let agent_filter = if ctx.authoritative_transfer_targets()
            || !ctx.run_config().transfer_targets.is_empty()
        {
            Some(agent_name)
        } else {
            None
        };
        let mut session_history =
            ctx.session().conversation_history_scoped(agent_filter, ctx.branch());
        let mut current_user_content = ctx.user_content().clone();
        if let Some(index) = &self.skills_index {
            apply_skill_injection(
                &mut current_user_content,
                index.as_ref(),
                &self.skill_policy,
                self.max_skill_chars,
            );
        }
        if let Some(index) = session_history.iter().rposition(|content| content.role == "user") {
            session_history[index] = current_user_content.clone();
        } else {
            session_history.push(current_user_content.clone());
        }

        Ok(match self.include_contents {
            adk_core::IncludeContents::None => {
                preamble.push(current_user_content);
                preamble
            }
            adk_core::IncludeContents::Default => {
                preamble.extend(session_history);
                preamble
            }
        })
    }
}

struct ToolSetup {
    tools: Vec<Arc<dyn Tool>>,
    toolsets: Vec<Arc<dyn Toolset>>,
    sub_agents: Vec<Arc<dyn Agent>>,
    disallow_transfer_to_parent: bool,
    disallow_transfer_to_peers: bool,
}

struct ResolvedTools {
    map: HashMap<String, Arc<dyn Tool>>,
    declarations: HashMap<String, serde_json::Value>,
    transfer_targets: Vec<String>,
}

impl ToolSetup {
    fn from_agent(agent: &LlmAgent) -> Self {
        Self {
            tools: agent.tools.clone(),
            toolsets: agent.toolsets.clone(),
            sub_agents: agent.sub_agents.clone(),
            disallow_transfer_to_parent: agent.disallow_transfer_to_parent,
            disallow_transfer_to_peers: agent.disallow_transfer_to_peers,
        }
    }

    async fn resolve(&self, ctx: &Arc<dyn InvocationContext>) -> Result<ResolvedTools> {
        let mut tools = self.tools.clone();
        let static_tool_names: std::collections::HashSet<_> =
            tools.iter().map(|tool| tool.name().to_string()).collect();
        let mut toolset_sources = std::collections::HashMap::<String, String>::new();
        let mut active_toolsets: Vec<&dyn Toolset> =
            self.toolsets.iter().map(AsRef::as_ref).collect();
        active_toolsets.extend(
            ctx.run_config().runtime_toolsets.iter().map(|runtime| runtime.toolset().as_ref()),
        );

        for toolset in active_toolsets {
            for tool in toolset.tools(ctx.clone() as Arc<dyn ReadonlyContext>).await? {
                let name = tool.name().to_string();
                if static_tool_names.contains(&name) {
                    return Err(adk_core::AdkError::agent(format!(
                        "Duplicate tool name '{name}': conflict between static tool and toolset '{}'",
                        toolset.name()
                    )));
                }
                if let Some(other_toolset) = toolset_sources.get(&name) {
                    return Err(adk_core::AdkError::agent(format!(
                        "Duplicate tool name '{name}': conflict between toolset '{other_toolset}' and toolset '{}'",
                        toolset.name()
                    )));
                }
                toolset_sources.insert(name, toolset.name().to_string());
                tools.push(tool);
            }
        }

        let map = tools.iter().map(|tool| (tool.name().to_string(), tool.clone())).collect();
        let mut declarations = tools
            .iter()
            .map(|tool| (tool.name().to_string(), tool.declaration()))
            .collect::<std::collections::HashMap<_, _>>();
        let mut transfer_targets: Vec<String> = if ctx.authoritative_transfer_targets() {
            Vec::new()
        } else {
            self.sub_agents.iter().map(|agent| agent.name().to_string()).collect()
        };
        let child_names: std::collections::HashSet<_> =
            self.sub_agents.iter().map(|agent| agent.name()).collect();
        let parent_name = ctx.run_config().parent_agent.as_deref();

        for target in &ctx.run_config().transfer_targets {
            if child_names.contains(target.as_str()) {
                continue;
            }
            let is_parent = parent_name == Some(target.as_str());
            if (is_parent && self.disallow_transfer_to_parent)
                || (!is_parent && self.disallow_transfer_to_peers)
            {
                continue;
            }
            transfer_targets.push(target.clone());
        }

        if !transfer_targets.is_empty() {
            declarations.insert(
                "transfer_to_agent".to_string(),
                serde_json::json!({
                    "name": "transfer_to_agent",
                    "description": format!(
                        "Transfer execution to another agent. Valid targets: {}",
                        transfer_targets.join(", ")
                    ),
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "agent_name": {
                                "type": "string",
                                "description": "The name of the agent to transfer to.",
                                "enum": transfer_targets
                            }
                        },
                        "required": ["agent_name"]
                    }
                }),
            );
        }

        Ok(ResolvedTools { map, declarations, transfer_targets })
    }
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

/// Resolves a static confirmation decision for one exact tool call.
///
/// Decisions are keyed by function call ID rather than tool name, so an approval
/// cannot be replayed onto a different call that happens to use the same tool. When
/// the run also supplies a fingerprint for that ID, the call's own fingerprint must
/// match it; a mismatch is treated as no decision, which leaves the call
/// unconfirmed rather than silently authorising different arguments.
fn static_confirmation_decision(
    decisions: &std::collections::HashMap<String, ToolConfirmationDecision>,
    fingerprints: &std::collections::HashMap<String, String>,
    function_call_id: &str,
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<ToolConfirmationDecision> {
    let decision = decisions.get(function_call_id).copied()?;
    if let Some(expected) = fingerprints.get(function_call_id) {
        let actual = adk_core::tool_call_fingerprint(tool_name, args);
        if &actual != expected {
            tracing::warn!(
                tool.name = %tool_name,
                function_call.id = %function_call_id,
                "confirmation decision does not match this call's arguments, treating as unconfirmed"
            );
            return None;
        }
    }
    Some(decision)
}

impl LlmAgent {
    /// Returns the sandbox configuration attached to this agent, if any.
    ///
    /// The `SandboxRunner` uses this to provision a workspace and bind tools.
    /// Returns `None` when no sandbox config was set on the builder.
    ///
    /// Requires the `sandbox` feature.
    #[cfg(feature = "sandbox")]
    pub fn sandbox_config(&self) -> Option<&adk_sandbox::workspace::SandboxConfig> {
        self.sandbox_config.as_ref()
    }

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

/// Validate a JSON string against an output schema.
///
/// Returns `Ok(valid_json)` if the text parses as valid JSON and passes schema
/// validation. Returns `Err(error_message)` describing the validation failure.
fn validate_output_against_schema(
    text: &str,
    schema: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("Response is not valid JSON: {e}"))?;

    let validator =
        jsonschema::validator_for(schema).map_err(|e| format!("Invalid schema: {e}"))?;

    let errors: Vec<String> = validator.iter_errors(&parsed).map(|e| e.to_string()).collect();

    if errors.is_empty() { Ok(parsed) } else { Err(errors.join("; ")) }
}

/// Extract the text content from a series of events.
///
/// Scans events in reverse order for the last non-empty text content
/// produced by the agent. Used internally for output schema validation.
fn extract_text_from_events(events: &[Event]) -> Option<String> {
    for event in events.iter().rev() {
        if let Some(ref content) = event.llm_response.content {
            let text: String =
                content
                    .parts
                    .iter()
                    .filter_map(|p| {
                        if let Part::Text { text } = p { Some(text.as_str()) } else { None }
                    })
                    .collect::<Vec<_>>()
                    .join("");
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Extract a typed value from agent events.
///
/// Scans events for the last text content and deserializes it into `T`.
/// This is useful after running an agent with `output_schema` set to
/// extract the structured result.
///
/// # Example
///
/// ```rust,ignore
/// use serde::Deserialize;
/// use adk_agent::extract_typed;
///
/// #[derive(Deserialize)]
/// struct Weather {
///     temperature: f64,
///     condition: String,
/// }
///
/// let events: Vec<Event> = collect_events_from_stream(stream).await?;
/// let weather: Weather = extract_typed(&events)?;
/// ```
pub fn extract_typed<T: serde::de::DeserializeOwned>(events: &[Event]) -> Result<T> {
    let text = extract_text_from_events(events).ok_or_else(|| {
        adk_core::AdkError::agent("no text content found in events for typed extraction")
    })?;

    serde_json::from_str(&text)
        .map_err(|e| adk_core::AdkError::agent(format!("output deserialization failed: {e}")))
}

/// Builder for constructing an [`LlmAgent`] with all configuration options.
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
    output_max_retries: usize,
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
    tool_guardrails: ToolGuardrailSet,
    /// Enhanced plugins to register on the built agent.
    #[cfg(feature = "enhanced-plugins")]
    enhanced_plugins: Vec<Arc<dyn EnhancedPlugin>>,
    /// Optional sandbox configuration for workspace lifecycle management.
    #[cfg(feature = "sandbox")]
    sandbox_config: Option<adk_sandbox::workspace::SandboxConfig>,
}

impl LlmAgentBuilder {
    /// Create a new builder with the given agent name.
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
            output_max_retries: 3,
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
            tool_guardrails: ToolGuardrailSet::new(),
            #[cfg(feature = "enhanced-plugins")]
            enhanced_plugins: Vec::new(),
            #[cfg(feature = "sandbox")]
            sandbox_config: None,
        }
    }

    /// Set the agent description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set the LLM model for this agent.
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the system instruction for this agent.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Set a dynamic instruction provider evaluated per invocation.
    pub fn instruction_provider(mut self, provider: InstructionProvider) -> Self {
        self.instruction_provider = Some(Arc::new(provider));
        self
    }

    /// Set a global instruction prepended to all requests.
    pub fn global_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.global_instruction = Some(instruction.into());
        self
    }

    /// Set a dynamic global instruction provider evaluated per invocation.
    pub fn global_instruction_provider(mut self, provider: GlobalInstructionProvider) -> Self {
        self.global_instruction_provider = Some(Arc::new(provider));
        self
    }

    /// Set a preloaded skills index for this agent.
    ///
    /// The best matching skill is injected into the current user turn so stable
    /// instructions and conversation history remain available for prompt caching.
    #[cfg(feature = "skills")]
    pub fn with_skills(mut self, index: SkillIndex) -> Self {
        self.skills_index = Some(Arc::new(index));
        self
    }

    /// Auto-load skills from `.skills/` in the current working directory.
    #[cfg(feature = "skills")]
    pub fn with_auto_skills(self) -> Result<Self> {
        self.with_skills_from_root(".")
    }

    /// Auto-load skills from `.skills/` under a custom root directory.
    #[cfg(feature = "skills")]
    pub fn with_skills_from_root(mut self, root: impl AsRef<std::path::Path>) -> Result<Self> {
        let index = load_skill_index(root).map_err(|e| adk_core::AdkError::agent(e.to_string()))?;
        self.skills_index = Some(Arc::new(index));
        Ok(self)
    }

    /// Customize skill selection behavior.
    #[cfg(feature = "skills")]
    pub fn with_skill_policy(mut self, policy: SelectionPolicy) -> Self {
        self.skill_policy = policy;
        self
    }

    /// Limit injected skill content length.
    #[cfg(feature = "skills")]
    pub fn with_skill_budget(mut self, max_chars: usize) -> Self {
        self.max_skill_chars = max_chars;
        self
    }

    /// Set a JSON schema for validating user input.
    pub fn input_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = Some(schema);
        self
    }

    /// Set a JSON schema for structured output from the LLM.
    pub fn output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Derive the output schema from a Rust type using `schemars`.
    ///
    /// This is a convenience method that generates a JSON Schema from `T`'s
    /// `JsonSchema` implementation and sets it as the output schema.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use schemars::JsonSchema;
    /// use serde::Deserialize;
    ///
    /// #[derive(JsonSchema, Deserialize)]
    /// struct MyOutput {
    ///     name: String,
    ///     score: f64,
    /// }
    ///
    /// let agent = LlmAgentBuilder::new("my-agent")
    ///     .model(model)
    ///     .output_type::<MyOutput>()
    ///     .build()?;
    /// ```
    pub fn output_type<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.output_schema =
            Some(serde_json::to_value(schema).expect("schema serialization cannot fail"));
        self
    }

    /// Set the maximum number of retry attempts for output schema validation.
    ///
    /// When the LLM produces output that fails schema validation, the agent
    /// will retry up to this many times with a correction prompt. Default is 3.
    pub fn output_max_retries(mut self, n: usize) -> Self {
        self.output_max_retries = n;
        self
    }

    /// Prevent this agent from transferring control back to its parent.
    pub fn disallow_transfer_to_parent(mut self, disallow: bool) -> Self {
        self.disallow_transfer_to_parent = disallow;
        self
    }

    /// Prevent this agent from transferring control to peer agents.
    pub fn disallow_transfer_to_peers(mut self, disallow: bool) -> Self {
        self.disallow_transfer_to_peers = disallow;
        self
    }

    /// Control which conversation history contents are included in LLM requests.
    pub fn include_contents(mut self, include: adk_core::IncludeContents) -> Self {
        self.include_contents = include;
        self
    }

    /// Set a state key where the agent's final output will be stored.
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

    /// Add a tool to this agent's toolbox.
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

    /// Add a sub-agent that this agent can delegate to.
    pub fn sub_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.sub_agents.push(agent);
        self
    }

    /// Add a before-agent callback.
    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.before_callbacks.push(callback);
        self
    }

    /// Add an after-agent callback.
    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.after_callbacks.push(callback);
        self
    }

    /// Add a before-model callback invoked before each LLM request.
    pub fn before_model_callback(mut self, callback: BeforeModelCallback) -> Self {
        self.before_model_callbacks.push(callback);
        self
    }

    /// Add an after-model callback invoked after each LLM response.
    pub fn after_model_callback(mut self, callback: AfterModelCallback) -> Self {
        self.after_model_callbacks.push(callback);
        self
    }

    /// Add a before-tool callback invoked before each tool execution.
    pub fn before_tool_callback(mut self, callback: BeforeToolCallback) -> Self {
        self.before_tool_callbacks.push(callback);
        self
    }

    /// Add an after-tool callback invoked after each tool execution.
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
    /// `RunConfig` value is used. [`ToolExecutionStrategy::Parallel`] is an
    /// explicit override that bypasses tool safety metadata, so the caller owns
    /// concurrency safety.
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

    /// Set guardrails that screen tool calls before they execute.
    ///
    /// [`GuardrailSet`] validates `Content` and never sees a tool call, and
    /// [`ToolConfirmationPolicy`] decides per tool *name*. Neither can express "this tool may run,
    /// but not with these arguments". A [`ToolGuardrailSet`] receives the tool name and the
    /// arguments and may allow, deny, or narrow them.
    ///
    /// Screening runs before the tool executes and before confirmation is resolved, so a denied
    /// call neither prompts the user nor consumes a concurrency permit. A denial is reported to
    /// the model as the tool's result, letting it correct the call rather than stalling the run.
    ///
    /// Requires the `guardrails` feature.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_agent::guardrails::{PathAllowList, ToolGuardrailSet};
    ///
    /// let agent = LlmAgentBuilder::new("ops")
    ///     .tool_guardrails(ToolGuardrailSet::new().with(
    ///         PathAllowList::new("agents-only", ["path"], ["/Users/me/Library/LaunchAgents"])
    ///             .on_tools(["plist_write"]),
    ///     ))
    ///     .build()?;
    /// ```
    pub fn tool_guardrails(mut self, guardrails: ToolGuardrailSet) -> Self {
        self.tool_guardrails = guardrails;
        self
    }

    /// Register a single enhanced plugin for fine-grained tool/model call interception.
    ///
    /// Enhanced plugins can inspect and modify tool arguments, tool results,
    /// model requests, and model responses. They execute in priority order
    /// (lower priority values execute first).
    ///
    /// Requires the `enhanced-plugins` feature.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_plugin::EnhancedPlugin;
    ///
    /// let agent = LlmAgentBuilder::new("my-agent")
    ///     .model(model)
    ///     .enhanced_plugin(Arc::new(MyPlugin::new()))
    ///     .build()?;
    /// ```
    #[cfg(feature = "enhanced-plugins")]
    pub fn enhanced_plugin(mut self, plugin: Arc<dyn EnhancedPlugin>) -> Self {
        self.enhanced_plugins.push(plugin);
        self
    }

    /// Register multiple enhanced plugins at once.
    ///
    /// Plugins are sorted by priority when the agent is built. Lower priority
    /// values execute first. Same-priority plugins execute in registration order.
    ///
    /// Requires the `enhanced-plugins` feature.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_plugin::EnhancedPlugin;
    ///
    /// let agent = LlmAgentBuilder::new("my-agent")
    ///     .model(model)
    ///     .enhanced_plugins(vec![
    ///         Arc::new(SecurityPlugin::new()),  // priority = 10
    ///         Arc::new(LoggingPlugin::new()),   // priority = 100
    ///     ])
    ///     .build()?;
    /// ```
    #[cfg(feature = "enhanced-plugins")]
    pub fn enhanced_plugins(mut self, plugins: Vec<Arc<dyn EnhancedPlugin>>) -> Self {
        self.enhanced_plugins.extend(plugins);
        self
    }

    /// Attach a sandbox configuration for workspace lifecycle management.
    ///
    /// When a `SandboxConfig` is attached, the `SandboxRunner` will provision
    /// a workspace, bind tools based on enabled capabilities, and manage the
    /// session lifecycle. The config does NOT add tools directly to the agent —
    /// tool binding is the responsibility of the `SandboxRunner`.
    ///
    /// When no `SandboxConfig` is attached, the agent behaves identically to
    /// its behavior before this feature was introduced.
    ///
    /// Requires the `sandbox` feature.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_sandbox::workspace::{SandboxConfig, Capability, Manifest};
    /// use std::collections::HashSet;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// let config = SandboxConfig {
    ///     client: Arc::new(my_client),
    ///     manifest: Manifest { entries: vec![] },
    ///     capabilities: HashSet::from([Capability::Shell, Capability::Filesystem]),
    ///     snapshot_on_stop: true,
    ///     session_timeout: Duration::from_secs(600),
    ///     command_timeout: Duration::from_secs(120),
    /// };
    ///
    /// let agent = LlmAgentBuilder::new("coding-agent")
    ///     .model(model)
    ///     .sandbox_config(config)
    ///     .build()?;
    /// ```
    #[cfg(feature = "sandbox")]
    pub fn sandbox_config(mut self, config: adk_sandbox::workspace::SandboxConfig) -> Self {
        self.sandbox_config = Some(config);
        self
    }

    /// Build the [`LlmAgent`], returning an error if no model was set.
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

        // Validate: Gemini Interactions API + client-side sandbox tools conflict.
        // These provide competing filesystems and would produce nondeterministic behavior.
        #[cfg(feature = "sandbox")]
        if let Some(ref sandbox_cfg) = self.sandbox_config {
            use adk_sandbox::workspace::Capability;
            if model.uses_interactions_api()
                && (sandbox_cfg.capabilities.contains(&Capability::Shell)
                    || sandbox_cfg.capabilities.contains(&Capability::Filesystem))
            {
                return Err(adk_core::AdkError::new(
                    adk_core::ErrorComponent::Agent,
                    adk_core::ErrorCategory::InvalidInput,
                    "code.gemini_interactions_conflict",
                    "Cannot combine Gemini Interactions API (server-managed environment) \
                     with client-side sandbox tools (Shell/Filesystem). These provide \
                     competing filesystems and would produce nondeterministic behavior. \
                     Either disable use_interactions_api or remove sandbox capabilities.",
                ));
            }
        }

        // Construct EnhancedPluginManager only when plugins are registered (zero overhead otherwise)
        #[cfg(feature = "enhanced-plugins")]
        let enhanced_plugin_manager = if self.enhanced_plugins.is_empty() {
            None
        } else {
            Some(Arc::new(EnhancedPluginManager::new(self.enhanced_plugins)))
        };

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
            output_max_retries: self.output_max_retries,
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
            tool_guardrails: Arc::new(self.tool_guardrails),
            #[cfg(feature = "enhanced-plugins")]
            enhanced_plugin_manager,
            #[cfg(feature = "sandbox")]
            sandbox_config: self.sandbox_config,
        })
    }
}

// AgentToolContext wraps the parent InvocationContext and preserves all context
// instead of throwing it away like SimpleToolContext did
/// Progress events held in flight for one tool batch.
///
/// The queue exists to decouple a tool that writes progress from a client that
/// reads it. It is bounded so a verbose tool cannot grow the queue without limit
/// while the consumer is slow.
const TOOL_PROGRESS_CAPACITY: usize = 256;

/// How long a tool waits for queue space before its chunk is dropped.
///
/// A short wait applies real backpressure to a tool that outruns its consumer,
/// without letting a stalled consumer block the tool indefinitely.
const TOOL_PROGRESS_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

/// Largest single progress chunk forwarded, in bytes.
const TOOL_PROGRESS_MAX_CHUNK_BYTES: usize = 8 * 1024;

/// Total progress bytes forwarded for one tool call.
const TOOL_PROGRESS_MAX_TOTAL_BYTES: usize = 1024 * 1024;

/// Text appended in place of progress output that was not forwarded.
const TOOL_PROGRESS_TRUNCATION_MARKER: &str = "[adk: tool progress truncated]";

/// Truncates `text` to at most `max_bytes`, never splitting a character.
fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct AgentToolContext {
    parent_ctx: Arc<dyn InvocationContext>,
    function_call_id: String,
    /// The tool this context was built for, recorded by the dispatcher so secret
    /// requests carry an identity the tool cannot choose.
    tool_name: Option<String>,
    actions: Mutex<EventActions>,
    progress_tx: Option<tokio::sync::mpsc::Sender<Event>>,
    /// Progress bytes forwarded so far for this call.
    progress_bytes: std::sync::atomic::AtomicUsize,
    /// Set once the truncation marker has been emitted, so it is emitted once.
    progress_truncated: std::sync::atomic::AtomicBool,
}

impl AgentToolContext {
    fn new(parent_ctx: Arc<dyn InvocationContext>, function_call_id: String) -> Self {
        Self {
            parent_ctx,
            function_call_id,
            tool_name: None,
            actions: Mutex::new(EventActions::default()),
            progress_tx: None,
            progress_bytes: std::sync::atomic::AtomicUsize::new(0),
            progress_truncated: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Record which tool this context serves.
    fn with_tool_name(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
        self
    }

    /// Attach a progress sink so [`ToolContext::emit_progress`] forwards chunks
    /// as partial [`Event`]s onto the agent's `EventStream`.
    fn with_progress(mut self, tx: tokio::sync::mpsc::Sender<Event>) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    /// Builds a described secret access from the identity the framework holds.
    ///
    /// The tool name comes from the dispatch record rather than from anything the tool
    /// supplied, so one tool cannot request a secret under another tool's identity.
    async fn request_secret(&self, name: &str, purpose: Option<&str>) -> Result<Option<String>> {
        let mut request = adk_core::SecretRequest::new(name)
            .with_identity(
                self.parent_ctx.app_name(),
                self.parent_ctx.user_id(),
                self.parent_ctx.session_id(),
            )
            .with_invocation_id(self.parent_ctx.invocation_id());
        if let Some(tool_name) = &self.tool_name {
            request = request.with_tool_name(tool_name);
        }
        if let Some(purpose) = purpose {
            request = request.with_purpose(purpose);
        }
        self.parent_ctx.get_secret_for(&request).await
    }

    /// Forwards one progress chunk under this call's budget.
    ///
    /// The policy is bounded and lossy by design: memory is capped, and output that
    /// does not fit is replaced by a single truncation marker rather than stalling
    /// the tool or growing without limit. A tool that outruns its consumer waits
    /// briefly, which slows the tool rather than the whole run.
    async fn forward_progress(
        &self,
        tx: &tokio::sync::mpsc::Sender<Event>,
        stream: &str,
        chunk: &str,
    ) {
        use std::sync::atomic::Ordering;

        if self.progress_truncated.load(Ordering::Relaxed) {
            return;
        }

        // Cap one chunk, then cap the call. Truncation respects char boundaries so
        // multi-byte text is never split mid-character.
        let payload = truncate_on_char_boundary(chunk, TOOL_PROGRESS_MAX_CHUNK_BYTES);
        let forwarded = self.progress_bytes.fetch_add(payload.len(), Ordering::Relaxed);
        if forwarded.saturating_add(payload.len()) > TOOL_PROGRESS_MAX_TOTAL_BYTES {
            self.mark_progress_truncated(tx, stream).await;
            return;
        }

        let event = Event::tool_progress(
            self.parent_ctx.invocation_id(),
            self.parent_ctx.agent_name(),
            &self.function_call_id,
            stream,
            payload,
        );

        match tx.try_send(event) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                // Wait briefly for the consumer to catch up, then give up on this
                // chunk so a stalled consumer cannot block the tool.
                match tokio::time::timeout(TOOL_PROGRESS_SEND_TIMEOUT, tx.send(event)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {}
                    Err(_) => self.mark_progress_truncated(tx, stream).await,
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    /// Emits the truncation marker once for this call.
    async fn mark_progress_truncated(&self, tx: &tokio::sync::mpsc::Sender<Event>, stream: &str) {
        use std::sync::atomic::Ordering;
        if self.progress_truncated.swap(true, Ordering::Relaxed) {
            return;
        }
        tracing::warn!(
            function_call.id = %self.function_call_id,
            progress.stream = %stream,
            "tool progress exceeded its budget, remaining output is not forwarded"
        );
        let marker = Event::tool_progress(
            self.parent_ctx.invocation_id(),
            self.parent_ctx.agent_name(),
            &self.function_call_id,
            stream,
            TOOL_PROGRESS_TRUNCATION_MARKER,
        );
        let _ = tx.try_send(marker);
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

    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.parent_ctx.shared_state()
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

    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        self.parent_ctx.memory()
    }

    fn session(&self) -> Option<&dyn adk_core::Session> {
        Some(self.parent_ctx.session())
    }

    fn run_config(&self) -> Option<&adk_core::RunConfig> {
        Some(self.parent_ctx.run_config())
    }

    fn is_cancelled(&self) -> bool {
        self.parent_ctx.is_cancelled()
    }

    fn request_metadata(&self) -> HashMap<String, serde_json::Value> {
        self.parent_ctx.request_metadata()
    }

    fn delegation_depth(&self) -> u32 {
        self.parent_ctx.delegation_depth()
    }

    fn max_delegation_depth(&self) -> Option<u32> {
        self.parent_ctx.max_delegation_depth()
    }

    fn orchestration_root_invocation_id(&self) -> &str {
        self.parent_ctx.orchestration_root_invocation_id()
    }

    fn orchestration_edge_id(&self) -> Option<&str> {
        self.parent_ctx.orchestration_edge_id()
    }

    async fn emit_event(&self, event: Event) {
        if let Some(tx) = &self.progress_tx
            && !tx.is_closed()
        {
            let _ = tokio::time::timeout(TOOL_PROGRESS_SEND_TIMEOUT, tx.send(event)).await;
        }
    }

    fn user_scopes(&self) -> Vec<String> {
        self.parent_ctx.user_scopes()
    }

    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.request_secret(name, None).await
    }

    async fn get_secret_for_purpose(&self, name: &str, purpose: &str) -> Result<Option<String>> {
        self.request_secret(name, Some(purpose)).await
    }

    async fn emit_progress(&self, stream: &str, chunk: &str) {
        // Primary path: forward as a partial Event on the agent's EventStream so
        // UIs consume tool progress through the same channel as everything else.
        if let Some(tx) = &self.progress_tx {
            // A closed receiver means nobody is listening, so stop building events.
            if !tx.is_closed() {
                self.forward_progress(tx, stream, chunk).await;
            }
        }
        // Secondary path: structured trace for log-based observability.
        tracing::debug!(
            target: "adk_agent::tool_progress",
            tool_call_id = %self.function_call_id,
            stream = %stream,
            "{chunk}",
        );
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

struct ToolExecutionResult {
    index: usize,
    content: Content,
    actions: EventActions,
    escalate_or_skip: bool,
}

struct ToolExecutor<'a> {
    ctx: Arc<dyn InvocationContext>,
    tool_map: &'a std::collections::HashMap<String, Arc<dyn Tool>>,
    tool_retry_budgets: &'a std::collections::HashMap<String, RetryBudget>,
    default_retry_budget: &'a Option<RetryBudget>,
    before_tool_callbacks: &'a Arc<Vec<BeforeToolCallback>>,
    after_tool_callbacks: &'a Arc<Vec<AfterToolCallback>>,
    after_tool_callbacks_full: &'a Arc<Vec<AfterToolCallbackFull>>,
    on_tool_error_callbacks: &'a Arc<Vec<OnToolErrorCallback>>,
    tool_confirmation_policy: &'a ToolConfirmationPolicy,
    cb_mutex: &'a std::sync::Mutex<Option<CircuitBreakerState>>,
    invocation_id: &'a str,
    concurrency_manager: &'a adk_core::ToolConcurrencyManager,
    progress_tx: tokio::sync::mpsc::Sender<Event>,
    tool_timeout: std::time::Duration,
    confirmation_decisions: &'a std::collections::HashMap<String, ToolConfirmationDecision>,
    confirmation_fingerprints: &'a std::collections::HashMap<String, String>,
    live_confirmation_decisions: &'a std::collections::HashMap<String, ToolConfirmationDecision>,
    #[cfg(feature = "enhanced-plugins")]
    enhanced_plugin_manager: &'a Option<Arc<EnhancedPluginManager>>,
}

impl ToolExecutor<'_> {
    async fn execute(&self, call: PendingToolCall) -> ToolExecutionResult {
        let PendingToolCall { index, name, args, id, function_call_id, guardrail_denial } = call;
        let mut tool_actions = EventActions::default();
        let mut response_content: Option<Content> = None;
        let mut run_after_tool_callbacks = true;
        let mut tool_outcome_for_callback: Option<ToolOutcome> = None;
        let mut executed_tool: Option<Arc<dyn Tool>> = None;
        let mut executed_tool_response: Option<serde_json::Value> = None;

        if let Some(reason) = guardrail_denial {
            // Screening happened before confirmation. Report a denial as the tool's result so the
            // model can correct the call instead of the run stalling.
            let denied_content = Content {
                role: "function".to_string(),
                parts: vec![Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        name.clone(),
                        serde_json::json!({ "error": reason }),
                    ),
                    id: id.clone(),
                    annotations: None,
                }],
            };
            return ToolExecutionResult {
                index,
                content: denied_content,
                actions: tool_actions,
                escalate_or_skip: false,
            };
        }

        // Acquire concurrency permit before tool execution.
        // The permit is held for the entire duration of this tool call
        // and released on drop when this async block completes.
        let _concurrency_permit = match self.concurrency_manager.acquire(&name).await {
            Ok(permit) => Some(permit),
            Err(e) => {
                // Concurrency limit reached with Fail policy — return error
                let error_content = Content {
                    role: "function".to_string(),
                    parts: vec![Part::FunctionResponse {
                        function_response: FunctionResponseData::new(
                            name.clone(),
                            serde_json::json!({ "error": e.to_string() }),
                        ),
                        id: id.clone(),
                        annotations: None,
                    }],
                };
                return ToolExecutionResult {
                    index,
                    content: error_content,
                    actions: tool_actions,
                    escalate_or_skip: false,
                };
            }
        };

        // Tool confirmation (deny case; None handled by pre-check)
        if self.tool_confirmation_policy.requires_confirmation(&name) {
            match self.live_confirmation_decisions.get(&function_call_id).copied().or_else(|| {
                static_confirmation_decision(
                    self.confirmation_decisions,
                    self.confirmation_fingerprints,
                    &function_call_id,
                    &name,
                    &args,
                )
            }) {
                Some(ToolConfirmationDecision::Approve) => {
                    tool_actions.tool_confirmation_decision =
                        Some(ToolConfirmationDecision::Approve);
                }
                Some(ToolConfirmationDecision::Deny) => {
                    tool_actions.tool_confirmation_decision = Some(ToolConfirmationDecision::Deny);
                    response_content = Some(Content {
                        role: "function".to_string(),
                        parts: vec![Part::FunctionResponse {
                            function_response: FunctionResponseData::new(
                                name.clone(),
                                serde_json::json!({
                                    "error": format!("Tool '{}' execution denied by confirmation policy", name)
                                }),
                            ),
                            id: id.clone(),
                            annotations: None,
                        }],
                    });
                    run_after_tool_callbacks = false;
                }
                None => {
                    response_content = Some(Content {
                        role: "function".to_string(),
                        parts: vec![Part::FunctionResponse {
                            function_response: FunctionResponseData::new(
                                name.clone(),
                                serde_json::json!({
                                    "error": format!("Tool '{}' requires confirmation", name)
                                }),
                            ),
                            id: id.clone(),
                            annotations: None,
                        }],
                    });
                    run_after_tool_callbacks = false;
                }
            }
        }

        // Before-tool callbacks
        // Track potentially modified args for enhanced plugin after-hook
        #[allow(unused_mut)]
        let mut final_args = args.clone();

        // ===== ENHANCED PLUGIN: BEFORE TOOL CALL =====
        #[cfg(feature = "enhanced-plugins")]
        if response_content.is_none()
            && let Some(epm) = self.enhanced_plugin_manager.as_ref()
            && let Some(tool_ref) = self.tool_map.get(&name)
        {
            match epm
                .run_before_tool_call(
                    tool_ref.clone(),
                    final_args.clone(),
                    self.ctx.clone() as Arc<dyn CallbackContext>,
                )
                .await
            {
                Ok(BeforeToolCallResult::Continue(modified_args)) => {
                    final_args = modified_args;
                }
                Ok(BeforeToolCallResult::ShortCircuit(synthetic_result)) => {
                    // Short-circuit: use synthetic result, skip tool execution
                    response_content = Some(Content {
                        role: "function".to_string(),
                        parts: vec![Part::FunctionResponse {
                            function_response: FunctionResponseData::from_tool_result(
                                name.clone(),
                                synthetic_result,
                            ),
                            id: id.clone(),
                            annotations: None,
                        }],
                    });
                    executed_tool = Some(tool_ref.clone());
                }
                Err(e) => {
                    response_content = Some(Content {
                        role: "function".to_string(),
                        parts: vec![Part::FunctionResponse {
                            function_response: FunctionResponseData::new(
                                name.clone(),
                                serde_json::json!({ "error": e.to_string() }),
                            ),
                            id: id.clone(),
                            annotations: None,
                        }],
                    });
                    run_after_tool_callbacks = false;
                }
            }
        }

        if response_content.is_none() {
            let tool_ctx = Arc::new(ToolCallbackContext::new(
                self.ctx.clone(),
                name.clone(),
                final_args.clone(),
            ));
            for callback in self.before_tool_callbacks.as_ref() {
                match callback(tool_ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(c)) => {
                        response_content = Some(c);
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        response_content = Some(Content {
                            role: "function".to_string(),
                            parts: vec![Part::FunctionResponse {
                                function_response: FunctionResponseData::new(
                                    name.clone(),
                                    serde_json::json!({ "error": e.to_string() }),
                                ),
                                id: id.clone(),
                                annotations: None,
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
            let guard = self.cb_mutex.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref cb_state) = *guard
                && cb_state.is_open(&name)
            {
                let msg = format!(
                    "Tool '{}' is temporarily disabled after {} consecutive failures",
                    name, cb_state.threshold
                );
                tracing::warn!(tool.name = %name, "circuit breaker open, skipping tool execution");
                response_content = Some(Content {
                    role: "function".to_string(),
                    parts: vec![Part::FunctionResponse {
                        function_response: FunctionResponseData::new(
                            name.clone(),
                            serde_json::json!({ "error": msg }),
                        ),
                        id: id.clone(),
                        annotations: None,
                    }],
                });
                run_after_tool_callbacks = false;
            }
            drop(guard);
        }

        // Execute tool with retry budget and tracing
        if response_content.is_none() {
            if let Some(tool) = self.tool_map.get(&name) {
                let tool_ctx: Arc<dyn ToolContext> = Arc::new(
                    AgentToolContext::new(self.ctx.clone(), function_call_id.clone())
                        .with_tool_name(tool.name())
                        .with_progress(self.progress_tx.clone()),
                );
                let span_name = format!("execute_tool {name}");
                let tool_span = tracing::info_span!(
                    "",
                    otel.name = %span_name,
                    tool.name = %name,
                    "gcp.vertex.agent.event_id" = %format!("{}_{}", self.invocation_id, name),
                    "gcp.vertex.agent.invocation_id" = %self.invocation_id,
                    "gcp.vertex.agent.session_id" = %self.ctx.session_id(),
                    "gen_ai.conversation.id" = %self.ctx.session_id()
                );

                let budget =
                    self.tool_retry_budgets.get(&name).or(self.default_retry_budget.as_ref());
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
                        let args_payload = trace_json_payload(
                            &final_args,
                            self.ctx.run_config().record_payloads,
                            self.ctx.run_config().trace_payload_max_bytes,
                        );
                        tracing::debug!(tool.name = %name, tool.args = %args_payload, attempt = attempt, "tool_call");
                        let exec_future = tool_clone.execute(tool_ctx.clone(), final_args.clone());
                        let unwind_safe_future = std::panic::AssertUnwindSafe(
                            tokio::time::timeout(self.tool_timeout, exec_future),
                        );
                        match futures::FutureExt::catch_unwind(unwind_safe_future).await {
                            Ok(result) => result,
                            Err(_panic) => Ok(Err(adk_core::AdkError::tool(format!(
                                "tool '{}' panicked during execution",
                                name
                            )))),
                        }
                    }
                    .instrument(tool_span.clone())
                    .await
                    {
                        Ok(Ok(value)) => {
                            let result_payload = trace_json_payload(
                                &value,
                                self.ctx.run_config().record_payloads,
                                self.ctx.run_config().trace_payload_max_bytes,
                            );
                            tracing::debug!(tool.name = %name, tool.result = %result_payload, "tool_result");
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
                                name,
                                self.tool_timeout.as_secs()
                            );
                            if attempt + 1 < max_attempts {
                                tracing::warn!(tool.name = %name, attempt = attempt, timeout_secs = self.tool_timeout.as_secs(), "tool timed out, retrying");
                            } else {
                                tracing::warn!(tool.name = %name, timeout_secs = self.tool_timeout.as_secs(), "tool_timeout");
                            }
                        }
                    }
                }

                let tool_duration = tool_start.elapsed();
                let (tool_success, tool_error_message, function_response) = match retry_result {
                    Some(value) => (true, None, value),
                    None => (
                        false,
                        Some(last_error.clone()),
                        serde_json::json!({ "error": last_error }),
                    ),
                };

                let outcome = ToolOutcome {
                    tool_name: name.clone(),
                    tool_args: final_args.clone(),
                    success: tool_success,
                    duration: tool_duration,
                    error_message: tool_error_message.clone(),
                    attempt: final_attempt,
                };
                tool_outcome_for_callback = Some(outcome);

                // Circuit breaker recording
                {
                    let mut guard = self.cb_mutex.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(ref mut cb_state) = *guard {
                        cb_state.record(tool_outcome_for_callback.as_ref().unwrap());
                    }
                }

                // On-tool-error callbacks
                let final_function_response = if !tool_success {
                    let mut fallback_result = None;
                    let error_msg = tool_error_message.clone().unwrap_or_default();
                    for callback in self.on_tool_error_callbacks.as_ref() {
                        match callback(
                            self.ctx.clone() as Arc<dyn CallbackContext>,
                            tool.clone(),
                            final_args.clone(),
                            error_msg.clone(),
                        )
                        .await
                        {
                            Ok(Some(result)) => {
                                fallback_result = Some(result);
                                break;
                            }
                            Ok(None) => continue,
                            Err(e) => {
                                tracing::warn!(error = %e, "on_tool_error callback failed");
                                break;
                            }
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
                        function_response: FunctionResponseData::from_tool_result(
                            name.clone(),
                            final_function_response,
                        ),
                        id: id.clone(),
                        annotations: None,
                    }],
                });
            } else {
                response_content = Some(Content {
                    role: "function".to_string(),
                    parts: vec![Part::FunctionResponse {
                        function_response: FunctionResponseData::new(
                            name.clone(),
                            serde_json::json!({
                                "error": format!("Tool {} not found", name)
                            }),
                        ),
                        id: id.clone(),
                        annotations: None,
                    }],
                });
            }
        }

        // After-tool callbacks
        let mut response_content = response_content.expect("tool response content is set");
        if run_after_tool_callbacks {
            let outcome_ctx: Arc<dyn CallbackContext> = match tool_outcome_for_callback {
                Some(outcome) => Arc::new(ToolOutcomeCallbackContext {
                    inner: self.ctx.clone() as Arc<dyn CallbackContext>,
                    outcome,
                }),
                None => self.ctx.clone() as Arc<dyn CallbackContext>,
            };
            let cb_ctx: Arc<dyn CallbackContext> =
                Arc::new(ToolCallbackContext::new(outcome_ctx, name.clone(), final_args.clone()));
            for callback in self.after_tool_callbacks.as_ref() {
                match callback(cb_ctx.clone()).await {
                    Ok(Some(modified)) => {
                        response_content = modified;
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        response_content = Content {
                            role: "function".to_string(),
                            parts: vec![Part::FunctionResponse {
                                function_response: FunctionResponseData::new(
                                    name.clone(),
                                    serde_json::json!({ "error": e.to_string() }),
                                ),
                                id: id.clone(),
                                annotations: None,
                            }],
                        };
                        break;
                    }
                }
            }
            if let (Some(tool_ref), Some(tool_resp)) = (&executed_tool, executed_tool_response) {
                for callback in self.after_tool_callbacks_full.as_ref() {
                    match callback(
                        cb_ctx.clone(),
                        tool_ref.clone(),
                        final_args.clone(),
                        tool_resp.clone(),
                    )
                    .await
                    {
                        Ok(Some(modified_value)) => {
                            response_content = Content {
                                role: "function".to_string(),
                                parts: vec![Part::FunctionResponse {
                                    function_response: FunctionResponseData::from_tool_result(
                                        name.clone(),
                                        modified_value,
                                    ),
                                    id: id.clone(),
                                    annotations: None,
                                }],
                            };
                            break;
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            response_content = Content {
                                role: "function".to_string(),
                                parts: vec![Part::FunctionResponse {
                                    function_response: FunctionResponseData::new(
                                        name.clone(),
                                        serde_json::json!({ "error": e.to_string() }),
                                    ),
                                    id: id.clone(),
                                    annotations: None,
                                }],
                            };
                            break;
                        }
                    }
                }
            }

            // ===== ENHANCED PLUGIN: AFTER TOOL CALL =====
            // Enhanced plugins can modify the tool result after legacy callbacks.
            #[cfg(feature = "enhanced-plugins")]
            if let Some(epm) = self.enhanced_plugin_manager.as_ref()
                && let Some(tool_ref) = &executed_tool
            {
                // Extract the result value from the response content
                let result_value = response_content
                    .parts
                    .iter()
                    .find_map(|p| {
                        if let Part::FunctionResponse { function_response, .. } = p {
                            Some(function_response.response.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(serde_json::json!(null));

                match epm
                    .run_after_tool_call(
                        tool_ref.clone(),
                        &final_args,
                        result_value,
                        self.ctx.clone() as Arc<dyn CallbackContext>,
                    )
                    .await
                {
                    Ok(adk_plugin::AfterToolCallResult::Continue(modified_result)) => {
                        response_content = Content {
                            role: "function".to_string(),
                            parts: vec![Part::FunctionResponse {
                                function_response: FunctionResponseData::from_tool_result(
                                    name.clone(),
                                    modified_result,
                                ),
                                id: id.clone(),
                                annotations: None,
                            }],
                        };
                    }
                    Err(e) => {
                        response_content = Content {
                            role: "function".to_string(),
                            parts: vec![Part::FunctionResponse {
                                function_response: FunctionResponseData::new(
                                    name.clone(),
                                    serde_json::json!({ "error": e.to_string() }),
                                ),
                                id: id.clone(),
                                annotations: None,
                            }],
                        };
                    }
                }
            }
        }

        let escalate_or_skip = tool_actions.escalate || tool_actions.skip_summarization;
        ToolExecutionResult {
            index,
            content: response_content,
            actions: tool_actions,
            escalate_or_skip,
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

    fn capabilities(&self) -> adk_core::AgentCapabilities {
        adk_core::AgentCapabilities {
            runtime_tools: true,
            handoff: true,
            relationship_confirmation: true,
            checkpoint_resume: false,
            shared_state: true,
            invocation_metadata: true,
        }
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
        let prompt_config = PromptConfig::from_agent(self);
        let tool_setup = ToolSetup::from_agent(self);
        let output_key = self.output_key.clone();
        let output_max_retries = self.output_max_retries;
        let generate_content_config = self.generate_content_config.clone();
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
        let tool_guardrails = Arc::clone(&self.tool_guardrails);
        let output_guardrails = self.output_guardrails.clone();
        let agent_tool_execution_strategy = self.tool_execution_strategy;
        #[cfg(feature = "enhanced-plugins")]
        let enhanced_plugin_manager = self.enhanced_plugin_manager.clone();

        let s = stream! {
            let confirmation_decisions =
                ctx.run_config().tool_confirmation_decisions.clone();
            let confirmation_fingerprints =
                ctx.run_config().tool_confirmation_fingerprints.clone();
            let mut live_confirmation_decisions =
                std::collections::HashMap::<String, ToolConfirmationDecision>::new();
            let confirmation_handler = ctx.run_config().tool_confirmation_handler.clone();

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
            let mut conversation_history = match prompt_config
                .prepare_conversation(&ctx, &agent_name)
                .await
            {
                Ok(history) => history,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };

            let resolved_tools = match tool_setup.resolve(&ctx).await {
                Ok(tools) => tools,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };
            let tool_map = resolved_tools.map;
            let tool_declarations = resolved_tools.declarations;
            let valid_transfer_targets = resolved_tools.transfer_targets;

            let collect_long_running_ids = |content: &Content| -> Vec<String> {
                content
                    .parts
                    .iter()
                    .filter_map(|part| {
                        if let Part::FunctionCall { name, .. } = part
                            && let Some(tool) = tool_map.get(name)
                            && tool.is_long_running()
                        {
                            return Some(name.clone());
                        }
                        None
                    })
                    .collect()
            };


            // ===== CIRCUIT BREAKER STATE =====
            // Created fresh per invocation so it resets between runs.
            let mut circuit_breaker_state = circuit_breaker_threshold.map(CircuitBreakerState::new);

            // ===== RESPONSE-ID CONTINUITY (provider-neutral) =====
            // Tracks the `interaction_id` carried by the most recent model
            // response so the next request can continue the conversation via
            // `LlmRequest.previous_response_id`. This is generic plumbing: it
            // contains no Gemini- or transport-specific logic. Providers that
            // do not support response chaining leave `interaction_id` as `None`,
            // so this stays `None` and `previous_response_id` is never set
            // (a no-op for generateContent and all other providers).
            let mut last_interaction_id: Option<String> = None;

            // Multi-turn loop with max iterations
            let mut iteration = 0;
            let mut schema_retry_count: usize = 0;

            loop {
                // Cooperative cancellation: exit before starting another turn
                // if the invocation was cancelled (e.g. Runner::interrupt()).
                if ctx.is_cancelled() {
                    tracing::info!(agent.name = %agent_name, "invocation cancelled — stopping agent loop");
                    return;
                }
                iteration += 1;
                if iteration > max_iterations {
                    yield Err(adk_core::AdkError::agent(
                        format!("Max iterations ({max_iterations}) exceeded")
                    ));
                    return;
                }

                let config = build_generation_config(
                    generate_content_config.as_ref(),
                    prompt_config.output_schema.as_ref(),
                    ctx.run_config().cached_content.as_deref(),
                );

                let request = LlmRequest {
                    model: model.name().to_string(),
                    contents: conversation_history.clone(),
                    tools: tool_declarations.clone(),
                    config,
                    // Provider-neutral continuity: carry the most recent
                    // response's `interaction_id` forward so transports that
                    // support response chaining (e.g. the Gemini Interactions
                    // transport, which maps this to `previous_interaction_id`)
                    // can continue server-side. `None` for the first turn and
                    // for providers that never populate `interaction_id`.
                    previous_response_id: last_interaction_id.clone(),
                };

                // ===== ENHANCED PLUGIN: BEFORE MODEL CALL =====
                // Enhanced plugins can modify the request or short-circuit the model call.
                // They run before legacy before_model_callbacks.
                #[cfg(feature = "enhanced-plugins")]
                let (request, model_response_override_from_plugin) = {
                    if let Some(epm) = &enhanced_plugin_manager {
                        match epm.run_before_model_call(request, ctx.clone() as Arc<dyn CallbackContext>).await {
                            Ok(BeforeModelCallResult::Continue(modified_request)) => {
                                (modified_request, None)
                            }
                            Ok(BeforeModelCallResult::ShortCircuit(response)) => {
                                // Use a default request since we're short-circuiting
                                (LlmRequest::new("", vec![]), Some(response))
                            }
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        }
                    } else {
                        (request, None)
                    }
                };
                #[cfg(not(feature = "enhanced-plugins"))]
                let model_response_override_from_plugin: Option<LlmResponse> = None;

                // ===== BEFORE MODEL CALLBACKS =====
                // These can modify the request or skip the model call by returning a response
                let mut current_request = request;
                let mut model_response_override = model_response_override_from_plugin;
                if model_response_override.is_none() {
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
                    // Surface and track the response id for provider-neutral continuity.
                    cached_event.llm_response.interaction_id = cached_response.interaction_id.clone();
                    if cached_response.interaction_id.is_some() {
                        last_interaction_id = cached_response.interaction_id.clone();
                    }
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
                    let trace_request_json = trace_json_payload(
                        &request,
                        ctx.run_config().record_payloads,
                        ctx.run_config().trace_payload_max_bytes,
                    );

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
                        "gcp.vertex.agent.llm_request" = %trace_request_json,
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
                        // Cooperative cancellation: stop consuming the model
                        // stream promptly when the invocation is cancelled. This
                        // drops `response_stream`, releasing the provider connection.
                        if ctx.is_cancelled() {
                            tracing::info!(agent.name = %agent_name, "invocation cancelled during LLM streaming");
                            return;
                        }
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
                            let long_running_tool_ids = chunk
                                .content
                                .as_ref()
                                .map(&collect_long_running_ids)
                                .unwrap_or_default();
                            yield Ok(build_partial_llm_event(
                                &llm_event_id,
                                &invocation_id,
                                &agent_name,
                                &request_json,
                                &chunk,
                                long_running_tool_ids,
                            ));
                        }

                        // Track the response id for provider-neutral continuity.
                        // Transports that support response chaining populate
                        // `interaction_id`; others leave it `None` (no-op).
                        if chunk.interaction_id.is_some() {
                            last_interaction_id = chunk.interaction_id.clone();
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

                        if let Some(last) = &last_chunk {
                            final_provider_metadata = last.provider_metadata.clone();
                        }
                        let long_running_tool_ids = accumulated_content
                            .as_ref()
                            .map(&collect_long_running_ids)
                            .unwrap_or_default();
                        yield Ok(build_final_llm_event(
                            &llm_event_id,
                            &invocation_id,
                            &agent_name,
                            &request_json,
                            accumulated_content.as_ref(),
                            last_chunk.as_ref(),
                            long_running_tool_ids,
                        ));
                    }

                    // A provider that reports a terminal error inside an `Ok`
                    // response ends the turn. The event above already carries the
                    // error fields so the failure is observable and persisted;
                    // this converts it into a `Result` failure so callers, retry
                    // policy, and telemetry see it rather than reading an empty
                    // turn as success.
                    //
                    // In this workspace `error_code` marks a genuine failure —
                    // truncation is reported through `finish_reason`
                    // (`FinishReason::MaxTokens`), not through `error_code`.
                    if let Some(ref last) = last_chunk
                        && let Some(ref code) = last.error_code
                    {
                        let message = last
                            .error_message
                            .clone()
                            .unwrap_or_else(|| "provider reported a terminal error".to_string());
                        tracing::error!(
                            error.code = %code,
                            error.message = %message,
                            agent = %agent_name,
                            "model reported a terminal error"
                        );
                        // The provider's own code is preserved in the ADK error code
                        // so retry policy and telemetry can key on it.
                        // Built before the yield point: a borrow may not cross it.
                        // `AdkError::code` is `&'static str`, so the provider's own
                        // code travels in the details metadata instead, where retry
                        // policy and telemetry can read it.
                        let mut details = adk_core::ErrorDetails::default();
                        details
                            .metadata
                            .insert("provider_error_code".to_string(), serde_json::json!(code));
                        let provider_error = adk_core::AdkError::new(
                            adk_core::ErrorComponent::Model,
                            adk_core::ErrorCategory::Internal,
                            "model.provider_error",
                            format!("{code}: {message}"),
                        )
                        .with_details(details);
                        yield Err(provider_error);
                        return;
                    }

                    // Record LLM response to span before guard drops
                    if let Some(ref content) = accumulated_content {
                        let response_json = trace_json_payload(
                            content,
                            ctx.run_config().record_payloads,
                            ctx.run_config().trace_payload_max_bytes,
                        );
                        llm_span.record("gcp.vertex.agent.llm_response", &response_json);
                    }
                }

                // ===== ENHANCED PLUGIN: AFTER MODEL CALL =====
                // Enhanced plugins can modify the accumulated model response.
                // They run after the full response is accumulated (not per-chunk).
                #[cfg(feature = "enhanced-plugins")]
                if let Some(epm) = &enhanced_plugin_manager
                    && let Some(ref content) = accumulated_content {
                        let response_for_hook = LlmResponse {
                            content: Some(content.clone()),
                            provider_metadata: final_provider_metadata.clone(),
                            ..Default::default()
                        };
                        match epm.run_after_model_call(response_for_hook, ctx.clone() as Arc<dyn CallbackContext>).await {
                            Ok(adk_plugin::AfterModelCallResult::Continue(modified_response)) => {
                                accumulated_content = modified_response.content;
                                if modified_response.provider_metadata.is_some() {
                                    final_provider_metadata = modified_response.provider_metadata;
                                }
                            }
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
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
                    if let Some(ref output_key) = output_key
                        && !has_function_calls
                    {
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

                if !has_function_calls {
                    // ===== OUTPUT SCHEMA VALIDATION =====
                    // When output_schema is set, validate the response text against
                    // the schema. If invalid, retry with a correction prompt up to
                    // output_max_retries times.
                    if let Some(schema) = &prompt_config.output_schema {
                        let text = accumulated_content
                            .as_ref()
                            .map(|c| {
                                c.parts
                                    .iter()
                                    .filter_map(|p| {
                                        if let Part::Text { text } = p {
                                            Some(text.as_str())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();

                        if !text.is_empty()
                            && let Err(validation_error) = validate_output_against_schema(&text, schema)
                        {
                                if schema_retry_count >= output_max_retries {
                                    yield Err(adk_core::AdkError::agent(format!(
                                        "output schema validation failed after {} attempts",
                                        output_max_retries
                                    )));
                                    return;
                                }
                                schema_retry_count += 1;

                                // Append a correction prompt and retry
                                let correction = format!(
                                    "Your output did not match the required schema. Error: {}. Please produce valid JSON matching the schema.",
                                    validation_error
                                );
                                conversation_history.push(Content {
                                    role: "user".to_string(),
                                    parts: vec![Part::Text { text: correction }],
                                });
                                continue;
                        }
                    }

                    // No function calls, we're done
                    // Record LLM response for tracing
                    if let Some(ref content) = accumulated_content {
                        let response_json = trace_json_payload(
                            content,
                            ctx.run_config().record_payloads,
                            ctx.run_config().trace_payload_max_bytes,
                        );
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

                    let fc_parts = collect_function_calls(content, &invocation_id);

                    // ===== HANDLE transfer_to_agent BEFORE DISPATCH =====
                    // Transfer calls cause an immediate return from the stream,
                    // so they must be handled inline regardless of strategy.
                    let mut transfer_handled = false;
                    for call in &fc_parts {
                        if call.name == "transfer_to_agent" {
                            let target_agent = call
                                .args
                                .get("agent_name")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                                .to_string();

                            let valid_target = valid_transfer_targets.iter().any(|n| n == &target_agent);
                            if !valid_target {
                                let error_content = Content {
                                    role: "function".to_string(),
                                    parts: vec![Part::FunctionResponse {
                                        function_response: FunctionResponseData::new(
                                            call.name.clone(),
                                            serde_json::json!({
                                                "error": format!(
                                                    "Agent '{}' not found. Available agents: {:?}",
                                                    target_agent, valid_transfer_targets
                                                )
                                            }),
                                        ),
                                        id: call.id.clone(),
                                        annotations: None,
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
                    let mut fc_parts: Vec<_> = fc_parts
                        .into_iter()
                        .filter(|call| {
                            if call.name == "transfer_to_agent" {
                                return false;
                            }
                            if let Some(tool) = tool_map.get(&call.name)
                                && tool.is_builtin()
                            {
                                adk_telemetry::debug!(tool.name = %call.name, "skipping built-in tool execution");
                                return false;
                            }
                            true
                        })
                        .collect();

                    // Guardrails must run before confirmation requests are constructed. This also
                    // makes a revised call's arguments the ones shown to the approver and included
                    // in its confirmation fingerprint.
                    for call in &mut fc_parts {
                        match screen_tool_call(&tool_guardrails, &call.name, &call.args).await {
                            ToolScreening::Allow(args) => call.args = args,
                            ToolScreening::Deny(reason) => {
                                call.guardrail_denial = Some(reason);
                            }
                        }
                    }

                    // ===== TOOL CONFIRMATION PRE-CHECK =====
                    // Tool confirmation interrupts cause an immediate return,
                    // so check before parallel dispatch.
                    let mut confirmation_interrupted = false;
                    for call in &fc_parts {
                        if call.guardrail_denial.is_none()
                            && (tool_confirmation_policy.requires_confirmation(&call.name)
                                || ctx.requires_tool_confirmation(&call.name))
                            && static_confirmation_decision(
                                &confirmation_decisions,
                                &confirmation_fingerprints,
                                &call.function_call_id,
                                &call.name,
                                &call.args,
                            )
                            .is_none()
                            && live_confirmation_decisions
                                .get(&call.function_call_id)
                                .copied()
                                .is_none()
                        {
                            let request = ToolConfirmationRequest {
                                tool_name: call.name.clone(),
                                function_call_id: Some(call.function_call_id.clone()),
                                args: call.args.clone(),
                            };
                            if let Some(handler) = confirmation_handler.as_ref() {
                                match handler.decide(&request).await {
                                    Ok(decision) => {
                                        live_confirmation_decisions
                                            .insert(call.function_call_id.clone(), decision);
                                        continue;
                                    }
                                    Err(error) => {
                                        yield Err(error);
                                        return;
                                    }
                                }
                            }

                                let mut ce = Event::new(&invocation_id);
                                ce.author = agent_name.clone();
                                ce.llm_response.interrupted = true;
                                ce.llm_response.turn_complete = true;
                                ce.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::Text {
                                        text: format!(
                                            "Tool confirmation required for '{}'. Provide approve/deny decision to continue.",
                                            call.name
                                        ),
                                    }],
                                });
                                ce.actions.tool_confirmation = Some(request);
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

                    // Create concurrency manager for semaphore-based tool dispatch enforcement.
                    // Per-tool overrides take precedence over the global limit.
                    let concurrency_manager = adk_core::ToolConcurrencyManager::new(
                        &ctx.run_config().tool_concurrency,
                    );

                    // Channel for streaming tool progress (stdout/stderr) onto the
                    // agent's EventStream while tools are still executing. Each
                    // AgentToolContext gets a clone; the dispatch loop below drains
                    // it concurrently and yields progress events to the client.
                    let (progress_tx, mut progress_rx) =
                        tokio::sync::mpsc::channel::<Event>(TOOL_PROGRESS_CAPACITY);

                    let executor = ToolExecutor {
                        ctx: ctx.clone(),
                        tool_map: &tool_map,
                        tool_retry_budgets: &tool_retry_budgets,
                        default_retry_budget: &default_retry_budget,
                        before_tool_callbacks: &before_tool_callbacks,
                        after_tool_callbacks: &after_tool_callbacks,
                        after_tool_callbacks_full: &after_tool_callbacks_full,
                        on_tool_error_callbacks: &on_tool_error_callbacks,
                        tool_confirmation_policy: &tool_confirmation_policy,
                        cb_mutex: &cb_mutex,
                        invocation_id: &invocation_id,
                        concurrency_manager: &concurrency_manager,
                        progress_tx: progress_tx.clone(),
                        tool_timeout,
                        confirmation_decisions: &confirmation_decisions,
                        confirmation_fingerprints: &confirmation_fingerprints,
                        live_confirmation_decisions: &live_confirmation_decisions,
                        #[cfg(feature = "enhanced-plugins")]
                        enhanced_plugin_manager: &enhanced_plugin_manager,
                    };

                    // Cooperative cancellation: skip tool execution if the
                    // invocation was cancelled while the model was streaming.
                    if ctx.is_cancelled() {
                        tracing::info!(agent.name = %agent_name, "invocation cancelled before tool dispatch");
                        return;
                    }

                    // ===== DISPATCH BASED ON STRATEGY =====
                    // Scoped so the dispatch future (which borrows the executor
                    // and circuit-breaker mutex) is dropped before we reclaim
                    // `cb_mutex` below.
                    let mut results = {
                        let dispatch = async {
                            let results: Vec<ToolExecutionResult> = match strategy {
                                ToolExecutionStrategy::Sequential => {
                                    let mut results = Vec::with_capacity(fc_parts.len());
                                    for call in fc_parts {
                                        results.push(executor.execute(call).await);
                                    }
                                    results
                                }
                                ToolExecutionStrategy::Parallel => {
                                    use futures::StreamExt as _;
                                    // Parallel is an explicit caller override. Tool
                                    // safety metadata is intentionally not inspected.
                                    // All concurrency enforcement is handled by the
                                    // ToolConcurrencyManager semaphore inside ToolExecutor.
                                    // Use fc_parts.len() as buffer so all futures can start
                                    // and queue on the semaphore for proper per-tool limiting.
                                    let buffer_size = fc_parts.len().max(1);
                                    futures::stream::iter(
                                        fc_parts.into_iter().map(|call| executor.execute(call)),
                                    )
                                    .buffer_unordered(buffer_size)
                                    .collect()
                                    .await
                                }
                                ToolExecutionStrategy::Auto => {
                                    // A call may overlap another only when its tool is
                                    // read-only *and* declares concurrency safety.
                                    let (concurrent_fcs, sequential_fcs): (Vec<_>, Vec<_>) =
                                        fc_parts.into_iter().partition(|call| {
                                            tool_map.get(&call.name).is_some_and(|tool| {
                                                tool.is_read_only() && tool.is_concurrency_safe()
                                            })
                                        });
                                    let mut all_results = Vec::new();

                                    // Concurrency enforcement is handled by the semaphore
                                    // inside ToolExecutor.
                                    if !concurrent_fcs.is_empty() {
                                        use futures::StreamExt as _;
                                        let buffer_size = concurrent_fcs.len().max(1);
                                        all_results.extend(
                                            futures::stream::iter(
                                                concurrent_fcs
                                                    .into_iter()
                                                    .map(|call| executor.execute(call)),
                                            )
                                            .buffer_unordered(buffer_size)
                                            .collect::<Vec<_>>()
                                            .await,
                                        );
                                    }

                                    // Everything else runs one at a time.
                                    for call in sequential_fcs {
                                        all_results.push(executor.execute(call).await);
                                    }
                                    all_results
                                }
                            };
                            results
                        };

                        // Drain tool progress concurrently with execution, yielding
                        // each chunk as a partial Event the moment it arrives. The
                        // dispatch future and the progress receiver are polled together
                        // so output streams live rather than buffering until the tool
                        // finishes.
                        tokio::pin!(dispatch);
                        let results = loop {
                            tokio::select! {
                                biased;
                                Some(progress_event) = progress_rx.recv() => {
                                    yield Ok(progress_event);
                                }
                                done = &mut dispatch => break done,
                            }
                        };
                        // Flush any progress chunks buffered between the last poll and completion.
                        while let Ok(progress_event) = progress_rx.try_recv() {
                            yield Ok(progress_event);
                        }
                        results
                    };
                    // Preserve LLM-returned order even when tool futures finish out of order.
                    results.sort_by_key(|r| r.index);

                    // Restore circuit breaker state from the mutex
                    circuit_breaker_state = cb_mutex.into_inner().unwrap_or_else(|e| e.into_inner());

                    // Yield results in original order
                    for result in results {
                        let mut tool_event = Event::new(&invocation_id);
                        tool_event.author = agent_name.clone();
                        tool_event.actions = result.actions;
                        tool_event.llm_response.content = Some(result.content.clone());
                        yield Ok(tool_event);

                        if result.escalate_or_skip {
                            return;
                        }

                        conversation_history.push(result.content);
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

#[cfg(test)]
mod run_helper_tests {
    use super::*;

    #[test]
    fn generation_config_layers_schema_and_cached_content() {
        let base =
            adk_core::GenerateContentConfig { temperature: Some(0.25), ..Default::default() };
        let schema = serde_json::json!({"type": "object"});

        let config = build_generation_config(Some(&base), Some(&schema), Some("cached/example"))
            .expect("config should be present");

        assert_eq!(config.temperature, Some(0.25));
        assert_eq!(config.response_schema, Some(schema));
        assert_eq!(config.cached_content.as_deref(), Some("cached/example"));
    }

    #[test]
    fn function_calls_preserve_order_and_create_fallback_ids() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![
                Part::Text { text: "before".to_string() },
                Part::FunctionCall {
                    name: "first".to_string(),
                    args: serde_json::json!({"value": 1}),
                    id: None,
                    thought_signature: None,
                },
                Part::FunctionCall {
                    name: "second".to_string(),
                    args: serde_json::json!({"value": 2}),
                    id: Some("provider-id".to_string()),
                    thought_signature: None,
                },
            ],
        };

        let calls = collect_function_calls(&content, "invocation");

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].index, 0);
        assert_eq!(calls[0].name, "first");
        assert_eq!(calls[0].function_call_id, "invocation_first_0");
        assert_eq!(calls[1].index, 1);
        assert_eq!(calls[1].name, "second");
        assert_eq!(calls[1].function_call_id, "provider-id");
    }
}
