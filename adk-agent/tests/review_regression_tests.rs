use adk_agent::{
    ConditionalAgent, CustomAgentBuilder, LlmAgentBuilder, LlmConditionalAgentBuilder,
    ParallelAgent, SequentialAgent,
};
use adk_core::{
    Agent, CallbackContext, Content, Event, EventStream, FinishReason, InvocationContext, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, Part, Result, RunConfig, Session, State, Tool,
    ToolContext,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[cfg(feature = "guardrails")]
use adk_agent::guardrails::{Guardrail, GuardrailResult, GuardrailSet, Severity};

#[derive(Default)]
struct TestState;

impl State for TestState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }

    fn set(&mut self, _key: String, _value: Value) {}

    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct TestSession {
    history: Vec<Content>,
}

impl TestSession {
    fn new(history: Vec<Content>) -> Self {
        Self { history }
    }
}

impl Session for TestSession {
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
        &TestState
    }

    fn conversation_history(&self) -> Vec<Content> {
        self.history.clone()
    }
}

struct TestContext {
    user_content: Content,
    session: TestSession,
    run_config: RunConfig,
}

impl TestContext {
    fn new(message: &str) -> Self {
        Self::with_history(message, Vec::new())
    }

    fn with_history(message: &str, history: Vec<Content>) -> Self {
        Self {
            user_content: Content::new("user").with_text(message),
            session: TestSession::new(history),
            run_config: RunConfig::default(),
        }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for TestContext {
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
impl CallbackContext for TestContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for TestContext {
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

struct RecordingModel {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
    responses: Arc<Mutex<VecDeque<LlmResponse>>>,
}

impl RecordingModel {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
        }
    }

    fn text_response(text: &str) -> LlmResponse {
        LlmResponse {
            content: Some(Content::new("model").with_text(text)),
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

    fn function_calls(parts: Vec<Part>) -> LlmResponse {
        LlmResponse {
            content: Some(Content { role: "model".to_string(), parts }),
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
impl Llm for RecordingModel {
    fn name(&self) -> &str {
        "recording-model"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap_or_else(|e| e.into_inner()).push(request);
        let response = self
            .responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .unwrap_or_else(|| Self::text_response("done"));
        let s = async_stream::stream! {
            yield Ok(response);
        };
        Ok(Box::pin(s))
    }
}

struct IdCapturingTool {
    ids: Arc<Mutex<Vec<String>>>,
}

impl IdCapturingTool {
    fn new() -> Self {
        Self { ids: Arc::new(Mutex::new(Vec::new())) }
    }
}

#[async_trait]
impl Tool for IdCapturingTool {
    fn name(&self) -> &str {
        "test_tool"
    }

    fn description(&self) -> &str {
        "Captures function call IDs"
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.ids.lock().unwrap_or_else(|e| e.into_inner()).push(ctx.function_call_id().to_string());
        Ok(json!({ "ok": true }))
    }
}

struct CountingTool {
    calls: Arc<Mutex<usize>>,
}

impl CountingTool {
    fn new() -> Self {
        Self { calls: Arc::new(Mutex::new(0)) }
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "test_tool"
    }

    fn description(&self) -> &str {
        "Counts executions"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        *calls += 1;
        Ok(json!({ "ok": true }))
    }
}

async fn drain_stream(mut stream: EventStream) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result?);
    }
    Ok(events)
}

#[tokio::test]
async fn test_parallel_agent_callbacks_execute() {
    let call_order = Arc::new(Mutex::new(Vec::new()));
    let before_order = call_order.clone();
    let after_order = call_order.clone();

    let agent = CustomAgentBuilder::new("worker")
        .handler(|_ctx| async move {
            let mut event = Event::new("inv-1");
            event.author = "worker".to_string();
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as EventStream)
        })
        .build()
        .unwrap();

    let parallel = ParallelAgent::new("parallel", vec![Arc::new(agent)])
        .before_callback(Box::new(move |_ctx| {
            let before_order = before_order.clone();
            Box::pin(async move {
                before_order.lock().unwrap_or_else(|e| e.into_inner()).push("before".to_string());
                Ok(None)
            })
        }))
        .after_callback(Box::new(move |_ctx| {
            let after_order = after_order.clone();
            Box::pin(async move {
                after_order.lock().unwrap_or_else(|e| e.into_inner()).push("after".to_string());
                Ok(None)
            })
        }));

    let stream = parallel.run(Arc::new(TestContext::new("hello"))).await.unwrap();
    let _ = drain_stream(stream).await.unwrap();

    assert_eq!(
        call_order.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        ["before", "after"]
    );
}

#[tokio::test]
async fn test_conditional_agent_callbacks_execute() {
    let call_order = Arc::new(Mutex::new(Vec::new()));
    let before_order = call_order.clone();
    let after_order = call_order.clone();

    let worker = CustomAgentBuilder::new("worker")
        .handler(|_ctx| async move {
            let mut event = Event::new("inv-1");
            event.author = "worker".to_string();
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as EventStream)
        })
        .build()
        .unwrap();

    let conditional = ConditionalAgent::new("conditional", |_ctx| true, Arc::new(worker))
        .before_callback(Box::new(move |_ctx| {
            let before_order = before_order.clone();
            Box::pin(async move {
                before_order.lock().unwrap_or_else(|e| e.into_inner()).push("before".to_string());
                Ok(None)
            })
        }))
        .after_callback(Box::new(move |_ctx| {
            let after_order = after_order.clone();
            Box::pin(async move {
                after_order.lock().unwrap_or_else(|e| e.into_inner()).push("after".to_string());
                Ok(None)
            })
        }));

    let stream = conditional.run(Arc::new(TestContext::new("hello"))).await.unwrap();
    let _ = drain_stream(stream).await.unwrap();

    assert_eq!(
        call_order.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        ["before", "after"]
    );
}

#[tokio::test]
async fn test_function_call_ids_are_unique_for_repeated_tool_calls() {
    let model = Arc::new(RecordingModel::new(vec![
        RecordingModel::function_calls(vec![
            Part::FunctionCall {
                name: "test_tool".to_string(),
                args: json!({}),
                id: None,
                thought_signature: None,
            },
            Part::FunctionCall {
                name: "test_tool".to_string(),
                args: json!({}),
                id: None,
                thought_signature: None,
            },
        ]),
        RecordingModel::text_response("done"),
    ]));
    let tool = Arc::new(IdCapturingTool::new());
    let ids = tool.ids.clone();

    let agent = LlmAgentBuilder::new("tool-agent").model(model).tool(tool).build().unwrap();

    let stream = agent.run(Arc::new(TestContext::new("run tools"))).await.unwrap();
    let _ = drain_stream(stream).await.unwrap();

    let ids = ids.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert!(ids[0].ends_with("_0"));
    assert!(ids[1].ends_with("_1"));
}

#[tokio::test]
async fn test_include_contents_none_keeps_only_current_turn() {
    let model = Arc::new(RecordingModel::new(vec![RecordingModel::text_response("done")]));
    let requests = model.requests.clone();
    let history = vec![
        Content::new("user").with_text("old question"),
        Content::new("model").with_text("old answer"),
        Content::new("user").with_text("current question"),
    ];

    let agent = LlmAgentBuilder::new("stateless")
        .model(model)
        .include_contents(adk_core::IncludeContents::None)
        .build()
        .unwrap();

    let stream =
        agent.run(Arc::new(TestContext::with_history("current question", history))).await.unwrap();
    let _ = drain_stream(stream).await.unwrap();

    let requests = requests.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let request = requests.first().expect("captured request");
    assert_eq!(request.contents.len(), 1);
    assert_eq!(request.contents[0].role, "user");
    assert_eq!(
        request.contents[0].parts,
        vec![Part::Text { text: "current question".to_string() }]
    );
}

#[test]
fn test_llm_agent_rejects_duplicate_sub_agent_names() {
    let model = Arc::new(RecordingModel::new(vec![RecordingModel::text_response("done")]));
    let sub_agent_a = Arc::new(
        CustomAgentBuilder::new("duplicate")
            .handler(|_ctx| async move { Ok(Box::pin(futures::stream::empty()) as EventStream) })
            .build()
            .unwrap(),
    );
    let sub_agent_b = Arc::new(
        CustomAgentBuilder::new("duplicate")
            .handler(|_ctx| async move { Ok(Box::pin(futures::stream::empty()) as EventStream) })
            .build()
            .unwrap(),
    );

    let result = LlmAgentBuilder::new("coordinator")
        .model(model)
        .sub_agent(sub_agent_a)
        .sub_agent(sub_agent_b)
        .build();

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Duplicate sub-agent name"));
}

#[tokio::test]
async fn test_llm_conditional_agent_resolves_overlap_deterministically() {
    let technical_agent = Arc::new(
        CustomAgentBuilder::new("technical-agent")
            .handler(|_ctx| async move {
                let mut event = Event::new("inv-1");
                event.author = "technical-agent".to_string();
                Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as EventStream)
            })
            .build()
            .unwrap(),
    );
    let support_agent = Arc::new(
        CustomAgentBuilder::new("support-agent")
            .handler(|_ctx| async move {
                let mut event = Event::new("inv-1");
                event.author = "support-agent".to_string();
                Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as EventStream)
            })
            .build()
            .unwrap(),
    );

    let router = LlmConditionalAgentBuilder::new(
        "router",
        Arc::new(RecordingModel::new(vec![RecordingModel::text_response("technical support")])),
    )
    .instruction("Route")
    .route("support", support_agent)
    .route("technical", technical_agent)
    .build()
    .unwrap();

    let stream = router.run(Arc::new(TestContext::new("help"))).await.unwrap();
    let events = drain_stream(stream).await.unwrap();

    assert_eq!(events[1].author, "technical-agent");
}

#[tokio::test]
async fn test_sequential_agent_accumulates_history_without_runner() {
    let first_model =
        Arc::new(RecordingModel::new(vec![RecordingModel::text_response("analysis")]));
    let second_model =
        Arc::new(RecordingModel::new(vec![RecordingModel::text_response("summary")]));
    let second_requests = second_model.requests.clone();

    let first = LlmAgentBuilder::new("first").model(first_model).build().unwrap();
    let second = LlmAgentBuilder::new("second").model(second_model).build().unwrap();

    let workflow = SequentialAgent::new("workflow", vec![Arc::new(first), Arc::new(second)]);
    let stream = workflow.run(Arc::new(TestContext::new("topic"))).await.unwrap();
    let _ = drain_stream(stream).await.unwrap();

    let requests = second_requests.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let request = requests.first().expect("captured request");
    let saw_first_output = request.contents.iter().any(|content| {
        content.parts.iter().any(|part| matches!(part, Part::Text { text } if text == "analysis"))
    });
    assert!(saw_first_output);
}

#[tokio::test]
async fn test_sequential_agent_applies_state_delta_without_runner() {
    let first_model =
        Arc::new(RecordingModel::new(vec![RecordingModel::text_response("positive")]));
    let second_model =
        Arc::new(RecordingModel::new(vec![RecordingModel::text_response("friendly reply")]));
    let second_requests = second_model.requests.clone();

    let first = LlmAgentBuilder::new("analyzer")
        .model(first_model)
        .instruction("Reply with only the sentiment")
        .output_key("sentiment")
        .build()
        .unwrap();
    let second = LlmAgentBuilder::new("responder")
        .model(second_model)
        .instruction("Generate a response for a {sentiment} sentiment message")
        .build()
        .unwrap();

    let workflow = SequentialAgent::new("workflow", vec![Arc::new(first), Arc::new(second)]);
    let stream = workflow.run(Arc::new(TestContext::new("I love this"))).await.unwrap();
    let _ = drain_stream(stream).await.unwrap();

    let requests = second_requests.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let request = requests.first().expect("captured request");
    let saw_resolved_state = request.contents.iter().any(|content| {
        content
            .parts
            .iter()
            .any(|part| matches!(part, Part::Text { text } if text.contains("positive")))
    });
    assert!(saw_resolved_state);
}

#[tokio::test]
async fn test_llm_agent_normalizes_tool_call_markup() {
    let model = Arc::new(RecordingModel::new(vec![
        RecordingModel::text_response(
            "<tool_call>\ntest_tool\n<arg_key>value</arg_key>\n<arg_value>1</arg_value>\n</tool_call>",
        ),
        RecordingModel::text_response("done"),
    ]));
    let tool = Arc::new(CountingTool::new());
    let calls = tool.calls.clone();

    let agent = LlmAgentBuilder::new("markup-agent").model(model).tool(tool).build().unwrap();

    let stream = agent.run(Arc::new(TestContext::new("use tool"))).await.unwrap();
    let _ = drain_stream(stream).await.unwrap();

    assert_eq!(*calls.lock().unwrap_or_else(|e| e.into_inner()), 1);
}

#[cfg(feature = "guardrails")]
struct ReplaceInputGuardrail;

#[cfg(feature = "guardrails")]
#[async_trait]
impl Guardrail for ReplaceInputGuardrail {
    fn name(&self) -> &str {
        "replace-input"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let new_text = content
            .parts
            .iter()
            .map(|part| match part {
                Part::Text { .. } => Part::Text { text: "redacted".to_string() },
                other => other.clone(),
            })
            .collect();
        GuardrailResult::transform(
            Content { role: content.role.clone(), parts: new_text },
            "redacted input",
        )
    }
}

#[cfg(feature = "guardrails")]
struct BlockOutputGuardrail;

#[cfg(feature = "guardrails")]
#[async_trait]
impl Guardrail for BlockOutputGuardrail {
    fn name(&self) -> &str {
        "block-output"
    }

    async fn validate(&self, _content: &Content) -> GuardrailResult {
        GuardrailResult::fail("blocked output", Severity::High)
    }
}

#[cfg(feature = "guardrails")]
#[tokio::test]
async fn test_input_guardrails_transform_model_request() {
    let model = Arc::new(RecordingModel::new(vec![RecordingModel::text_response("done")]));
    let requests = model.requests.clone();

    let agent = LlmAgentBuilder::new("guarded")
        .model(model)
        .input_guardrails(GuardrailSet::new().with(ReplaceInputGuardrail))
        .build()
        .unwrap();

    let stream = agent.run(Arc::new(TestContext::new("secret"))).await.unwrap();
    let _ = drain_stream(stream).await.unwrap();

    let requests = requests.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let request = requests.first().expect("captured request");
    let content = request.contents.last().expect("user content");
    assert_eq!(content.role, "user");
    assert_eq!(content.parts, vec![Part::Text { text: "redacted".to_string() }]);
}

#[cfg(feature = "guardrails")]
#[tokio::test]
async fn test_output_guardrails_block_response() {
    let model = Arc::new(RecordingModel::new(vec![RecordingModel::text_response("unsafe")]));

    let agent = LlmAgentBuilder::new("guarded")
        .model(model)
        .output_guardrails(GuardrailSet::new().with(BlockOutputGuardrail))
        .build()
        .unwrap();

    let mut stream = agent.run(Arc::new(TestContext::new("hello"))).await.unwrap();
    let first = stream.next().await.expect("stream item");
    assert!(first.is_err());
    assert!(first.unwrap_err().to_string().contains("output guardrails blocked content"));
}
