use adk_core::{
    Agent, Artifacts, CallbackContext, Content, Event, InvocationContext, Memory, ReadonlyContext,
    Result, RunConfig, Session, State, Tool, ToolContext,
    types::{AdkIdentity, InvocationId, SessionId, UserId},
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

    /// Set whether to skip summarization
    pub fn skip_summarization(mut self, skip: bool) -> Self {
        self.config.skip_summarization = skip;
        self
    }

    /// Set whether to forward artifacts
    pub fn forward_artifacts(mut self, forward: bool) -> Self {
        self.config.forward_artifacts = forward;
        self
    }

    /// Set execution timeout
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.config.timeout = Some(duration);
        self
    }

    /// Extract the final response from a stream of agent events.
    fn extract_response(events: &[Event]) -> Value {
        // Find the last event with content
        for event in events.iter().rev() {
            if let Some(content) = &event.llm_response.content {
                // If we want raw text, extract it
                for part in &content.parts {
                    if let Some(text) = part.as_text() {
                        return json!({ "response": text });
                    }
                }
            }
        }
        json!({ "response": "No response from agent" })
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
        Some(self.config.input_schema.clone().unwrap_or_else(|| {
            json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "The request or prompt for the agent"
                    }
                },
                "required": ["request"]
            })
        }))
    }

    fn response_schema(&self) -> Option<Value> {
        Some(self.config.output_schema.clone().unwrap_or_else(|| {
            json!({
                "type": "object",
                "properties": {
                    "response": {
                        "type": "string"
                    }
                }
            })
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let request = args
            .get("request")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| args.as_str().unwrap_or(""));

        let user_content = Content::user().with_text(request);

        // Create sub-context for the agent
        let sub_ctx = Arc::new(AgentToolInvocationContext::new(
            ctx.clone(),
            self.agent.clone(),
            user_content,
            self.config.forward_artifacts,
        ));

        // Run the agent
        let execution = async {
            let mut stream = self.agent.run(sub_ctx.clone()).await?;
            let mut events = Vec::new();
            let mut state_delta = HashMap::new();

            while let Some(event_result) = stream.next().await {
                let event = event_result?;
                events.push(event.clone());

                // Accumulate state deltas from the sub-agent
                state_delta.extend(event.actions.state_delta);

                if event.actions.escalate {
                    break;
                }
            }

            Ok((events, state_delta))
        };

        let result: Result<(Vec<Event>, HashMap<String, Value>)> =
            if let Some(timeout_duration) = self.config.timeout {
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
            Ok((events, state_delta)) => {
                // Forward state_delta to parent context
                if !state_delta.is_empty() {
                    let mut parent_actions = ctx.actions();
                    parent_actions.state_delta.extend(state_delta);
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
    identity: AdkIdentity,
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
        let mut identity = parent_ctx.identity().clone();
        identity.agent_name = agent.name().to_string();
        identity.invocation_id =
            InvocationId::new(format!("{}-sub-{}", identity.invocation_id, agent.name())).unwrap();

        Self {
            parent_ctx,
            agent,
            user_content,
            identity,
            ended: Arc::new(AtomicBool::new(false)),
            forward_artifacts,
            session: Arc::new(AgentToolSession::new()),
        }
    }
}

impl ReadonlyContext for AgentToolInvocationContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    fn metadata(&self) -> &HashMap<String, String> {
        self.parent_ctx.metadata()
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
        None
    }

    fn session(&self) -> &dyn Session {
        self.session.as_ref()
    }

    fn run_config(&self) -> &RunConfig {
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
    id: SessionId,
    user_id: UserId,
    state: std::sync::RwLock<HashMap<String, Value>>,
}

impl AgentToolSession {
    fn new() -> Self {
        Self {
            id: SessionId::new("sub-session".to_string()).unwrap(),
            user_id: UserId::new("system".to_string()).unwrap(),
            state: std::sync::RwLock::new(HashMap::new()),
        }
    }
}

impl Session for AgentToolSession {
    fn id(&self) -> &SessionId {
        &self.id
    }

    fn app_name(&self) -> &str {
        "adk-tool"
    }

    fn user_id(&self) -> &UserId {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

impl State for AgentToolSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.read().unwrap().get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        self.state.write().unwrap().insert(key, value);
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.read().unwrap().clone()
    }
}
