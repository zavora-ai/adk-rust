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
use std::collections::HashMap;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

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
}

impl AgentTool {
    /// Create a new AgentTool wrapping the given agent.
    pub fn new(agent: Arc<dyn Agent>) -> Self {
        Self { agent, config: AgentToolConfig::default() }
    }

    /// Create a new AgentTool with custom configuration.
    pub fn with_config(agent: Arc<dyn Agent>, config: AgentToolConfig) -> Self {
        Self { agent, config }
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
            if let Some(last_event) = events.last() {
                if let Some(content) = &last_event.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            return json!({ "response": text });
                        }
                    }
                }
            }
            json!({ "response": "No response from agent" })
        } else {
            json!({ "response": responses.join("\n") })
        }
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

        // Extract the request from args
        let request_text = self.extract_request(&args);

        // Create user content for the sub-agent
        let user_content = Content::new("user").with_text(&request_text);

        // Create an isolated context for the sub-agent
        let sub_ctx = Arc::new(AgentToolInvocationContext::new(
            ctx.clone(),
            self.agent.clone(),
            user_content.clone(),
            self.config.forward_artifacts,
        ));

        // Execute the sub-agent
        let execution = async {
            let mut event_stream = self.agent.run(sub_ctx.clone()).await?;

            // Collect all events
            let mut events = Vec::new();
            let mut state_delta = HashMap::new();
            let mut artifact_delta = HashMap::new();

            while let Some(result) = event_stream.next().await {
                match result {
                    Ok(event) => {
                        // Merge state deltas
                        state_delta.extend(event.actions.state_delta.clone());
                        artifact_delta.extend(event.actions.artifact_delta.clone());
                        events.push(event);
                    }
                    Err(e) => {
                        adk_telemetry::error!("Error in sub-agent execution: {}", e);
                        return Err(e);
                    }
                }
            }

            Ok((events, state_delta, artifact_delta))
        };

        // Apply timeout if configured
        let result = if let Some(timeout_duration) = self.config.timeout {
            match tokio::time::timeout(timeout_duration, execution).await {
                Ok(r) => r,
                Err(_) => {
                    return Ok(json!({
                        "error": "Agent execution timed out",
                        "agent": self.agent.name()
                    }));
                }
            }
        } else {
            execution.await
        };

        match result {
            Ok((events, state_delta, artifact_delta)) => {
                // Forward state_delta and artifact_delta to parent context
                if !state_delta.is_empty() || !artifact_delta.is_empty() {
                    let mut parent_actions = ctx.actions();
                    parent_actions.state_delta.extend(state_delta);
                    parent_actions.artifact_delta.extend(artifact_delta);
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
            Err(e) => Ok(json!({
                "error": format!("Agent execution failed: {}", e),
                "agent": self.agent.name()
            })),
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
    session: Arc<AgentToolSession>,
}

impl AgentToolInvocationContext {
    fn new(
        parent_ctx: Arc<dyn ToolContext>,
        agent: Arc<dyn Agent>,
        user_content: Content,
        forward_artifacts: bool,
    ) -> Self {
        let invocation_id = format!("agent-tool-{}", uuid::Uuid::new_v4());
        Self {
            parent_ctx,
            agent,
            user_content,
            invocation_id,
            ended: Arc::new(AtomicBool::new(false)),
            forward_artifacts,
            session: Arc::new(AgentToolSession::new()),
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
        if self.forward_artifacts { self.parent_ctx.artifacts() } else { None }
    }
}

#[async_trait]
impl InvocationContext for AgentToolInvocationContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        // Sub-agents don't have direct memory access in this implementation
        // Could be extended to forward memory if needed
        None
    }

    fn session(&self) -> &dyn Session {
        self.session.as_ref()
    }

    fn run_config(&self) -> &RunConfig {
        // Use default config for sub-agent (SSE mode)
        static DEFAULT_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        DEFAULT_CONFIG.get_or_init(RunConfig::default)
    }

    fn end_invocation(&self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// Minimal session for sub-agent execution
struct AgentToolSession {
    id: String,
    state: std::sync::RwLock<HashMap<String, Value>>,
}

impl AgentToolSession {
    fn new() -> Self {
        Self {
            id: format!("agent-tool-session-{}", uuid::Uuid::new_v4()),
            state: Default::default(),
        }
    }
}

impl Session for AgentToolSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        "agent-tool"
    }

    fn user_id(&self) -> &str {
        "agent-tool-user"
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn conversation_history(&self) -> Vec<Content> {
        // Sub-agent starts with empty history
        Vec::new()
    }
}

impl State for AgentToolSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.read().ok()?.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
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
        event.llm_response.content = Some(Content::new("model").with_text("The answer is 4"));

        let events = vec![event];
        let response = AgentTool::extract_response(&events);

        assert_eq!(response["response"], "The answer is 4");
    }
}
