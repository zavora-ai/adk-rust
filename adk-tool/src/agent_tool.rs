//! AgentTool - Use agents as callable tools
//!
//! This module provides `AgentTool` which wraps an `Agent` instance to make it
//! callable as a `Tool`. This enables powerful composition patterns where a
//! coordinator agent can invoke specialized sub-agents.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_tool::AgentTool;
//! use adk_agent::LlmAgentBuilder;
//!
//! // Create a specialized agent
//! let math_agent = LlmAgentBuilder::new("math_expert")
//!     .description("Solves mathematical problems")
//!     .instruction("You are a math expert. Solve problems step by step.")
//!     .model(model.clone())
//!     .build()?;
//!
//! // Wrap it as a tool
//! let math_tool = AgentTool::new(Arc::new(math_agent));
//!
//! // Use in coordinator agent
//! let coordinator = LlmAgentBuilder::new("coordinator")
//!     .instruction("Help users by delegating to specialists")
//!     .tools(vec![Arc::new(math_tool)])
//!     .build()?;
//! ```

use adk_core::{
    Agent, Artifacts, CallbackContext, Content, Event, InvocationContext, Memory, Part,
    ReadonlyContext, Result, RunConfig, Session, State, Tool, ToolContext,
};
use async_trait::async_trait;
use futures::StreamExt;
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
        }
    }
}

struct AgentToolChildConfig {
    forward_artifacts: bool,
    artifact_prefixes: Option<Arc<Vec<String>>>,
    forward_memory: bool,
    forward_shared_state: bool,
    session_snapshot: AgentToolSessionSnapshot,
    run_config: RunConfig,
    delegation_depth: u32,
    max_delegation_depth: Option<u32>,
    history_max_events: Option<usize>,
    state_keys: Option<Arc<HashSet<String>>>,
    orchestration_root_invocation_id: String,
    orchestration_edge_id: String,
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
    pub fn forward_events(mut self, forward: bool) -> Self {
        self.runtime.forward_events = forward;
        self
    }

    /// Choose whether delegated failures are returned as JSON or propagated.
    pub fn failure_mode(mut self, mode: AgentToolFailureMode) -> Self {
        self.runtime.failure_mode = mode;
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

    /// Extract the final response text from agent events.
    fn extract_response(events: &[Event]) -> Value {
        // Collect all text responses from final events
        let mut responses = Vec::new();

        for event in events.iter().rev() {
            if event.is_final_response() {
                if let Some(content) = &event.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            responses.push(text.clone());
                        }
                    }
                }
                break; // Only get the last final response
            }
        }

        if responses.is_empty() {
            // Try to get any text from the last event
            if let Some(last_event) = events.last()
                && let Some(content) = &last_event.llm_response.content
            {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        return json!({ "response": text });
                    }
                }
            }
            json!({ "response": "No response from agent" })
        } else {
            json!({ "response": responses.concat() })
        }
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

        // Create user content for the sub-agent
        let user_content = Content::new("user").with_text(&request_text);

        // Create an isolated context for the sub-agent
        let sub_ctx = Arc::new(AgentToolInvocationContext::new(
            ctx.clone(),
            self.agent.clone(),
            user_content.clone(),
            AgentToolChildConfig {
                forward_artifacts: self.config.forward_artifacts,
                artifact_prefixes: self.runtime.artifact_prefixes.clone(),
                forward_memory: self.runtime.forward_memory,
                forward_shared_state: self.runtime.forward_shared_state,
                session_snapshot: self.runtime.session_snapshot,
                run_config: parent_run_config,
                delegation_depth: child_depth,
                max_delegation_depth: max_depth,
                history_max_events: self.runtime.history_max_events,
                state_keys: self.runtime.state_keys.clone(),
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
            let mut active_agent = self.agent.clone();
            let mut active_ctx = sub_ctx.clone();
            let mut transfer_depth = 0_u32;
            let mut events = Vec::new();
            let mut state_delta = HashMap::new();
            let mut artifact_delta = HashMap::new();

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
                            state_delta.extend(event.actions.state_delta.clone());
                            artifact_delta.extend(event.actions.artifact_delta.clone());
                            sub_ctx.session.apply_event(&event);
                            if self.runtime.forward_events {
                                let mut forwarded = event.clone();
                                if self.runtime.execute_child_handoffs && transfer.is_some() {
                                    // The AgentTool consumes this control-flow edge internally.
                                    // Do not let the parent Runner execute the same handoff again.
                                    forwarded.actions.transfer_to_agent = None;
                                }
                                ctx.emit_event(forwarded).await;
                            }
                            events.push(event);
                            if transfer.is_some() {
                                break;
                            }
                        }
                        Err(error) => {
                            adk_telemetry::error!("Error in sub-agent execution: {error}");
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

            Ok((events, state_delta, artifact_delta))
        };

        // Apply timeout if configured
        let result = if let Some(timeout_duration) = self.config.timeout {
            match tokio::time::timeout(timeout_duration, execution).await {
                Ok(r) => r,
                Err(_) => {
                    return self.failure(format!(
                        "agent '{}' execution timed out after {timeout_duration:?}",
                        self.agent.name()
                    ));
                }
            }
        } else {
            execution.await
        };

        match result {
            Ok((events, mut state_delta, artifact_delta)) => {
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
                // Forward state_delta and artifact_delta to parent context
                if !state_delta.is_empty()
                    || !artifact_delta.is_empty()
                    || self.config.skip_summarization
                {
                    let mut parent_actions = ctx.actions();
                    parent_actions.state_delta.extend(state_delta);
                    parent_actions.artifact_delta.extend(artifact_delta);
                    parent_actions.skip_summarization |= self.config.skip_summarization;
                    ctx.set_actions(parent_actions);
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
            Err(e) => self.failure(format!("agent execution failed: {e}")),
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
            session_snapshot,
            mut run_config,
            delegation_depth,
            max_delegation_depth,
            history_max_events,
            state_keys,
            orchestration_root_invocation_id,
            orchestration_edge_id,
        } = child_config;
        let invocation_id = format!("agent-tool-{}", uuid::Uuid::new_v4());
        let (mut state, mut history) = match (session_snapshot, parent_ctx.session()) {
            (AgentToolSessionSnapshot::Parent, Some(session)) => {
                (session.state().all(), session.conversation_history())
            }
            _ => (HashMap::new(), Vec::new()),
        };
        if let Some(keys) = state_keys {
            state.retain(|key, _| keys.contains(key));
        }
        history = AgentTool::project_parent_history(history, history_max_events);
        run_config.streaming_mode = adk_core::StreamingMode::None;
        Self {
            session: Arc::new(AgentToolSession::new(
                invocation_id.clone(),
                parent_ctx.app_name().to_string(),
                parent_ctx.user_id().to_string(),
                state,
                history,
            )),
            parent_ctx,
            agent,
            user_content,
            invocation_id,
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
        agent.configure_run(agent.name(), &mut run_config);
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

    fn session_id(&self) -> &str {
        // Use a unique session ID for the sub-agent
        &self.invocation_id
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &Content {
        &self.user_content
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
            state: std::sync::RwLock::new(state),
            history: std::sync::RwLock::new(history),
        }
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
        if let Some(content) = &event.llm_response.content
            && let Ok(mut history) = self.history.write()
        {
            history.push(content.clone());
        }
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
    async fn timeout_obeys_failure_mode() {
        let tool = AgentTool::new(Arc::new(PendingAgent))
            .timeout(Duration::from_millis(1))
            .propagate_failures(true);
        let error = tool
            .execute(Arc::new(TestToolContext::new()), json!({"request": "wait"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }
}
