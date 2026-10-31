use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, InvocationContext, LlmRequest, Part, ReadonlyContext, RunConfig, ToolContext,
};
#[cfg(feature = "skills")]
use adk_skill::SelectionPolicy;
use adk_tool::FunctionTool;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::{Arc, Mutex};

struct MockLlm {
    response_text: String,
}

impl MockLlm {
    fn new(response_text: &str) -> Self {
        Self { response_text: response_text.to_string() }
    }
}

#[async_trait]
impl adk_core::Llm for MockLlm {
    fn name(&self) -> &str {
        "mock-llm"
    }

    async fn generate_content(
        &self,
        _request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let text = self.response_text.clone();
        let s = async_stream::stream! {
            yield Ok(adk_core::LlmResponse {
                content: Some(adk_core::Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::Text { text }],
                }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                provider_metadata: None,
                interaction_id: None,
                model: None,
                provider: None,
            });
        };
        Ok(Box::pin(s))
    }
}

struct SpyLlm {
    response_text: String,
    last_request: Arc<Mutex<Option<LlmRequest>>>,
}

impl SpyLlm {
    fn new(response_text: &str) -> Self {
        Self { response_text: response_text.to_string(), last_request: Arc::new(Mutex::new(None)) }
    }
}

#[async_trait]
impl adk_core::Llm for SpyLlm {
    fn name(&self) -> &str {
        "spy-llm"
    }

    async fn generate_content(
        &self,
        request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        *self.last_request.lock().unwrap() = Some(request);

        let text = self.response_text.clone();
        let s = async_stream::stream! {
            yield Ok(adk_core::LlmResponse {
                content: Some(adk_core::Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::Text { text }],
                }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                provider_metadata: None,
                interaction_id: None,
                model: None,
                provider: None,
            });
        };
        Ok(Box::pin(s))
    }
}

struct TestContext {
    content: Content,
    config: RunConfig,
    session: DummySession,
}

impl TestContext {
    fn new(message: &str) -> Self {
        Self {
            content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: message.to_string() }],
            },
            config: RunConfig::default(),
            session: DummySession::default(),
        }
    }

    fn with_history(message: &str, history: Vec<Content>) -> Self {
        Self { session: DummySession { history }, ..Self::new(message) }
    }
}

#[async_trait]
impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str {
        "test-invocation"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "test-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl adk_core::CallbackContext for TestContext {
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
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
    fn session(&self) -> &dyn adk_core::Session {
        &self.session
    }
}

// Dummy session for testing
#[derive(Default)]
struct DummySession {
    history: Vec<Content>,
}

impl adk_core::Session for DummySession {
    fn id(&self) -> &str {
        "test-session"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn state(&self) -> &dyn adk_core::State {
        &DummyState
    }
    fn conversation_history(&self) -> Vec<adk_core::Content> {
        self.history.clone()
    }
}

struct DummyState;

impl adk_core::State for DummyState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

#[test]
fn test_llm_agent_builder() {
    let model = MockLlm::new("test");

    let agent = LlmAgentBuilder::new("test_agent")
        .description("A test agent")
        .model(Arc::new(model))
        .instruction("You are a helpful assistant.")
        .build()
        .unwrap();

    assert_eq!(agent.name(), "test_agent");
    assert_eq!(agent.description(), "A test agent");
    assert_eq!(agent.sub_agents().len(), 0);
}

#[test]
fn test_llm_agent_builder_missing_model() {
    let result = LlmAgentBuilder::new("test_agent").description("A test agent").build();

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Model is required"));
}

#[tokio::test]
async fn test_llm_agent_basic_generation() {
    let model = MockLlm::new("4");

    let agent = LlmAgentBuilder::new("math_agent")
        .description("Answers math questions")
        .model(Arc::new(model))
        .instruction("You are a math tutor. Answer briefly.")
        .build()
        .unwrap();

    let ctx = Arc::new(TestContext::new("What is 2+2?"));
    let mut stream = agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        events.push(event);
    }

    assert!(!events.is_empty());
    let event = &events[0];
    assert_eq!(event.author, "math_agent");
    assert!(event.llm_response.content.is_some());

    let content = event.llm_response.content.as_ref().unwrap();
    let text = content
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    println!("Response: {}", text);
    assert!(text.contains("4"));
}

#[tokio::test]
async fn test_llm_agent_with_instruction() {
    let model = MockLlm::new("Ahoy matey!");

    let agent = LlmAgentBuilder::new("pirate_agent")
        .description("Talks like a pirate")
        .model(Arc::new(model))
        .instruction("You are a pirate. Always respond in pirate speak. Be brief.")
        .build()
        .unwrap();

    let ctx = Arc::new(TestContext::new("Hello!"));
    let mut stream = agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        events.push(event);
    }

    assert!(!events.is_empty());
    let event = &events[0];
    assert_eq!(event.author, "pirate_agent");
    assert!(event.llm_response.content.is_some());

    let content = event.llm_response.content.as_ref().unwrap();
    let text = content
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
        .to_lowercase();

    println!("Pirate response: {}", text);
    assert!(text.contains("ahoy") || text.contains("matey"));
}

#[tokio::test]
async fn test_llm_agent_with_function_tool() {
    // For this test, we want to verify the agent CAN be built with a tool.
    // Verifying the LLM *calls* the tool requires a smarter MockLlm that returns a FunctionCall part.
    // For now, let's just verify the agent runs and returns the mock response.
    // A more advanced test would mock the LLM returning a function call.

    let model = MockLlm::new("The time is 2025-11-23T14:30:00Z");

    let get_time_tool = FunctionTool::new(
        "get_current_time",
        "Returns the current time in ISO format",
        |_ctx: Arc<dyn ToolContext>, _args: Value| async move {
            Ok(serde_json::json!({ "time": "2025-11-23T14:30:00Z" }))
        },
    );

    let agent = LlmAgentBuilder::new("time_agent")
        .description("Can tell the current time")
        .model(Arc::new(model))
        .instruction("You must use the get_current_time tool to answer questions about time. Always use the tool.")
        .tool(Arc::new(get_time_tool))
        .build()
        .unwrap();

    let ctx = Arc::new(TestContext::new("What time is it right now?"));
    let mut stream = agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        events.push(event);
    }

    assert!(!events.is_empty());
}

#[tokio::test]
async fn test_llm_agent_output_key() {
    let model = MockLlm::new("Hello World");

    let agent = LlmAgentBuilder::new("test_agent")
        .description("Test agent")
        .model(Arc::new(model))
        .instruction("Say 'Hello World' and nothing else")
        .output_key("agent_response")
        .build()
        .expect("Failed to build agent");

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = agent.run(ctx).await.expect("Failed to run agent");

    use futures::StreamExt;
    let mut found_state_delta = false;
    while let Some(result) = stream.next().await {
        let event = result.expect("Event error");
        if !event.actions.state_delta.is_empty() {
            assert!(event.actions.state_delta.contains_key("agent_response"));
            let value = &event.actions.state_delta["agent_response"];
            assert!(value.is_string());
            let text = value.as_str().unwrap();
            assert!(text.contains("Hello"));
            found_state_delta = true;
        }
    }

    assert!(found_state_delta, "No state_delta found in events");
}

#[test]
fn test_llm_agent_builder_with_callbacks() {
    use std::sync::{Arc, Mutex};

    let model = MockLlm::new("response");

    let before_called = Arc::new(Mutex::new(false));
    let after_called = Arc::new(Mutex::new(false));

    let before_flag = before_called.clone();
    let after_flag = after_called.clone();

    let agent = LlmAgentBuilder::new("test_agent")
        .description("Test agent with callbacks")
        .model(Arc::new(model))
        .instruction("Say hello")
        .before_callback(Box::new(move |_ctx| {
            let flag = before_flag.clone();
            Box::pin(async move {
                *flag.lock().unwrap() = true;
                Ok(Some(Content {
                    role: "system".to_string(),
                    parts: vec![Part::Text { text: "Before callback".to_string() }],
                }))
            })
        }))
        .after_callback(Box::new(move |_ctx| {
            let flag = after_flag.clone();
            Box::pin(async move {
                *flag.lock().unwrap() = true;
                Ok(Some(Content {
                    role: "system".to_string(),
                    parts: vec![Part::Text { text: "After callback".to_string() }],
                }))
            })
        }))
        .build()
        .expect("Failed to build agent");

    // Verify agent was created successfully
    assert_eq!(agent.name(), "test_agent");
    assert_eq!(agent.description(), "Test agent with callbacks");
}

#[cfg(feature = "skills")]
#[tokio::test]
async fn test_llm_agent_injects_skill_after_cacheable_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".skills")).unwrap();
    std::fs::write(
        root.join(".skills/search.md"),
        "---\nname: search\ndescription: Search source code\ntags: [code, search]\n---\nUse rg --files then rg <pattern>.\n",
    )
    .unwrap();

    let model = SpyLlm::new("{}");
    let captured = model.last_request.clone();

    let builder = LlmAgentBuilder::new("skill_agent")
        .description("Agent with skills")
        .model(Arc::new(model))
        .global_instruction("GLOBAL INSTRUCTION")
        .instruction("AGENT INSTRUCTION")
        .output_schema(serde_json::json!({"type": "object"}))
        .with_skills_from_root(root)
        .unwrap()
        .with_skill_policy(SelectionPolicy {
            top_k: 1,
            min_score: 0.1,
            ..SelectionPolicy::default()
        });

    let agent = builder.build().unwrap();
    let ctx = Arc::new(TestContext::with_history(
        "Please search this repository",
        vec![
            Content::new("user").with_text("Earlier question"),
            Content::new("model").with_text("Earlier answer"),
            Content::new("user").with_text("Please search this repository"),
        ],
    ));
    let mut stream = agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    while let Some(result) = stream.next().await {
        result.unwrap();
    }

    let request = captured.lock().unwrap().clone().expect("expected captured request");
    let messages = request
        .contents
        .iter()
        .map(|content| content.parts.iter().filter_map(Part::text).collect::<Vec<_>>().join("\n"))
        .collect::<Vec<_>>();

    assert_eq!(messages.len(), 6);
    assert_eq!(messages[0], "GLOBAL INSTRUCTION");
    assert_eq!(messages[1], "AGENT INSTRUCTION");
    assert!(messages[2].starts_with("You MUST respond with valid JSON"));
    assert_eq!(messages[3], "Earlier question");
    assert_eq!(messages[4], "Earlier answer");
    assert!(messages[5].starts_with("[skill:search]"));
    assert!(messages[5].contains("Use rg --files then rg <pattern>."));
    assert!(messages[5].ends_with("Please search this repository"));
}

/// Replies once with a fixed set of function calls.
struct FunctionCallLlm {
    calls: Vec<Part>,
}

#[async_trait]
impl adk_core::Llm for FunctionCallLlm {
    fn name(&self) -> &str {
        "function-call-llm"
    }

    async fn generate_content(
        &self,
        _request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let parts = self.calls.clone();
        let s = async_stream::stream! {
            yield Ok(adk_core::LlmResponse {
                content: Some(adk_core::Content { role: "model".to_string(), parts }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                provider_metadata: None,
                interaction_id: None,
                model: None,
                provider: None,
            });
        };
        Ok(Box::pin(s))
    }
}

fn function_call(name: &str, id: &str, args: serde_json::Value) -> Part {
    Part::FunctionCall {
        name: name.to_string(),
        args,
        id: Some(id.to_string()),
        thought_signature: None,
    }
}

#[tokio::test]
async fn test_transfer_event_answers_every_call_in_the_turn() {
    let child = LlmAgentBuilder::new("child")
        .description("Child agent")
        .model(Arc::new(MockLlm::new("child answer")))
        .build()
        .unwrap();
    let parent = LlmAgentBuilder::new("parent")
        .description("Parent agent")
        .model(Arc::new(FunctionCallLlm {
            calls: vec![
                function_call("lookup", "call_1", serde_json::json!({})),
                function_call(
                    "transfer_to_agent",
                    "call_2",
                    serde_json::json!({"agent_name": "child"}),
                ),
            ],
        }))
        .sub_agent(Arc::new(child))
        .build()
        .unwrap();

    let mut stream = parent.run(Arc::new(TestContext::new("hand this off"))).await.unwrap();
    use futures::StreamExt;
    let mut transfer_event = None;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        if event.actions.transfer_to_agent.is_some() {
            transfer_event = Some(event);
        }
    }

    let event = transfer_event.expect("expected a transfer event");
    assert_eq!(event.actions.transfer_to_agent.as_deref(), Some("child"));
    let content = event.llm_response.content.expect("transfer event content");
    assert_eq!(content.role, "function");
    let responses: Vec<_> = content
        .parts
        .iter()
        .map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                (id.clone(), function_response.name.clone(), function_response.response.clone())
            }
            other => panic!("unexpected part {other:?}"),
        })
        .collect();
    assert_eq!(
        responses,
        vec![
            (
                Some("call_1".to_string()),
                "lookup".to_string(),
                serde_json::json!({"error": "not run: control transferred to child"})
            ),
            (
                Some("call_2".to_string()),
                "transfer_to_agent".to_string(),
                serde_json::json!({"transferred_to": "child"})
            ),
        ]
    );
}

/// Collects `(id, response)` for every function response across `events`.
fn function_responses(events: &[adk_core::Event]) -> Vec<(Option<String>, Value)> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                Some((id.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn test_invalid_then_valid_transfer_answers_each_call_once() {
    let child = LlmAgentBuilder::new("child")
        .description("Child agent")
        .model(Arc::new(MockLlm::new("child answer")))
        .build()
        .unwrap();
    let parent = LlmAgentBuilder::new("parent")
        .description("Parent agent")
        .model(Arc::new(FunctionCallLlm {
            calls: vec![
                function_call(
                    "transfer_to_agent",
                    "call_a",
                    serde_json::json!({"agent_name": "nobody"}),
                ),
                function_call(
                    "transfer_to_agent",
                    "call_b",
                    serde_json::json!({"agent_name": "child"}),
                ),
            ],
        }))
        .sub_agent(Arc::new(child))
        .build()
        .unwrap();

    let mut stream = parent.run(Arc::new(TestContext::new("hand this off"))).await.unwrap();
    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }

    assert_eq!(
        function_responses(&events),
        vec![
            (
                Some("call_a".to_string()),
                serde_json::json!({"error": "Agent 'nobody' not found. Available agents: [\"child\"]"})
            ),
            (Some("call_b".to_string()), serde_json::json!({"transferred_to": "child"})),
        ]
    );
}

/// Replies with each scripted turn in order, then with text, recording every request.
struct ScriptedLlm {
    turns: Mutex<std::collections::VecDeque<Vec<Part>>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl ScriptedLlm {
    fn new(turns: Vec<Vec<Part>>) -> Self {
        Self { turns: Mutex::new(turns.into()), requests: Arc::new(Mutex::new(Vec::new())) }
    }
}

#[async_trait]
impl adk_core::Llm for ScriptedLlm {
    fn name(&self) -> &str {
        "scripted-llm"
    }

    async fn generate_content(
        &self,
        request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        self.requests.lock().unwrap().push(request);
        let parts = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![Part::Text { text: "done".to_string() }]);
        let s = async_stream::stream! {
            yield Ok(adk_core::LlmResponse {
                content: Some(adk_core::Content { role: "model".to_string(), parts }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                provider_metadata: None,
                interaction_id: None,
                model: None,
                provider: None,
            });
        };
        Ok(Box::pin(s))
    }
}

#[tokio::test]
async fn test_request_after_hand_back_pairs_every_call_with_one_response() {
    use adk_core::{SessionId, UserId};
    use adk_runner::Runner;
    use adk_session::{CreateRequest, InMemorySessionService, SessionService};
    use futures::StreamExt;

    let child = LlmAgentBuilder::new("child")
        .description("Child agent")
        .model(Arc::new(ScriptedLlm::new(vec![vec![function_call(
            "transfer_to_agent",
            "call_c",
            serde_json::json!({"agent_name": "coordinator"}),
        )]])))
        .build()
        .unwrap();
    let coordinator_model = ScriptedLlm::new(vec![vec![
        function_call("transfer_to_agent", "call_a", serde_json::json!({"agent_name": "nobody"})),
        function_call("transfer_to_agent", "call_b", serde_json::json!({"agent_name": "child"})),
    ]]);
    let coordinator_requests = coordinator_model.requests.clone();
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Coordinator agent")
        .model(Arc::new(coordinator_model))
        .sub_agent(Arc::new(child))
        .build()
        .unwrap();

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "hand-back".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: std::collections::HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("hand-back")
        .agent(Arc::new(coordinator) as Arc<dyn Agent>)
        .session_service(sessions)
        .build()
        .unwrap();

    let mut stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("hand this off"),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }

    let requests = coordinator_requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "the coordinator runs again after the hand-back");
    let mut call_ids = Vec::new();
    let mut response_ids = Vec::new();
    for part in requests[1].contents.iter().flat_map(|content| &content.parts) {
        match part {
            Part::FunctionCall { id, .. } => call_ids.push(id.clone()),
            Part::FunctionResponse { id, .. } => response_ids.push(id.clone()),
            _ => {}
        }
    }
    call_ids.sort();
    response_ids.sort();
    assert!(call_ids.contains(&Some("call_a".to_string())));
    assert_eq!(response_ids, call_ids);
}

#[tokio::test]
async fn test_llm_agent_sends_instructions_as_system_contents() {
    let model = SpyLlm::new("{}");
    let captured = model.last_request.clone();

    let agent = LlmAgentBuilder::new("system_role_agent")
        .description("Agent with instructions")
        .model(Arc::new(model))
        .global_instruction("GLOBAL INSTRUCTION")
        .instruction("AGENT INSTRUCTION")
        .output_schema(serde_json::json!({"type": "object"}))
        .build()
        .unwrap();
    let ctx = Arc::new(TestContext::with_history(
        "Second question",
        vec![
            Content::new("user").with_text("First question"),
            Content::new("model").with_text("First answer"),
            Content::new("user").with_text("Second question"),
        ],
    ));
    let mut stream = agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    while let Some(result) = stream.next().await {
        result.unwrap();
    }

    let request = captured.lock().unwrap().clone().expect("expected captured request");
    let messages = request
        .contents
        .iter()
        .map(|content| {
            let text = content.parts.iter().filter_map(Part::text).collect::<Vec<_>>().join("\n");
            (content.role.as_str(), text)
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[0], ("system", "GLOBAL INSTRUCTION".to_string()));
    assert_eq!(messages[1], ("system", "AGENT INSTRUCTION".to_string()));
    assert_eq!(messages[2].0, "system");
    assert_eq!(messages[3], ("user", "First question".to_string()));
    assert_eq!(messages[4], ("model", "First answer".to_string()));
    assert_eq!(messages[5], ("user", "Second question".to_string()));
}

#[tokio::test]
async fn test_llm_agent_legacy_builder_path_has_no_skill_injection() {
    let model = SpyLlm::new("ok");
    let captured = model.last_request.clone();

    let agent = LlmAgentBuilder::new("legacy_agent")
        .description("Legacy builder path")
        .model(Arc::new(model))
        .instruction("Respond briefly")
        .build()
        .unwrap();

    let ctx = Arc::new(TestContext::new("Please search this repository"));
    let mut stream = agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    while let Some(result) = stream.next().await {
        result.unwrap();
    }

    let request = captured.lock().unwrap().clone().expect("expected captured request");
    let combined = request
        .contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| p.text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!combined.contains("[skill:"));
}

// --- Gemini Interactions conflict validation tests ---

#[cfg(feature = "sandbox")]
mod sandbox_conflict_tests {
    use super::*;
    use adk_sandbox::workspace::{Capability, Manifest, SandboxConfig};
    use std::collections::HashSet;

    /// A mock LLM that reports `uses_interactions_api() == true`.
    struct InteractionsLlm;

    #[async_trait]
    impl adk_core::Llm for InteractionsLlm {
        fn name(&self) -> &str {
            "gemini-interactions-mock"
        }

        fn uses_interactions_api(&self) -> bool {
            true
        }

        async fn generate_content(
            &self,
            _request: adk_core::LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<adk_core::LlmResponseStream> {
            unimplemented!("not needed for build-time validation tests")
        }
    }

    /// A mock SandboxClient (required by SandboxConfig).
    struct MockSandboxClient;

    #[async_trait]
    impl adk_sandbox::workspace::SandboxClient for MockSandboxClient {
        async fn provision(
            &self,
            _manifest: &Manifest,
        ) -> std::result::Result<adk_sandbox::workspace::SessionHandle, adk_sandbox::SandboxError>
        {
            unimplemented!()
        }
        async fn start(
            &self,
            _handle: &adk_sandbox::workspace::SessionHandle,
        ) -> std::result::Result<
            Box<dyn adk_sandbox::workspace::SandboxSession>,
            adk_sandbox::SandboxError,
        > {
            unimplemented!()
        }
        async fn stop(
            &self,
            _handle: &adk_sandbox::workspace::SessionHandle,
        ) -> std::result::Result<(), adk_sandbox::SandboxError> {
            unimplemented!()
        }
        async fn snapshot(
            &self,
            _handle: &adk_sandbox::workspace::SessionHandle,
        ) -> std::result::Result<adk_sandbox::workspace::SnapshotId, adk_sandbox::SandboxError>
        {
            unimplemented!()
        }
        async fn resume(
            &self,
            _snapshot_id: &adk_sandbox::workspace::SnapshotId,
        ) -> std::result::Result<adk_sandbox::workspace::SessionHandle, adk_sandbox::SandboxError>
        {
            unimplemented!()
        }
    }

    fn make_sandbox_config(capabilities: HashSet<Capability>) -> SandboxConfig {
        SandboxConfig::new(Arc::new(MockSandboxClient), Manifest::new(vec![]), capabilities)
    }

    #[test]
    fn test_interactions_api_with_shell_capability_returns_error() {
        let config = make_sandbox_config(HashSet::from([Capability::Shell]));

        let result = LlmAgentBuilder::new("test")
            .model(Arc::new(InteractionsLlm))
            .sandbox_config(config)
            .build();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, "code.gemini_interactions_conflict");
        assert!(err.message.contains("Cannot combine Gemini Interactions API"));
    }

    #[test]
    fn test_interactions_api_with_filesystem_capability_returns_error() {
        let config = make_sandbox_config(HashSet::from([Capability::Filesystem]));

        let result = LlmAgentBuilder::new("test")
            .model(Arc::new(InteractionsLlm))
            .sandbox_config(config)
            .build();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, "code.gemini_interactions_conflict");
    }

    #[test]
    fn test_interactions_api_with_both_capabilities_returns_error() {
        let config =
            make_sandbox_config(HashSet::from([Capability::Shell, Capability::Filesystem]));

        let result = LlmAgentBuilder::new("test")
            .model(Arc::new(InteractionsLlm))
            .sandbox_config(config)
            .build();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, "code.gemini_interactions_conflict");
    }

    #[test]
    fn test_non_interactions_model_with_sandbox_config_succeeds() {
        // MockLlm returns false for uses_interactions_api (default)
        let config =
            make_sandbox_config(HashSet::from([Capability::Shell, Capability::Filesystem]));

        let result = LlmAgentBuilder::new("test")
            .model(Arc::new(MockLlm::new("hello")))
            .sandbox_config(config)
            .build();

        assert!(result.is_ok());
    }

    #[test]
    fn test_interactions_api_with_empty_capabilities_succeeds() {
        // No Shell or Filesystem capabilities — should be allowed
        let config = make_sandbox_config(HashSet::new());

        let result = LlmAgentBuilder::new("test")
            .model(Arc::new(InteractionsLlm))
            .sandbox_config(config)
            .build();

        assert!(result.is_ok());
    }
}

// ===== History processor =====

fn lookup_tool() -> Arc<dyn adk_core::Tool> {
    Arc::new(FunctionTool::new(
        "lookup",
        "Looks up one page of rows",
        |_ctx: Arc<dyn ToolContext>, _args: Value| async move { Ok(serde_json::json!({ "rows": 3 })) },
    ))
}

/// The contents of the run's complete events, and its errors, in order.
async fn run_to_end(agent: &adk_agent::LlmAgent, ctx: TestContext) -> Vec<Result<Content, String>> {
    use futures::StreamExt;

    let stream = agent.run(Arc::new(ctx)).await.unwrap();
    stream
        .filter_map(|item| async move {
            match item {
                Ok(event) if event.llm_response.partial => None,
                Ok(event) => event.llm_response.content.map(Ok),
                Err(error) => Some(Err(error.to_string())),
            }
        })
        .collect()
        .await
}

#[tokio::test]
async fn test_history_processor_replaces_the_history_once_and_later_steps_append() {
    let model =
        ScriptedLlm::new(vec![vec![function_call("lookup", "call_1", serde_json::json!({}))]]);
    let requests = model.requests.clone();
    let seen = Arc::new(Mutex::new(Vec::<Vec<Content>>::new()));
    let recorder = seen.clone();
    let agent = LlmAgentBuilder::new("compacting")
        .model(Arc::new(model))
        .tool(lookup_tool())
        .history_processor(Box::new(move |_ctx, history: Vec<Content>| {
            let seen = recorder.clone();
            Box::pin(async move {
                let first = {
                    let mut seen = seen.lock().unwrap();
                    seen.push(history.clone());
                    seen.len() == 1
                };
                Ok(if first { vec![Content::new("user").with_text(SUMMARY)] } else { history })
            })
        }))
        .build()
        .unwrap();
    let session_history = vec![
        Content::new("user").with_text("an old question"),
        Content::new("model").with_text("an old answer"),
        Content::new("user").with_text("count the rows"),
    ];

    let contents =
        run_to_end(&agent, TestContext::with_history("count the rows", session_history.clone()))
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

    let summary = Content::new("user").with_text(SUMMARY);
    let (call, result) = (contents[0].clone(), contents[1].clone());
    let json = |contents: Vec<Vec<Content>>| serde_json::to_value(contents).unwrap();
    // The processor received the session history, then its own summary with the tool step
    // appended, and the provider received the same.
    assert_eq!(
        json(seen.lock().unwrap().clone()),
        json(vec![session_history, vec![summary.clone(), call.clone(), result.clone()]])
    );
    let sent = requests.lock().unwrap().iter().map(|request| request.contents.clone()).collect();
    assert_eq!(json(sent), json(vec![vec![summary.clone()], vec![summary, call, result]]));
}

#[tokio::test]
async fn test_history_processor_error_ends_the_turn() {
    let model =
        ScriptedLlm::new(vec![vec![function_call("lookup", "call_1", serde_json::json!({}))]]);
    let requests = model.requests.clone();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new("failing")
        .model(Arc::new(model))
        .tool(lookup_tool())
        .history_processor(Box::new(move |_ctx, history: Vec<Content>| {
            let calls = calls.clone();
            Box::pin(async move {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Ok(history)
                } else {
                    Err(adk_core::AdkError::agent("the history store is unavailable"))
                }
            })
        }))
        .build()
        .unwrap();

    let outcomes = run_to_end(&agent, TestContext::new("count the rows"))
        .await
        .into_iter()
        .map(|outcome| outcome.map(|content| content.role))
        .collect::<Vec<_>>();

    // The call and its response, then the error in place of the second request.
    assert_eq!(
        outcomes,
        vec![
            Ok("model".to_string()),
            Ok("function".to_string()),
            Err(adk_core::AdkError::agent("the history store is unavailable").to_string()),
        ]
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
}

const MEMORY_HEADER: &str = "--- Relevant Memories ---";
const HOOK_MARK: &str = "<plugin-context>";
const REMINDER: &str = "<system-reminder>";
const SUMMARY: &str = "summary of earlier work";

/// Adds memories to the system content of every request, as
/// `PreloadMemoryTool::into_before_model_callback` does.
fn preload_memory_like() -> adk_core::BeforeModelCallback {
    Box::new(|_ctx, mut request: LlmRequest| {
        Box::pin(async move {
            let memory_text = format!(
                "\n\n{MEMORY_HEADER}\n[2026-10-01T00:00:00+00:00] user: rows are counted per page\n--- End Memories ---\n"
            );
            match request.contents.iter_mut().find(|content| content.role == "system") {
                Some(system) => system.parts.push(Part::Text { text: memory_text }),
                None => request.contents.insert(0, Content::new("system").with_text(memory_text)),
            }
            Ok(adk_core::BeforeModelResult::Continue(request))
        })
    })
}

/// Adds request context, as a runner plugin does through `RunConfig::invocation_hooks`.
#[derive(Debug)]
struct ContextHook;

#[async_trait]
impl adk_core::InvocationHooks for ContextHook {
    async fn before_model(
        &self,
        _ctx: Arc<dyn adk_core::CallbackContext>,
        mut request: LlmRequest,
    ) -> adk_core::Result<adk_core::BeforeModelResult> {
        request.contents.insert(
            0,
            Content::new("user").with_text(format!("{HOOK_MARK}tenant=acme</plugin-context>")),
        );
        Ok(adk_core::BeforeModelResult::Continue(request))
    }
}

/// Appends a reminder to every request, as adk-harness's toolset does.
struct ReminderToolset;

#[async_trait]
impl adk_core::Toolset for ReminderToolset {
    fn name(&self) -> &str {
        "reminders"
    }

    async fn tools(
        &self,
        _ctx: Arc<dyn ReadonlyContext>,
    ) -> adk_core::Result<Vec<Arc<dyn adk_core::Tool>>> {
        Ok(Vec::new())
    }

    async fn process_llm_request(
        &self,
        _ctx: Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> adk_core::Result<()> {
        request.contents.push(
            Content::new("user")
                .with_text(format!("{REMINDER}\nmode: default\n</system-reminder>")),
        );
        Ok(())
    }
}

/// A session of `cycles` earlier turns that each used a tool, then the message.
fn tool_history(cycles: usize) -> Vec<Content> {
    let mut history = Vec::new();
    for i in 0..cycles {
        let id = format!("h{i}");
        history.push(Content::new("user").with_text(format!("q{i}")));
        history.push(Content {
            role: "model".into(),
            parts: vec![function_call("lookup", &id, serde_json::json!({}))],
        });
        history.push(Content {
            role: "function".into(),
            parts: vec![Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new(
                    "lookup",
                    serde_json::json!({ "rows": i }),
                ),
                id: Some(id),
                annotations: None,
            }],
        });
        history.push(Content::new("model").with_text(format!("a{i}")));
    }
    history.push(Content::new("user").with_text("count the rows"));
    history
}

/// Occurrences of `needle` in the request's text parts.
fn count(request: &LlmRequest, needle: &str) -> usize {
    request
        .contents
        .iter()
        .flat_map(|content| content.parts.iter().filter_map(Part::text))
        .map(|text| text.matches(needle).count())
        .sum()
}

/// Function responses whose call is not in the content right before them, where Anthropic,
/// OpenAI and Gemini require it. Paired by ID, or by name when the ID is absent.
fn orphaned_responses(request: &LlmRequest) -> Vec<String> {
    let mut orphans = Vec::new();
    for (index, content) in request.contents.iter().enumerate() {
        for part in &content.parts {
            let Part::FunctionResponse { function_response, id, .. } = part else { continue };
            let paired = index.checked_sub(1).is_some_and(|previous| {
                request.contents[previous].parts.iter().any(|part| match part {
                    Part::FunctionCall { name, id: call_id, .. } => match (id, call_id) {
                        (Some(id), Some(call_id)) => id == call_id,
                        (None, None) => name == &function_response.name,
                        _ => false,
                    },
                    _ => false,
                })
            });
            if !paired {
                orphans.push(format!("{}#{id:?}@{index}", function_response.name));
            }
        }
    }
    orphans
}

struct ProcessorRun {
    requests: Vec<LlmRequest>,
    cuts: usize,
    seen: Vec<Vec<Content>>,
}

/// Runs `steps` tool steps under a threshold compaction, a memory callback, a run hook and a
/// reminder toolset, starting from 25 turns of history.
async fn run_with_processor(call_ids: bool, steps: usize) -> ProcessorRun {
    let turns = (0..steps)
        .map(|i| {
            vec![Part::FunctionCall {
                name: "lookup".to_string(),
                args: serde_json::json!({ "page": i }),
                id: call_ids.then(|| format!("call_{i}")),
                thought_signature: None,
            }]
        })
        .collect();
    let model = ScriptedLlm::new(turns);
    let requests = model.requests.clone();
    let cuts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (cuts_in, seen_in) = (cuts.clone(), seen.clone());
    let agent = LlmAgentBuilder::new("compacting")
        .model(Arc::new(model))
        .instruction("You count rows.")
        .tool(lookup_tool())
        .toolset(Arc::new(ReminderToolset))
        .before_model_callback(preload_memory_like())
        // Past 24 turns, keeps about the last 11 behind a summary.
        .history_processor(Box::new(move |_ctx, history: Vec<Content>| {
            let (cuts, seen) = (cuts_in.clone(), seen_in.clone());
            Box::pin(async move {
                seen.lock().unwrap().push(history.clone());
                let (system, turns): (Vec<_>, Vec<_>) =
                    history.into_iter().partition(|content| content.role == "system");
                if turns.len() <= 24 {
                    return Ok(system.into_iter().chain(turns).collect());
                }
                cuts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let cut = adk_core::history::balanced_content_cut(&turns, turns.len() - 11);
                Ok(system
                    .into_iter()
                    .chain([Content::new("user").with_text(SUMMARY)])
                    .chain(turns.into_iter().skip(cut))
                    .collect())
            })
        }))
        .build()
        .unwrap();
    let ctx = TestContext {
        config: RunConfig { invocation_hooks: vec![Arc::new(ContextHook)], ..RunConfig::default() },
        ..TestContext::with_history("count the rows", tool_history(6))
    };

    for outcome in run_to_end(&agent, ctx).await {
        outcome.unwrap();
    }
    let requests = requests.lock().unwrap().clone();
    let seen = seen.lock().unwrap().clone();
    ProcessorRun { requests, cuts: cuts.load(std::sync::atomic::Ordering::SeqCst), seen }
}

#[tokio::test]
async fn test_history_processor_keeps_per_request_material_per_request() {
    let run = run_with_processor(true, 5).await;

    let per_request = run
        .requests
        .iter()
        .map(|request| {
            (
                count(request, MEMORY_HEADER),
                count(request, HOOK_MARK),
                count(request, REMINDER),
                count(request, SUMMARY),
                orphaned_responses(request),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(per_request, vec![(1, 1, 1, 1, Vec::<String>::new()); 6]);
    // The compaction ran on the first request and carried over to the other five.
    assert_eq!(run.cuts, 1);
}

#[tokio::test]
async fn test_history_processor_sees_the_agents_own_call_ids() {
    let run = run_with_processor(false, 3).await;

    let ids = |contents: &[Content]| {
        let ids = contents
            .iter()
            .flat_map(|content| &content.parts)
            .filter_map(|part| match part {
                Part::FunctionCall { id, .. } | Part::FunctionResponse { id, .. } => Some(id),
                _ => None,
            })
            .collect::<Vec<_>>();
        let generated = ids
            .iter()
            .filter(|id| {
                id.as_deref().is_some_and(|id| id.starts_with(adk_core::GENERATED_CALL_ID_PREFIX))
            })
            .count();
        (generated, ids.iter().filter(|id| id.is_none()).count())
    };
    // Three calls and their responses: the processor reads the IDs the agent generated, and
    // the provider receives the calls without them.
    assert_eq!(ids(run.seen.last().unwrap()), (6, 0));
    assert_eq!(ids(&run.requests.last().unwrap().contents), (0, 6));
    assert!(run.requests.iter().all(|request| orphaned_responses(request).is_empty()));
}
