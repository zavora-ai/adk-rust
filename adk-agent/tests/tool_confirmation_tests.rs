use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, CallbackContext, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, Session, State, Tool, ToolConfirmationDecision,
    ToolContext,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct SequencedModel {
    responses: Arc<Mutex<VecDeque<LlmResponse>>>,
}

impl SequencedModel {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self { responses: Arc::new(Mutex::new(responses.into_iter().collect())) }
    }

    fn function_call_response(name: &str, args: Value, id: &str) -> LlmResponse {
        LlmResponse {
            content: Some(Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: name.to_string(),
                    args,
                    id: Some(id.to_string()),
                    thought_signature: None,
                }],
            }),
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
        }
    }

    fn text_response(text: &str) -> LlmResponse {
        LlmResponse {
            content: Some(Content {
                role: "model".to_string(),
                parts: vec![Part::Text { text: text.to_string() }],
            }),
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
        }
    }
}

#[async_trait]
impl Llm for SequencedModel {
    fn name(&self) -> &str {
        "sequenced-model"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Self::text_response("done"));
        let s = async_stream::stream! {
            yield Ok(response);
        };
        Ok(Box::pin(s))
    }
}

struct CountingTool {
    calls: Arc<AtomicUsize>,
}

impl CountingTool {
    fn new() -> Self {
        Self { calls: Arc::new(AtomicUsize::new(0)) }
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "test_tool"
    }

    fn description(&self) -> &str {
        "Tool used in confirmation tests"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "status": "tool-ok" }))
    }
}

struct MockState;

impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }

    fn set(&mut self, _key: String, _value: Value) {}

    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct MockSession {
    state: MockState,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        "session-1"
    }

    fn app_name(&self) -> &str {
        "test-app"
    }

    fn user_id(&self) -> &str {
        "user-1"
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct MockContext {
    session: MockSession,
    user_content: Content,
    run_config: RunConfig,
}

impl MockContext {
    fn new(run_config: RunConfig) -> Self {
        Self {
            session: MockSession { state: MockState },
            user_content: Content::new("user").with_text("start"),
            run_config,
        }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }

    fn agent_name(&self) -> &str {
        "test-agent"
    }

    fn user_id(&self) -> &str {
        "user-1"
    }

    fn app_name(&self) -> &str {
        "test-app"
    }

    fn session_id(&self) -> &str {
        "session-1"
    }

    fn branch(&self) -> &str {
        "main"
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }

    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }

    fn session(&self) -> &dyn Session {
        &self.session
    }

    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }

    fn end_invocation(&self) {}

    fn ended(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn test_tool_confirmation_interrupts_when_decision_missing() {
    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::function_call_response("test_tool", json!({"x": 1}), "call-1"),
        SequencedModel::text_response("done"),
    ]));
    let tool = Arc::new(CountingTool::new());
    let tool_calls = tool.calls.clone();

    let agent = LlmAgentBuilder::new("test-agent")
        .model(model)
        .tool(tool)
        .require_tool_confirmation("test_tool")
        .build()
        .unwrap();

    let mut stream = agent.run(Arc::new(MockContext::new(RunConfig::default()))).await.unwrap();
    let mut saw_confirmation_interrupt = false;

    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if event.llm_response.interrupted {
            let request = event.actions.tool_confirmation.as_ref().unwrap();
            assert_eq!(request.tool_name, "test_tool");
            assert_eq!(request.function_call_id.as_deref(), Some("call-1"));
            saw_confirmation_interrupt = true;
        }
    }

    assert!(saw_confirmation_interrupt, "expected confirmation interrupt event");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0, "tool should not execute before approval");
}

#[tokio::test]
async fn test_tool_confirmation_deny_skips_tool_execution() {
    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::function_call_response("test_tool", json!({"x": 1}), "call-2"),
        SequencedModel::text_response("done"),
    ]));
    let tool = Arc::new(CountingTool::new());
    let tool_calls = tool.calls.clone();

    let agent = LlmAgentBuilder::new("test-agent")
        .model(model)
        .tool(tool)
        .require_tool_confirmation("test_tool")
        .build()
        .unwrap();

    let mut run_config = RunConfig::default();
    run_config
        .tool_confirmation_decisions
        .insert("test_tool".to_string(), ToolConfirmationDecision::Deny);

    let mut stream = agent.run(Arc::new(MockContext::new(run_config))).await.unwrap();
    let mut saw_denied_response = false;

    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if event.actions.tool_confirmation_decision == Some(ToolConfirmationDecision::Deny) {
            let content = event.llm_response.content.as_ref().unwrap();
            if let Some(Part::FunctionResponse { function_response, .. }) = content.parts.first() {
                let error = function_response
                    .response
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if error.contains("denied") {
                    saw_denied_response = true;
                }
            }
        }
    }

    assert!(saw_denied_response, "expected denied function response");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0, "tool must not execute when denied");
}

#[tokio::test]
async fn test_tool_confirmation_approve_executes_tool() {
    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::function_call_response("test_tool", json!({"x": 1}), "call-3"),
        SequencedModel::text_response("done"),
    ]));
    let tool = Arc::new(CountingTool::new());
    let tool_calls = tool.calls.clone();

    let agent = LlmAgentBuilder::new("test-agent")
        .model(model)
        .tool(tool)
        .require_tool_confirmation("test_tool")
        .build()
        .unwrap();

    let mut run_config = RunConfig::default();
    run_config
        .tool_confirmation_decisions
        .insert("test_tool".to_string(), ToolConfirmationDecision::Approve);

    let mut stream = agent.run(Arc::new(MockContext::new(run_config))).await.unwrap();
    let mut saw_tool_result = false;

    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if event.actions.tool_confirmation_decision == Some(ToolConfirmationDecision::Approve) {
            let content = event.llm_response.content.as_ref().unwrap();
            if let Some(Part::FunctionResponse { function_response, .. }) = content.parts.first() {
                if function_response.response.get("status") == Some(&json!("tool-ok")) {
                    saw_tool_result = true;
                }
            }
        }
    }

    assert!(saw_tool_result, "expected approved tool execution response");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1, "tool should execute exactly once");
}
