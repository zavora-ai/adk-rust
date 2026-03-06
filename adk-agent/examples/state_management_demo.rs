use adk_agent::{CustomAgentBuilder, LlmAgentBuilder, SequentialAgent};
use adk_core::{
    Agent, CallbackContext, Content, Event, InvocationContext, Memory, Part, ReadonlyContext,
    Result, RunConfig, Session, State, types::AdkIdentity,
};
use adk_model::gemini::GeminiModel;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use adk_core::types::{SessionId, UserId};

struct TestSession {
    id: SessionId,
    user_id: UserId,
}

impl TestSession {
    fn new() -> Self {
        Self {
            id: SessionId::new("test-session".to_string()).unwrap(),
            user_id: UserId::new("test-user".to_string()).unwrap(),
        }
    }
}

impl Session for TestSession {
    fn id(&self) -> &SessionId {
        &self.id
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &UserId {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &TestState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

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

// Simple test context
struct TestContext {
    identity: AdkIdentity,
    session: TestSession,
    user_content: Content,
    metadata: HashMap<String, String>,
}

impl TestContext {
    fn new(text: &str) -> Self {
        Self {
            identity: AdkIdentity::default(),
            session: TestSession::new(),
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::text(text.to_string())],
            },
            metadata: HashMap::new(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for TestContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    fn metadata(&self) -> &HashMap<String, String> {
        &self.metadata
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

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        None
    }

    fn session(&self) -> &dyn Session {
        &self.session
    }

    fn run_config(&self) -> &RunConfig {
        unimplemented!()
    }

    fn end_invocation(&self) {}

    fn ended(&self) -> bool {
        false
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== State Management Demo ===\n");

    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model1 = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Demo 1: Single agent with output_key
    println!("Demo 1: Single Agent with OutputKey");
    println!("-------------------------------------");

    let agent = LlmAgentBuilder::new("summarizer")
        .description("Summarizes text")
        .model(Arc::new(model1))
        .instruction("Summarize the user's message in exactly 3 words")
        .output_key("summary")
        .build()?;

    let ctx = Arc::new(TestContext::new("The quick brown fox jumps over the lazy dog"));
    let mut stream = agent.run(ctx).await?;

    use futures::StreamExt;
    println!("\nAgent: summarizer");
    while let Some(result) = stream.next().await {
        let event = result?;

        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Some(text) = part.as_text() {
                    println!("Response: {}", text);
                }
            }
        }

        if !event.actions.state_delta.is_empty() {
            println!("\n✅ State Delta:");
            for (key, value) in &event.actions.state_delta {
                println!("  {} = {:?}", key, value);
            }
        }
    }

    // Demo 2: Sequential agents with state coordination
    println!("\n\nDemo 2: Sequential Agents with State Coordination");
    println!("--------------------------------------------------");

    let model2 = GeminiModel::new(&api_key, "gemini-2.5-flash")?;
    let model3 = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let analyzer = LlmAgentBuilder::new("analyzer")
        .description("Analyzes sentiment")
        .model(Arc::new(model2))
        .instruction(
            "Analyze the sentiment of the message. Reply with only: positive, negative, or neutral",
        )
        .output_key("sentiment")
        .build()?;

    let responder = LlmAgentBuilder::new("responder")
        .description("Generates response")
        .model(Arc::new(model3))
        .instruction("Generate a friendly response. The sentiment was: {sentiment}")
        .output_key("response")
        .build()?;

    let pipeline =
        SequentialAgent::new("sentiment_pipeline", vec![Arc::new(analyzer), Arc::new(responder)])
            .with_description("Analyzes sentiment then responds");

    let ctx2 = Arc::new(TestContext::new("I love this amazing product!"));
    let mut stream2 = pipeline.run(ctx2).await?;

    println!("\nPipeline: sentiment_pipeline");
    let mut all_state = std::collections::HashMap::new();

    while let Some(result) = stream2.next().await {
        let event = result?;

        println!("\nAgent: {}", event.author);

        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Some(text) = part.as_text() {
                    println!("Response: {}", text);
                }
            }
        }

        if !event.actions.state_delta.is_empty() {
            println!("State Delta:");
            for (key, value) in &event.actions.state_delta {
                println!("  {} = {:?}", key, value);
                all_state.insert(key.clone(), value.clone());
            }
        }
    }

    println!("\n✅ Final Accumulated State:");
    for (key, value) in &all_state {
        println!("  {} = {:?}", key, value);
    }

    // Demo 3: Custom agent reading state
    println!("\n\nDemo 3: Custom Agent Reading State");
    println!("-----------------------------------");

    let state_reader = CustomAgentBuilder::new("state_reader")
        .description("Reads and displays state")
        .handler(move |_ctx| {
            let state = all_state.clone();
            async move {
                let mut event = Event::new("demo-invocation");
                event.author = "state_reader".to_string();

                let mut text = String::from("State summary:\n");
                for (key, value) in &state {
                    text.push_str(&format!("- {}: {}\n", key, value));
                }

                event.llm_response.content =
                    Some(Content { role: "assistant".to_string(), parts: vec![Part::text(text)] });

                Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
        })
        .build()?;

    let ctx3 = Arc::new(TestContext::new("Show me the state"));
    let mut stream3 = state_reader.run(ctx3).await?;

    while let Some(result) = stream3.next().await {
        let event = result?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Some(text) = part.as_text() {
                    println!("{}", text);
                }
            }
        }
    }

    println!("\n=== Demo Complete ===");
    Ok(())
}
