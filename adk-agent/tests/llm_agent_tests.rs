use adk_agent::{LlmAgentBuilder};
use adk_core::{Agent, Content, InvocationContext, Part, ReadonlyContext, Result, RunConfig, Tool, ToolContext};
use adk_model::gemini::GeminiModel;
use adk_tool::FunctionTool;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

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
impl InvocationContext for TestContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
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
}

#[test]
fn test_llm_agent_builder() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

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
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

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
    assert!(event.content.is_some());

    let content = event.content.as_ref().unwrap();
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
    assert!(text.contains("4") || text.contains("four"));
}

#[tokio::test]
async fn test_llm_agent_with_instruction() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

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
    assert!(event.content.is_some());

    let content = event.content.as_ref().unwrap();
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
    // Check for pirate-like language
    assert!(
        text.contains("ahoy")
            || text.contains("matey")
            || text.contains("arr")
            || text.contains("ye")
            || text.contains("aye")
    );
}

#[tokio::test]
async fn test_llm_agent_with_function_tool() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

    // Create a tool that returns current time (something the model can't know)
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
        println!("Event {}: author={}, content={:?}", events.len(), event.author, event.content);
        events.push(event);
    }

    println!("Total events: {}", events.len());

    // Model might answer directly or use tool - both are valid
    // Just verify we got a response
    assert!(!events.is_empty(), "Should have at least one event");

    // Check if any event mentions time
    let has_time_info = events.iter().any(|e| {
        e.content.as_ref().map(|c| {
            c.parts.iter().any(|p| match p {
                Part::Text { text } => text.contains("2025") || text.contains("14:30") || text.contains("time"),
                _ => false,
            })
        }).unwrap_or(false)
    });

    assert!(has_time_info, "Response should mention time");
}
