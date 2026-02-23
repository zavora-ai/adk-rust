use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, CallbackContext, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, Session, State, Tool, ToolContext,
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
        "Test tool"
    }

    fn parameters_schema(&self) -> Option<Value> {
        None
    }

    fn response_schema(&self) -> Option<Value> {
        None
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "status": "tool-ok" }))
    }
}

struct MockSession;

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
        &MockState
    }

    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
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

struct MockContext {
    session: MockSession,
    user_content: Content,
}

impl MockContext {
    fn new() -> Self {
        Self { session: MockSession, user_content: Content::new("user").with_text("start") }
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
        static RUN_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }

    fn end_invocation(&self) {}

    fn ended(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn test_before_tool_callback_short_circuits_tool_execution() {
    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::function_call_response("test_tool", json!({}), "call-1"),
        SequencedModel::text_response("done"),
    ]));
    let tool = Arc::new(CountingTool::new());
    let tool_calls = tool.calls.clone();

    let agent = LlmAgentBuilder::new("test-agent")
        .model(model)
        .tool(tool)
        .before_tool_callback(Box::new(|_ctx| {
            Box::pin(async move {
                Ok(Some(Content {
                    role: "function".to_string(),
                    parts: vec![Part::Text { text: "blocked".to_string() }],
                }))
            })
        }))
        .build()
        .unwrap();

    let mut stream = agent.run(Arc::new(MockContext::new())).await.unwrap();
    let mut saw_blocked = false;

    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if let Some(content) = event.llm_response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    if text == "blocked" {
                        saw_blocked = true;
                    }
                }
            }
        }
    }

    assert!(saw_blocked, "before_tool callback output should be emitted");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0, "tool should be skipped");
}

#[tokio::test]
async fn test_after_tool_callback_overrides_result_and_order() {
    let call_order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let before_order = call_order.clone();
    let after_order = call_order.clone();

    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::function_call_response("test_tool", json!({}), "call-2"),
        SequencedModel::text_response("done"),
    ]));
    let tool = Arc::new(CountingTool::new());
    let tool_calls = tool.calls.clone();

    let agent = LlmAgentBuilder::new("test-agent")
        .model(model)
        .tool(tool)
        .before_tool_callback(Box::new(move |_ctx| {
            let before_order = before_order.clone();
            Box::pin(async move {
                before_order.lock().unwrap().push("before_tool".to_string());
                Ok(None)
            })
        }))
        .after_tool_callback(Box::new(move |_ctx| {
            let after_order = after_order.clone();
            Box::pin(async move {
                after_order.lock().unwrap().push("after_tool".to_string());
                Ok(Some(Content {
                    role: "function".to_string(),
                    parts: vec![Part::Text { text: "after-override".to_string() }],
                }))
            })
        }))
        .build()
        .unwrap();

    let mut stream = agent.run(Arc::new(MockContext::new())).await.unwrap();
    let mut saw_override = false;

    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if let Some(content) = event.llm_response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    if text == "after-override" {
                        saw_override = true;
                    }
                }
            }
        }
    }

    assert_eq!(tool_calls.load(Ordering::SeqCst), 1, "tool should execute once");
    assert!(saw_override, "after_tool callback override should be emitted");
    assert_eq!(call_order.lock().unwrap().clone(), vec!["before_tool", "after_tool"]);
}

#[tokio::test]
async fn test_before_tool_callback_error_aborts_tool_execution() {
    let model = Arc::new(SequencedModel::new(vec![SequencedModel::function_call_response(
        "test_tool",
        json!({}),
        "call-3",
    )]));
    let tool = Arc::new(CountingTool::new());
    let tool_calls = tool.calls.clone();

    let agent = LlmAgentBuilder::new("test-agent")
        .model(model)
        .tool(tool)
        .before_tool_callback(Box::new(|_ctx| {
            Box::pin(async move { Err(adk_core::AdkError::Agent("blocked".to_string())) })
        }))
        .build()
        .unwrap();

    let mut stream = agent.run(Arc::new(MockContext::new())).await.unwrap();
    let mut saw_error = false;

    while let Some(result) = stream.next().await {
        if result.is_err() {
            saw_error = true;
            break;
        }
    }

    assert!(saw_error, "callback error should be propagated to stream");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0, "tool should not execute on callback error");
}
