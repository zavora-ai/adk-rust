//! AgentTool - Use agents as callable tools
//!
//! This module provides `AgentTool` which wraps an `Agent` instance to make it
//! callable as a `Tool`. This enables powerful composition patterns where a
//! coordinator agent can invoke specialized sub-agents.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use adk_agent::{LlmAgent, LlmAgentBuilder};
//! use adk_core::Llm;
//! use adk_tool::AgentTool;
//!
//! fn coordinator(model: Arc<dyn Llm>) -> adk_core::Result<LlmAgent> {
//!     // Create a specialized agent
//!     let math_agent = LlmAgentBuilder::new("math_expert")
//!         .description("Solves mathematical problems")
//!         .instruction("You are a math expert. Solve problems step by step.")
//!         .model(model.clone())
//!         .build()?;
//!
//!     // Wrap it as a tool and give it to a coordinator agent
//!     let math_tool = AgentTool::new(Arc::new(math_agent));
//!     LlmAgentBuilder::new("coordinator")
//!         .instruction("Help users by delegating to specialists")
//!         .model(model)
//!         .tool(Arc::new(math_tool))
//!         .build()
//! }
//! ```

use adk_core::{
    Agent, Artifacts, CallbackContext, Content, Event, InvocationContext, Memory, Part,
    ReadonlyContext, Result, RunConfig, Session, State, Tool, ToolConfirmationRequest, ToolContext,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

/// Controls which parent session data is copied into an agent-tool invocation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentToolSessionSnapshot {
    /// Start each delegated invocation with empty history and state.
    #[default]
    Isolated,
    /// Copy the parent's current conversation history and state into the isolated child session.
    Parent,
}

/// Controls how an [`AgentTool`] reports delegated execution failures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentToolFailureMode {
    /// Return the legacy JSON error object as a successful tool result.
    #[default]
    ReturnErrorObject,
    /// Propagate the failure through the tool's [`Result`].
    Propagate,
}

/// Merge behavior for state produced by an agent-as-tool invocation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentToolStateMergePolicy {
    /// Preserve legacy last-writer-wins behavior.
    #[default]
    Overwrite,
    /// Reject a child write when the parent value changed after delegation began.
    RejectConflicts,
}

#[derive(Clone)]
struct AgentToolRuntimeConfig {
    session_snapshot: AgentToolSessionSnapshot,
    failure_mode: AgentToolFailureMode,
    forward_memory: bool,
    forward_shared_state: bool,
    forward_events: bool,
    max_delegation_depth: Option<u32>,
    reject_child_handoffs: bool,
    execute_child_handoffs: bool,
    handoff_agents: Arc<HashMap<String, Arc<dyn Agent>>>,
    history_max_events: Option<usize>,
    state_keys: Option<Arc<HashSet<String>>>,
    output_state_keys: Option<Arc<HashSet<String>>>,
    state_merge_policy: AgentToolStateMergePolicy,
    state_merge_exempt_keys: Arc<HashSet<String>>,
    artifact_prefixes: Option<Arc<Vec<String>>>,
    principal: Option<adk_core::AgentPrincipal>,
    expose_error_messages: bool,
}

impl Default for AgentToolRuntimeConfig {
    fn default() -> Self {
        Self {
            session_snapshot: AgentToolSessionSnapshot::Isolated,
            failure_mode: AgentToolFailureMode::ReturnErrorObject,
            forward_memory: false,
            forward_shared_state: true,
            forward_events: false,
            max_delegation_depth: None,
            reject_child_handoffs: false,
            execute_child_handoffs: false,
            handoff_agents: Arc::new(HashMap::new()),
            history_max_events: None,
            state_keys: None,
            output_state_keys: None,
            state_merge_policy: AgentToolStateMergePolicy::Overwrite,
            state_merge_exempt_keys: Arc::new(HashSet::new()),
            artifact_prefixes: None,
            principal: None,
            expose_error_messages: false,
        }
    }
}

struct AgentToolChildConfig {
    forward_artifacts: bool,
    artifact_prefixes: Option<Arc<Vec<String>>>,
    forward_memory: bool,
    forward_shared_state: bool,
    session: Arc<AgentToolSession>,
    run_config: RunConfig,
    delegation_depth: u32,
    max_delegation_depth: Option<u32>,
    orchestration_root_invocation_id: String,
    orchestration_edge_id: String,
}

/// How much later than its caller the agent tool enforces its own timeout.
///
/// Timers fire on whole milliseconds, so the margin puts the agent tool's deadline on a later
/// tick than that of a caller enforcing the same limit.
const CALLER_TIMEOUT_GRACE: Duration = Duration::from_millis(5);

/// Session-state key prefix of the record a held delegation resumes from.
const HELD_DELEGATION_STATE_PREFIX: &str = "adk:agent_tool:held:";

/// A delegation that held one of its delegate's calls for confirmation.
///
/// Stored in the caller's session state, under the caller's call, so a later run that
/// resumes that call rebuilds the delegate's session and the delegate resumes its own
/// held call as the same call, without asking its model again.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeldDelegation {
    /// Fingerprint of the agent-tool call the delegation ran for.
    call: String,
    /// The agent that held the call: the wrapped agent or one of its handoff targets.
    agent: String,
    /// The delegate's request, under the delegate's own call ID.
    request: ToolConfirmationRequest,
    /// The delegate's other held requests, when it holds more than one call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    more: Vec<ToolConfirmationRequest>,
    /// The state the delegation started with.
    state: HashMap<String, Value>,
    /// The history the delegation started with.
    history: Vec<Content>,
    /// The request the delegation was given.
    user_content: Content,
    /// The delegate's events so far.
    events: Vec<Event>,
}

/// Every request the delegate's events still hold, in the order they were held.
///
/// A delegation's events can hold calls of several agents (a handoff target, a workflow's
/// sub-agents), each on its own branch.
fn delegate_held_requests(events: &[Event]) -> Vec<ToolConfirmationRequest> {
    let mut holders = Vec::<(&str, &str)>::new();
    for event in events {
        if event.actions.tool_confirmation.is_some()
            && !holders.contains(&(event.author.as_str(), event.branch.as_str()))
        {
            holders.push((event.author.as_str(), event.branch.as_str()));
        }
    }
    let mut requests = Vec::<ToolConfirmationRequest>::new();
    for (author, branch) in holders {
        for held in adk_core::held_tool_calls(events, author, branch) {
            let fingerprint = held.request.fingerprint();
            if !requests.iter().any(|known| known.fingerprint() == fingerprint) {
                requests.push(held.request);
            }
        }
    }
    requests
}

/// The event that surfaces one of a delegate's held requests as the caller's own hold, as
/// the caller's agent surfaces the request it ends its run on.
fn surfaced_confirmation_event(ctx: &dyn ToolContext, request: ToolConfirmationRequest) -> Event {
    let mut event = Event::new(ctx.invocation_id());
    event.author = ctx.agent_name().to_string();
    event.llm_response.interrupted = true;
    event.llm_response.turn_complete = true;
    event.llm_response.content = Some(Content::new("model").with_text(format!(
        "Tool confirmation required for '{}'. Provide approve/deny decision to continue.",
        request.tool_name
    )));
    event.actions.tool_confirmation = Some(request);
    event
}

/// Builds the run config of a delegated invocation from its caller's.
///
/// Every field is named, so a new one has to be classed as shared with the delegate or as
/// belonging to the caller's own agent.
/// The output text of a delegation that produced nothing.
const NO_RESPONSE: &str = "No response from agent";

fn delegated_run_config(parent: RunConfig) -> RunConfig {
    let RunConfig {
        streaming_mode: _,
        tool_confirmation_decisions: _,
        tool_confirmation_fingerprints: _,
        tool_confirmation_handler,
        runtime_toolsets: _,
        cached_content: _,
        transfer_targets: _,
        parent_agent: _,
        auto_cache,
        history_max_events,
        tool_concurrency,
        record_payloads,
        trace_payload_max_bytes,
        max_transfer_depth,
        invocation_hooks,
        budget,
        budget_tracker,
        action_ledger,
        tool_policy,
        governance,
        tool_approvals,
        approval_store,
        approval_service,
        durable_run_id,
        request_context,
        tool_confirmation_timeout,
        spend_ledger,
        llm_spend_estimate,
        audit,
        acting,
    } = parent;
    RunConfig {
        // The delegate's output is read whole.
        streaming_mode: adk_core::StreamingMode::None,
        // Call-ID decisions name the caller's calls; the delegate's calls have their own IDs.
        tool_confirmation_decisions: HashMap::new(),
        tool_confirmation_fingerprints: HashMap::new(),
        // The handoff topology, the invocation's toolsets and the context cache were built
        // for the caller's agent. The delegate's own topology is applied by the caller.
        runtime_toolsets: Vec::new(),
        cached_content: None,
        transfer_targets: Vec::new(),
        parent_agent: None,
        // Limits, governance, approvals, ledgers and identity span the whole invocation.
        tool_confirmation_handler,
        auto_cache,
        history_max_events,
        tool_concurrency,
        record_payloads,
        trace_payload_max_bytes,
        max_transfer_depth,
        invocation_hooks,
        budget,
        budget_tracker,
        action_ledger,
        tool_policy,
        governance,
        tool_approvals,
        approval_store,
        approval_service,
        durable_run_id,
        // The delegate acts for the caller's authenticated user.
        request_context,
        tool_confirmation_timeout,
        spend_ledger,
        llm_spend_estimate,
        audit,
        acting,
    }
}

/// Configuration options for AgentTool behavior.
#[derive(Debug, Clone)]
pub struct AgentToolConfig {
    /// Skip summarization after sub-agent execution.
    /// When true, returns the raw output from the sub-agent.
    pub skip_summarization: bool,

    /// Forward artifacts between parent and sub-agent.
    /// When true, the sub-agent can access parent's artifacts.
    pub forward_artifacts: bool,

    /// Optional timeout for sub-agent execution.
    ///
    /// The parent agent applies this instead of its own tool timeout, so a
    /// delegation is not cut short by a limit sized for ordinary tool calls.
    /// `None` imposes no limit beyond the run's own budget and cancellation.
    pub timeout: Option<Duration>,

    /// Custom input schema for the tool.
    /// If None, defaults to `{"request": "string"}`.
    pub input_schema: Option<Value>,

    /// Custom output schema for the tool.
    pub output_schema: Option<Value>,
}

impl Default for AgentToolConfig {
    fn default() -> Self {
        Self {
            skip_summarization: false,
            forward_artifacts: true,
            timeout: None,
            input_schema: None,
            output_schema: None,
        }
    }
}

/// AgentTool wraps an Agent to make it callable as a Tool.
///
/// When the parent LLM generates a function call targeting this tool,
/// the framework executes the wrapped agent, captures its final response,
/// and returns it as the tool's result.
///
/// The wrapped agent runs as a delegate of its caller. It shares the caller's budget,
/// governance, tool policy, approvals, ledgers, audit trail and session scope, and acts for
/// the caller's subject. It does not inherit the caller's handoff targets, context cache,
/// invocation toolsets or call-ID decisions; its own topology comes from
/// [`Agent::configure_run`] and [`Agent::transfer_targets_for`].
///
/// Every call the delegate holds for confirmation holds this call too: the tool surfaces
/// each of the delegate's requests under the caller's call ID — the last through
/// [`EventActions::tool_confirmation`](adk_core::EventActions::tool_confirmation), the
/// others before it through [`ToolContext::emit_event`] — and records the delegate's session
/// in the caller's session state. When the caller resumes the call, the delegate resumes
/// each held call that has a decision from that session; a call still undecided keeps this
/// call held (spec 3.1 §10 F4).
pub struct AgentTool {
    agent: Arc<dyn Agent>,
    config: AgentToolConfig,
    runtime: AgentToolRuntimeConfig,
}

impl AgentTool {
    /// Create a new AgentTool wrapping the given agent.
    pub fn new(agent: Arc<dyn Agent>) -> Self {
        Self {
            agent,
            config: AgentToolConfig::default(),
            runtime: AgentToolRuntimeConfig::default(),
        }
    }

    /// Create a new AgentTool with custom configuration.
    pub fn with_config(agent: Arc<dyn Agent>, config: AgentToolConfig) -> Self {
        Self { agent, config, runtime: AgentToolRuntimeConfig::default() }
    }

    /// Set whether to skip summarization.
    pub fn skip_summarization(mut self, skip: bool) -> Self {
        self.config.skip_summarization = skip;
        self
    }

    /// Set whether to forward artifacts.
    pub fn forward_artifacts(mut self, forward: bool) -> Self {
        self.config.forward_artifacts = forward;
        self
    }

    /// Set timeout for sub-agent execution.
    ///
    /// The parent agent applies this instead of its own tool timeout. Without
    /// one, the delegation runs until it finishes or the run is cancelled.
    ///
    /// A delegation that runs past it is a failed call with an
    /// [`ErrorCategory::Timeout`](adk_core::ErrorCategory::Timeout) error in every
    /// [`AgentToolFailureMode`]: `tool.timeout` from a calling agent that enforces the
    /// limit, or `tool.agent.timeout` when the tool is executed directly.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_tool::AgentTool;
    /// use std::time::Duration;
    ///
    /// let research = AgentTool::new(researcher).timeout(Duration::from_secs(20 * 60));
    /// ```
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.config.timeout = Some(timeout);
        self
    }

    /// Set custom input schema.
    pub fn input_schema(mut self, schema: Value) -> Self {
        self.config.input_schema = Some(schema);
        self
    }

    /// Set custom output schema.
    pub fn output_schema(mut self, schema: Value) -> Self {
        self.config.output_schema = Some(schema);
        self
    }

    /// Choose which parent session data is snapshotted into the isolated child session.
    pub fn session_snapshot(mut self, snapshot: AgentToolSessionSnapshot) -> Self {
        self.runtime.session_snapshot = snapshot;
        self
    }

    /// Set whether the wrapped agent can access the parent's memory service.
    pub fn forward_memory(mut self, forward: bool) -> Self {
        self.runtime.forward_memory = forward;
        self
    }

    /// Set whether the wrapped agent can access the parent's parallel shared state.
    ///
    /// This defaults to `true` to preserve the historical AgentTool behavior.
    pub fn forward_shared_state(mut self, forward: bool) -> Self {
        self.runtime.forward_shared_state = forward;
        self
    }

    /// Set whether child events are emitted through the parent tool context.
    ///
    /// The default is `false` for backward compatibility. State and artifact
    /// deltas are merged regardless of this setting.
    ///
    /// Forwarded events carry the tool's branch, `{caller branch}.{tool}@{call id}`
    /// ([`adk_core::history::tool_branch`]), so observers can follow the delegate's progress
    /// while the caller's history leaves it out: the caller's model gets the delegate's answer
    /// once, as the tool's response.
    pub fn forward_events(mut self, forward: bool) -> Self {
        self.runtime.forward_events = forward;
        self
    }

    /// Choose whether delegated failures are returned as JSON or propagated.
    pub fn failure_mode(mut self, mode: AgentToolFailureMode) -> Self {
        self.runtime.failure_mode = mode;
        self
    }

    /// Set whether a delegate's error message is included in the result it fails with.
    ///
    /// By default ([`AgentToolFailureMode::ReturnErrorObject`]) a failed delegation returns
    /// `{"error": "agent execution failed: <component>.<category> (<code>)", "agent": ...}`
    /// ([`AdkError::redacted`](adk_core::AdkError::redacted)), because the message can quote
    /// credentials, internal hosts, or upstream bodies, and the result is sent to the calling
    /// agent's model provider and stored in its session. An error with code
    /// [`TOOL_INVALID_ARGUMENTS_CODE`](adk_core::TOOL_INVALID_ARGUMENTS_CODE) keeps its message.
    /// This matches `LlmAgentBuilder::expose_tool_error_messages`. In
    /// [`AgentToolFailureMode::Propagate`] the calling agent applies its own setting.
    ///
    /// Enable this only when every error the delegate can fail with is safe to send to the
    /// provider.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use adk_core::Agent;
    /// use adk_tool::AgentTool;
    ///
    /// fn billing_tool(billing_agent: Arc<dyn Agent>) -> AgentTool {
    ///     AgentTool::new(billing_agent).expose_error_messages(true)
    /// }
    /// ```
    pub fn expose_error_messages(mut self, expose: bool) -> Self {
        self.runtime.expose_error_messages = expose;
        self
    }

    /// Set whether delegated failures should be propagated as tool errors.
    pub fn propagate_failures(self, propagate: bool) -> Self {
        self.failure_mode(if propagate {
            AgentToolFailureMode::Propagate
        } else {
            AgentToolFailureMode::ReturnErrorObject
        })
    }

    /// Bound the number of nested agent-as-tool delegations.
    pub fn max_delegation_depth(mut self, max_depth: u32) -> Self {
        self.runtime.max_delegation_depth = Some(max_depth);
        self
    }

    /// Sets the principal the wrapped agent acts as when it is delegated to.
    ///
    /// Without one, the delegate acts as
    /// [`AgentPrincipal::derived`](adk_core::AgentPrincipal::derived) from the app name and
    /// the wrapped agent's name, in the caller's tenant. Either way it acts for the
    /// caller's subject, and the caller is appended to its delegation chain.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_core::{AgentId, AgentPrincipal};
    /// use adk_tool::AgentTool;
    ///
    /// let tool = AgentTool::new(researcher)
    ///     .agent_principal(AgentPrincipal::new(AgentId::new("spiffe://acme/agent/researcher")?));
    /// ```
    pub fn agent_principal(mut self, principal: adk_core::AgentPrincipal) -> Self {
        self.runtime.principal = Some(principal);
        self
    }

    /// Reject child handoff events that cannot be executed by `AgentTool` itself.
    pub fn reject_child_handoffs(mut self, reject: bool) -> Self {
        self.runtime.reject_child_handoffs = reject;
        self
    }

    /// Executes child handoffs using the supplied exact target registry.
    ///
    /// Disabled by default for backward compatibility. When enabled, a child
    /// transfer remains inside the nested invocation until the terminal member
    /// returns, after which its result is returned to the original caller.
    pub fn execute_child_handoffs(
        mut self,
        agents: impl IntoIterator<Item = Arc<dyn Agent>>,
    ) -> Self {
        self.runtime.execute_child_handoffs = true;
        self.runtime.reject_child_handoffs = false;
        self.runtime.handoff_agents =
            Arc::new(agents.into_iter().map(|agent| (agent.name().to_string(), agent)).collect());
        self
    }

    /// Limits the parent history copied into a delegated session.
    ///
    /// A value of zero copies no history while still allowing a filtered state
    /// snapshot when parent-session forwarding is enabled.
    pub fn history_max_events(mut self, max_events: usize) -> Self {
        self.runtime.history_max_events = Some(max_events);
        self
    }

    /// Copies only these exact state keys into a delegated session.
    pub fn state_keys(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.runtime.state_keys = Some(Arc::new(keys.into_iter().map(Into::into).collect()));
        self
    }

    /// Allows delegated state writes only to these exact keys.
    pub fn output_state_keys(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.runtime.output_state_keys = Some(Arc::new(keys.into_iter().map(Into::into).collect()));
        self
    }

    /// Selects how child state writes merge with concurrent parent updates.
    pub fn state_merge_policy(mut self, policy: AgentToolStateMergePolicy) -> Self {
        self.runtime.state_merge_policy = policy;
        self
    }

    /// Exempts trusted framework-owned keys from concurrent merge checks.
    ///
    /// Output allowlists still apply. This is intended for runtime bookkeeping
    /// that is deliberately updated by both the parent and delegated context.
    pub fn state_merge_exempt_keys(
        mut self,
        keys: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.runtime.state_merge_exempt_keys = Arc::new(keys.into_iter().map(Into::into).collect());
        self
    }

    /// Allows artifact writes only when their names start with one of these prefixes.
    pub fn artifact_prefixes(
        mut self,
        prefixes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.runtime.artifact_prefixes =
            Some(Arc::new(prefixes.into_iter().map(Into::into).collect()));
        self
    }

    fn failure(&self, message: String) -> Result<Value> {
        match self.runtime.failure_mode {
            AgentToolFailureMode::ReturnErrorObject => Ok(json!({
                "error": message,
                "agent": self.agent.name()
            })),
            AgentToolFailureMode::Propagate => Err(adk_core::AdkError::tool(message)),
        }
    }

    fn state_policy_failure(&self, message: String) -> Result<Value> {
        match self.runtime.failure_mode {
            AgentToolFailureMode::ReturnErrorObject => Ok(json!({
                "error": message,
                "agent": self.agent.name(),
                "code": "tool.agent.state_policy_violation"
            })),
            AgentToolFailureMode::Propagate => Err(adk_core::AdkError::new(
                adk_core::ErrorComponent::Tool,
                adk_core::ErrorCategory::InvalidInput,
                "tool.agent.state_policy_violation",
                message,
            )),
        }
    }

    /// Generate the default parameters schema for this agent tool.
    fn default_parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "request": {
                    "type": "string",
                    "description": format!("The request to send to the {} agent", self.agent.name())
                }
            },
            "required": ["request"]
        })
    }

    /// Extract the request text from the tool arguments.
    fn extract_request(&self, args: &Value) -> String {
        // Try to get "request" field first
        if let Some(request) = args.get("request").and_then(|v| v.as_str()) {
            return request.to_string();
        }

        // If custom schema, try to serialize the whole args
        if self.config.input_schema.is_some() {
            return serde_json::to_string(args).unwrap_or_default();
        }

        // Fallback: convert args to string
        match args {
            Value::String(s) => s.clone(),
            Value::Object(map) => {
                // Try to find any string field
                for value in map.values() {
                    if let Value::String(s) = value {
                        return s.clone();
                    }
                }
                serde_json::to_string(args).unwrap_or_default()
            }
            _ => serde_json::to_string(args).unwrap_or_default(),
        }
    }

    /// Extracts the delegation's output from its events.
    ///
    /// The output is the text of the final response, as `{"response": text}`. Results of
    /// the delegate's provider-side tools, such as Gemini code execution, are added as
    /// `"server_tool_responses"`: the code's output is often the answer, and the model's
    /// text may not repeat it. A delegate that ends on a tool result, as one whose tool
    /// skips summarization does, hands back that result: the payload of a single function
    /// response, or `{"responses": [{"name", "response"}, ...]}` for several.
    fn extract_response(events: &[Event]) -> Value {
        let mut output = Self::final_output(events);
        let server_tool_responses = Self::server_tool_responses(events);
        if !server_tool_responses.is_empty()
            && let Some(map) = output.as_object_mut()
            && map.len() == 1
            && map.contains_key("response")
        {
            // The code's output is the answer; no placeholder text beside it.
            if map["response"] == NO_RESPONSE {
                map.insert("response".to_string(), json!(""));
            }
            map.insert("server_tool_responses".to_string(), Value::Array(server_tool_responses));
        }
        output
    }

    /// The `ServerToolResponse` payloads of the delegation, each once.
    ///
    /// A streamed turn delivers each part in a partial event and may repeat it in the
    /// aggregated final event, so complete events are read when they carry any.
    fn server_tool_responses(events: &[Event]) -> Vec<Value> {
        let collect = |partial: bool| -> Vec<Value> {
            events
                .iter()
                .filter(|event| event.llm_response.partial == partial)
                .flat_map(|event| event.llm_response.content.iter())
                .flat_map(|content| content.parts.iter())
                .filter_map(|part| match part {
                    Part::ServerToolResponse { server_tool_response } => {
                        Some(server_tool_response.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        let complete = collect(false);
        if complete.is_empty() { collect(true) } else { complete }
    }

    fn final_output(events: &[Event]) -> Value {
        let function_responses = |event: &Event| {
            let responses: Vec<_> = event
                .llm_response
                .content
                .iter()
                .flat_map(|content| content.parts.iter())
                .filter_map(|part| match part {
                    Part::FunctionResponse { function_response, .. } => Some(function_response),
                    _ => None,
                })
                .collect();
            match responses.as_slice() {
                [] => None,
                [response] => Some(response.response.clone()),
                responses => Some(json!({
                    "responses": responses
                        .iter()
                        .map(|response| json!({
                            "name": response.name,
                            "response": response.response,
                        }))
                        .collect::<Vec<_>>()
                })),
            }
        };

        // An `output_key` write or another state-only event follows the answer, so only
        // events that carry content are read.
        let carries_content = |event: &&Event| {
            event.llm_response.content.as_ref().is_some_and(|content| !content.parts.is_empty())
        };
        if let Some(event) =
            events.iter().rev().filter(carries_content).find(|event| event.is_final_response())
        {
            let text: String = event
                .llm_response
                .content
                .iter()
                .flat_map(|content| content.parts.iter())
                .filter_map(|part| match part {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let has_text =
                event.llm_response.content.iter().any(|content| {
                    content.parts.iter().any(|part| matches!(part, Part::Text { .. }))
                });
            if has_text {
                return json!({ "response": text });
            }
            if let Some(payload) = function_responses(event) {
                return payload;
            }
        }

        if let Some(last_event) = events.iter().rev().find(carries_content) {
            if let Some(text) = last_event
                .llm_response
                .content
                .iter()
                .flat_map(|content| content.parts.iter())
                .find_map(|part| match part {
                    Part::Text { text } => Some(text),
                    _ => None,
                })
            {
                return json!({ "response": text });
            }
            if let Some(payload) = function_responses(last_event) {
                return payload;
            }
        }
        json!({ "response": NO_RESPONSE })
    }

    fn project_parent_history(history: Vec<Content>, max_events: Option<usize>) -> Vec<Content> {
        let mut pending_calls = HashMap::<String, usize>::new();
        let mut projected = Vec::with_capacity(history.len());
        let mut balanced_boundaries = vec![0];
        let mut open_group_start = None;

        for content in history {
            let was_pending = !pending_calls.is_empty();
            let has_function_call =
                content.parts.iter().any(|part| matches!(part, Part::FunctionCall { .. }));
            let has_function_response =
                content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }));

            // Progress from a delegated agent may be emitted while its caller's
            // tool call is still open. It is useful to stream to observers, but
            // it is not valid provider history between that call and response.
            if was_pending && !has_function_call && !has_function_response {
                continue;
            }
            if !was_pending && has_function_response && !has_function_call {
                continue;
            }

            if !was_pending && has_function_call {
                open_group_start = Some(projected.len());
            }
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, id, .. } => {
                        let key = id.as_ref().unwrap_or(name);
                        *pending_calls.entry(key.clone()).or_default() += 1;
                    }
                    Part::FunctionResponse { function_response, id, .. } => {
                        let key = id.as_ref().unwrap_or(&function_response.name);
                        if let Some(count) = pending_calls.get_mut(key) {
                            *count -= 1;
                            if *count == 0 {
                                pending_calls.remove(key);
                            }
                        }
                    }
                    _ => {}
                }
            }
            projected.push(content);
            if pending_calls.is_empty() {
                open_group_start = None;
                balanced_boundaries.push(projected.len());
            }
        }

        // ToolContext execution occurs before the caller's FunctionResponse is
        // added to its session. Never expose that currently-open call (or any
        // trailing content after it) as child model history.
        if let Some(group_start) = open_group_start {
            projected.truncate(group_start);
        }
        let balanced_end = projected.len();

        if let Some(max_events) = max_events {
            let desired_start = balanced_end.saturating_sub(max_events);
            let balanced_start = balanced_boundaries
                .into_iter()
                .find(|boundary| *boundary >= desired_start)
                .unwrap_or(balanced_end);
            projected.drain(..balanced_start);
        }

        projected
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        self.agent.name()
    }

    fn description(&self) -> &str {
        self.agent.description()
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.config.input_schema.clone().unwrap_or_else(|| self.default_parameters_schema()))
    }

    fn response_schema(&self) -> Option<Value> {
        self.config.output_schema.clone()
    }

    fn is_long_running(&self) -> bool {
        // Agent execution could take time, but we wait for completion
        false
    }

    fn is_agent_delegation(&self) -> bool {
        true
    }

    // The delegation's own timeout replaces the parent's generic tool timeout.
    fn timeout_override(&self) -> Option<Option<Duration>> {
        Some(self.config.timeout)
    }

    #[adk_telemetry::instrument(
        skip(self, ctx, args),
        fields(
            agent_tool.name = %self.agent.name(),
            agent_tool.description = %self.agent.description(),
            function_call.id = %ctx.function_call_id()
        )
    )]
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        adk_telemetry::debug!("Executing agent tool: {}", self.agent.name());

        let parent_run_config = ctx.run_config().cloned().unwrap_or_default();
        // A decision the caller gave this call by its ID was given for the request this call
        // surfaced when its delegate held a call; it is handed to that call below.
        let call_decision =
            parent_run_config.tool_confirmation_decisions.get(ctx.function_call_id()).copied().map(
                |decision| {
                    let bound = parent_run_config
                        .tool_confirmation_fingerprints
                        .get(ctx.function_call_id())
                        .cloned();
                    (decision, bound)
                },
            );
        let mut child_run_config = delegated_run_config(parent_run_config);
        self.agent.configure_run(self.agent.name(), &mut child_run_config);
        if let Some(targets) = self.agent.transfer_targets_for(self.agent.name()) {
            child_run_config.transfer_targets = targets;
        }
        let caller = ctx.acting_context();
        let delegate = self.runtime.principal.clone().unwrap_or_else(|| {
            let mut derived = adk_core::AgentPrincipal::derived(ctx.app_name(), self.agent.name());
            derived.tenant = caller.agent.tenant.clone();
            derived
        });
        // The delegate acts for the caller's subject, with the caller appended to the chain;
        // the subject was admitted when the caller's run started.
        let delegated = caller.delegate(delegate);
        child_run_config.acting.principals.insert(self.agent.name().to_string(), delegated.agent);
        child_run_config.acting.on_behalf_of = delegated.on_behalf_of;
        child_run_config.acting.delegation_chain = delegated.delegation_chain;
        let child_depth = ctx.delegation_depth().saturating_add(1);
        let max_depth = self.runtime.max_delegation_depth.or(ctx.max_delegation_depth());
        if max_depth.is_some_and(|max| child_depth > max) {
            return self.failure(format!(
                "agent delegation depth {child_depth} exceeds the configured maximum of {}",
                max_depth.unwrap_or_default()
            ));
        }
        if ctx.is_cancelled() {
            return self.failure("agent delegation was cancelled before execution".to_string());
        }

        // Extract the request from args
        let request_text = self.extract_request(&args);
        let parent_state_baseline =
            ctx.session().map(|session| session.state().all()).unwrap_or_default();
        let user_content = Content::new("user").with_text(&request_text);

        // A delegation that held a call in an earlier run resumes from where it stopped, so the
        // held call runs as the same call once it has a decision.
        let held_key = format!(
            "{HELD_DELEGATION_STATE_PREFIX}{}",
            adk_core::json_digest(&json!([ctx.agent_name(), self.name(), ctx.function_call_id()]))
        );
        let call_fingerprint = adk_core::tool_call_fingerprint(self.name(), &args);
        let resumed = ctx
            .session()
            .and_then(|session| session.state().get(&held_key))
            .and_then(|value| serde_json::from_value::<HeldDelegation>(value).ok())
            .filter(|held| held.call == call_fingerprint)
            .and_then(|held| {
                let agent = if held.agent == self.agent.name() {
                    Some(self.agent.clone())
                } else {
                    self.runtime.handoff_agents.get(&held.agent).cloned()
                };
                agent.map(|agent| (agent, held))
            });

        let resumed_holder = resumed.as_ref().map(|(_, held)| held.agent.clone());
        let (start_agent, session, child_user_content, origin_user_content) = match resumed {
            Some((agent, held)) => {
                // Whatever this run ends with replaces the record.
                let mut actions = ctx.actions();
                actions.state_delta.insert(held_key.clone(), Value::Null);
                ctx.set_actions(actions);
                // With several held requests under this one call, a call-ID decision reaches
                // only the request its fingerprint binding names.
                if let Some((decision, bound)) = call_decision {
                    let single = held.more.is_empty();
                    for request in std::iter::once(&held.request).chain(&held.more) {
                        let Some(held_call_id) = request.function_call_id.clone() else {
                            continue;
                        };
                        let fingerprint = request.fingerprint();
                        let applies = match &bound {
                            Some(bound) => *bound == fingerprint,
                            None => single,
                        };
                        if applies {
                            child_run_config
                                .tool_confirmation_decisions
                                .insert(held_call_id.clone(), decision);
                            child_run_config
                                .tool_confirmation_fingerprints
                                .insert(held_call_id, fingerprint);
                        }
                    }
                }
                tracing::info!(
                    agent_tool.name = %self.agent.name(),
                    agent.name = %held.agent,
                    tool.name = %held.request.tool_name,
                    "resuming a delegation that held a call for confirmation"
                );
                let session = AgentToolSession::resume(
                    ctx.session_id().to_string(),
                    ctx.app_name().to_string(),
                    ctx.user_id().to_string(),
                    &held,
                );
                // A run that resumes held calls carries no new user input.
                (agent, session, Content::new("user"), held.user_content)
            }
            None => {
                let (mut state, history) = match (self.runtime.session_snapshot, ctx.session()) {
                    (AgentToolSessionSnapshot::Parent, Some(session)) => {
                        (session.state().all(), session.conversation_history())
                    }
                    _ => (HashMap::new(), Vec::new()),
                };
                state.retain(|key, _| {
                    !key.starts_with(HELD_DELEGATION_STATE_PREFIX)
                        && self.runtime.state_keys.as_ref().is_none_or(|keys| keys.contains(key))
                });
                let session = AgentToolSession::new(
                    ctx.session_id().to_string(),
                    ctx.app_name().to_string(),
                    ctx.user_id().to_string(),
                    state,
                    AgentTool::project_parent_history(history, self.runtime.history_max_events),
                );
                (self.agent.clone(), session, user_content.clone(), user_content)
            }
        };

        // Create an isolated context for the sub-agent
        let sub_ctx = Arc::new(AgentToolInvocationContext::new(
            ctx.clone(),
            self.agent.clone(),
            child_user_content,
            AgentToolChildConfig {
                forward_artifacts: self.config.forward_artifacts,
                artifact_prefixes: self.runtime.artifact_prefixes.clone(),
                forward_memory: self.runtime.forward_memory,
                forward_shared_state: self.runtime.forward_shared_state,
                session: Arc::new(session),
                run_config: child_run_config,
                delegation_depth: child_depth,
                max_delegation_depth: max_depth,
                orchestration_root_invocation_id: ctx
                    .orchestration_root_invocation_id()
                    .to_string(),
                orchestration_edge_id: ctx.orchestration_edge_id().map_or_else(
                    || ctx.function_call_id().to_string(),
                    |parent| format!("{parent}/{}", ctx.function_call_id()),
                ),
            },
        ));

        // Execute the sub-agent
        let execution = async {
            // A delegation held by one of its handoff targets resumes in that target.
            let mut active_ctx = if Arc::ptr_eq(&start_agent, &self.agent) {
                sub_ctx.clone()
            } else {
                Arc::new(sub_ctx.for_agent(start_agent.clone()))
            };
            let mut active_agent = start_agent;
            let mut transfer_depth = 0_u32;
            let mut events = Vec::new();
            let mut state_delta = HashMap::new();
            let mut artifact_delta = HashMap::new();
            // The agent whose run ended on the latest hold; the delegation resumes in it.
            let mut holder = None;

            loop {
                let mut event_stream = active_agent.run(active_ctx.clone()).await?;
                let mut transfer = None;
                while let Some(result) = event_stream.next().await {
                    match result {
                        Ok(event) => {
                            if let Some(target) = &event.actions.transfer_to_agent {
                                if self.runtime.reject_child_handoffs {
                                    return Err(adk_core::AdkError::tool(format!(
                                        "agent '{}' requested handoff to '{target}', but AgentTool cannot execute child handoffs in this configuration",
                                        active_agent.name()
                                    )));
                                }
                                if self.runtime.execute_child_handoffs {
                                    transfer = Some(target.clone());
                                }
                            }
                            if event.actions.tool_confirmation.is_some() {
                                holder = Some(active_agent.name().to_string());
                            }
                            // A delegation's own resume records are not the caller's state.
                            state_delta.extend(
                                event
                                    .actions
                                    .state_delta
                                    .iter()
                                    .filter(|(key, _)| {
                                        !key.starts_with(HELD_DELEGATION_STATE_PREFIX)
                                    })
                                    .map(|(key, value)| (key.clone(), value.clone())),
                            );
                            artifact_delta.extend(event.actions.artifact_delta.clone());
                            sub_ctx.session.apply_event(&event);
                            if self.runtime.forward_events {
                                let mut forwarded = event.clone();
                                if self.runtime.execute_child_handoffs && transfer.is_some() {
                                    // The AgentTool consumes this control-flow edge internally.
                                    // Do not let the parent Runner execute the same handoff again.
                                    forwarded.actions.transfer_to_agent = None;
                                }
                                // The caller surfaces the delegate's hold as its own call's.
                                forwarded.actions.tool_confirmation = None;
                                // On the tool's own branch the caller's history leaves the
                                // delegate's turns out; its model gets the tool's response.
                                let tool_branch = adk_core::history::tool_branch(
                                    ctx.branch(),
                                    self.name(),
                                    ctx.function_call_id(),
                                );
                                forwarded.branch = if forwarded.branch.is_empty() {
                                    tool_branch
                                } else {
                                    format!("{tool_branch}.{}", forwarded.branch)
                                };
                                ctx.emit_event(forwarded).await;
                            }
                            events.push(event);
                            if transfer.is_some() {
                                break;
                            }
                        }
                        Err(error) => {
                            // The message can quote credentials or upstream bodies.
                            adk_telemetry::error!(
                                error = %error.diagnostics(),
                                "sub-agent execution failed"
                            );
                            return Err(error);
                        }
                    }
                }

                let Some(target_name) = transfer else {
                    break;
                };
                transfer_depth = transfer_depth.saturating_add(1);
                let max_transfer_depth = active_ctx.run_config.max_transfer_depth.unwrap_or(10);
                if transfer_depth > max_transfer_depth {
                    return Err(adk_core::AdkError::tool(format!(
                        "nested handoff depth {transfer_depth} exceeds the configured maximum of {max_transfer_depth}"
                    )));
                }
                let target = self
                    .runtime
                    .handoff_agents
                    .get(&target_name)
                    .cloned()
                    .ok_or_else(|| {
                        adk_core::AdkError::tool(format!(
                            "agent '{}' requested nested handoff to unregistered target '{target_name}'",
                            active_agent.name()
                        ))
                    })?;
                active_ctx = Arc::new(active_ctx.for_agent(target.clone()));
                active_agent = target;
            }

            Ok((events, state_delta, artifact_delta, holder))
        };

        // A delegation that runs out of time is a failed call, so retries, error hooks, the
        // circuit breaker and the action ledger all see it as one. A caller that enforces the
        // limit itself through `timeout_override` reaches it first and reports its own timeout,
        // which treats a delegation cut mid-flight as one whose effects are unknown.
        let result = if let Some(timeout_duration) = self.config.timeout {
            match tokio::time::timeout(timeout_duration + CALLER_TIMEOUT_GRACE, execution).await {
                Ok(r) => r,
                Err(_) => {
                    return Err(adk_core::AdkError::new(
                        adk_core::ErrorComponent::Tool,
                        adk_core::ErrorCategory::Timeout,
                        "tool.agent.timeout",
                        format!(
                            "agent '{}' execution timed out after {timeout_duration:?}",
                            self.agent.name()
                        ),
                    ));
                }
            }
        } else {
            execution.await
        };

        match result {
            Ok((events, mut state_delta, artifact_delta, holder)) => {
                // Every call the delegate still holds, including one an earlier run held
                // that this run's decisions did not reach, keeps the delegation held.
                let held_requests = delegate_held_requests(&sub_ctx.session.events());
                let hold = (!held_requests.is_empty()).then(|| {
                    let agent =
                        holder.or(resumed_holder).unwrap_or_else(|| self.agent.name().to_string());
                    (agent, held_requests)
                });
                if let Some(allowed) = &self.runtime.output_state_keys {
                    if let Some(key) = state_delta.keys().find(|key| !allowed.contains(*key)) {
                        return self.state_policy_failure(format!(
                            "agent '{}' attempted unauthorized state write to '{key}'",
                            self.agent.name()
                        ));
                    }
                    state_delta.retain(|key, _| allowed.contains(key));
                }
                if self.runtime.state_merge_policy == AgentToolStateMergePolicy::RejectConflicts
                    && let Some(parent_session) = ctx.session()
                    && let Some(key) = state_delta.keys().find(|key| {
                        !self.runtime.state_merge_exempt_keys.contains(*key)
                            && parent_session.state().get(key)
                                != parent_state_baseline.get(*key).cloned()
                    })
                {
                    return self.state_policy_failure(format!(
                        "agent '{}' state write to '{key}' conflicts with a concurrent parent update",
                        self.agent.name()
                    ));
                }
                if let Some(prefixes) = &self.runtime.artifact_prefixes
                    && let Some(name) = artifact_delta
                        .keys()
                        .find(|name| !prefixes.iter().any(|prefix| name.starts_with(prefix)))
                {
                    return self.state_policy_failure(format!(
                        "agent '{}' attempted unauthorized artifact write to '{name}'",
                        self.agent.name()
                    ));
                }
                // A held delegation has no output yet, so it does not end the caller's turn.
                let skip_summarization = self.config.skip_summarization && hold.is_none();
                // Forward state_delta and artifact_delta to parent context
                if !state_delta.is_empty() || !artifact_delta.is_empty() || skip_summarization {
                    let mut parent_actions = ctx.actions();
                    parent_actions.state_delta.extend(state_delta);
                    parent_actions.artifact_delta.extend(artifact_delta);
                    parent_actions.skip_summarization |= skip_summarization;
                    ctx.set_actions(parent_actions);
                }

                // The delegate held calls for confirmation: this call is held with them, under
                // each of the delegate's requests, and the delegation is recorded so it resumes.
                if let Some((agent, mut requests)) = hold {
                    let request = requests.remove(0);
                    let more = requests;
                    let rekeyed = |request: &ToolConfirmationRequest| ToolConfirmationRequest {
                        function_call_id: Some(ctx.function_call_id().to_string()),
                        ..request.clone()
                    };
                    let reason = match more.len() {
                        0 => format!(
                            "agent '{agent}' is waiting for a decision on tool '{}'",
                            request.tool_name
                        ),
                        others => format!(
                            "agent '{agent}' is waiting for decisions on {} tool calls",
                            others + 1
                        ),
                    };
                    // The caller ends its run on the last request; the others are surfaced
                    // before it, in the order the delegate held them (spec 3.1 §10 F4).
                    let mut surfaced: Vec<ToolConfirmationRequest> =
                        std::iter::once(&request).chain(&more).map(rekeyed).collect();
                    let last = surfaced.pop().expect("the delegate holds at least one call");
                    for request in surfaced {
                        ctx.emit_event(surfaced_confirmation_event(ctx.as_ref(), request)).await;
                    }
                    let record = HeldDelegation {
                        call: call_fingerprint,
                        agent,
                        request,
                        more,
                        state: sub_ctx.session.initial_state.clone(),
                        history: sub_ctx.session.initial_history.clone(),
                        user_content: origin_user_content,
                        events: sub_ctx.session.events(),
                    };
                    let record = serde_json::to_value(record).map_err(|error| {
                        adk_core::AdkError::new(
                            adk_core::ErrorComponent::Tool,
                            adk_core::ErrorCategory::Internal,
                            "tool.agent.hold_not_recorded",
                            format!("the held delegation could not be recorded: {error}"),
                        )
                    })?;
                    let mut parent_actions = ctx.actions();
                    parent_actions.state_delta.insert(held_key, record);
                    parent_actions.tool_confirmation = Some(last);
                    ctx.set_actions(parent_actions);
                    tracing::info!(
                        agent_tool.name = %self.agent.name(),
                        confirmation.reason = %reason,
                        "delegated agent held a call for confirmation"
                    );
                    return Ok(adk_core::confirmation_pending_response(reason));
                }

                // Extract and return the response
                let response = Self::extract_response(&events);

                adk_telemetry::debug!(
                    "Agent tool {} completed with {} events",
                    self.agent.name(),
                    events.len()
                );

                Ok(response)
            }
            Err(error) => match self.runtime.failure_mode {
                // The result goes to the caller's model provider and session, so the delegate's
                // message stays out of it unless exposed, as `LlmAgent` treats tool errors.
                AgentToolFailureMode::ReturnErrorObject => {
                    let failure = if self.runtime.expose_error_messages
                        || error.code == adk_core::TOOL_INVALID_ARGUMENTS_CODE
                    {
                        error.to_string()
                    } else {
                        error.redacted()
                    };
                    Ok(json!({
                        "error": format!("agent execution failed: {failure}"),
                        "agent": self.agent.name()
                    }))
                }
                // The delegate's error keeps its category and code, so the caller can tell a
                // timeout or rate limit from an internal failure.
                AgentToolFailureMode::Propagate => {
                    let mut error = error;
                    error.message = format!(
                        "agent '{}' execution failed: {}",
                        self.agent.name(),
                        error.message
                    );
                    Err(error)
                }
            },
        }
    }
}

// Internal context for sub-agent execution
struct AgentToolInvocationContext {
    parent_ctx: Arc<dyn ToolContext>,
    agent: Arc<dyn Agent>,
    user_content: Content,
    invocation_id: String,
    ended: Arc<AtomicBool>,
    forward_artifacts: bool,
    artifact_prefixes: Option<Arc<Vec<String>>>,
    forward_memory: bool,
    forward_shared_state: bool,
    session: Arc<AgentToolSession>,
    run_config: RunConfig,
    delegation_depth: u32,
    max_delegation_depth: Option<u32>,
    orchestration_root_invocation_id: String,
    orchestration_edge_id: String,
}

impl AgentToolInvocationContext {
    fn new(
        parent_ctx: Arc<dyn ToolContext>,
        agent: Arc<dyn Agent>,
        user_content: Content,
        child_config: AgentToolChildConfig,
    ) -> Self {
        let AgentToolChildConfig {
            forward_artifacts,
            artifact_prefixes,
            forward_memory,
            forward_shared_state,
            session,
            run_config,
            delegation_depth,
            max_delegation_depth,
            orchestration_root_invocation_id,
            orchestration_edge_id,
        } = child_config;
        Self {
            session,
            parent_ctx,
            agent,
            user_content,
            invocation_id: format!("agent-tool-{}", uuid::Uuid::new_v4()),
            ended: Arc::new(AtomicBool::new(false)),
            forward_artifacts,
            artifact_prefixes,
            forward_memory,
            forward_shared_state,
            run_config,
            delegation_depth,
            max_delegation_depth,
            orchestration_root_invocation_id,
            orchestration_edge_id,
        }
    }

    fn for_agent(&self, agent: Arc<dyn Agent>) -> Self {
        let mut run_config = self.run_config.clone();
        run_config.transfer_targets = Vec::new();
        run_config.parent_agent = None;
        agent.configure_run(agent.name(), &mut run_config);
        if let Some(targets) = agent.transfer_targets_for(agent.name()) {
            run_config.transfer_targets = targets;
        }
        Self {
            parent_ctx: self.parent_ctx.clone(),
            agent,
            user_content: self.user_content.clone(),
            invocation_id: self.invocation_id.clone(),
            ended: self.ended.clone(),
            forward_artifacts: self.forward_artifacts,
            artifact_prefixes: self.artifact_prefixes.clone(),
            forward_memory: self.forward_memory,
            forward_shared_state: self.forward_shared_state,
            session: self.session.clone(),
            run_config,
            delegation_depth: self.delegation_depth,
            max_delegation_depth: self.max_delegation_depth,
            orchestration_root_invocation_id: self.orchestration_root_invocation_id.clone(),
            orchestration_edge_id: self.orchestration_edge_id.clone(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for AgentToolInvocationContext {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    fn agent_name(&self) -> &str {
        self.agent.name()
    }

    fn user_id(&self) -> &str {
        self.parent_ctx.user_id()
    }

    fn app_name(&self) -> &str {
        self.parent_ctx.app_name()
    }

    // The delegate runs inside the caller's session: its history and state are its own, but
    // its approvals, policy requests and audit records belong to the caller's session.
    fn session_id(&self) -> &str {
        self.session.id()
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    fn acting_context(&self) -> adk_core::ActingContext {
        self.run_config.acting_context_for(self.app_name(), self.agent_name(), self.user_id())
    }

    fn is_authenticated(&self) -> bool {
        self.parent_ctx.is_authenticated()
    }
}

#[async_trait]
impl CallbackContext for AgentToolInvocationContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        if !self.forward_artifacts {
            return None;
        }
        let artifacts = self.parent_ctx.artifacts()?;
        self.artifact_prefixes.as_ref().map_or(Some(artifacts.clone()), |prefixes| {
            Some(Arc::new(AgentToolArtifacts {
                inner: artifacts,
                allowed_write_prefixes: prefixes.clone(),
            }) as Arc<dyn Artifacts>)
        })
    }

    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.forward_shared_state.then(|| self.parent_ctx.shared_state()).flatten()
    }
}

struct AgentToolArtifacts {
    inner: Arc<dyn Artifacts>,
    allowed_write_prefixes: Arc<Vec<String>>,
}

#[async_trait]
impl Artifacts for AgentToolArtifacts {
    async fn save(&self, name: &str, data: &Part) -> Result<i64> {
        if !self.allowed_write_prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            return Err(adk_core::AdkError::new(
                adk_core::ErrorComponent::Artifact,
                adk_core::ErrorCategory::Forbidden,
                "artifact.agent_tool.write_denied",
                format!("delegated artifact write to '{name}' is outside the allowed prefixes"),
            ));
        }
        self.inner.save(name, data).await
    }

    async fn load(&self, name: &str) -> Result<Part> {
        self.inner.load(name).await
    }

    async fn list(&self) -> Result<Vec<String>> {
        self.inner.list().await
    }
}

#[async_trait]
impl InvocationContext for AgentToolInvocationContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.forward_memory.then(|| self.parent_ctx.memory()).flatten()
    }

    fn session(&self) -> &dyn Session {
        self.session.as_ref()
    }

    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }

    fn end_invocation(&self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn is_cancelled(&self) -> bool {
        self.parent_ctx.is_cancelled()
    }

    /// Authenticated scopes are forwarded so a scope-guarded tool used by the
    /// wrapped agent sees the caller's grants rather than an empty set.
    fn user_scopes(&self) -> Vec<String> {
        self.parent_ctx.user_scopes()
    }

    fn request_metadata(&self) -> HashMap<String, Value> {
        self.parent_ctx.request_metadata()
    }

    fn delegation_depth(&self) -> u32 {
        self.delegation_depth
    }

    fn max_delegation_depth(&self) -> Option<u32> {
        self.max_delegation_depth
    }

    fn orchestration_root_invocation_id(&self) -> &str {
        &self.orchestration_root_invocation_id
    }

    fn orchestration_edge_id(&self) -> Option<&str> {
        Some(&self.orchestration_edge_id)
    }

    /// Secret access is forwarded so the wrapped agent's tools can resolve
    /// secrets through the same provider as the calling agent.
    async fn get_secret(&self, name: &str) -> adk_core::Result<Option<String>> {
        self.parent_ctx.get_secret(name).await
    }

    async fn get_secret_for(
        &self,
        request: &adk_core::SecretRequest,
    ) -> adk_core::Result<Option<String>> {
        // The parent here is a `ToolContext`, which carries no identity of its own, so
        // only the stated purpose survives the hop. An agent invoked as a tool
        // therefore presents that agent's identity rather than the inner tool's.
        match &request.purpose {
            Some(purpose) => self.parent_ctx.get_secret_for_purpose(&request.name, purpose).await,
            None => self.parent_ctx.get_secret(&request.name).await,
        }
    }

    // Cancellation and request metadata are forwarded above through the
    // backward-compatible ToolContext capability methods.
}

// Minimal session for sub-agent execution
struct AgentToolSession {
    id: String,
    app_name: String,
    user_id: String,
    state: std::sync::RwLock<HashMap<String, Value>>,
    history: std::sync::RwLock<Vec<Content>>,
    /// The state and history the delegation started with, kept so a held delegation can be
    /// rebuilt.
    initial_state: HashMap<String, Value>,
    initial_history: Vec<Content>,
    /// The delegated agents' complete events, which their held calls are read from.
    events: std::sync::RwLock<Vec<Event>>,
}

impl AgentToolSession {
    fn new(
        id: String,
        app_name: String,
        user_id: String,
        state: HashMap<String, Value>,
        history: Vec<Content>,
    ) -> Self {
        Self {
            id,
            app_name,
            user_id,
            state: std::sync::RwLock::new(state.clone()),
            history: std::sync::RwLock::new(history.clone()),
            initial_state: state,
            initial_history: history,
            events: std::sync::RwLock::new(Vec::new()),
        }
    }

    /// Rebuilds the session of a delegation that held a call.
    ///
    /// The history is the one the delegate's model saw, ending on an empty user turn, as a
    /// session does when a run resumes held calls.
    fn resume(id: String, app_name: String, user_id: String, held: &HeldDelegation) -> Self {
        let mut history = held.history.clone();
        // The delegate was given the request as its current user turn.
        match history.iter().rposition(|content| content.role == "user") {
            Some(index) => history[index] = held.user_content.clone(),
            None => history.push(held.user_content.clone()),
        }
        let session = Self {
            id,
            app_name,
            user_id,
            state: std::sync::RwLock::new(held.state.clone()),
            history: std::sync::RwLock::new(history),
            initial_state: held.state.clone(),
            initial_history: held.history.clone(),
            events: std::sync::RwLock::new(Vec::new()),
        };
        for event in &held.events {
            session.apply_event(event);
        }
        if let Ok(mut history) = session.history.write() {
            history.push(Content::new("user"));
        }
        session
    }

    fn apply_event(&self, event: &Event) {
        if !event.actions.state_delta.is_empty()
            && let Ok(mut state) = self.state.write()
        {
            for (key, value) in &event.actions.state_delta {
                if adk_core::validate_state_key(key).is_ok() {
                    state.insert(key.clone(), value.clone());
                }
            }
        }
        // A resumed call's response takes its pending response's place.
        if let Some(content) = &event.llm_response.content
            && let Ok(mut history) = self.history.write()
            && let Some(rest) = adk_core::supersede_pending_responses(&mut history, content.clone())
        {
            history.push(rest);
        }
        if !event.llm_response.partial
            && let Ok(mut events) = self.events.write()
        {
            // Provider debug copies are not needed to resume, and would be stored again.
            let mut kept = event.clone();
            kept.provider_metadata.clear();
            kept.llm_request = None;
            events.push(kept);
        }
    }

    fn events(&self) -> Vec<Event> {
        self.events.read().map(|events| events.clone()).unwrap_or_default()
    }
}

impl Session for AgentToolSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn conversation_history(&self) -> Vec<Content> {
        self.history.read().map(|history| history.clone()).unwrap_or_default()
    }

    fn held_tool_calls(&self, agent_name: &str, branch: &str) -> Vec<adk_core::HeldToolCall> {
        self.events
            .read()
            .map(|events| adk_core::held_tool_calls(&events, agent_name, branch))
            .unwrap_or_default()
    }

    fn workflow_resume_positions(
        &self,
        workflow: &str,
        branch: &str,
    ) -> Vec<adk_core::WorkflowPosition> {
        self.events
            .read()
            .map(|events| adk_core::workflow_resume_positions(&events, workflow, branch))
            .unwrap_or_default()
    }
}

impl State for AgentToolSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.read().ok()?.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        if let Err(msg) = adk_core::validate_state_key(&key) {
            tracing::warn!(key = %key, "rejecting invalid state key: {msg}");
            return;
        }
        if let Ok(mut state) = self.state.write() {
            state.insert(key, value);
        }
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.read().ok().map(|s| s.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{EventActions, MemoryEntry, StreamingMode};
    use std::sync::Mutex;

    struct MockAgent {
        name: String,
        description: String,
    }

    #[async_trait]
    impl Agent for MockAgent {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            &self.description
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            use async_stream::stream;

            let name = self.name.clone();
            let s = stream! {
                let mut event = Event::new("mock-inv");
                event.author = name;
                event.llm_response.content = Some(Content::new("model").with_text("Mock response"));
                yield Ok(event);
            };

            Ok(Box::pin(s))
        }
    }

    #[test]
    fn test_agent_tool_creation() {
        let agent = Arc::new(MockAgent {
            name: "test_agent".to_string(),
            description: "A test agent".to_string(),
        });

        let tool = AgentTool::new(agent);
        assert_eq!(tool.name(), "test_agent");
        assert_eq!(tool.description(), "A test agent");
    }

    #[test]
    fn test_agent_tool_config() {
        let agent =
            Arc::new(MockAgent { name: "test".to_string(), description: "test".to_string() });

        let tool = AgentTool::new(agent)
            .skip_summarization(true)
            .forward_artifacts(false)
            .timeout(Duration::from_secs(30));

        assert!(tool.config.skip_summarization);
        assert!(!tool.config.forward_artifacts);
        assert_eq!(tool.config.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_parameters_schema() {
        let agent = Arc::new(MockAgent {
            name: "calculator".to_string(),
            description: "Performs calculations".to_string(),
        });

        let tool = AgentTool::new(agent);
        let schema = tool.parameters_schema().unwrap();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["request"].is_object());
    }

    #[test]
    fn test_extract_request() {
        let agent =
            Arc::new(MockAgent { name: "test".to_string(), description: "test".to_string() });

        let tool = AgentTool::new(agent);

        // Test with request field
        let args = json!({"request": "solve 2+2"});
        assert_eq!(tool.extract_request(&args), "solve 2+2");

        // Test with string value
        let args = json!("direct request");
        assert_eq!(tool.extract_request(&args), "direct request");
    }

    #[test]
    fn test_extract_response() {
        let mut event = Event::new("inv-123");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![
                Part::Text { text: "The answer ".to_string() },
                Part::Text { text: "is 4".to_string() },
            ],
        });

        let events = vec![event];
        let response = AgentTool::extract_response(&events);

        assert_eq!(response["response"], "The answer is 4");
    }

    #[test]
    fn parent_history_projection_keeps_only_complete_tool_exchanges() {
        let history = vec![
            Content::new("user").with_text("first request"),
            Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "completed_tool".to_string(),
                    args: json!({}),
                    id: Some("call-complete".to_string()),
                    thought_signature: None,
                }],
            },
            Content::new("model").with_text("forwarded child progress"),
            Content {
                role: "function".to_string(),
                parts: vec![Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new(
                        "completed_tool",
                        json!({"ok": true}),
                    ),
                    id: Some("call-complete".to_string()),
                    annotations: None,
                }],
            },
            Content::new("model").with_text("completed result"),
            Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "delegated_agent".to_string(),
                    args: json!({"request": "current request"}),
                    id: Some("call-open".to_string()),
                    thought_signature: None,
                }],
            },
        ];

        let projected = AgentTool::project_parent_history(history.clone(), None);
        assert_eq!(projected.len(), 4);
        assert_eq!(projected.last().expect("projected history").role, "model");
        assert!(projected.iter().all(|content| {
            content.parts.iter().all(
                |part| !matches!(part, Part::Text { text } if text == "forwarded child progress"),
            )
        }));

        let bounded = AgentTool::project_parent_history(history, Some(2));
        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0].parts, vec![Part::Text { text: "completed result".to_string() }]);
    }

    struct TestSession {
        state: std::sync::RwLock<HashMap<String, Value>>,
        history: Vec<Content>,
    }

    impl State for TestSession {
        fn get(&self, key: &str) -> Option<Value> {
            self.state.read().ok()?.get(key).cloned()
        }

        fn set(&mut self, key: String, value: Value) {
            self.state.get_mut().expect("state lock").insert(key, value);
        }

        fn all(&self) -> HashMap<String, Value> {
            self.state.read().expect("state lock").clone()
        }
    }

    impl Session for TestSession {
        fn id(&self) -> &str {
            "parent-session"
        }

        fn app_name(&self) -> &str {
            "parent-app"
        }

        fn user_id(&self) -> &str {
            "parent-user"
        }

        fn state(&self) -> &dyn State {
            self
        }

        fn conversation_history(&self) -> Vec<Content> {
            self.history.clone()
        }
    }

    struct TestMemory;

    #[async_trait]
    impl Memory for TestMemory {
        async fn search(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
    }

    struct TestToolContext {
        actions: Mutex<EventActions>,
        session: TestSession,
        memory: Arc<dyn Memory>,
        run_config: RunConfig,
        cancelled: bool,
        shared_state: Arc<adk_core::SharedState>,
        emitted_events: Mutex<Vec<Event>>,
        delegation_depth: u32,
        max_delegation_depth: Option<u32>,
        authenticated: bool,
    }

    impl TestToolContext {
        fn new() -> Self {
            Self {
                actions: Mutex::new(EventActions::default()),
                session: TestSession {
                    state: std::sync::RwLock::new(HashMap::from([(
                        "parent-key".to_string(),
                        json!("parent-value"),
                    )])),
                    history: vec![Content::new("user").with_text("parent history")],
                },
                memory: Arc::new(TestMemory),
                run_config: RunConfig::default(),
                cancelled: false,
                shared_state: Arc::new(adk_core::SharedState::new()),
                emitted_events: Mutex::new(Vec::new()),
                delegation_depth: 0,
                max_delegation_depth: None,
                authenticated: false,
            }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestToolContext {
        fn invocation_id(&self) -> &str {
            "parent-invocation"
        }
        fn agent_name(&self) -> &str {
            "parent-agent"
        }
        fn user_id(&self) -> &str {
            "parent-user"
        }
        fn app_name(&self) -> &str {
            "parent-app"
        }
        fn session_id(&self) -> &str {
            "parent-session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.session.history[0]
        }
        fn is_authenticated(&self) -> bool {
            self.authenticated
        }
    }

    #[async_trait]
    impl CallbackContext for TestToolContext {
        fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
            None
        }

        fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
            Some(self.shared_state.clone())
        }
    }

    #[async_trait]
    impl ToolContext for TestToolContext {
        fn function_call_id(&self) -> &str {
            "call-1"
        }
        fn actions(&self) -> EventActions {
            self.actions.lock().expect("actions lock").clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().expect("actions lock") = actions;
        }
        async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>> {
            self.memory.search(query).await
        }
        fn memory(&self) -> Option<Arc<dyn Memory>> {
            Some(self.memory.clone())
        }
        fn session(&self) -> Option<&dyn Session> {
            Some(&self.session)
        }
        fn run_config(&self) -> Option<&RunConfig> {
            Some(&self.run_config)
        }
        fn is_cancelled(&self) -> bool {
            self.cancelled
        }
        fn request_metadata(&self) -> HashMap<String, Value> {
            HashMap::from([("request-id".to_string(), json!("req-1"))])
        }
        fn delegation_depth(&self) -> u32 {
            self.delegation_depth
        }
        fn max_delegation_depth(&self) -> Option<u32> {
            self.max_delegation_depth
        }
        async fn emit_event(&self, event: Event) {
            self.emitted_events.lock().expect("event lock").push(event);
        }
    }

    struct ContextProbeAgent;

    #[async_trait]
    impl Agent for ContextProbeAgent {
        fn name(&self) -> &str {
            "probe"
        }

        fn description(&self) -> &str {
            "records delegated context behavior"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            use async_stream::stream;
            let stream = stream! {
                assert_eq!(ctx.session_id(), ctx.session().id());
                assert_eq!(ctx.app_name(), ctx.session().app_name());
                assert_eq!(ctx.user_id(), ctx.session().user_id());
                assert_eq!(ctx.session().state().get("parent-key"), Some(json!("parent-value")));
                assert_eq!(ctx.session().conversation_history().len(), 1);
                assert!(ctx.memory().is_some());
                assert!(ctx.shared_state().is_some());
                assert!(!ctx.is_cancelled());
                assert_eq!(ctx.request_metadata().get("request-id"), Some(&json!("req-1")));
                assert_eq!(ctx.run_config().streaming_mode, StreamingMode::None);
                assert_eq!(ctx.delegation_depth(), 3);
                assert_eq!(ctx.max_delegation_depth(), Some(4));

                let mut first = Event::new(ctx.invocation_id());
                first.author = "probe".to_string();
                first.actions.state_delta.insert("child-key".to_string(), json!(42));
                first.actions.artifact_delta.insert("report.txt".to_string(), 2);
                first.llm_response.content = Some(Content::new("model").with_text("first"));
                yield Ok(first);

                assert_eq!(ctx.session().state().get("child-key"), Some(json!(42)));
                assert_eq!(ctx.session().conversation_history().len(), 2);
                let mut final_event = Event::new(ctx.invocation_id());
                final_event.author = "probe".to_string();
                final_event.llm_response.content = Some(Content::new("model").with_text("done"));
                yield Ok(final_event);
            };
            Ok(Box::pin(stream))
        }
    }

    #[tokio::test]
    async fn forwards_snapshot_runtime_context_and_applies_child_events_immediately() {
        let mut parent = TestToolContext::new();
        parent.delegation_depth = 2;
        parent.max_delegation_depth = Some(4);
        let parent = Arc::new(parent);
        let tool = AgentTool::new(Arc::new(ContextProbeAgent))
            .session_snapshot(AgentToolSessionSnapshot::Parent)
            .forward_memory(true)
            .forward_events(true)
            .skip_summarization(true);

        let response = tool.execute(parent.clone(), json!({"request": "inspect"})).await.unwrap();

        assert_eq!(response["response"], "done");
        let actions = parent.actions();
        assert_eq!(actions.state_delta.get("child-key"), Some(&json!(42)));
        assert_eq!(actions.artifact_delta.get("report.txt"), Some(&2));
        assert!(actions.skip_summarization);
        assert_eq!(parent.session.state().get("child-key"), None);
        assert_eq!(parent.emitted_events.lock().expect("event lock").len(), 2);
    }

    /// Records whether the delegate's context reports an authenticated identity.
    struct AuthenticationProbe {
        seen: Arc<Mutex<Option<bool>>>,
    }

    #[async_trait]
    impl Agent for AuthenticationProbe {
        fn name(&self) -> &str {
            "auth_probe"
        }

        fn description(&self) -> &str {
            "records whether its caller was authenticated"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            *self.seen.lock().expect("seen lock") = Some(ctx.is_authenticated());
            let mut event = Event::new(ctx.invocation_id());
            event.author = "auth_probe".to_string();
            event.llm_response.content = Some(Content::new("model").with_text("done"));
            Ok(Box::pin(futures::stream::once(async { Ok(event) })))
        }
    }

    #[tokio::test]
    async fn a_delegate_sees_whether_its_caller_was_authenticated() {
        for authenticated in [true, false] {
            let seen = Arc::new(Mutex::new(None));
            let mut parent = TestToolContext::new();
            parent.authenticated = authenticated;
            let tool = AgentTool::new(Arc::new(AuthenticationProbe { seen: seen.clone() }));
            tool.execute(Arc::new(parent), json!({"request": "go"})).await.unwrap();
            assert_eq!(*seen.lock().expect("seen lock"), Some(authenticated));
        }
    }

    struct WritePolicyAgent;

    #[async_trait]
    impl Agent for WritePolicyAgent {
        fn name(&self) -> &str {
            "write_policy"
        }

        fn description(&self) -> &str {
            "emits state and artifact writes"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            let mut event = Event::new(ctx.invocation_id());
            event.actions.state_delta.insert("child-key".to_string(), json!(42));
            event.actions.artifact_delta.insert("report.txt".to_string(), 2);
            event.llm_response.content = Some(Content::new("model").with_text("done"));
            Ok(Box::pin(futures::stream::once(async { Ok(event) })))
        }
    }

    #[tokio::test]
    async fn enforces_delegated_state_and_artifact_write_allowlists() {
        let state_tool = AgentTool::new(Arc::new(WritePolicyAgent))
            .session_snapshot(AgentToolSessionSnapshot::Parent)
            .output_state_keys(["allowed-key"])
            .propagate_failures(true);
        let error = state_tool
            .execute(Arc::new(TestToolContext::new()), json!({"request": "inspect"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unauthorized state write to 'child-key'"));

        let artifact_tool = AgentTool::new(Arc::new(WritePolicyAgent))
            .session_snapshot(AgentToolSessionSnapshot::Parent)
            .output_state_keys(["child-key"])
            .artifact_prefixes(["team/"])
            .propagate_failures(true);
        let error = artifact_tool
            .execute(Arc::new(TestToolContext::new()), json!({"request": "inspect"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unauthorized artifact write to 'report.txt'"));
    }

    struct ConflictingStateAgent {
        parent: Arc<TestToolContext>,
    }

    #[async_trait]
    impl Agent for ConflictingStateAgent {
        fn name(&self) -> &str {
            "conflicting_state"
        }

        fn description(&self) -> &str {
            "changes parent state while returning a child write"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            self.parent
                .session
                .state
                .write()
                .expect("parent state lock")
                .insert("parent-key".to_string(), json!("concurrent-update"));
            let mut event = Event::new(ctx.invocation_id());
            event.actions.state_delta.insert("parent-key".to_string(), json!("child-update"));
            event.llm_response.content = Some(Content::new("model").with_text("done"));
            Ok(Box::pin(futures::stream::once(async { Ok(event) })))
        }
    }

    #[tokio::test]
    async fn rejects_delegated_state_merge_conflicts_transactionally() {
        let parent = Arc::new(TestToolContext::new());
        let tool = AgentTool::new(Arc::new(ConflictingStateAgent { parent: parent.clone() }))
            .session_snapshot(AgentToolSessionSnapshot::Parent)
            .state_merge_policy(AgentToolStateMergePolicy::RejectConflicts)
            .propagate_failures(true);
        let error = tool.execute(parent.clone(), json!({"request": "write"})).await.unwrap_err();
        assert!(error.to_string().contains("conflicts with a concurrent parent update"));
        assert!(parent.actions().state_delta.is_empty());
    }

    #[tokio::test]
    async fn permits_explicit_framework_state_merge_exemptions() {
        let parent = Arc::new(TestToolContext::new());
        let tool = AgentTool::new(Arc::new(ConflictingStateAgent { parent: parent.clone() }))
            .session_snapshot(AgentToolSessionSnapshot::Parent)
            .state_merge_policy(AgentToolStateMergePolicy::RejectConflicts)
            .state_merge_exempt_keys(["parent-key"])
            .propagate_failures(true);

        tool.execute(parent.clone(), json!({"request": "write"})).await.unwrap();
        assert_eq!(parent.actions().state_delta.get("parent-key"), Some(&json!("child-update")));
    }

    struct TestArtifacts;

    #[async_trait]
    impl Artifacts for TestArtifacts {
        async fn save(&self, _name: &str, _data: &Part) -> Result<i64> {
            Ok(1)
        }

        async fn load(&self, _name: &str) -> Result<Part> {
            Ok(Part::Text { text: "artifact".to_string() })
        }

        async fn list(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn rejects_artifact_writes_at_the_storage_boundary() {
        let artifacts = AgentToolArtifacts {
            inner: Arc::new(TestArtifacts),
            allowed_write_prefixes: Arc::new(vec!["research/".to_string()]),
        };
        let data = Part::Text { text: "data".to_string() };
        let error = artifacts.save("private/report.txt", &data).await.unwrap_err();
        assert_eq!(error.code, "artifact.agent_tool.write_denied");
        assert_eq!(artifacts.save("research/report.txt", &data).await.unwrap(), 1);
    }

    struct FailingAgent;

    #[async_trait]
    impl Agent for FailingAgent {
        fn name(&self) -> &str {
            "failing"
        }
        fn description(&self) -> &str {
            "always fails"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            Err(adk_core::AdkError::agent("planned failure"))
        }
    }

    #[tokio::test]
    async fn preserves_legacy_failure_object_and_supports_propagation() {
        let legacy = AgentTool::new(Arc::new(FailingAgent));
        let value = legacy
            .execute(Arc::new(TestToolContext::new()), json!({"request": "fail"}))
            .await
            .unwrap();
        assert_eq!(
            value,
            json!({
                "error": format!(
                    "agent execution failed: {}",
                    adk_core::AdkError::agent("planned failure").redacted()
                ),
                "agent": "failing",
            })
        );

        let exposed = AgentTool::new(Arc::new(FailingAgent)).expose_error_messages(true);
        let value = exposed
            .execute(Arc::new(TestToolContext::new()), json!({"request": "fail"}))
            .await
            .unwrap();
        assert!(value["error"].as_str().unwrap().contains("planned failure"));

        let strict = AgentTool::new(Arc::new(FailingAgent)).propagate_failures(true);
        let error = strict
            .execute(Arc::new(TestToolContext::new()), json!({"request": "fail"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("planned failure"));
    }

    #[tokio::test]
    async fn enforces_inherited_and_tool_specific_delegation_depth() {
        let mut inherited_ctx = TestToolContext::new();
        inherited_ctx.delegation_depth = 2;
        inherited_ctx.max_delegation_depth = Some(2);
        let tool = AgentTool::new(Arc::new(MockAgent {
            name: "child".to_string(),
            description: "child".to_string(),
        }))
        .propagate_failures(true);
        let error = tool
            .execute(Arc::new(inherited_ctx), json!({"request": "too deep"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("depth 3"));

        let local_limit = AgentTool::new(Arc::new(MockAgent {
            name: "child".to_string(),
            description: "child".to_string(),
        }))
        .max_delegation_depth(0)
        .propagate_failures(true);
        assert!(
            local_limit
                .execute(Arc::new(TestToolContext::new()), json!({"request": "too deep"}))
                .await
                .is_err()
        );
    }

    struct HandoffAgent;

    #[async_trait]
    impl Agent for HandoffAgent {
        fn name(&self) -> &str {
            "handoff"
        }
        fn description(&self) -> &str {
            "requests a handoff"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            let mut event = Event::new(ctx.invocation_id());
            event.actions.transfer_to_agent = Some("other".to_string());
            Ok(Box::pin(futures::stream::iter([Ok(event)])))
        }
    }

    #[tokio::test]
    async fn strict_mode_rejects_unconsumed_child_handoff() {
        let tool = AgentTool::new(Arc::new(HandoffAgent))
            .reject_child_handoffs(true)
            .propagate_failures(true);
        let error = tool
            .execute(Arc::new(TestToolContext::new()), json!({"request": "handoff"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot execute child handoffs"));
    }

    #[tokio::test]
    async fn executes_registered_child_handoff_and_returns_final_result() {
        let target = Arc::new(MockAgent {
            name: "other".to_string(),
            description: "handoff target".to_string(),
        }) as Arc<dyn Agent>;
        let tool = AgentTool::new(Arc::new(HandoffAgent))
            .execute_child_handoffs([target])
            .forward_events(true)
            .propagate_failures(true);
        let parent = Arc::new(TestToolContext::new());
        let response = tool.execute(parent.clone(), json!({"request": "handoff"})).await.unwrap();
        assert_eq!(response["response"], "Mock response");
        let forwarded = parent.emitted_events.lock().expect("event lock");
        assert_eq!(forwarded.len(), 2);
        assert!(forwarded[0].actions.transfer_to_agent.is_none());
        assert_eq!(forwarded[1].author, "other");
    }

    #[tokio::test]
    async fn cancellation_is_observed_before_child_execution() {
        let mut ctx = TestToolContext::new();
        ctx.cancelled = true;
        let tool = AgentTool::new(Arc::new(MockAgent {
            name: "child".to_string(),
            description: "child".to_string(),
        }))
        .propagate_failures(true);
        let error = tool.execute(Arc::new(ctx), json!({"request": "go"})).await.unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }

    struct IsolatedProbeAgent;

    #[async_trait]
    impl Agent for IsolatedProbeAgent {
        fn name(&self) -> &str {
            "isolated"
        }
        fn description(&self) -> &str {
            "checks legacy isolation defaults"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            assert_eq!(ctx.session_id(), ctx.session().id());
            assert_eq!(ctx.app_name(), ctx.session().app_name());
            assert_eq!(ctx.user_id(), ctx.session().user_id());
            assert!(ctx.session().state().all().is_empty());
            assert!(ctx.session().conversation_history().is_empty());
            assert!(ctx.memory().is_none());
            let mut event = Event::new(ctx.invocation_id());
            event.llm_response.content = Some(Content::new("model").with_text("isolated"));
            Ok(Box::pin(futures::stream::iter([Ok(event)])))
        }
    }

    #[tokio::test]
    async fn default_keeps_each_child_session_isolated() {
        let tool = AgentTool::new(Arc::new(IsolatedProbeAgent));
        let parent = Arc::new(TestToolContext::new());
        let response = tool.execute(parent.clone(), json!({"request": "inspect"})).await.unwrap();
        assert_eq!(response["response"], "isolated");
        assert!(parent.emitted_events.lock().expect("event lock").is_empty());
    }

    struct ProjectionProbeAgent;

    #[async_trait]
    impl Agent for ProjectionProbeAgent {
        fn name(&self) -> &str {
            "projection"
        }

        fn description(&self) -> &str {
            "checks explicit history and state projections"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            assert!(ctx.session().conversation_history().is_empty());
            assert!(ctx.session().state().all().is_empty());
            let mut event = Event::new(ctx.invocation_id());
            event.llm_response.content = Some(Content::new("model").with_text("projected"));
            Ok(Box::pin(futures::stream::once(async { Ok(event) })))
        }
    }

    #[tokio::test]
    async fn applies_exact_history_and_state_projection() {
        let tool = AgentTool::new(Arc::new(ProjectionProbeAgent))
            .session_snapshot(AgentToolSessionSnapshot::Parent)
            .history_max_events(0)
            .state_keys(["not-present"])
            .propagate_failures(true);
        let response = tool
            .execute(Arc::new(TestToolContext::new()), json!({"request": "inspect"}))
            .await
            .unwrap();
        assert_eq!(response["response"], "projected");
    }

    struct StateWithoutHistoryProbe;

    #[async_trait]
    impl Agent for StateWithoutHistoryProbe {
        fn name(&self) -> &str {
            "state_without_history"
        }

        fn description(&self) -> &str {
            "checks independent state and history projections"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            assert!(ctx.session().conversation_history().is_empty());
            assert_eq!(ctx.session().state().get("parent-key"), Some(json!("parent-value")));
            let mut event = Event::new(ctx.invocation_id());
            event.llm_response.content = Some(Content::new("model").with_text("independent"));
            Ok(Box::pin(futures::stream::once(async { Ok(event) })))
        }
    }

    #[tokio::test]
    async fn projects_state_independently_from_history() {
        let tool = AgentTool::new(Arc::new(StateWithoutHistoryProbe))
            .session_snapshot(AgentToolSessionSnapshot::Parent)
            .history_max_events(0)
            .state_keys(["parent-key"])
            .propagate_failures(true);
        let response = tool
            .execute(Arc::new(TestToolContext::new()), json!({"request": "inspect"}))
            .await
            .unwrap();
        assert_eq!(response["response"], "independent");
    }

    struct PendingAgent;

    #[async_trait]
    impl Agent for PendingAgent {
        fn name(&self) -> &str {
            "pending"
        }
        fn description(&self) -> &str {
            "never completes"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            Ok(Box::pin(futures::stream::pending()))
        }
    }

    #[tokio::test]
    async fn a_timeout_is_a_timeout_error_in_every_failure_mode() {
        for propagate in [false, true] {
            let tool = AgentTool::new(Arc::new(PendingAgent))
                .timeout(Duration::from_millis(1))
                .propagate_failures(propagate);
            let error = tool
                .execute(Arc::new(TestToolContext::new()), json!({"request": "wait"}))
                .await
                .unwrap_err();
            assert_eq!(error.category, adk_core::ErrorCategory::Timeout);
            assert_eq!(error.code, "tool.agent.timeout");
            assert!(error.message.contains("timed out after 1ms"), "{}", error.message);
        }
    }

    struct RateLimitedAgent;

    #[async_trait]
    impl Agent for RateLimitedAgent {
        fn name(&self) -> &str {
            "rate_limited"
        }
        fn description(&self) -> &str {
            "fails with a rate limit"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            Err(adk_core::AdkError::new(
                adk_core::ErrorComponent::Model,
                adk_core::ErrorCategory::RateLimited,
                "model.test.rate_limited",
                "slow down",
            ))
        }
    }

    #[tokio::test]
    async fn propagated_failures_keep_the_delegates_category_and_code() {
        let error = AgentTool::new(Arc::new(RateLimitedAgent))
            .propagate_failures(true)
            .execute(Arc::new(TestToolContext::new()), json!({"request": "go"}))
            .await
            .unwrap_err();
        assert_eq!(error.category, adk_core::ErrorCategory::RateLimited);
        assert_eq!(error.code, "model.test.rate_limited");
        assert_eq!(error.message, "agent 'rate_limited' execution failed: slow down");
    }

    fn function_response_event(responses: &[(&str, Value)], skip: bool) -> Event {
        let mut event = Event::new("inv");
        event.actions.skip_summarization = skip;
        event.llm_response.content = Some(Content {
            role: "function".to_string(),
            parts: responses
                .iter()
                .map(|(name, response)| Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new(*name, response.clone()),
                    id: Some(format!("{name}-id")),
                    annotations: None,
                })
                .collect(),
        });
        event
    }

    #[test]
    fn the_answer_is_read_past_a_trailing_state_only_event() {
        let mut answer = Event::new("inv");
        answer.llm_response.content = Some(Content::new("model").with_text("Nairobi"));
        // The `output_key` write `LlmAgent` emits after its answer.
        let mut state = Event::new("inv");
        state.actions.state_delta.insert("capital".to_string(), json!("Nairobi"));

        assert_eq!(
            AgentTool::extract_response(&[answer.clone(), state.clone()]),
            json!({ "response": "Nairobi" })
        );

        let mut partial = answer;
        partial.llm_response.partial = true;
        assert_eq!(
            AgentTool::extract_response(&[partial, state]),
            json!({ "response": "Nairobi" })
        );
    }

    #[test]
    fn code_execution_results_are_part_of_the_delegations_output() {
        let result = json!({"codeExecutionResult": {"outcome": "OUTCOME_OK", "output": "5117\n"}});
        let mut answer = Event::new("inv");
        answer.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![
                Part::ServerToolCall {
                    server_tool_call: json!({"executableCode": {"language": "PYTHON", "code": "print(5117)"}}),
                },
                Part::ServerToolResponse { server_tool_response: result.clone() },
                Part::Text { text: "The sum is 5117.".to_string() },
            ],
        });
        let mut streamed = answer.clone();
        streamed.llm_response.partial = true;

        assert_eq!(
            AgentTool::extract_response(&[streamed.clone(), answer]),
            json!({"response": "The sum is 5117.", "server_tool_responses": [result.clone()]})
        );

        // Only the code ran; no final text followed.
        let mut code_only = Event::new("inv");
        code_only.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::ServerToolResponse { server_tool_response: result.clone() }],
        });
        assert_eq!(
            AgentTool::extract_response(&[code_only]),
            json!({"response": "", "server_tool_responses": [result]})
        );
    }

    #[test]
    fn a_delegate_that_ends_on_a_tool_result_hands_back_its_payload() {
        let single = function_response_event(&[("compute", json!({"answer": 42}))], true);
        assert_eq!(AgentTool::extract_response(&[single]), json!({"answer": 42}));

        let several = function_response_event(
            &[("first", json!({"a": 1})), ("second", json!({"b": 2}))],
            true,
        );
        assert_eq!(
            AgentTool::extract_response(&[several]),
            json!({"responses": [
                {"name": "first", "response": {"a": 1}},
                {"name": "second", "response": {"b": 2}},
            ]})
        );

        // A delegate ending on an escalating tool result is not a final response; its
        // result is still the delegation's output.
        let escalated = function_response_event(&[("stop", json!({"done": true}))], false);
        assert_eq!(AgentTool::extract_response(&[escalated]), json!({"done": true}));

        let mut empty_text = Event::new("inv");
        empty_text.llm_response.content = Some(Content::new("model").with_text(""));
        assert_eq!(AgentTool::extract_response(&[empty_text]), json!({"response": ""}));
        assert_eq!(AgentTool::extract_response(&[]), json!({"response": "No response from agent"}));
    }

    struct RunConfigProbe {
        seen: Arc<Mutex<Option<(RunConfig, String)>>>,
    }

    #[async_trait]
    impl Agent for RunConfigProbe {
        fn name(&self) -> &str {
            "probe"
        }
        fn description(&self) -> &str {
            "records the delegated run config"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            *self.seen.lock().unwrap() =
                Some((ctx.run_config().clone(), ctx.session_id().to_string()));
            let mut event = Event::new(ctx.invocation_id());
            event.llm_response.content = Some(Content::new("model").with_text("done"));
            Ok(Box::pin(futures::stream::iter([Ok(event)])))
        }
    }

    #[tokio::test]
    async fn a_delegate_gets_the_shared_run_config_without_its_callers_agent_scoped_fields() {
        let tracker = Arc::new(adk_core::BudgetTracker::new(adk_core::RunBudget::default()));
        let mut parent = TestToolContext::new();
        parent.run_config = RunConfig {
            transfer_targets: vec!["coordinator".to_string(), "peer".to_string()],
            parent_agent: Some("coordinator".to_string()),
            cached_content: Some("cachedContents/coordinator".to_string()),
            runtime_toolsets: vec![adk_core::RuntimeToolset::new(Arc::new(
                crate::BasicToolset::new("client", Vec::new()),
            ))],
            tool_confirmation_decisions: HashMap::from([(
                "call-1".to_string(),
                adk_core::ToolConfirmationDecision::Approve,
            )]),
            tool_confirmation_fingerprints: HashMap::from([(
                "call-1".to_string(),
                "fingerprint".to_string(),
            )]),
            tool_approvals: HashMap::from([(
                "fingerprint".to_string(),
                adk_core::ToolApproval::approve(),
            )]),
            budget_tracker: Some(tracker.clone()),
            durable_run_id: Some("run-1".to_string()),
            max_transfer_depth: Some(3),
            record_payloads: true,
            ..RunConfig::default()
        };
        let seen = Arc::new(Mutex::new(None));
        AgentTool::new(Arc::new(RunConfigProbe { seen: seen.clone() }))
            .execute(Arc::new(parent), json!({"request": "inspect"}))
            .await
            .unwrap();

        let (config, session_id) = seen.lock().unwrap().take().expect("the delegate ran");
        assert!(config.transfer_targets.is_empty());
        assert_eq!(config.parent_agent, None);
        assert_eq!(config.cached_content, None);
        assert!(config.runtime_toolsets.is_empty());
        assert!(config.tool_confirmation_decisions.is_empty());
        assert!(config.tool_confirmation_fingerprints.is_empty());
        assert_eq!(config.streaming_mode, StreamingMode::None);
        assert!(config.tool_approvals.contains_key("fingerprint"));
        assert!(config.budget_tracker.is_some_and(|shared| Arc::ptr_eq(&shared, &tracker)));
        assert_eq!(config.durable_run_id.as_deref(), Some("run-1"));
        assert_eq!(config.max_transfer_depth, Some(3));
        assert!(config.record_payloads);
        // Approvals, policy requests and audit records of the delegate belong to the
        // caller's session.
        assert_eq!(session_id, "parent-session");
    }

    /// What a resumed delegate saw: its held calls, its user turn, and its run config.
    type Resumed = (Vec<adk_core::HeldToolCall>, Content, RunConfig);

    /// Holds a call on its first run; on the run that resumes, reports what it resumed.
    struct HoldingAgent {
        resumed: Arc<Mutex<Option<Resumed>>>,
    }

    #[async_trait]
    impl Agent for HoldingAgent {
        fn name(&self) -> &str {
            "holder"
        }
        fn description(&self) -> &str {
            "holds a call for confirmation"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            let held = ctx.session().held_tool_calls("holder", ctx.branch());
            if !held.is_empty() {
                *self.resumed.lock().unwrap() =
                    Some((held, ctx.user_content().clone(), ctx.run_config().clone()));
                // The resumed call is answered, as `LlmAgent` answers it.
                let mut answered = Event::new(ctx.invocation_id());
                answered.author = "holder".to_string();
                answered.llm_response.content = Some(Content {
                    role: "function".to_string(),
                    parts: vec![Part::FunctionResponse {
                        function_response: adk_core::FunctionResponseData::new(
                            "delete_file",
                            json!({"deleted": true}),
                        ),
                        id: Some("inner-1".to_string()),
                        annotations: None,
                    }],
                });
                let mut done = Event::new(ctx.invocation_id());
                done.author = "holder".to_string();
                done.llm_response.content = Some(Content::new("model").with_text("deleted"));
                return Ok(Box::pin(futures::stream::iter([Ok(answered), Ok(done)])));
            }
            let args = json!({"path": "a.txt"});
            let mut issued = Event::new(ctx.invocation_id());
            issued.author = "holder".to_string();
            issued.llm_response.content = Some(Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "delete_file".to_string(),
                    args: args.clone(),
                    id: Some("inner-1".to_string()),
                    thought_signature: None,
                }],
            });
            let mut pending = Event::new(ctx.invocation_id());
            pending.author = "holder".to_string();
            pending.llm_response.content = Some(Content {
                role: "function".to_string(),
                parts: vec![Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new(
                        "delete_file",
                        adk_core::confirmation_pending_response("held"),
                    ),
                    id: Some("inner-1".to_string()),
                    annotations: None,
                }],
            });
            let mut hold = Event::new(ctx.invocation_id());
            hold.author = "holder".to_string();
            hold.llm_response.content =
                Some(Content::new("model").with_text("confirmation required"));
            hold.actions.tool_confirmation = Some(
                ToolConfirmationRequest::new("delete_file", args)
                    .with_function_call_id("inner-1")
                    .with_acting_context(ctx.acting_context()),
            );
            Ok(Box::pin(futures::stream::iter([Ok(issued), Ok(pending), Ok(hold)])))
        }
    }

    #[tokio::test]
    async fn a_delegates_hold_holds_the_call_and_resumes_where_it_stopped() {
        let resumed = Arc::new(Mutex::new(None));
        let tool = AgentTool::new(Arc::new(HoldingAgent { resumed: resumed.clone() }))
            .forward_events(true)
            .skip_summarization(true);
        let args = json!({"request": "delete a.txt"});

        let first = Arc::new(TestToolContext::new());
        let response = tool.execute(first.clone(), args.clone()).await.unwrap();
        assert!(adk_core::is_confirmation_pending(&response), "{response}");
        let actions = first.actions();
        assert!(!actions.skip_summarization, "a held delegation does not end the turn");
        let surfaced = actions.tool_confirmation.expect("the call is held with the delegate");
        assert_eq!(surfaced.tool_name, "delete_file");
        assert_eq!(surfaced.args, json!({"path": "a.txt"}));
        assert_eq!(surfaced.function_call_id.as_deref(), Some("call-1"));
        assert_eq!(
            surfaced.acting.as_ref().map(|acting| acting.agent.id.as_str()),
            Some("urn:adk:agent:parent-app/holder")
        );
        let (key, record) = actions
            .state_delta
            .iter()
            .find(|(key, _)| key.starts_with(HELD_DELEGATION_STATE_PREFIX))
            .expect("the held delegation is recorded");
        assert!(
            first
                .emitted_events
                .lock()
                .unwrap()
                .iter()
                .all(|event| event.actions.tool_confirmation.is_none()),
            "the delegate's own request is not forwarded"
        );

        // The next run carries the record in session state and a decision for the call.
        let mut second = TestToolContext::new();
        second.session.state.write().unwrap().insert(key.clone(), record.clone());
        second.run_config.tool_confirmation_decisions =
            HashMap::from([("call-1".to_string(), adk_core::ToolConfirmationDecision::Approve)]);
        second.run_config.tool_confirmation_fingerprints =
            HashMap::from([("call-1".to_string(), surfaced.fingerprint())]);
        let second = Arc::new(second);
        let response = tool.execute(second.clone(), args).await.unwrap();
        assert_eq!(response, json!({"response": "deleted"}));
        assert_eq!(second.actions().state_delta.get(key), Some(&Value::Null));
        assert!(second.actions().skip_summarization);

        let (held, user_content, config) = resumed.lock().unwrap().take().expect("it resumed");
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].id.as_deref(), Some("inner-1"));
        assert_eq!(held[0].name, "delete_file");
        assert!(user_content.parts.is_empty(), "a resuming run carries no new input");
        // The caller's decision for its own call reaches the call it was surfaced for.
        assert_eq!(
            config.tool_confirmation_decisions.get("inner-1"),
            Some(&adk_core::ToolConfirmationDecision::Approve)
        );
        assert_eq!(
            config.tool_confirmation_fingerprints.get("inner-1"),
            Some(&surfaced.fingerprint())
        );
    }

    /// Holds two calls in one turn, as a model that issues both at once does.
    struct DoubleHoldingAgent;

    #[async_trait]
    impl Agent for DoubleHoldingAgent {
        fn name(&self) -> &str {
            "holder"
        }
        fn description(&self) -> &str {
            "holds two calls for confirmation"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<adk_core::EventStream> {
            let event = |content: Content| {
                let mut event = Event::new(ctx.invocation_id());
                event.author = "holder".to_string();
                event.llm_response.content = Some(content);
                event
            };
            let files = [("inner-1", "a.txt"), ("inner-2", "b.txt")];
            let issued = event(Content {
                role: "model".to_string(),
                parts: files
                    .iter()
                    .map(|(id, file)| Part::FunctionCall {
                        name: "delete_file".to_string(),
                        args: json!({ "path": file }),
                        id: Some((*id).to_string()),
                        thought_signature: None,
                    })
                    .collect(),
            });
            let pending = event(Content {
                role: "function".to_string(),
                parts: files
                    .iter()
                    .map(|(id, _)| Part::FunctionResponse {
                        function_response: adk_core::FunctionResponseData::new(
                            "delete_file",
                            adk_core::confirmation_pending_response("held"),
                        ),
                        id: Some((*id).to_string()),
                        annotations: None,
                    })
                    .collect(),
            });
            let mut events = vec![Ok(issued), Ok(pending)];
            for (id, file) in files {
                let mut hold = event(Content::new("model").with_text("confirmation required"));
                hold.actions.tool_confirmation = Some(
                    ToolConfirmationRequest::new("delete_file", json!({ "path": file }))
                        .with_function_call_id(id)
                        .with_acting_context(ctx.acting_context()),
                );
                events.push(Ok(hold));
            }
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn every_call_a_delegate_holds_is_surfaced_under_the_callers_call() {
        let tool = AgentTool::new(Arc::new(DoubleHoldingAgent));
        let ctx = Arc::new(TestToolContext::new());

        let response = tool.execute(ctx.clone(), json!({"request": "delete both"})).await.unwrap();

        assert!(adk_core::is_confirmation_pending(&response), "{response}");
        let emitted = ctx.emitted_events.lock().unwrap().clone();
        assert_eq!(emitted.len(), 1, "the earlier request is surfaced before the last");
        assert_eq!(emitted[0].author, "parent-agent");
        assert_eq!(emitted[0].invocation_id, "parent-invocation");
        let mut surfaced: Vec<ToolConfirmationRequest> =
            emitted.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect();
        surfaced.extend(ctx.actions().tool_confirmation);
        let shown: Vec<_> = surfaced
            .iter()
            .map(|request| (request.args.clone(), request.function_call_id.as_deref()))
            .collect();
        assert_eq!(
            shown,
            vec![
                (json!({"path": "a.txt"}), Some("call-1")),
                (json!({"path": "b.txt"}), Some("call-1")),
            ]
        );
        let record = ctx
            .actions()
            .state_delta
            .into_iter()
            .find(|(key, _)| key.starts_with(HELD_DELEGATION_STATE_PREFIX))
            .map(|(_, record)| serde_json::from_value::<HeldDelegation>(record).unwrap())
            .expect("the held delegation is recorded");
        assert_eq!(record.request.function_call_id.as_deref(), Some("inner-1"));
        assert_eq!(record.more.len(), 1);
        assert_eq!(record.more[0].function_call_id.as_deref(), Some("inner-2"));
    }

    #[test]
    fn a_record_without_further_requests_still_reads() {
        let request = ToolConfirmationRequest::new("delete_file", json!({"path": "a.txt"}))
            .with_function_call_id("inner-1");
        let written = json!({
            "call": "fingerprint",
            "agent": "holder",
            "request": request,
            "state": {},
            "history": [],
            "userContent": Content::new("user").with_text("delete a.txt"),
            "events": [],
        });
        let record: HeldDelegation = serde_json::from_value(written).unwrap();
        assert!(record.more.is_empty());
    }
}
