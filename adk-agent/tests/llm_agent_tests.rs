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
