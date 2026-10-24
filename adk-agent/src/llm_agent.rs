use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, AfterToolCallbackFull, Agent,
    BeforeAgentCallback, BeforeModelCallback, BeforeModelResult, BeforeToolCallback,
    CallbackContext, Content, Event, EventActions, FunctionResponseData, GlobalInstructionProvider,
    GovernedCall, InstructionProvider, InvocationContext, Llm, LlmRequest, LlmResponse,
    MemoryEntry, OnToolErrorCallback, Part, ReadonlyContext, Result, RetryBudget, Tool,
    ToolAuthorization, ToolCallbackContext, ToolConfirmationPolicy, ToolConfirmationRequest,
    ToolContext, ToolEffect, ToolExecutionStrategy, ToolGate, ToolOutcome, Toolset,
    authorize_tool_call,
};
use async_stream::stream;
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
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
        GuardrailScreen, GuardrailSet, Phase, Screened, ToolGuardrailSet, Warnings,
        enforce_guardrails, screen_model_output, screen_tool_result,
    },
    invocation_hooks::HookCallbacks,
    skill_shim::{SelectionPolicy, SkillIndex, apply_skill_injection},
    tool_call_markup::normalize_option_content,
    workflow::with_user_content_override,
};

/// Default maximum number of LLM round-trips (iterations) before the agent stops.
pub const DEFAULT_MAX_ITERATIONS: u32 = 100;

/// Default tool execution timeout (5 minutes).
pub const DEFAULT_TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Recorded in place of a request, response, or tool payload when
/// [`RunConfig::record_payloads`](adk_core::RunConfig::record_payloads) is off.
const PAYLOAD_OMITTED: &str = "[omitted: set RunConfig::record_payloads to record payloads]";

/// Renders a payload for a span field or debug log.
///
/// Payloads carry user text and tool data, so nothing of them is recorded unless
/// `record_payloads` is set: in full with the `record-payloads` feature, otherwise
/// truncated to `max_bytes`.
fn trace_json_payload<T: serde::Serialize>(
    value: &T,
    record_payloads: bool,
    max_bytes: usize,
) -> String {
    if !record_payloads {
        return PAYLOAD_OMITTED.to_string();
    }
    let json = serde_json::to_string(value).unwrap_or_default();
    if cfg!(feature = "record-payloads") {
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

/// The text logged for an error message or a denial reason: the text when the run records
/// payloads, else a placeholder.
///
/// Provider errors can echo prompts, and tool errors and guardrail reasons can quote
/// arguments and credentials, so their text follows the same rule as payloads. Log the content-free part
/// with [`AdkError::diagnostics`](adk_core::AdkError::diagnostics) beside it.
fn traced_message(run_config: &adk_core::RunConfig, message: &str) -> String {
    trace_json_payload(&message, run_config.record_payloads, run_config.trace_payload_max_bytes)
}

/// The audit action of a tool call a hook, callback, or plugin answered before the governed
/// path saw it (spec 3.1 §10, F1).
const SHORT_CIRCUIT_ACTION: &str = "tool.short_circuit";
/// `answeredBy` of an enhanced plugin's `ShortCircuit`.
#[cfg(feature = "enhanced-plugins")]
const SHORT_CIRCUIT_BY_ENHANCED_PLUGIN: &str = "enhanced_plugin";
/// `answeredBy` of a run-wide `InvocationHooks::before_tool` answer.
const SHORT_CIRCUIT_BY_INVOCATION_HOOK: &str = "invocation_hook";
/// `answeredBy` of the agent's own `BeforeToolCallback` answer.
const SHORT_CIRCUIT_BY_BEFORE_TOOL_CALLBACK: &str = "before_tool_callback";

#[derive(Debug, Clone)]
struct PendingToolCall {
    index: usize,
    name: String,
    args: serde_json::Value,
    id: Option<String>,
    function_call_id: String,
    /// The held request this call resumes, when it is the call that request describes.
    resumes: Option<ToolConfirmationRequest>,
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

/// Appends streamed parts, joining a delta onto the part it continues.
///
/// Consecutive text deltas form one text part. A thinking delta joins the previous
/// thinking part until that part is signed, since a signature closes its block.
fn append_streamed_parts(parts: &mut Vec<Part>, incoming: Vec<Part>) {
    for part in incoming {
        match (parts.last_mut(), part) {
            (Some(Part::Text { text }), Part::Text { text: delta }) => text.push_str(&delta),
            (
                Some(Part::Thinking { thinking, signature: signature @ None }),
                Part::Thinking { thinking: delta, signature: delta_signature },
            ) => {
                thinking.push_str(&delta);
                *signature = delta_signature;
            }
            (_, part) => parts.push(part),
        }
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
            resumes: None,
        })
        .collect()
}

fn collect_long_running_tool_ids(
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    content: &Content,
) -> Vec<String> {
    content
        .parts
        .iter()
        .filter_map(|part| {
            if let Part::FunctionCall { name, .. } = part
                && tool_map.get(name).is_some_and(|tool| tool.is_long_running())
            {
                return Some(name.clone());
            }
            None
        })
        .collect()
}

/// Builds one streamed model event.
///
/// `request_debug` is the bounded request copy from [`trace_json_payload`]: the
/// full request carries the whole conversation, so embedding it in every
/// terminal event would make stored history grow quadratically.
fn build_partial_llm_event(
    event_id: &str,
    invocation_id: &str,
    agent_name: &str,
    request_debug: &str,
    chunk: &LlmResponse,
    long_running_tool_ids: Vec<String>,
) -> Event {
    let mut event = Event::with_id(event_id, invocation_id);
    event.author = agent_name.to_string();
    // Payload snapshots go on the terminal chunk only; repeating them on every
    // incremental chunk turns streaming into O(request_size * chunk_count) work.
    if !chunk.partial {
        event.llm_request = Some(request_debug.to_string());
        event.provider_metadata.insert(
            "gcp.vertex.agent.llm_response".to_string(),
            serde_json::to_string(chunk).unwrap_or_default(),
        );
    }
    event.llm_response.partial = chunk.partial;
    event.llm_response.turn_complete = chunk.turn_complete;
    event.llm_response.finish_reason = chunk.finish_reason;
    event.llm_response.usage_metadata = chunk.usage_metadata.clone();
    event.llm_response.citation_metadata = chunk.citation_metadata.clone();
    event.llm_response.content = chunk.content.clone();
    event.llm_response.provider_metadata = chunk.provider_metadata.clone();
    event.llm_response.interaction_id = chunk.interaction_id.clone();
    // Provider failures delivered as `Ok(LlmResponse { error_code, .. })` must
    // remain observable on the streamed event.
    event.llm_response.interrupted = chunk.interrupted;
    event.llm_response.error_code = chunk.error_code.clone();
    event.llm_response.error_message = chunk.error_message.clone();
    event.llm_response.model = chunk.model.clone();
    event.llm_response.provider = chunk.provider.clone();
    event.long_running_tool_ids = long_running_tool_ids;
    event
}

/// Builds the single model event of a non-streaming turn; see
/// [`build_partial_llm_event`] for `request_debug`.
fn build_final_llm_event(
    event_id: &str,
    invocation_id: &str,
    agent_name: &str,
    request_debug: &str,
    content: Option<&Content>,
    last_chunk: Option<&LlmResponse>,
    long_running_tool_ids: Vec<String>,
) -> Event {
    let mut event = Event::with_id(event_id, invocation_id);
    event.author = agent_name.to_string();
    event.llm_request = Some(request_debug.to_string());
    event.llm_response.content = content.cloned();
    event.llm_response.partial = false;
    event.llm_response.turn_complete = true;

    if let Some(last_chunk) = last_chunk {
        event.llm_response.turn_complete = last_chunk.turn_complete;
        event.llm_response.finish_reason = last_chunk.finish_reason;
        event.llm_response.usage_metadata = last_chunk.usage_metadata.clone();
        event.llm_response.citation_metadata = last_chunk.citation_metadata.clone();
        event.llm_response.provider_metadata = last_chunk.provider_metadata.clone();
        event.llm_response.interaction_id = last_chunk.interaction_id.clone();
        event.llm_response.interrupted = last_chunk.interrupted;
        event.llm_response.error_code = last_chunk.error_code.clone();
        event.llm_response.error_message = last_chunk.error_message.clone();
        event.llm_response.model = last_chunk.model.clone();
        event.llm_response.provider = last_chunk.provider.clone();
        event.provider_metadata.insert(
            "gcp.vertex.agent.llm_response".to_string(),
            serde_json::to_string(last_chunk).unwrap_or_default(),
        );
    }

    event.long_running_tool_ids = long_running_tool_ids;
    event
}

/// The event that ends a run on a call held for confirmation.
fn confirmation_event(
    invocation_id: &str,
    agent_name: &str,
    request: ToolConfirmationRequest,
) -> Event {
    let mut event = Event::new(invocation_id);
    event.author = agent_name.to_string();
    event.llm_response.interrupted = true;
    event.llm_response.turn_complete = true;
    event.llm_response.content = Some(Content::new("model").with_text(format!(
        "Tool confirmation required for '{}'. Provide approve/deny decision to continue.",
        request.tool_name
    )));
    event.actions.tool_confirmation = Some(request);
    event
}

/// An LLM-powered agent that orchestrates tool calls and sub-agent delegation.
///
/// `LlmAgent` is the primary agent type in ADK. It sends requests to an LLM,
/// executes tool calls from the response, and iterates until the model produces
/// a final text response or the iteration limit is reached.
///
/// Use [`LlmAgentBuilder::new`] to construct instances.
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
    /// Whether a toolset that fails to resolve its tools fails the turn.
    strict_toolsets: bool,
    sub_agents: Vec<Arc<dyn Agent>>,
    output_key: Option<String>,
    /// Default generation config (temperature, top_p, etc.) applied to every LLM request.
    generate_content_config: Option<adk_core::GenerateContentConfig>,
    /// Maximum number of LLM round-trips before stopping
    max_iterations: u32,
    /// Timeout for individual tool executions
    tool_timeout: std::time::Duration,
    /// Whether tool and callback error messages reach the model.
    expose_tool_error_messages: bool,
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
    /// Whether consecutive agent-delegation calls may execute concurrently.
    parallelize_agent_delegations: bool,
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
        // Instructions travel as `system` contents so providers place them in their
        // native system prompt instead of guessing from leading user turns.
        let mut preamble = Vec::new();

        if let Some(provider) = &self.global_instruction_provider {
            let instruction = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            if !instruction.is_empty() {
                preamble.push(Content::new("system").with_text(instruction));
            }
        } else if let Some(template) = &self.global_instruction {
            let instruction = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            if !instruction.is_empty() {
                preamble.push(Content::new("system").with_text(instruction));
            }
        }

        if let Some(provider) = &self.instruction_provider {
            let instruction = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            if !instruction.is_empty() {
                preamble.push(Content::new("system").with_text(instruction));
            }
        } else if let Some(template) = &self.instruction {
            let instruction = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            if !instruction.is_empty() {
                preamble.push(Content::new("system").with_text(instruction));
            }
        }

        if let Some(schema) = &self.output_schema {
            preamble.push(Content::new("system").with_text(format!(
                "You MUST respond with valid JSON conforming to this schema: {schema}. Do not include any text outside the JSON object."
            )));
        }

        // An agent that received control by transfer sees only the user's turns and its own.
        // Otherwise, as in a workflow, other agents' turns reach it as user-role context, so
        // it never replays an earlier agent's answer as its own.
        let mut session_history = if ctx.authoritative_transfer_targets()
            || !ctx.run_config().transfer_targets.is_empty()
        {
            ctx.session().conversation_history_scoped(Some(agent_name), ctx.branch())
        } else {
            ctx.session().conversation_history_with_context(agent_name, ctx.branch())
        };
        let mut current_user_content = ctx.user_content().clone();
        if let Some(index) = &self.skills_index {
            apply_skill_injection(
                &mut current_user_content,
                index.as_ref(),
                &self.skill_policy,
                self.max_skill_chars,
            );
        }
        // A run that resumes held calls carries an empty user turn: no parts, or only blank
        // text. A turn without parts never reaches the projected history, so the last user
        // turn there is the person's message, which an empty turn must not stand in for.
        let empty_turn = current_user_content
            .parts
            .iter()
            .all(|part| matches!(part, Part::Text { text } if text.trim().is_empty()));
        if !empty_turn {
            match session_history.iter().rposition(|content| {
                content.role == "user" && !adk_core::history::is_other_agent_context(content)
            }) {
                Some(index) => session_history[index] = current_user_content.clone(),
                None => session_history.push(current_user_content.clone()),
            }
        }
        // An empty user turn adds nothing to the conversation, and providers such as
        // Anthropic reject empty text blocks.
        session_history.retain(|content| {
            content.role != "user"
                || content
                    .parts
                    .iter()
                    .any(|part| !matches!(part, Part::Text { text } if text.trim().is_empty()))
        });
        // An agent limited to its current turn sees the latest user input and what followed.
        // A run that resumes held calls begins that turn at the message that led to the
        // earliest one, so each resumed result answers a call the model can see; an empty
        // turn without held calls begins it at the person's last message. Any other run sends
        // the current user turn alone.
        let turn_start = match self.include_contents {
            adk_core::IncludeContents::Default => None,
            adk_core::IncludeContents::None => {
                let held: Vec<String> = ctx
                    .session()
                    .held_tool_calls(agent_name, ctx.branch())
                    .into_iter()
                    .filter_map(|call| call.id)
                    .collect();
                let earliest_held = session_history.iter().position(|content| {
                    content.parts.iter().any(|part| {
                        matches!(part, Part::FunctionCall { id: Some(id), .. } if held.contains(id))
                    })
                });
                let is_message = |content: &Content| {
                    content.role == "user"
                        && content.carries_user_input()
                        && !adk_core::history::is_other_agent_context(content)
                };
                match earliest_held {
                    Some(call) => {
                        Some(session_history[..call].iter().rposition(is_message).unwrap_or(0))
                    }
                    None if empty_turn => {
                        Some(session_history.iter().rposition(is_message).unwrap_or(0))
                    }
                    None => None,
                }
            }
        };
        // A history that ends on the model's answer — a resuming run whose model already
        // answered after the held call, or a loop running this agent again after its own
        // answer — would be read as a prefill, which models without prefill support
        // (Anthropic's newer ones) reject. It gets a user turn to answer.
        if session_history.last().is_some_and(|last| {
            last.role == "model"
                && !last.parts.iter().any(|part| matches!(part, Part::FunctionCall { .. }))
        }) {
            let note = if empty_turn { RESUMED_TURN_NOTE } else { CONTINUE_TURN_NOTE };
            session_history.push(Content::new("user").with_text(note));
        }

        Ok(match (self.include_contents, turn_start) {
            (adk_core::IncludeContents::None, Some(start)) => {
                preamble.extend(session_history.drain(start..));
                preamble
            }
            (adk_core::IncludeContents::None, None) => {
                preamble.push(current_user_content);
                preamble
            }
            (adk_core::IncludeContents::Default, _) => {
                preamble.extend(session_history);
                preamble
            }
        })
    }
}

/// The user turn a run without user input sends when the conversation would otherwise
/// end on a model message.
const RESUMED_TURN_NOTE: &str =
    "(No new message from the user. Continue from the tool results above.)";

/// The user turn a run sends when its history ends on this agent's own answer.
const CONTINUE_TURN_NOTE: &str =
    "(No new message from the user. Continue from the conversation above.)";

struct ToolSetup {
    tools: Vec<Arc<dyn Tool>>,
    toolsets: Vec<Arc<dyn Toolset>>,
    strict_toolsets: bool,
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
            strict_toolsets: agent.strict_toolsets,
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
            let provided = match toolset.tools(ctx.clone() as Arc<dyn ReadonlyContext>).await {
                Ok(provided) => provided,
                // One unreachable server costs the model that server's tools, not the turn.
                Err(error) if !self.strict_toolsets => {
                    tracing::warn!(
                        agent.name = %ctx.agent_name(),
                        toolset.name = %toolset.name(),
                        error = %error.diagnostics(),
                        error.message = %traced_message(ctx.run_config(), &error.message),
                        "toolset failed to resolve its tools; continuing without them"
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            for tool in provided {
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

    /// Lets static and invocation-scoped toolsets enrich each LLM request.
    async fn process_llm_request(
        &self,
        ctx: &Arc<dyn InvocationContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        for toolset in self.toolsets.iter().map(AsRef::as_ref).chain(
            ctx.run_config().runtime_toolsets.iter().map(|runtime| runtime.toolset().as_ref()),
        ) {
            toolset.process_llm_request(ctx.clone() as Arc<dyn ReadonlyContext>, request).await?;
        }
        Ok(())
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
            enforce_guardrails(input_guardrails.as_ref(), ctx.user_content(), Phase::Input)
                .await?
                .content;
        if content.role != ctx.user_content().role || content.parts != ctx.user_content().parts {
            Ok(with_user_content_override(ctx, content))
        } else {
            Ok(ctx)
        }
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

    fn continues_turn(metadata: Option<&serde_json::Value>) -> bool {
        metadata
            .and_then(|metadata| metadata.get("continue_turn"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
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
    let parsed: serde_json::Value = serde_json::from_str(strip_json_code_fence(text))
        .map_err(|e| format!("Response is not valid JSON: {e}"))?;

    let validator =
        jsonschema::validator_for(schema).map_err(|e| format!("Invalid schema: {e}"))?;

    let errors: Vec<String> = validator.iter_errors(&parsed).map(|e| e.to_string()).collect();

    if errors.is_empty() { Ok(parsed) } else { Err(errors.join("; ")) }
}

/// The JSON inside one Markdown code fence wrapping all of `text`, or `text` unchanged.
///
/// A model asked for structured output sometimes wraps it in a ```` ```json ```` fence,
/// most often when tools are configured and the schema constraint is best-effort. JSON
/// never starts with a fence, so valid output is unchanged.
fn strip_json_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return text;
    };
    let Some(body) = rest.strip_suffix("```") else {
        return text;
    };
    // The info string (`json`, `JSON`, ...) runs to the end of the opening line.
    let body = match body.find('\n') {
        Some(newline) if body[..newline].chars().all(|c| c.is_ascii_alphanumeric()) => {
            &body[newline + 1..]
        }
        _ => body.trim_start_matches(|c: char| c.is_ascii_alphanumeric()),
    };
    if body.contains("```") {
        return text;
    }
    body.trim()
}

/// Rebuilds the chunk an after-model callback replaced.
///
/// The replacement's content and metadata win. The chunk's place in the stream and what the
/// call used stay with the original: `partial` and `turn_complete` stay set when the
/// original had them, and `finish_reason` and `usage_metadata` carry over unless the
/// replacement sets them. A callback that returns `LlmResponse { content, ..Default::default() }`
/// for a streamed delta therefore still produces one persisted event per response, with usage.
fn replace_chunk(original: &LlmResponse, mut replacement: LlmResponse) -> LlmResponse {
    replacement.partial |= original.partial;
    replacement.turn_complete |= original.turn_complete;
    if replacement.finish_reason.is_none() {
        replacement.finish_reason = original.finish_reason;
    }
    if replacement.usage_metadata.is_none() {
        replacement.usage_metadata = original.usage_metadata.clone();
    }
    replacement
}

/// Whether a final model turn carries an answer: text that is not only whitespace, data, a
/// file, a resource, or a provider tool part. Thinking alone is not an answer.
fn has_answer_content(content: &Content) -> bool {
    content.parts.iter().any(|part| match part {
        Part::Text { text } => !text.trim().is_empty(),
        Part::Thinking { .. } => false,
        Part::InlineData { .. }
        | Part::FileData { .. }
        | Part::FunctionCall { .. }
        | Part::FunctionResponse { .. }
        | Part::ServerToolCall { .. }
        | Part::ServerToolResponse { .. }
        | Part::EmbeddedResource { .. } => true,
    })
}

/// The error for a model turn that ended without an answer.
fn empty_response_error(finish_reason: Option<adk_core::FinishReason>) -> adk_core::AdkError {
    let mut details = adk_core::ErrorDetails::default();
    if let Some(reason) = finish_reason {
        details
            .metadata
            .insert("finish_reason".to_string(), serde_json::json!(format!("{reason:?}")));
    }
    let reason = finish_reason.map_or_else(|| "none".to_string(), |reason| format!("{reason:?}"));
    adk_core::AdkError::new(
        adk_core::ErrorComponent::Model,
        adk_core::ErrorCategory::Internal,
        "model.empty_response",
        format!(
            "the model ended its turn without an answer: no text, data or tool call, only \
             thinking or whitespace (finish reason: {reason}); retry the run, or check the \
             model's output limit and safety settings"
        ),
    )
    .with_details(details)
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
/// extract the structured result. One Markdown code fence wrapping the whole
/// text is removed first.
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

    serde_json::from_str(strip_json_code_fence(&text))
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
    strict_toolsets: bool,
    sub_agents: Vec<Arc<dyn Agent>>,
    output_key: Option<String>,
    generate_content_config: Option<adk_core::GenerateContentConfig>,
    max_iterations: u32,
    tool_timeout: std::time::Duration,
    expose_tool_error_messages: bool,
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
    parallelize_agent_delegations: bool,
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
            strict_toolsets: false,
            sub_agents: Vec::new(),
            output_key: None,
            generate_content_config: None,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            expose_tool_error_messages: false,
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
            parallelize_agent_delegations: false,
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
    ///
    /// The model is not offered the parent as a target, and the runner refuses a transfer to
    /// the parent that a tool of this agent requests.
    pub fn disallow_transfer_to_parent(mut self, disallow: bool) -> Self {
        self.disallow_transfer_to_parent = disallow;
        self
    }

    /// Prevent this agent from transferring control to peer agents.
    ///
    /// The model is not offered its parent's other sub-agents as targets, and the runner
    /// refuses a transfer to one of them that a tool of this agent requests.
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
    ///
    /// The value is the concatenated text of the final model response: the last turn, the
    /// one without tool calls. Text the model wrote beside a tool call in an earlier turn is
    /// not included, and each run replaces the value. When a before-agent callback answers
    /// for the agent, its content is stored instead. When an `output_schema` is set, it is
    /// written only after the response passes schema validation; a response that exhausts
    /// `output_max_retries` leaves the key unset.
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
    ///
    /// A tool whose [`Tool::timeout_override`] returns `Some` uses that instead,
    /// so an `AgentTool` delegation is bounded by its own timeout. A
    /// [`ToolEffect::NonIdempotent`] call that times out is answered with an
    /// [`outcome_unknown_response`](adk_core::outcome_unknown_response), because
    /// its side effect may already have happened.
    pub fn tool_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    /// Sends the full message of a tool or tool-callback error to the model.
    ///
    /// By default a call that fails with an [`AdkError`](adk_core::AdkError) is answered with
    /// its component, category, and code only
    /// ([`tool_error_response`](adk_core::tool_error_response)), because the message can quote
    /// credentials, internal hosts, or upstream bodies, and the function response is sent to the
    /// model provider and stored in the session. The message is logged when
    /// [`RunConfig::record_payloads`](adk_core::RunConfig::record_payloads) is set. Errors with
    /// code [`TOOL_INVALID_ARGUMENTS_CODE`](adk_core::TOOL_INVALID_ARGUMENTS_CODE) always keep
    /// their message, as do timeouts, the agent's own refusals, and a tool's rejections
    /// ([`ErrorCategory::is_rejection`](adk_core::ErrorCategory::is_rejection)), cut at
    /// [`TOOL_ERROR_MESSAGE_LIMIT`](adk_core::TOOL_ERROR_MESSAGE_LIMIT) characters.
    ///
    /// Enable this only when every tool's error messages are safe to send to the provider. An
    /// [`on_tool_error`](Self::on_tool_error) callback chooses the text for one failure instead.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agent = LlmAgentBuilder::new("support")
    ///     .model(model)
    ///     .tool(lookup_order)
    ///     .expose_tool_error_messages(true)
    ///     .build()?;
    /// ```
    pub fn expose_tool_error_messages(mut self, expose: bool) -> Self {
        self.expose_tool_error_messages = expose;
        self
    }

    /// Add a tool to this agent's toolbox.
    ///
    /// The model calls tools by name, so each tool added here needs a name of
    /// its own: [`build`](Self::build) fails when two share one.
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Register a dynamic toolset for per-invocation tool resolution.
    ///
    /// Toolsets are resolved at the start of each `run()` call, and again after
    /// each round of tool calls, using the invocation's `ReadonlyContext`. This
    /// enables context-dependent tools like per-user browser sessions from a
    /// pool.
    ///
    /// A toolset whose `tools()` call fails is skipped for that resolution with
    /// a `warn`-level trace naming the toolset and the error, and the turn
    /// continues with the remaining tools. Use
    /// [`strict_toolsets`](Self::strict_toolsets) to fail the turn instead.
    pub fn toolset(mut self, toolset: Arc<dyn Toolset>) -> Self {
        self.toolsets.push(toolset);
        self
    }

    /// Fail the turn when any toolset cannot resolve its tools.
    ///
    /// Off by default: a failing toolset, such as an MCP server that is down,
    /// is skipped and the model runs with the tools that did resolve. Turn this
    /// on when the agent must not run without every toolset, for example when
    /// a toolset supplies the only tools that can complete its task. It applies
    /// to toolsets registered here and to `RunConfig::runtime_toolsets`.
    /// Duplicate tool names fail the turn either way.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agent = LlmAgentBuilder::new("operator")
    ///     .model(model)
    ///     .toolset(Arc::new(mcp_toolset))
    ///     .strict_toolsets(true)
    ///     .build()?;
    /// ```
    pub fn strict_toolsets(mut self, strict: bool) -> Self {
        self.strict_toolsets = strict;
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
    /// The callback receives the error's full message, and
    /// `ctx.tool_outcome()` carries the structured error in
    /// [`ToolOutcome::error`](adk_core::ToolOutcome::error).
    /// If the callback returns `Ok(Some(value))`, the value is used as a
    /// fallback function response to the LLM. If it returns `Ok(None)`,
    /// the next callback in the chain is tried. If no callback provides a
    /// fallback, the LLM receives
    /// [`tool_error_response`](adk_core::tool_error_response), which omits the message unless
    /// [`expose_tool_error_messages`](Self::expose_tool_error_messages) is set.
    pub fn on_tool_error(mut self, callback: OnToolErrorCallback) -> Self {
        self.on_tool_error_callbacks.push(callback);
        self
    }

    /// Set a default retry budget applied to all tools that do not have
    /// a per-tool override.
    ///
    /// When a tool execution fails and a retry budget applies, the agent
    /// retries up to `budget.max_retries` times, with the backoff described on
    /// [`RetryBudget::backoff_delay`]. Only a tool whose [`Tool::effect`] is
    /// [`ToolEffect::ReadOnly`] or [`ToolEffect::Idempotent`] is retried, and
    /// only after a retryable error or a timeout. Each attempt gets a fresh tool
    /// context, so a retried attempt's state delta and escalation are discarded.
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
    ///
    /// The effective requirement is the union of this policy and
    /// [`InvocationContext::requires_tool_confirmation`], so a runtime-injected requirement
    /// (for example a team relationship with required approval) still pauses the call when
    /// no decision exists and blocks it when the decision is
    /// [`ToolConfirmationDecision::Deny`](adk_core::ToolConfirmationDecision::Deny).
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

    /// Allow consecutive agent-delegation calls to execute concurrently.
    ///
    /// Ordinary tools remain sequential and retain their position relative to
    /// each delegation batch. This setting takes precedence over the general
    /// tool execution strategy for the agent.
    pub fn parallelize_agent_delegations(mut self, parallelize: bool) -> Self {
        self.parallelize_agent_delegations = parallelize;
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
    /// They screen every model turn before the client or the session sees it: the final
    /// response through `Guardrail::validate`, and the text of a turn that also calls tools, or
    /// that the provider paused, through `Guardrail::validate_preamble`, so a transform
    /// rewrites that text and keeps the calls. A block ends the run with an `InvalidInput`
    /// error coded `guardrail.output_blocked`, and the blocked turn's calls do not run. A
    /// tool result that ends the turn is screened too. With output guardrails set, an `SSE`
    /// or `Bidi` run delivers each model turn as one event.
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
    /// Screening runs on the call's final arguments, after plugin and callback rewrites and the
    /// run's [`ToolPolicy`](adk_core::ToolPolicy), and before confirmation is resolved, so a
    /// denied call neither prompts the user nor consumes a concurrency permit. A denial is
    /// reported to the model as the tool's result, letting it correct the call rather than
    /// stalling the run.
    ///
    /// The set also screens each call's result (`ToolGuardrail::validate_result`) after the
    /// after-tool callbacks and plugins, before the model, the client, or the session sees it:
    /// a revision replaces the result, and a withheld result is answered with
    /// [`result_blocked_response`](adk_core::result_blocked_response).
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

    /// Build the [`LlmAgent`].
    ///
    /// # Errors
    ///
    /// Returns an error when no model was set, when two sub-agents share a
    /// name, or when two tools added with [`tool`](Self::tool) share a name
    /// (code `agent.duplicate_tool_name`).
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
        adk_core::ensure_unique_tool_names(&self.name, &self.tools)?;

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
            strict_toolsets: self.strict_toolsets,
            sub_agents: self.sub_agents,
            output_key: self.output_key,
            generate_content_config: self.generate_content_config,
            max_iterations: self.max_iterations,
            tool_timeout: self.tool_timeout,
            expose_tool_error_messages: self.expose_tool_error_messages,
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
            parallelize_agent_delegations: self.parallelize_agent_delegations,
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

/// Progress stream that carries the marker for dropped forwarded events.
const FORWARDED_EVENTS_STREAM: &str = "events";

/// Text emitted once, ahead of the next forwarded event, after a tool's forwarded partial
/// events were dropped.
const FORWARDED_EVENTS_DROPPED_MARKER: &str = "[adk: forwarded events dropped]";

/// Markers that found the progress queue full, delivered by the dispatch loop after the
/// queued events.
type ProgressOverflow = Arc<std::sync::Mutex<Vec<Event>>>;

/// Queues `marker`, keeping it in `overflow` when the queue is full: a marker reports a
/// backlog, so the full queue is exactly when it must not be dropped.
fn send_marker(tx: &tokio::sync::mpsc::Sender<Event>, overflow: &ProgressOverflow, marker: Event) {
    if let Err(tokio::sync::mpsc::error::TrySendError::Full(marker)) = tx.try_send(marker) {
        overflow.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(marker);
    }
}

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
    /// The key of the held call this context resumes, shared by every run that resumes
    /// it; `None` for a call made in this invocation (spec 3.1 §7.12).
    idempotency_key: Option<String>,
    actions: Mutex<EventActions>,
    progress_tx: Option<tokio::sync::mpsc::Sender<Event>>,
    /// Where a marker goes when the progress queue is full.
    progress_overflow: ProgressOverflow,
    /// Progress bytes forwarded so far for this call.
    progress_bytes: std::sync::atomic::AtomicUsize,
    /// Set once the truncation marker has been emitted, so it is emitted once.
    progress_truncated: std::sync::atomic::AtomicBool,
    /// Forwarded partial events dropped because the consumer did not keep up.
    events_dropped: std::sync::atomic::AtomicUsize,
    /// Set once the marker for dropped forwarded events has been sent.
    events_drop_marked: std::sync::atomic::AtomicBool,
}

impl Drop for AgentToolContext {
    fn drop(&mut self) {
        let dropped = *self.events_dropped.get_mut();
        if dropped == 0 {
            return;
        }
        tracing::warn!(
            function_call.id = %self.function_call_id,
            events.dropped = dropped,
            "forwarded partial events were dropped for this tool call"
        );
        if !*self.events_drop_marked.get_mut()
            && let Some(tx) = &self.progress_tx
        {
            send_marker(tx, &self.progress_overflow, self.events_dropped_marker());
        }
    }
}

impl AgentToolContext {
    fn new(parent_ctx: Arc<dyn InvocationContext>, function_call_id: String) -> Self {
        Self {
            parent_ctx,
            function_call_id,
            tool_name: None,
            idempotency_key: None,
            actions: Mutex::new(EventActions::default()),
            progress_tx: None,
            progress_overflow: ProgressOverflow::default(),
            progress_bytes: std::sync::atomic::AtomicUsize::new(0),
            progress_truncated: std::sync::atomic::AtomicBool::new(false),
            events_dropped: std::sync::atomic::AtomicUsize::new(0),
            events_drop_marked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Record which tool this context serves.
    fn with_tool_name(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
        self
    }

    /// Keys the call by `key` instead of this invocation and its call ID.
    fn with_idempotency_key(mut self, key: Option<String>) -> Self {
        self.idempotency_key = key;
        self
    }

    /// Attach a progress sink so [`ToolContext::emit_progress`] forwards chunks
    /// as partial [`Event`]s onto the agent's `EventStream`.
    fn with_progress(
        mut self,
        tx: tokio::sync::mpsc::Sender<Event>,
        overflow: ProgressOverflow,
    ) -> Self {
        self.progress_tx = Some(tx);
        self.progress_overflow = overflow;
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

    /// The marker that stands in for forwarded partial events this call dropped.
    fn events_dropped_marker(&self) -> Event {
        Event::tool_progress(
            self.parent_ctx.invocation_id(),
            self.parent_ctx.agent_name(),
            &self.function_call_id,
            FORWARDED_EVENTS_STREAM,
            FORWARDED_EVENTS_DROPPED_MARKER,
        )
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
        send_marker(tx, &self.progress_overflow, marker);
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

    fn acting_context(&self) -> adk_core::ActingContext {
        self.parent_ctx.acting_context()
    }

    fn is_authenticated(&self) -> bool {
        self.parent_ctx.is_authenticated()
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

    /// A resumed held call keeps the key it was first dispatched under, so a payment or
    /// another downstream idempotency key is the same in every run that resumes it
    /// (spec 3.1 §7.12). Any other call uses the trait's default form.
    fn idempotency_key(&self) -> String {
        self.idempotency_key.clone().unwrap_or_else(|| {
            format!(
                "{}/{}/{}/{}/{}",
                self.app_name(),
                self.user_id(),
                self.session_id(),
                self.invocation_id(),
                self.function_call_id
            )
        })
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
        use std::sync::atomic::Ordering;

        let Some(tx) = &self.progress_tx else {
            return;
        };
        if tx.is_closed() {
            return;
        }
        // A complete event becomes session history, so it waits for the consumer as a tool
        // result does. Only a partial event, which is never persisted, may be dropped.
        if !event.llm_response.partial {
            let _ = tx.send(event).await;
            return;
        }
        // A consumer that missed partial events is told so before the next one reaches it.
        if self.events_dropped.load(Ordering::Relaxed) > 0
            && !self.events_drop_marked.load(Ordering::Relaxed)
            && tokio::time::timeout(
                TOOL_PROGRESS_SEND_TIMEOUT,
                tx.send(self.events_dropped_marker()),
            )
            .await
            .is_ok_and(|sent| sent.is_ok())
        {
            self.events_drop_marked.store(true, Ordering::Relaxed);
        }
        if tokio::time::timeout(TOOL_PROGRESS_SEND_TIMEOUT, tx.send(event)).await.is_ok() {
            return;
        }
        if self.events_dropped.fetch_add(1, Ordering::Relaxed) == 0 {
            tracing::warn!(
                function_call.id = %self.function_call_id,
                "consumer is not keeping up; forwarded partial events are being dropped"
            );
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

    fn acting_context(&self) -> adk_core::ActingContext {
        self.inner.acting_context()
    }

    fn is_authenticated(&self) -> bool {
        self.inner.is_authenticated()
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

    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.inner.shared_state()
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
    /// Set when the call is held for a confirmation decision that does not exist yet.
    pending: Option<ToolConfirmationRequest>,
}

/// Runs output guardrails on the function responses of a result that ends the turn.
///
/// Guardrails read text parts, so each response is screened as its JSON text. A transformed
/// text replaces the response, parsed back to JSON when it still parses and wrapped as
/// `{"result": text}` when it does not.
async fn screen_terminal_tool_output(
    guardrails: &GuardrailSet,
    mut content: Content,
) -> Result<Screened> {
    let mut warnings = Warnings::default();
    for part in &mut content.parts {
        if let Part::FunctionResponse { function_response, .. } = part {
            let text = function_response.response.to_string();
            let screened = enforce_guardrails(
                guardrails,
                &Content::new("model").with_text(&text),
                Phase::Output,
            )
            .await?;
            let screened_text: String =
                screened.content.parts.iter().filter_map(Part::text).collect();
            if screened_text != text {
                function_response.response = serde_json::from_str(&screened_text)
                    .unwrap_or_else(|_| serde_json::json!({ "result": screened_text }));
            }
            warnings.extend(screened.warnings);
        }
    }
    Ok(Screened { content, warnings })
}

/// Turns a tool callback's substitute content into the response to the call it replaces.
///
/// Providers reject a request whose function call has no function response with the same id,
/// so a function response part is re-addressed to this call and text-only content is wrapped
/// as one by `wrap_text`. Only the function response is kept, as OpenAI Chat reads the first
/// part of a tool message and nothing else.
fn callback_tool_response(
    content: Content,
    name: &str,
    id: Option<&String>,
    wrap_text: impl FnOnce(String) -> serde_json::Value,
) -> Content {
    let mut text = String::new();
    let mut response = None;
    for part in content.parts {
        match part {
            Part::FunctionResponse { mut function_response, annotations, .. }
                if response.is_none() =>
            {
                function_response.name = name.to_string();
                response = Some(Part::FunctionResponse {
                    function_response,
                    id: id.cloned(),
                    annotations,
                });
            }
            other => {
                if let Some(chunk) = other.text() {
                    text.push_str(chunk);
                }
            }
        }
    }
    let response = response.unwrap_or_else(|| Part::FunctionResponse {
        function_response: FunctionResponseData::new(name, wrap_text(text)),
        id: id.cloned(),
        annotations: None,
    });
    Content { role: "function".to_string(), parts: vec![response] }
}

#[derive(Clone, Copy)]
enum ToolDispatchMode {
    Sequential,
    Parallel,
    ParallelDelegations,
    Auto,
}

impl ToolDispatchMode {
    fn resolve(strategy: ToolExecutionStrategy, parallelize_agent_delegations: bool) -> Self {
        if parallelize_agent_delegations {
            return Self::ParallelDelegations;
        }
        match strategy {
            ToolExecutionStrategy::Sequential => Self::Sequential,
            ToolExecutionStrategy::Parallel => Self::Parallel,
            ToolExecutionStrategy::Auto => Self::Auto,
        }
    }
}

struct ToolExecutor<'a> {
    ctx: Arc<dyn InvocationContext>,
    /// The agent the results are authored by. Under a workflow parent the context names the
    /// parent, but a call's response belongs to the agent that issued it, as its hold does.
    agent_name: &'a str,
    tool_map: &'a std::collections::HashMap<String, Arc<dyn Tool>>,
    tool_retry_budgets: &'a std::collections::HashMap<String, RetryBudget>,
    default_retry_budget: &'a Option<RetryBudget>,
    before_tool_callbacks: &'a Arc<Vec<BeforeToolCallback>>,
    after_tool_callbacks: &'a Arc<Vec<AfterToolCallback>>,
    after_tool_callbacks_full: &'a Arc<Vec<AfterToolCallbackFull>>,
    on_tool_error_callbacks: &'a Arc<Vec<OnToolErrorCallback>>,
    /// The run's invocation hooks, iterated ahead of the agent's own tool callbacks.
    hook_callbacks: &'a HookCallbacks,
    tool_confirmation_policy: &'a ToolConfirmationPolicy,
    tool_guardrails: &'a ToolGuardrailSet,
    /// Screens a result that ends the turn, since that result is the final response.
    output_guardrails: &'a GuardrailSet,
    cb_mutex: &'a std::sync::Mutex<Option<CircuitBreakerState>>,
    invocation_id: &'a str,
    concurrency_manager: &'a adk_core::ToolConcurrencyManager,
    progress_tx: tokio::sync::mpsc::Sender<Event>,
    progress_overflow: ProgressOverflow,
    tool_timeout: std::time::Duration,
    expose_tool_error_messages: bool,
    /// Set when an approval fails, so calls still waiting for theirs stop waiting.
    approval_failed: tokio::sync::watch::Sender<bool>,
    #[cfg(feature = "enhanced-plugins")]
    enhanced_plugin_manager: &'a Option<Arc<EnhancedPluginManager>>,
}

impl ToolExecutor<'_> {
    /// Runs one batch of calls under `strategy`, returning results in completion order.
    async fn dispatch(
        &self,
        strategy: ToolDispatchMode,
        fc_parts: Vec<PendingToolCall>,
    ) -> Result<Vec<ToolExecutionResult>> {
        let results: Vec<ToolExecutionResult> = match strategy {
            ToolDispatchMode::Sequential => {
                let mut results = Vec::with_capacity(fc_parts.len());
                for call in fc_parts {
                    results.push(self.execute(call).await?);
                }
                results
            }
            ToolDispatchMode::Parallel => {
                use futures::StreamExt as _;
                // Parallel is an explicit caller override. Tool
                // safety metadata is intentionally not inspected.
                // All concurrency enforcement is handled by the
                // ToolConcurrencyManager semaphore inside ToolExecutor.
                // Use fc_parts.len() as buffer so all futures can start
                // and queue on the semaphore for proper per-tool limiting.
                let buffer_size = fc_parts.len().max(1);
                futures::stream::iter(fc_parts.into_iter().map(|call| self.execute(call)))
                    .buffer_unordered(buffer_size)
                    // Running tools finish before an approval error ends the turn.
                    .collect::<Vec<_>>()
                    .await
                    .into_iter()
                    .collect::<Result<Vec<_>>>()?
            }
            ToolDispatchMode::ParallelDelegations => {
                use futures::StreamExt as _;

                let mut all_results = Vec::with_capacity(fc_parts.len());
                let mut calls = fc_parts.into_iter().peekable();
                while let Some(call) = calls.next() {
                    let is_delegation = self
                        .tool_map
                        .get(&call.name)
                        .is_some_and(|tool| tool.is_agent_delegation());
                    if !is_delegation {
                        all_results.push(self.execute(call).await?);
                        continue;
                    }

                    let mut delegation_batch = vec![call];
                    while calls.peek().is_some_and(|next| {
                        self.tool_map.get(&next.name).is_some_and(|tool| tool.is_agent_delegation())
                    }) {
                        if let Some(next) = calls.next() {
                            delegation_batch.push(next);
                        }
                    }

                    let buffer_size = delegation_batch.len();
                    all_results.extend(
                        futures::stream::iter(
                            delegation_batch.into_iter().map(|call| self.execute(call)),
                        )
                        .buffer_unordered(buffer_size)
                        .collect::<Vec<_>>()
                        .await
                        .into_iter()
                        .collect::<Result<Vec<_>>>()?,
                    );
                }
                all_results
            }
            ToolDispatchMode::Auto => {
                // A call may overlap another only when its tool is
                // read-only *and* declares concurrency safety.
                let (concurrent_fcs, sequential_fcs): (Vec<_>, Vec<_>) =
                    fc_parts.into_iter().partition(|call| {
                        self.tool_map
                            .get(&call.name)
                            .is_some_and(|tool| tool.is_read_only() && tool.is_concurrency_safe())
                    });
                let mut all_results = Vec::new();

                // Concurrency enforcement is handled by the semaphore
                // inside ToolExecutor.
                if !concurrent_fcs.is_empty() {
                    use futures::StreamExt as _;
                    let buffer_size = concurrent_fcs.len().max(1);
                    all_results.extend(
                        futures::stream::iter(
                            concurrent_fcs.into_iter().map(|call| self.execute(call)),
                        )
                        .buffer_unordered(buffer_size)
                        .collect::<Vec<_>>()
                        .await
                        .into_iter()
                        .collect::<Result<Vec<_>>>()?,
                    );
                }

                // Everything else runs one at a time.
                for call in sequential_fcs {
                    all_results.push(self.execute(call).await?);
                }
                all_results
            }
        };
        Ok(results)
    }

    async fn execute(&self, call: PendingToolCall) -> Result<ToolExecutionResult> {
        let screened_call =
            (!self.tool_guardrails.is_empty()).then(|| (call.name.clone(), call.args.clone()));
        let mut result = self.execute_inner(call).await?;
        // Tool guardrails see the result the model is about to be sent, after every after-tool
        // stage. A held call's pending response is not a result.
        if let Some((name, args)) = screened_call
            && result.pending.is_none()
        {
            let (content, withheld) =
                screen_tool_result(self.tool_guardrails, &name, &args, result.content).await;
            result.content = content;
            for reason in withheld {
                // A guardrail's reason can describe the result it withheld.
                tracing::debug!(
                    tool.name = %name,
                    reason = %self.logged_message(&reason),
                    "tool result withheld"
                );
            }
        }
        // A result that ends the turn is the final response, so it is screened like one,
        // before the client or the session sees it.
        let mut warnings = Warnings::default();
        if result.escalate_or_skip && result.pending.is_none() && !self.output_guardrails.is_empty()
        {
            let screened =
                screen_terminal_tool_output(self.output_guardrails, result.content).await?;
            result.content = screened.content;
            warnings = screened.warnings;
        }
        let mut event = Event::new(self.invocation_id);
        event.author = self.agent_name.to_owned();
        event.branch = self.ctx.branch().to_owned();
        warnings.record_on(&mut event);
        event.actions = result.actions.clone();
        event.llm_response.content = Some(result.content.clone());
        // Runner commits each result while other calls are still awaiting approval.
        let _ = self.progress_tx.send(event).await;
        Ok(result)
    }

    /// The text logged for an error message, see [`traced_message`].
    fn logged_message(&self, message: &str) -> String {
        traced_message(self.ctx.run_config(), message)
    }

    /// Records, through the run's audit trail, a call that `answered_by` answered before
    /// the governed path saw it, so the trail shows every call that skipped authorization.
    ///
    /// # Errors
    ///
    /// Returns `audit.record_failed`, which ends the turn, when the record cannot be
    /// written under [`AuditFailureMode::Block`](adk_core::audit_log::AuditFailureMode::Block).
    async fn record_short_circuit(
        &self,
        name: &str,
        function_call_id: &str,
        args: &serde_json::Value,
        answered_by: &str,
    ) -> Result<()> {
        tracing::debug!(
            tool.name = %name,
            tool.answered_by = %answered_by,
            "tool call answered before authorization; the tool was not run"
        );
        let Some(trail) = &self.ctx.run_config().audit else {
            return Ok(());
        };
        let mut draft = AuditRecordDraft::new(AuditRecordKind::ToolCall, SHORT_CIRCUIT_ACTION)
            .actor(AuditActor::for_context(self.ctx.as_ref()))
            .args_digest(trail.digest(args))
            .outcome(AuditRecordOutcome::NotRun)
            .ext("toolName", serde_json::Value::String(name.to_string()))
            .ext("agentName", serde_json::Value::String(self.agent_name.to_string()))
            .ext("answeredBy", serde_json::Value::String(answered_by.to_string()))
            .ext("functionCallId", serde_json::Value::String(function_call_id.to_string()))
            .ext("sessionId", serde_json::Value::String(self.ctx.session_id().to_string()));
        if let Some(tool) = self.tool_map.get(name) {
            let origin = adk_core::ToolOrigin::of(tool.as_ref());
            draft = draft
                .ext("toolSource", serde_json::Value::String(origin.source))
                .ext("declarationDigest", serde_json::Value::String(origin.declaration_digest));
        }
        if let Err(error) = trail.record(draft).await {
            self.approval_failed.send_replace(true);
            return Err(error);
        }
        Ok(())
    }

    async fn execute_inner(&self, call: PendingToolCall) -> Result<ToolExecutionResult> {
        let PendingToolCall { index, name, args, id, function_call_id, resumes } = call;
        // A resumed held call runs under a later invocation, so it is ledgered under its
        // original identity, which every resumption of it shares (spec 3.1 §7.12, I37).
        let resumed_ledger_key = resumes.as_ref().map(|held| {
            crate::tool_ledger::held_call_ledger_key(
                self.ctx.app_name(),
                self.ctx.user_id(),
                self.ctx.session_id(),
                &function_call_id,
                &held.fingerprint(),
            )
        });
        let mut tool_actions = EventActions::default();
        let mut response_content: Option<Content> = None;
        let mut run_after_tool_callbacks = true;
        let mut tool_outcome_for_callback: Option<ToolOutcome> = None;
        let mut executed_tool: Option<Arc<dyn Tool>> = None;
        let mut executed_tool_response: Option<serde_json::Value> = None;
        // Set once `execute` was called, so its side effect may have happened.
        let mut tool_executed = false;
        let mut pending_confirmation: Option<ToolConfirmationRequest> = None;
        let respond = |response: serde_json::Value| Content {
            role: "function".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new(name.clone(), response),
                id: id.clone(),
                annotations: None,
            }],
        };
        let error_response = |message: String| respond(serde_json::json!({ "error": message }));
        // A callback's error message stays local unless the agent exposes error messages.
        let callback_failure = |error: &adk_core::AdkError| {
            if self.expose_tool_error_messages { error.to_string() } else { error.redacted() }
        };

        // The provider could not decode the model's arguments into an object, so there is
        // nothing to authorize or execute; the model is asked to send the call again.
        if let Some(reason) = adk_core::invalid_tool_arguments_reason(&args) {
            tracing::warn!(
                tool.name = %name,
                reason = %reason,
                "tool call arguments are not a JSON object; answering without executing"
            );
            return Ok(ToolExecutionResult {
                index,
                content: error_response(format!(
                    "tool '{name}' was not run: {reason}. Call it again with arguments that are \
                     one JSON object matching its parameters."
                )),
                actions: tool_actions,
                escalate_or_skip: false,
                pending: None,
            });
        }

        // Before-tool hooks run first and may rewrite the arguments. Governance below sees
        // only the final arguments, so nothing can change a call after it is authorized.
        let mut final_args = args;

        // ===== ENHANCED PLUGIN: BEFORE TOOL CALL =====
        #[cfg(feature = "enhanced-plugins")]
        if let Some(epm) = self.enhanced_plugin_manager.as_ref()
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
                    self.record_short_circuit(
                        &name,
                        &function_call_id,
                        &final_args,
                        SHORT_CIRCUIT_BY_ENHANCED_PLUGIN,
                    )
                    .await?;
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
                    tracing::warn!(
                        tool.name = %name,
                        error.code = %e.code,
                        error.category = %e.category,
                        error.message = %self.logged_message(&e.message),
                        "before-tool plugin failed; the tool was not run"
                    );
                    response_content = Some(error_response(format!(
                        "tool '{name}' was not run: a before-tool plugin failed with {}",
                        callback_failure(&e)
                    )));
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
            let hooks = self.hook_callbacks.before_tool.len();
            for (position, callback) in self
                .hook_callbacks
                .before_tool
                .iter()
                .chain(self.before_tool_callbacks.iter())
                .enumerate()
            {
                match callback(tool_ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(c)) => {
                        let answered_by = if position < hooks {
                            SHORT_CIRCUIT_BY_INVOCATION_HOOK
                        } else {
                            SHORT_CIRCUIT_BY_BEFORE_TOOL_CALLBACK
                        };
                        self.record_short_circuit(
                            &name,
                            &function_call_id,
                            &final_args,
                            answered_by,
                        )
                        .await?;
                        // The tool did not run, so text is reported as the call's error.
                        response_content = Some(callback_tool_response(
                            c,
                            &name,
                            id.as_ref(),
                            |text| serde_json::json!({ "error": text }),
                        ));
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(
                            tool.name = %name,
                            error.code = %e.code,
                            error.category = %e.category,
                            error.message = %self.logged_message(&e.message),
                            "before-tool callback failed; the tool was not run"
                        );
                        response_content = Some(error_response(format!(
                            "tool '{name}' was not run: a before-tool callback failed with {}",
                            callback_failure(&e)
                        )));
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
                response_content = Some(error_response(msg));
                run_after_tool_callbacks = false;
            }
            drop(guard);
        }

        // ===== GOVERNED PATH: POLICY, GUARDRAILS, CONFIRMATION =====
        // Runtime-injected requirements such as team relationship approval gate execution
        // exactly like the agent's own confirmation policy.
        if response_content.is_none()
            && let Some(tool) = self.tool_map.get(&name)
        {
            let screen = GuardrailScreen(self.tool_guardrails);
            let mut gate = ToolGate::for_context(self.ctx.as_ref())
                .with_abandon_signal(self.approval_failed.subscribe());
            if !self.tool_guardrails.is_empty() {
                gate = gate.with_screen(&screen);
            }
            let mut call =
                GovernedCall::for_tool(tool.as_ref(), function_call_id.clone(), final_args.clone())
                    .requiring_confirmation(
                        self.tool_confirmation_policy.requires_confirmation(&name)
                            || self.ctx.requires_tool_confirmation(&name),
                    );
            // The governed path resumes a held call only for the agent and tool it was held
            // for. A request naming another tool belongs to a delegate the tool runs, whose
            // own governed path binds it.
            if let Some(held) = resumes.filter(|held| held.tool_name == name) {
                call = call.resuming(held);
            }
            match authorize_tool_call(&gate, call).await {
                Ok(ToolAuthorization::Execute { args, confirmation }) => {
                    final_args = args;
                    tool_actions.tool_confirmation_decision = confirmation;
                }
                Ok(ToolAuthorization::Refuse { reason, confirmation }) => {
                    // A guardrail's reason can quote the arguments it refused.
                    tracing::debug!(
                        tool.name = %name,
                        reason = %self.logged_message(&reason),
                        "tool call refused"
                    );
                    tool_actions.tool_confirmation_decision = confirmation;
                    response_content = Some(error_response(reason));
                    run_after_tool_callbacks = false;
                }
                Ok(ToolAuthorization::Pending { request, reason }) => {
                    response_content =
                        Some(respond(adk_core::confirmation_pending_response(reason)));
                    pending_confirmation = Some(request);
                    run_after_tool_callbacks = false;
                }
                // The kill switch refused the call mid-turn: it is answered `not run`, the
                // batch's remaining calls meet the same refusal, and the turn ends at the
                // next model call, where a durable worker parks the run (spec 3.2 §10 S3).
                Err(error)
                    if error.code == adk_core::GOVERNANCE_FROZEN_CODE
                        || error.code == adk_core::GOVERNANCE_STALE_CODE =>
                {
                    tracing::info!(
                        tool.name = %name,
                        error = %error.diagnostics(),
                        "tool call not run: the governance control refuses work"
                    );
                    response_content = Some(error_response(format!("not run: {}", error.message)));
                    run_after_tool_callbacks = false;
                }
                Err(error) => {
                    self.approval_failed.send_replace(true);
                    return Err(error);
                }
            }
        }

        // Acquire concurrency permit after confirmation, so an approval waiting on a person
        // does not hold the tool's slot, and a call that will not run takes none.
        // The permit is held for the entire duration of this tool call
        // and released on drop when this async block completes.
        let _concurrency_permit = if response_content.is_some() {
            None
        } else {
            match self.concurrency_manager.acquire(&name).await {
                Ok(permit) => Some(permit),
                Err(e) => {
                    // Concurrency limit reached with Fail policy — return error
                    return Ok(ToolExecutionResult {
                        index,
                        content: error_response(e.to_string()),
                        actions: tool_actions,
                        escalate_or_skip: false,
                        pending: None,
                    });
                }
            }
        };

        // Execute tool with retry budget and tracing
        if response_content.is_none() {
            if let Some(tool) = self.tool_map.get(&name) {
                // Each attempt gets its own context, so the state delta and escalation of an
                // attempt that is retried are never committed.
                let attempt_context = || -> Arc<dyn ToolContext> {
                    Arc::new(
                        AgentToolContext::new(self.ctx.clone(), function_call_id.clone())
                            .with_tool_name(tool.name())
                            .with_idempotency_key(resumed_ledger_key.clone())
                            .with_progress(
                                self.progress_tx.clone(),
                                self.progress_overflow.clone(),
                            ),
                    )
                };
                let mut tool_ctx = attempt_context();
                let effect = tool.effect();

                // A non-idempotent call executes at most once per idempotency key. A record
                // that exists answers the call; a ledger that cannot be read or written
                // fails the call closed.
                let ledgered = match crate::tool_ledger::begin_ledgered_call(
                    self.ctx.as_ref(),
                    effect,
                    // A resumed call's context answers its held call's key.
                    tool_ctx.idempotency_key(),
                    &name,
                    &final_args,
                )
                .await?
                {
                    Ok(ledgered) => ledgered,
                    Err(answer) => {
                        return Ok(ToolExecutionResult {
                            index,
                            content: Content {
                                role: "function".to_string(),
                                parts: vec![Part::FunctionResponse {
                                    function_response: FunctionResponseData::from_tool_result(
                                        name.clone(),
                                        answer,
                                    ),
                                    id: id.clone(),
                                    annotations: None,
                                }],
                            },
                            actions: tool_actions,
                            escalate_or_skip: false,
                            pending: None,
                        });
                    }
                };

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

                // Only a call that cannot repeat a side effect is retried.
                let budget = self
                    .tool_retry_budgets
                    .get(&name)
                    .or(self.default_retry_budget.as_ref())
                    .filter(|_| effect.is_retry_safe());
                let max_attempts = budget.map(|b| b.max_retries + 1).unwrap_or(1);
                let timeout = tool.timeout_override().unwrap_or(Some(self.tool_timeout));

                let mut started = Event::new(self.invocation_id);
                started.author = self.agent_name.to_owned();
                started.branch = self.ctx.branch().to_owned();
                // Partial, as tool progress is: never persisted, never a final response.
                started.llm_response.partial = true;
                started.provider_metadata.insert(
                    "adk_tool_started".into(),
                    serde_json::json!({"id": id, "name": name}).to_string(),
                );
                let _ = self.progress_tx.send(started).await;
                tool_executed = true;
                let tool_clone = tool.clone();
                let tool_start = std::time::Instant::now();
                let mut last_error: Option<Arc<adk_core::AdkError>> = None;
                // Set when the last attempt ended mid-flight (timeout or panic), so its side
                // effect may or may not have happened. Such an error is the agent's own, and
                // its message is always shown to the model.
                let mut interrupted = false;
                let mut final_attempt: u32 = 0;
                let mut retry_result: Option<serde_json::Value> = None;

                for attempt in 0..max_attempts {
                    final_attempt = attempt;
                    if attempt > 0 {
                        if let Some(budget) = budget {
                            tokio::time::sleep(budget.backoff_delay(attempt)).await;
                        }
                        tool_ctx = attempt_context();
                    }
                    let attempt_result = async {
                        let args_payload = trace_json_payload(
                            &final_args,
                            self.ctx.run_config().record_payloads,
                            self.ctx.run_config().trace_payload_max_bytes,
                        );
                        tracing::debug!(tool.name = %name, tool.args = %args_payload, attempt = attempt, "tool_call");
                        let exec_future = execute_tool(
                            Arc::clone(&tool_clone),
                            tool_ctx.clone(),
                            final_args.clone(),
                        );
                        let bounded = async {
                            match timeout {
                                Some(limit) => tokio::time::timeout(limit, exec_future)
                                    .await
                                    .map_err(|_| limit),
                                None => Ok(exec_future.await),
                            }
                        };
                        match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(bounded)).await {
                            Ok(Ok(result)) => result.map_err(|error| {
                                let retryable = error.is_retryable();
                                (error, retryable, false)
                            }),
                            Ok(Err(limit)) => Err((
                                adk_core::AdkError::new(
                                    adk_core::ErrorComponent::Tool,
                                    adk_core::ErrorCategory::Timeout,
                                    "tool.timeout",
                                    format!("Tool '{name}' timed out after {limit:?}"),
                                ),
                                true,
                                true,
                            )),
                            Err(_panic) => Err((
                                adk_core::AdkError::new(
                                    adk_core::ErrorComponent::Tool,
                                    adk_core::ErrorCategory::Internal,
                                    "tool.panicked",
                                    format!("tool '{name}' panicked during execution"),
                                ),
                                false,
                                true,
                            )),
                        }
                    }
                    .instrument(tool_span.clone())
                    .await;
                    match attempt_result {
                        Ok(value) => {
                            let result_payload = trace_json_payload(
                                &value,
                                self.ctx.run_config().record_payloads,
                                self.ctx.run_config().trace_payload_max_bytes,
                            );
                            tracing::debug!(tool.name = %name, tool.result = %result_payload, "tool_result");
                            retry_result = Some(value);
                            break;
                        }
                        Err((error, retryable, mid_flight)) => {
                            interrupted = mid_flight;
                            let message = if mid_flight {
                                error.message.clone()
                            } else {
                                self.logged_message(&error.message)
                            };
                            if retryable && attempt + 1 < max_attempts {
                                tracing::warn!(tool.name = %name, attempt = attempt, error.code = %error.code, error.category = %error.category, error.message = %message, "tool execution failed, retrying");
                            } else {
                                tracing::warn!(tool.name = %name, error.code = %error.code, error.category = %error.category, error.message = %message, "tool_error");
                            }
                            let retry = retryable && attempt + 1 < max_attempts;
                            last_error = Some(Arc::new(error));
                            if !retry {
                                break;
                            }
                        }
                    }
                }

                let outcome_unknown = interrupted && effect == ToolEffect::NonIdempotent;
                // What the model is told about the failure: the agent's own timeout and panic
                // messages, or the tool error without its message by default.
                let failure_response = match (&retry_result, &last_error) {
                    (None, Some(error)) if outcome_unknown => {
                        Some(crate::tool_ledger::interrupted_call_response(&error.message))
                    }
                    (None, Some(error)) if interrupted => {
                        Some(serde_json::json!({ "error": error.message }))
                    }
                    (None, Some(error)) => Some(adk_core::tool_error_response(
                        &name,
                        error,
                        self.expose_tool_error_messages,
                    )),
                    (None, None) | (Some(_), _) => None,
                };
                if outcome_unknown {
                    crate::tool_ledger::record_outcome_unknown(
                        self.ctx.as_ref(),
                        &name,
                        &final_args,
                        "interrupted",
                    )
                    .await?;
                }
                // A call that ended mid-flight keeps its begun record, so a replay is answered
                // with an unknown outcome instead of executing again. The ledger records what
                // the model was told, since a replay answers with it.
                if let Some(ledgered) = ledgered
                    && !outcome_unknown
                {
                    let failure_text = failure_response
                        .as_ref()
                        .and_then(|response| response.get("error"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    ledgered.complete(&name, retry_result.as_ref(), failure_text).await;
                }

                let tool_duration = tool_start.elapsed();
                // Callbacks run in this process, so they see the full message.
                let error_message =
                    last_error.as_ref().filter(|_| retry_result.is_none()).map(|error| {
                        if interrupted { error.message.clone() } else { error.to_string() }
                    });
                let tool_success = retry_result.is_some();
                let function_response = match retry_result {
                    Some(value) => value,
                    None => failure_response.unwrap_or_else(
                        || serde_json::json!({ "error": format!("tool '{name}' failed") }),
                    ),
                };

                let outcome = ToolOutcome {
                    tool_name: name.clone(),
                    tool_args: final_args.clone(),
                    success: tool_success,
                    duration: tool_duration,
                    error_message: error_message.clone(),
                    error: if tool_success { None } else { last_error.clone() },
                    attempt: final_attempt,
                };

                // Circuit breaker recording
                {
                    let mut guard = self.cb_mutex.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(ref mut cb_state) = *guard {
                        cb_state.record(&outcome);
                    }
                }

                // On-tool-error callbacks
                let final_function_response = if !tool_success {
                    let error_ctx: Arc<dyn CallbackContext> =
                        Arc::new(ToolOutcomeCallbackContext {
                            inner: self.ctx.clone() as Arc<dyn CallbackContext>,
                            outcome: outcome.clone(),
                        });
                    let mut fallback_result = None;
                    let error_msg = error_message.unwrap_or_default();
                    for callback in self
                        .hook_callbacks
                        .on_tool_error
                        .iter()
                        .chain(self.on_tool_error_callbacks.iter())
                    {
                        match callback(
                            error_ctx.clone(),
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
                                tracing::warn!(error.code = %e.code, error.message = %self.logged_message(&e.message), "on_tool_error callback failed");
                                break;
                            }
                        }
                    }
                    fallback_result.unwrap_or(function_response)
                } else {
                    function_response
                };
                tool_outcome_for_callback = Some(outcome);

                let confirmation_decision = tool_actions.tool_confirmation_decision;
                tool_actions = tool_ctx.actions();
                if tool_actions.tool_confirmation_decision.is_none() {
                    tool_actions.tool_confirmation_decision = confirmation_decision;
                }
                // An agent tool whose delegate held a call of its own holds this call with it:
                // the run ends on the delegate's request, and this call resumes once that
                // request has a decision.
                let final_function_response = match tool_actions.tool_confirmation.take() {
                    Some(request) => {
                        tool_actions.skip_summarization = false;
                        tool_actions.escalate = false;
                        pending_confirmation = Some(request);
                        run_after_tool_callbacks = false;
                        if adk_core::is_confirmation_pending(&final_function_response) {
                            final_function_response
                        } else {
                            adk_core::confirmation_pending_response(format!(
                                "Tool '{name}' is waiting for a confirmation"
                            ))
                        }
                    }
                    None => final_function_response,
                };
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
                response_content = Some(error_response(format!("Tool {name} not found")));
            }
        }

        // ===== AFTER-TOOL PIPELINE =====
        // Runner hooks and `after_tool_callback`s, then `after_tool_callback_full`s, then
        // enhanced plugins. The first failure ends the pipeline: a later stage may be handed the
        // unscreened result, so none runs once a redaction or screening stage has failed.
        let mut response_content = response_content.expect("tool response content is set");
        if run_after_tool_callbacks {
            let mut failure: Option<adk_core::AdkError> = None;
            let outcome_ctx: Arc<dyn CallbackContext> = match tool_outcome_for_callback {
                Some(outcome) => Arc::new(ToolOutcomeCallbackContext {
                    inner: self.ctx.clone() as Arc<dyn CallbackContext>,
                    outcome,
                }),
                None => self.ctx.clone() as Arc<dyn CallbackContext>,
            };
            let cb_ctx: Arc<dyn CallbackContext> =
                Arc::new(ToolCallbackContext::new(outcome_ctx, name.clone(), final_args.clone()));
            for callback in
                self.hook_callbacks.after_tool.iter().chain(self.after_tool_callbacks.iter())
            {
                match callback(cb_ctx.clone()).await {
                    Ok(Some(modified)) => {
                        response_content = callback_tool_response(
                            modified,
                            &name,
                            id.as_ref(),
                            |text| serde_json::json!({ "result": text }),
                        );
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            if failure.is_none()
                && let (Some(tool_ref), Some(tool_resp)) = (&executed_tool, executed_tool_response)
            {
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
                            failure = Some(e);
                            break;
                        }
                    }
                }
            }

            // ===== ENHANCED PLUGIN: AFTER TOOL CALL =====
            // Enhanced plugins can modify the tool result after legacy callbacks.
            #[cfg(feature = "enhanced-plugins")]
            if failure.is_none()
                && let Some(epm) = self.enhanced_plugin_manager.as_ref()
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

                // The outcome context carries the full error message, which the function
                // response no longer does, so plugins can still classify the failure.
                match epm
                    .run_after_tool_call(
                        tool_ref.clone(),
                        &final_args,
                        result_value,
                        cb_ctx.clone(),
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
                    Err(e) => failure = Some(e),
                }
            }

            if let Some(error) = failure {
                tracing::error!(
                    tool.name = %name,
                    tool.executed = tool_executed,
                    error.code = %error.code,
                    error.category = %error.category,
                    error.message = %self.logged_message(&error.message),
                    "after-tool pipeline failed; withholding the tool result"
                );
                // The tool's side effect stands and its ledger record says so; the model is
                // told not to repeat it, since a new call has a new idempotency key.
                response_content = if tool_executed {
                    respond(adk_core::result_withheld_response(
                        &name,
                        &error,
                        self.expose_tool_error_messages,
                    ))
                } else {
                    error_response(format!(
                        "tool '{name}' was not run, and its after-tool pipeline failed with {}",
                        callback_failure(&error)
                    ))
                };
            }
        }

        let escalate_or_skip = tool_actions.escalate || tool_actions.skip_summarization;
        Ok(ToolExecutionResult {
            index,
            content: response_content,
            actions: tool_actions,
            escalate_or_skip,
            pending: pending_confirmation,
        })
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

    fn allows_transfer_to_parent(&self) -> bool {
        !self.disallow_transfer_to_parent
    }

    fn allows_transfer_to_peers(&self) -> bool {
        !self.disallow_transfer_to_peers
    }

    async fn close(&self) {
        for toolset in &self.toolsets {
            toolset.close().await;
        }
        #[cfg(feature = "enhanced-plugins")]
        if let Some(manager) = &self.enhanced_plugin_manager {
            manager.close().await;
        }
        for agent in &self.sub_agents {
            agent.close().await;
        }
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

    async fn screen_user_input(&self, content: &Content) -> Result<Option<Content>> {
        let own = if self.input_guardrails.is_empty() {
            None
        } else {
            Some(
                enforce_guardrails(self.input_guardrails.as_ref(), content, Phase::Input)
                    .await?
                    .content,
            )
        };
        let below =
            adk_core::screen_user_input_with(&self.sub_agents, own.as_ref().unwrap_or(content))
                .await?;
        Ok(below.or(own))
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
        // Under a workflow or team parent, the agent still acts as itself.
        let ctx = crate::acting_agent_context::ActingAgentContext::scope(ctx, &self.name);
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
        let expose_tool_error_messages = self.expose_tool_error_messages;
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
        let parallelize_agent_delegations = self.parallelize_agent_delegations;
        #[cfg(feature = "enhanced-plugins")]
        let enhanced_plugin_manager = self.enhanced_plugin_manager.clone();
        // Runner plugins and other run-wide hooks reach this agent through the run config.
        let mut hook_callbacks = HookCallbacks::new(&ctx.run_config().invocation_hooks);
        // The before- and after-agent stages run around `body`, which runs the model and tools.
        let hook_before_agent = std::mem::take(&mut hook_callbacks.before_agent);
        let hook_after_agent = Arc::new(std::mem::take(&mut hook_callbacks.after_agent));
        let stage_ctx = ctx.clone();
        let stage_invocation_id = invocation_id.clone();
        let stage_agent_name = agent_name.clone();
        let stage_output_key = output_key.clone();
        let structured_output = self.output_schema.is_some();

        // Boxed where it is built, so the stage stream below holds a pointer: a body moved
        // into it would put a copy of the whole body state on the stack of every poll, once
        // per nested agent behind an `AgentTool`.
        let body: adk_core::EventStream = Box::pin(stream! {
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
            let mut tool_map = resolved_tools.map;
            let mut tool_declarations = resolved_tools.declarations;
            let mut valid_transfer_targets = resolved_tools.transfer_targets;

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

            // Shared with sub-agents, transfer targets and agent tools through the run config.
            let budget_tracker = ctx.run_config().budget_tracker.clone();
            // Provider-executed built-in tools never reach the governed call path, so the
            // policy and budget decide before each request whether they are attached.
            let mut builtin_governor = adk_core::BuiltinToolGovernor::new();

            // ===== RESUME HELD CALLS =====
            // A call held for confirmation in an earlier run, with a decision now, is
            // dispatched again as the same call before the first model call, so the
            // decision applies whether or not the model would issue the call again. It
            // runs the whole tool path, where the governed path finds the decision.
            let mut resumed_calls = Vec::<PendingToolCall>::new();
            {
                let gate = ToolGate::for_context(ctx.as_ref());
                for held in ctx.session().held_tool_calls(&agent_name, ctx.branch()) {
                    let Some(function_call_id) = held.request.function_call_id.clone() else {
                        continue;
                    };
                    match adk_core::confirmation_decision_available(&gate, &held.request).await {
                        // An agent tool holds its one call for every call its delegate holds,
                        // so the call runs once, for its latest request with a decision, and
                        // the delegate resumes each of its calls that has one (spec 3.1 §10 F4).
                        Ok(true) => match resumed_calls
                            .iter_mut()
                            .find(|resumed| resumed.function_call_id == function_call_id)
                        {
                            Some(resumed) => resumed.resumes = Some(held.request),
                            None => resumed_calls.push(PendingToolCall {
                                index: resumed_calls.len(),
                                name: held.name,
                                args: held.args,
                                id: held.id,
                                function_call_id,
                                resumes: Some(held.request),
                            }),
                        },
                        Ok(false) => {}
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
            }
            if !resumed_calls.is_empty() {
                if ctx.is_cancelled() {
                    return;
                }
                // The held calls keep their pending responses, so a refused batch leaves
                // them held for a later run.
                if let Some(tracker) = budget_tracker.as_ref()
                    && let Err(exceeded) = tracker.begin_tool_calls(resumed_calls.len() as u64)
                {
                    yield Err(exceeded.into());
                    return;
                }
                tracing::info!(
                    agent.name = %agent_name,
                    tool.calls = resumed_calls.len(),
                    "resuming tool calls held for confirmation"
                );
                let strategy = ToolDispatchMode::resolve(
                    agent_tool_execution_strategy.unwrap_or(ToolExecutionStrategy::Sequential),
                    parallelize_agent_delegations,
                );
                let cb_mutex = std::sync::Mutex::new(circuit_breaker_state.take());
                let concurrency_manager =
                    adk_core::ToolConcurrencyManager::new(&ctx.run_config().tool_concurrency);
                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::channel::<Event>(TOOL_PROGRESS_CAPACITY);
                let progress_overflow = ProgressOverflow::default();
                let executor = ToolExecutor {
                    ctx: ctx.clone(),
                    agent_name: &agent_name,
                    tool_map: &tool_map,
                    tool_retry_budgets: &tool_retry_budgets,
                    default_retry_budget: &default_retry_budget,
                    output_guardrails: output_guardrails.as_ref(),
                    before_tool_callbacks: &before_tool_callbacks,
                    after_tool_callbacks: &after_tool_callbacks,
                    after_tool_callbacks_full: &after_tool_callbacks_full,
                    on_tool_error_callbacks: &on_tool_error_callbacks,
                    hook_callbacks: &hook_callbacks,
                    tool_confirmation_policy: &tool_confirmation_policy,
                    tool_guardrails: tool_guardrails.as_ref(),
                    cb_mutex: &cb_mutex,
                    invocation_id: &invocation_id,
                    concurrency_manager: &concurrency_manager,
                    progress_tx,
                    progress_overflow: progress_overflow.clone(),
                    tool_timeout,
                    expose_tool_error_messages,
                    approval_failed: tokio::sync::watch::channel(false).0,
                    #[cfg(feature = "enhanced-plugins")]
                    enhanced_plugin_manager: &enhanced_plugin_manager,
                };
                let mut results = {
                    let dispatch = executor.dispatch(strategy, resumed_calls);
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
                    while let Ok(progress_event) = progress_rx.try_recv() {
                        yield Ok(progress_event);
                    }
                    let markers = std::mem::take(
                        &mut *progress_overflow.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
                    for marker in markers {
                        yield Ok(marker);
                    }
                    match results {
                        Ok(results) => results,
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                };
                results.sort_by_key(|r| r.index);
                drop(executor);
                circuit_breaker_state = cb_mutex.into_inner().unwrap_or_else(|e| e.into_inner());

                // A call held again has not run, and counts when a later run resumes it.
                if let Some(tracker) = budget_tracker.as_ref() {
                    let held = results.iter().filter(|result| result.pending.is_some()).count();
                    tracker.release_tool_calls(held as u64);
                }
                let mut held_again = Vec::new();
                for result in results {
                    if result.escalate_or_skip {
                        // The call ends the turn before any model call, so a message this
                        // run brought would otherwise go unanswered without a word.
                        if ctx.user_content().carries_user_input() {
                            yield Err(adk_core::AdkError::new(
                                adk_core::ErrorComponent::Agent,
                                adk_core::ErrorCategory::InvalidInput,
                                "agent.resume_ended_turn",
                                "a resumed tool call ended the turn, so the message sent with \
                                 this run is saved but was not answered; run again with an \
                                 empty message to answer it, or resume held calls with an \
                                 empty message before sending a new one",
                            ));
                        }
                        return;
                    }
                    held_again.extend(result.pending);
                    // Each result takes the place of its call's pending response, matched by
                    // call ID, so the model sees one response per call.
                    let _ = adk_core::supersede_pending_responses(
                        &mut conversation_history,
                        result.content,
                    );
                }
                if !held_again.is_empty() {
                    for request in held_again {
                        yield Ok(confirmation_event(&invocation_id, &agent_name, request));
                    }
                    return;
                }

                let refreshed_tools = match tool_setup.resolve(&ctx).await {
                    Ok(tools) => tools,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                };
                tool_map = refreshed_tools.map;
                tool_declarations = refreshed_tools.declarations;
                valid_transfer_targets = refreshed_tools.transfer_targets;
            }

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
                // The kill switch is checked before every model call.
                if let Err(error) = ctx.run_config().check_governance() {
                    yield Err(error);
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

                // Call IDs the agent generated for a provider that sent none stay out of
                // what the provider receives.
                let mut contents = conversation_history.clone();
                contents.iter_mut().for_each(Content::strip_generated_call_ids);
                let mut request = LlmRequest {
                    model: model.name().to_string(),
                    contents,
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

                if let Err(error) = tool_setup.process_llm_request(&ctx, &mut request).await {
                    yield Err(error);
                    return;
                }

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
                    for callback in
                        hook_callbacks.before_model.iter().chain(before_model_callbacks.iter())
                    {
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
                let mut request = current_request;
                // Governed after every request rewrite, so a callback cannot re-attach a
                // withheld tool. A skipped model call sends nothing, so it is not governed.
                let withheld_builtins = if model_response_override.is_none() {
                    let gate = ToolGate::for_context(ctx.as_ref());
                    match builtin_governor.govern(&gate, &mut request.tools, &tool_map).await {
                        Ok(withheld) => withheld,
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                } else {
                    Vec::new()
                };
                let withheld_builtins = (!withheld_builtins.is_empty())
                    .then(|| serde_json::to_string(&withheld_builtins).unwrap_or_default());
                let request = request;

                // Determine streaming source: cached response or real model
                let mut accumulated_content: Option<Content> = None;
                let mut final_provider_metadata: Option<serde_json::Value> = None;

                let produced_by_model = model_response_override.is_none();
                let mut last_turn_finish_reason = None;
                if let Some(cached_response) = model_response_override {
                    // Use callback-provided response (e.g., from cache)
                    // Yield it as an event
                    accumulated_content = cached_response.content.clone();
                    final_provider_metadata = cached_response.provider_metadata.clone();
                    normalize_option_content(&mut accumulated_content);
                    // Text beside a tool call, or in a paused turn, reaches the client and the
                    // session as the final text does, so it is screened too.
                    let mut output_warnings = Warnings::default();
                    if let Some(content) = accumulated_content.take() {
                        let preamble = content
                            .parts
                            .iter()
                            .any(|part| matches!(part, Part::FunctionCall { .. }))
                            || Self::continues_turn(final_provider_metadata.as_ref());
                        let screened =
                            screen_model_output(output_guardrails.as_ref(), content, preamble).await?;
                        accumulated_content = Some(screened.content);
                        output_warnings = screened.warnings;
                    }

                    let mut cached_event = Event::new(&invocation_id);
                    cached_event.author = agent_name.clone();
                    output_warnings.record_on(&mut cached_event);
                    cached_event.llm_response.content = accumulated_content.clone();
                    cached_event.llm_response.turn_complete = cached_response.turn_complete;
                    cached_event.llm_response.citation_metadata = cached_response.citation_metadata.clone();
                    cached_event.llm_response.provider_metadata = cached_response.provider_metadata.clone();
                    // Surface and track the response id for provider-neutral continuity.
                    cached_event.llm_response.interaction_id = cached_response.interaction_id.clone();
                    if cached_response.interaction_id.is_some() {
                        last_interaction_id = cached_response.interaction_id.clone();
                    }
                    cached_event.llm_request = Some(trace_json_payload(
                        &request,
                        ctx.run_config().record_payloads,
                        ctx.run_config().trace_payload_max_bytes,
                    ));
                    // The debug copy carries the screened content, not what the guardrails replaced.
                    let mut debug_response = cached_response.clone();
                    if !output_guardrails.is_empty() {
                        debug_response.content = accumulated_content.clone();
                    }
                    cached_event.provider_metadata.insert("gcp.vertex.agent.llm_response".to_string(), serde_json::to_string(&debug_response).unwrap_or_default());

                    // Populate long_running_tool_ids for function calls from long-running tools
                    if let Some(ref content) = accumulated_content {
                        cached_event.long_running_tool_ids =
                            collect_long_running_tool_ids(&tool_map, content);
                    }

                    yield Ok(cached_event);
                } else {
                    // One bounded copy of the request serves the span and the event.
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
                    // NOTE: do not `llm_span.enter()` here. This is an
                    // `async_stream` generator body: an entered guard is tied to the
                    // current thread, not the task, so it is neither exited when the
                    // generator suspends at an `.await`/`yield` nor re-entered when
                    // it resumes (possibly on another worker thread). That both
                    // detaches everything after the first suspension point from
                    // `call_llm` and leaks the entry onto the worker thread, where
                    // unrelated tasks then inherit it as a parent. Instrument the
                    // individual futures instead — `Instrumented::poll` enters the
                    // span per poll, so it is correct across suspension.

                    // Check streaming mode from run config
                    use adk_core::StreamingMode;
                    let streaming_mode = ctx.run_config().streaming_mode;
                    let should_stream_to_client = matches!(streaming_mode, StreamingMode::SSE | StreamingMode::Bidi)
                        && output_guardrails.is_empty();

                    // Reserves the call's upper-bound cost in the spend ledger and one model
                    // call in the run budget before it starts; the stream records the call's
                    // usage once, from its first non-partial response carrying usage, and
                    // commits its cost. A refusal stops the run before anything is sent.
                    let call_scope = adk_core::ModelCallScope::for_run(
                        ctx.run_config(),
                        ctx.app_name(),
                        &agent_name,
                    );
                    let call_metered = call_scope.budget().is_some();
                    // Kept for the run's `on_model_error` hooks, which see what the model was sent.
                    let failed_request = (!ctx.run_config().invocation_hooks.is_empty())
                        .then(|| request.clone());
                    // Always use streaming internally for LLM calls
                    let mut response_stream = match call_scope
                        .generate(model.as_ref(), request, true)
                        .instrument(llm_span.clone())
                        .await
                    {
                        Ok(stream) => stream,
                        Err(error) => {
                            // Provider errors can echo the prompt, so the message follows the
                            // payload rule.
                            llm_span.in_scope(|| {
                                tracing::warn!(
                                    agent.name = %agent_name,
                                    error = %error.diagnostics(),
                                    error.message = %traced_message(ctx.run_config(), &error.message),
                                    "model call failed"
                                )
                            });
                            // A hook's response, from a fallback model for example, answers
                            // the turn in the failed call's place.
                            match recover_model_error(&ctx, failed_request.as_ref(), &error).await {
                                Ok(Some(response)) => {
                                    Box::pin(futures::stream::iter([Ok(response)]))
                                        as adk_core::LlmResponseStream
                                }
                                Ok(None) => {
                                    yield Err(error);
                                    return;
                                }
                                Err(hook_error) => {
                                    yield Err(hook_error);
                                    return;
                                }
                            }
                        }
                    };

                    use futures::StreamExt;

                    // Track last chunk for final event metadata (used in None mode)
                    let mut last_chunk: Option<LlmResponse> = None;

                    // Stream and process chunks with AfterModel callbacks
                    while let Some(chunk_result) =
                        response_stream.next().instrument(llm_span.clone()).await
                    {
                        // Cooperative cancellation: stop consuming the model
                        // stream promptly when the invocation is cancelled. This
                        // drops `response_stream`, releasing the provider connection.
                        if ctx.is_cancelled() {
                            llm_span.in_scope(|| {
                                tracing::info!(agent.name = %agent_name, "invocation cancelled during LLM streaming")
                            });
                            return;
                        }
                        let mut chunk = match chunk_result {
                            Ok(c) => c,
                            Err(e) => {
                                llm_span.in_scope(|| {
                                    tracing::warn!(
                                        agent.name = %agent_name,
                                        error = %e.diagnostics(),
                                        error.message = %traced_message(ctx.run_config(), &e.message),
                                        "model stream failed"
                                    )
                                });
                                // A hook's response replaces the whole failed response, so
                                // the deltas already received are dropped.
                                match recover_model_error(&ctx, failed_request.as_ref(), &e).await {
                                    Ok(Some(response)) => {
                                        accumulated_content = None;
                                        response_stream = Box::pin(futures::stream::iter([Ok(response)]));
                                        continue;
                                    }
                                    Ok(None) => {
                                        yield Err(e);
                                        return;
                                    }
                                    Err(hook_error) => {
                                        yield Err(hook_error);
                                        return;
                                    }
                                }
                            }
                        };
                        // ===== AFTER MODEL CALLBACKS (per chunk) =====
                        // Callbacks can modify each streaming chunk
                        for callback in
                            hook_callbacks.after_model.iter().chain(after_model_callbacks.iter())
                        {
                            match callback(ctx.clone() as Arc<dyn CallbackContext>, chunk.clone())
                                .instrument(llm_span.clone())
                                .await
                            {
                                Ok(Some(modified_chunk)) => {
                                    chunk = replace_chunk(&chunk, modified_chunk);
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
                            if !chunk.partial && chunk.provider_metadata.as_ref()
                                .and_then(|metadata| metadata.get("content_complete"))
                                .and_then(serde_json::Value::as_bool) == Some(true)
                            {
                                accumulated_content = Some(chunk_content);
                            } else if let Some(ref mut acc) = accumulated_content {
                                append_streamed_parts(&mut acc.parts, chunk_content.parts);
                            } else {
                                accumulated_content = Some(chunk_content);
                            }
                        }
                        // Tool-call markup can span deltas, so it is parsed again once the
                        // text is complete.
                        if !chunk.partial {
                            normalize_option_content(&mut accumulated_content);
                        }

                        // For SSE/Bidi mode: yield each chunk immediately with stable event ID
                        if should_stream_to_client {
                            let long_running_tool_ids = chunk
                                .content
                                .as_ref()
                                .map(|content| collect_long_running_tool_ids(&tool_map, content))
                                .unwrap_or_default();
                            let mut event = build_partial_llm_event(
                                &llm_event_id,
                                &invocation_id,
                                &agent_name,
                                &trace_request_json,
                                &chunk,
                                long_running_tool_ids,
                            );
                            if call_metered {
                                event.provider_metadata.insert(
                                    adk_core::BUDGET_RECORDED_KEY.to_string(),
                                    "true".to_string(),
                                );
                            }
                            if !chunk.partial
                                && let Some(withheld) = &withheld_builtins
                            {
                                event.provider_metadata.insert(
                                    adk_core::BUILTIN_TOOLS_WITHHELD_KEY.to_string(),
                                    withheld.clone(),
                                );
                            }
                            // Runner persists terminal events as complete content.
                            // Providers can finish with tools/metadata after text
                            // deltas, so the last chunk alone is insufficient.
                            if !chunk.partial {
                                event.llm_response.content = accumulated_content.clone();
                                if let Some(metadata) = event.llm_response.provider_metadata
                                    .get_or_insert_with(|| serde_json::json!({}))
                                    .as_object_mut()
                                {
                                    metadata.insert("content_complete".into(), true.into());
                                }
                            }
                            yield Ok(event);
                        }

                        // Track the response id for provider-neutral continuity.
                        // Transports that support response chaining populate
                        // `interaction_id`; others leave it `None` (no-op).
                        if chunk.interaction_id.is_some() {
                            last_interaction_id = chunk.interaction_id.clone();
                        }

                        if chunk.finish_reason.is_some() {
                            last_turn_finish_reason = chunk.finish_reason;
                        }
                        // Store last chunk for final event metadata
                        last_chunk = Some(chunk.clone());

                        // Check if turn is complete
                        if chunk.turn_complete {
                            break;
                        }
                    }

                    if let Some(last) = &last_chunk {
                        final_provider_metadata = last.provider_metadata.clone();
                    }
                    normalize_option_content(&mut accumulated_content);

                    // A stream whose last chunk is partial never produced the complete event
                    // the runner persists, so one is emitted here with the whole response.
                    if should_stream_to_client
                        && let Some(last) = last_chunk.as_ref().filter(|chunk| chunk.partial)
                        && accumulated_content.is_some()
                    {
                        let mut closing = last.clone();
                        closing.partial = false;
                        closing.content = accumulated_content.clone();
                        closing.turn_complete = !closing
                            .content
                            .as_ref()
                            .is_some_and(|content| {
                                content.parts.iter().any(|part| matches!(part, Part::FunctionCall { .. }))
                            });
                        let long_running_tool_ids = closing
                            .content
                            .as_ref()
                            .map(|content| collect_long_running_tool_ids(&tool_map, content))
                            .unwrap_or_default();
                        let mut event = build_partial_llm_event(
                            &llm_event_id,
                            &invocation_id,
                            &agent_name,
                            &trace_request_json,
                            &closing,
                            long_running_tool_ids,
                        );
                        if call_metered {
                            event.provider_metadata.insert(
                                adk_core::BUDGET_RECORDED_KEY.to_string(),
                                "true".to_string(),
                            );
                        }
                        if let Some(withheld) = &withheld_builtins {
                            event.provider_metadata.insert(
                                adk_core::BUILTIN_TOOLS_WITHHELD_KEY.to_string(),
                                withheld.clone(),
                            );
                        }
                        if let Some(metadata) = event.llm_response.provider_metadata
                            .get_or_insert_with(|| serde_json::json!({}))
                            .as_object_mut()
                        {
                            metadata.insert("content_complete".into(), true.into());
                        }
                        yield Ok(event);
                    }
                    // A stream that ended without a non-partial usage report is settled
                    // here, before any tool runs, from the best usage it carried.
                    drop(response_stream);
                    let metered = call_metered;

                    // For None mode: yield single final event with accumulated content
                    if !should_stream_to_client {
                        // Text beside a tool call, or in a paused turn, reaches the client and
                        // the session as the final text does, so it is screened too.
                        let mut output_warnings = Warnings::default();
                        if let Some(content) = accumulated_content.take() {
                            let preamble = content
                                .parts
                                .iter()
                                .any(|part| matches!(part, Part::FunctionCall { .. }))
                                || Self::continues_turn(final_provider_metadata.as_ref());
                            let screened =
                                screen_model_output(output_guardrails.as_ref(), content, preamble)
                                    .instrument(llm_span.clone())
                                    .await?;
                            accumulated_content = Some(screened.content);
                            output_warnings = screened.warnings;
                        }
                        // The debug copy of the last chunk carries the screened content, not
                        // what the guardrails replaced.
                        if !output_guardrails.is_empty()
                            && let Some(last) = last_chunk.as_mut()
                            && last.content.is_some()
                        {
                            last.content = accumulated_content.clone();
                        }

                        let long_running_tool_ids = accumulated_content
                            .as_ref()
                            .map(|content| collect_long_running_tool_ids(&tool_map, content))
                            .unwrap_or_default();
                        let mut final_event = build_final_llm_event(
                            &llm_event_id,
                            &invocation_id,
                            &agent_name,
                            &trace_request_json,
                            accumulated_content.as_ref(),
                            last_chunk.as_ref(),
                            long_running_tool_ids,
                        );
                        output_warnings.record_on(&mut final_event);
                        if metered {
                            final_event.provider_metadata.insert(
                                adk_core::BUDGET_RECORDED_KEY.to_string(),
                                "true".to_string(),
                            );
                        }
                        if let Some(withheld) = &withheld_builtins {
                            final_event.provider_metadata.insert(
                                adk_core::BUILTIN_TOOLS_WITHHELD_KEY.to_string(),
                                withheld.clone(),
                            );
                        }
                        yield Ok(final_event);
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
                        // Provider text can echo the request, so the caller gets a bounded
                        // excerpt and the log follows the payload rule.
                        let message = last
                            .error_message
                            .as_deref()
                            .map(adk_core::upstream_excerpt)
                            .unwrap_or("provider reported a terminal error");
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
                        llm_span.in_scope(|| {
                            tracing::error!(
                                error = %provider_error.diagnostics(),
                                error.message = %traced_message(ctx.run_config(), message),
                                agent = %agent_name,
                                "model reported a terminal error"
                            )
                        });
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
                let continues_turn = Self::continues_turn(final_provider_metadata.as_ref());

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
                }

                // Native server tools can pause a turn without calling a client
                // tool. Continue through this same bounded, cancellable loop.
                if !has_function_calls && continues_turn {
                    continue;
                }

                if !has_function_calls {
                    // ===== EMPTY MODEL TURN =====
                    // A turn the model ended with no answer — no parts, only thinking, or only
                    // whitespace — fails the run, so callers and retry policy can tell it from
                    // an answer. A callback that answered instead of the model is not checked.
                    if produced_by_model
                        && !accumulated_content.as_ref().is_some_and(has_answer_content)
                    {
                        yield Err(empty_response_error(last_turn_finish_reason));
                        return;
                    }

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

                    // Persist the final output only once it has passed schema validation, so a
                    // rejected attempt never lands in `state[output_key]`.
                    if let Some(ref output_key) = output_key
                        && let Some(ref content) = accumulated_content
                    {
                        // Structured output is stored as the JSON it validated as.
                        let text_parts =
                            output_text(content, prompt_config.output_schema.is_some());
                        if !text_parts.is_empty() {
                            let mut state_event = Event::new(&invocation_id);
                            state_event.author = agent_name.clone();
                            state_event.actions.state_delta.insert(
                                output_key.clone(),
                                serde_json::Value::String(text_parts),
                            );
                            yield Ok(state_event);
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
                    let strategy = ToolDispatchMode::resolve(
                        agent_tool_execution_strategy.unwrap_or(ToolExecutionStrategy::Sequential),
                        parallelize_agent_delegations,
                    );

                    // Providers that omit call IDs get one unique to this model turn, so two
                    // turns of one invocation never share an idempotency key.
                    let fc_parts = collect_function_calls(
                        content,
                        &format!("{invocation_id}_{}", uuid::Uuid::new_v4().simple()),
                    );

                    // ===== HANDLE transfer_to_agent BEFORE DISPATCH =====
                    // Transfer calls cause an immediate return from the stream,
                    // so they must be handled inline regardless of strategy.
                    let mut transfer_handled = false;
                    // Calls already answered by an invalid-target error, so a later valid
                    // transfer does not answer them a second time.
                    let mut answered_calls = HashSet::new();
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
                                answered_calls.insert(call.index);
                                continue;
                            }

                            // Every call in this model turn gets exactly one response, so providers
                            // that require one per call accept any later history containing it.
                            let responses = fc_parts
                                .iter()
                                .filter(|other| !answered_calls.contains(&other.index))
                                .map(|other| {
                                    let response = if other.index == call.index {
                                        serde_json::json!({ "transferred_to": target_agent })
                                    } else {
                                        serde_json::json!({
                                            "error": format!("not run: control transferred to {target_agent}")
                                        })
                                    };
                                    Part::FunctionResponse {
                                        function_response: FunctionResponseData::new(
                                            other.name.clone(),
                                            response,
                                        ),
                                        id: other.id.clone(),
                                        annotations: None,
                                    }
                                })
                                .collect();
                            let mut transfer_event = Event::new(&invocation_id);
                            transfer_event.author = agent_name.clone();
                            transfer_event.llm_response.content =
                                Some(Content { role: "function".to_string(), parts: responses });
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
                    let fc_parts: Vec<_> = fc_parts
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

                    // ===== RUN BUDGET: TOOL CALLS =====
                    // The whole batch is reserved before any call starts. A batch that does
                    // not fit is answered with "not run" responses, so the history keeps one
                    // response per call, and the run ends with the budget error.
                    if let Some(tracker) = budget_tracker.as_ref()
                        && let Err(exceeded) = tracker.begin_tool_calls(fc_parts.len() as u64)
                    {
                        let parts = fc_parts
                            .iter()
                            .map(|call| Part::FunctionResponse {
                                function_response: FunctionResponseData::new(
                                    call.name.clone(),
                                    serde_json::json!({ "error": format!("not run: {exceeded}") }),
                                ),
                                id: call.id.clone(),
                                annotations: None,
                            })
                            .collect();
                        let mut budget_event = Event::new(&invocation_id);
                        budget_event.author = agent_name.clone();
                        budget_event.llm_response.content =
                            Some(Content { role: "function".to_string(), parts });
                        yield Ok(budget_event);
                        yield Err(exceeded.into());
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
                    let progress_overflow = ProgressOverflow::default();

                    let executor = ToolExecutor {
                        ctx: ctx.clone(),
                        agent_name: &agent_name,
                        tool_map: &tool_map,
                        tool_retry_budgets: &tool_retry_budgets,
                        default_retry_budget: &default_retry_budget,
                        output_guardrails: output_guardrails.as_ref(),
                        before_tool_callbacks: &before_tool_callbacks,
                        after_tool_callbacks: &after_tool_callbacks,
                        after_tool_callbacks_full: &after_tool_callbacks_full,
                        on_tool_error_callbacks: &on_tool_error_callbacks,
                        hook_callbacks: &hook_callbacks,
                        tool_confirmation_policy: &tool_confirmation_policy,
                        tool_guardrails: tool_guardrails.as_ref(),
                        cb_mutex: &cb_mutex,
                        invocation_id: &invocation_id,
                        concurrency_manager: &concurrency_manager,
                        progress_tx: progress_tx.clone(),
                        progress_overflow: progress_overflow.clone(),
                        tool_timeout,
                        expose_tool_error_messages,
                        approval_failed: tokio::sync::watch::channel(false).0,
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
                        let dispatch = executor.dispatch(strategy, fc_parts);

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
                        // Then the markers that found the queue full.
                        let markers = std::mem::take(
                            &mut *progress_overflow
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner),
                        );
                        for marker in markers {
                            yield Ok(marker);
                        }
                        match results {
                            Ok(results) => results,
                            Err(error) => {
                                yield Err(error);
                                return;
                            }
                        }
                    };
                    // Preserve LLM-returned order even when tool futures finish out of order.
                    results.sort_by_key(|r| r.index);

                    // Restore circuit breaker state from the mutex
                    circuit_breaker_state = cb_mutex.into_inner().unwrap_or_else(|e| e.into_inner());

                    // Events were already emitted on completion. Only model context
                    // is assembled in call order; never emit a second result event.
                    // A held call has not run; it counts once, when a later run resumes it.
                    if let Some(tracker) = budget_tracker.as_ref() {
                        let held = results.iter().filter(|result| result.pending.is_some()).count();
                        tracker.release_tool_calls(held as u64);
                    }
                    let mut held_for_confirmation = Vec::new();
                    for result in results {
                        if result.escalate_or_skip {
                            return;
                        }
                        held_for_confirmation.extend(result.pending);
                        conversation_history.push(result.content);
                    }

                    // ===== TOOL CONFIRMATION INTERRUPT =====
                    // A call held for a decision ends the run. Its request carries the final
                    // arguments, and the next run that has a decision for it resumes the call
                    // before its first model call.
                    if !held_for_confirmation.is_empty() {
                        for request in held_for_confirmation {
                            yield Ok(confirmation_event(&invocation_id, &agent_name, request));
                        }
                        return;
                    }

                    // A tool can update session state as part of its response. Resolve
                    // toolsets again before the next model turn so context-sensitive
                    // toolsets (notably progressive skills) can expose newly activated
                    // capabilities in the same invocation.
                    let refreshed_tools = match tool_setup.resolve(&ctx).await {
                        Ok(tools) => tools,
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    };
                    tool_map = refreshed_tools.map;
                    tool_declarations = refreshed_tools.declarations;
                    valid_transfer_targets = refreshed_tools.transfer_targets;
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
        });

        let s = stream! {
            use futures::StreamExt;
            let ctx = stage_ctx;
            let invocation_id = stage_invocation_id;
            let agent_name = stage_agent_name;

            // ===== BEFORE AGENT CALLBACKS =====
            // The first callback that returns content answers for the agent, which then skips
            // its body and goes to the after-agent stage.
            let mut answered = false;
            for callback in hook_before_agent.iter().chain(before_agent_callbacks.iter()) {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut early_event = Event::new(&invocation_id);
                        early_event.author = agent_name.clone();
                        // The callback's answer is the agent's output, so `output_key` keeps it.
                        if let Some(key) = &stage_output_key {
                            let text = output_text(&content, structured_output);
                            if !text.is_empty() {
                                early_event
                                    .actions
                                    .state_delta
                                    .insert(key.clone(), serde_json::Value::String(text));
                            }
                        }
                        early_event.llm_response.content = Some(content);
                        yield Ok(early_event);
                        answered = true;
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            if !answered {
                // A body that is dropped before it finishes — the run cancelled, the client
                // gone, a parallel sibling escalated — still runs the after-agent stage. A
                // durable attempt dropped by its worker is resumed by the worker that
                // recovers the run, which runs the stage then.
                let mut on_drop = AfterAgentOnDrop::armed(
                    Arc::clone(&hook_after_agent),
                    Arc::clone(&after_agent_callbacks),
                    ctx.clone() as Arc<dyn CallbackContext>,
                );
                if ctx.run_config().durable_run_id.is_some() {
                    on_drop.disarm();
                }
                let mut body = body;
                let mut finished = true;
                while let Some(item) = body.next().await {
                    // A failed agent has not completed, and a held call resumes the agent in a
                    // later run, which runs the stage then.
                    let pauses = match &item {
                        Err(_) => true,
                        Ok(event) => event.actions.tool_confirmation.is_some(),
                    };
                    if pauses {
                        finished = false;
                        on_drop.disarm();
                    }
                    yield item;
                }
                on_drop.disarm();
                if !finished {
                    return;
                }
            }

            // ===== AFTER AGENT CALLBACKS =====
            // Runs on every other exit: an answer, a transfer, a turn-ending tool result, and
            // a cancellation the agent observed. The first callback that returns content wins.
            for callback in hook_after_agent.iter().chain(after_agent_callbacks.iter()) {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut after_event = Event::new(&invocation_id);
                        after_event.author = agent_name.clone();
                        after_event.llm_response.content = Some(content);
                        yield Ok(after_event);
                        break;
                    }
                    Ok(None) => {}
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

/// Runs an agent's after-agent callbacks when its stream is dropped before the agent
/// finishes, on the current Tokio runtime.
///
/// Nothing reads the stream any more, so content a callback returns is discarded.
struct AfterAgentOnDrop {
    hooks: Arc<Vec<AfterAgentCallback>>,
    callbacks: Arc<Vec<AfterAgentCallback>>,
    ctx: Arc<dyn CallbackContext>,
    armed: bool,
}

impl AfterAgentOnDrop {
    fn armed(
        hooks: Arc<Vec<AfterAgentCallback>>,
        callbacks: Arc<Vec<AfterAgentCallback>>,
        ctx: Arc<dyn CallbackContext>,
    ) -> Self {
        Self { hooks, callbacks, ctx, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AfterAgentOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                agent.name = %self.ctx.agent_name(),
                "no Tokio runtime to run after-agent callbacks on; they are skipped"
            );
            return;
        };
        let (hooks, callbacks, ctx) =
            (Arc::clone(&self.hooks), Arc::clone(&self.callbacks), Arc::clone(&self.ctx));
        runtime.spawn(async move {
            for callback in hooks.iter().chain(callbacks.iter()) {
                match callback(ctx.clone()).await {
                    Ok(Some(_)) => break,
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(
                            agent.name = %ctx.agent_name(),
                            error.code = %error.code,
                            "after-agent callback failed after the agent stopped"
                        );
                        break;
                    }
                }
            }
        });
    }
}

/// Runs one tool call. An agent-delegation tool, such as an `AgentTool`, runs on a Tokio task
/// of its own.
///
/// A delegate's agent is polled from inside this agent's tool call, and its own delegates from
/// inside its tool calls, so without a task boundary every level of delegation stacks another
/// agent's frames on one thread's stack — several hundred KiB per level in debug builds,
/// enough for three levels to overflow a 2 MiB worker thread. The task is aborted when this
/// call is dropped, by cancellation or a timeout, and a panic in it resumes here.
async fn execute_tool(
    tool: Arc<dyn Tool>,
    ctx: Arc<dyn ToolContext>,
    args: serde_json::Value,
) -> Result<serde_json::Value> {
    let runtime = tokio::runtime::Handle::try_current().ok().filter(|_| tool.is_agent_delegation());
    let Some(runtime) = runtime else {
        return tool.execute(ctx, args).await;
    };
    let call = async move { tool.execute(ctx, args).await };
    let mut task = AbortOnDrop(runtime.spawn(call.in_current_span()));
    match (&mut task.0).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_) => Err(adk_core::AdkError::new(
            adk_core::ErrorComponent::Tool,
            adk_core::ErrorCategory::Cancelled,
            "tool.cancelled",
            "the delegated call's task was cancelled before it finished",
        )),
    }
}

/// Aborts a spawned task when it is dropped unfinished.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The text `output_key` stores for `content`: its text parts, and for structured output
/// the JSON inside a surrounding code fence.
fn output_text(content: &Content, structured: bool) -> String {
    let text: String = content.parts.iter().filter_map(Part::text).collect();
    if structured { strip_json_code_fence(&text).to_string() } else { text }
}

/// Whether `error` is the run's own budget or spend ledger refusing a model call, which no
/// fallback may answer.
fn is_run_limit_refusal(error: &adk_core::AdkError) -> bool {
    error.component == adk_core::ErrorComponent::Guardrail
        || (error.component == adk_core::ErrorComponent::Agent
            && error.category == adk_core::ErrorCategory::ResourceExhausted)
}

/// Asks the run's hooks for a response to a failed model call; the first response wins.
///
/// # Errors
///
/// Returns the error of a hook that fails.
async fn recover_model_error(
    ctx: &Arc<dyn InvocationContext>,
    request: Option<&LlmRequest>,
    error: &adk_core::AdkError,
) -> Result<Option<LlmResponse>> {
    let Some(request) = request else {
        return Ok(None);
    };
    if is_run_limit_refusal(error) {
        return Ok(None);
    }
    for hook in &ctx.run_config().invocation_hooks {
        if let Some(response) = hook
            .on_model_error(ctx.clone() as Arc<dyn CallbackContext>, request.clone(), error)
            .await?
        {
            return Ok(Some(response));
        }
    }
    Ok(None)
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

    #[test]
    fn partial_stream_events_do_not_repeat_large_payloads() {
        let marker = "PRIVATE-REQUEST-PAYLOAD";
        let request_json = format!("{marker}{}", "x".repeat(128 * 1024));
        let chunk = LlmResponse {
            content: Some(Content::new("model").with_text("x")),
            partial: true,
            ..Default::default()
        };
        let mut serialized_bytes = 0;

        for index in 0..64 {
            let event = build_partial_llm_event(
                &format!("event-{index}"),
                "invocation",
                "agent",
                &request_json,
                &chunk,
                Vec::new(),
            );
            let serialized = serde_json::to_string(&event).expect("event should serialize");

            assert!(!serialized.contains(marker));
            assert!(event.llm_request.is_none());
            assert!(!event.provider_metadata.contains_key("gcp.vertex.agent.llm_request"));
            assert!(!event.provider_metadata.contains_key("gcp.vertex.agent.llm_response"));
            serialized_bytes += serialized.len();
        }

        assert!(
            serialized_bytes < 1024 * 1024,
            "64 tiny chunks unexpectedly serialized to {serialized_bytes} bytes"
        );
    }
}
