use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, InvocationContext, Part, ReadonlyContext, RunConfig, Tool, ToolContext};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

struct MockLlm {
    response_text: String,
}

impl MockLlm {
    fn new(response_text: &str) -> Self {
        Self {
            response_text: response_text.to_string(),
        }
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
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
            });
        };
        Ok(Box::pin(s))
    }
}

struct TestContext {
    content: Content,
    config: RunConfig,
}

impl TestContext {
    fn new(message: &str) -> Self {
        Self {
            content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text {
                    text: message.to_string(),
                }],
            },
            config: RunConfig::default(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str { "test-invocation" }
    fn agent_name(&self) -> &str { "test-agent" }
    fn user_id(&self) -> &str { "test-user" }
    fn app_name(&self) -> &str { "test-app" }
    fn session_id(&self) -> &str { "test-session" }
    fn branch(&self) -> &str { "" }
    fn user_content(&self) -> &Content { &self.content }
}

#[async_trait]
impl adk_core::CallbackContext for TestContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> { None }
}

#[async_trait]
impl InvocationContext for TestContext {
    fn agent(&self) -> Arc<dyn Agent> { unimplemented!() }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> { None }
    fn run_config(&self) -> &RunConfig { &self.config }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool { false }
    fn session(&self) -> &dyn adk_core::Session { &DummySession }
}

// Dummy session for testing
struct DummySession;

impl adk_core::Session for DummySession {
    fn id(&self) -> &str { "test-session" }
    fn app_name(&self) -> &str { "test-app" }
    fn user_id(&self) -> &str { "test-user" }
    fn state(&self) -> &dyn adk_core::State { &DummyState }
    fn conversation_history(&self) -> Vec<adk_core::Content> { Vec::new() }
}

struct DummyState;

impl adk_core::State for DummyState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> { None }
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
    let result = LlmAgentBuilder::new("test_agent")
        .description("A test agent")
        .build();

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
                    parts: vec![Part::Text {
                        text: "Before callback".to_string(),
                    }],
                }))
            })
        }))
        .after_callback(Box::new(move |_ctx| {
            let flag = after_flag.clone();
            Box::pin(async move {
                *flag.lock().unwrap() = true;
                Ok(Some(Content {
                    role: "system".to_string(),
                    parts: vec![Part::Text {
                        text: "After callback".to_string(),
                    }],
                }))
            })
        }))
        .build()
        .expect("Failed to build agent");

    // Verify agent was created successfully
    assert_eq!(agent.name(), "test_agent");
    assert_eq!(agent.description(), "Test agent with callbacks");
}
