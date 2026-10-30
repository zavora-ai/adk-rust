//! Meta Muse Spark agent with tool calling.
//!
//! Runs one agentic turn: a `muse-spark-1.3` model behind `LlmAgent` answers a
//! weather question by calling a `get_weather` tool through `Runner`, then
//! summarizes the result. The run fails loudly when the tool is never called or
//! the final text is empty, so this doubles as a live provider check.
//!
//! ```bash
//! META_API_KEY=... cargo run --manifest-path examples/muse_agentic/Cargo.toml
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, Tool, ToolContext, UserId};
use adk_model::muse::{MuseClient, MuseConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "muse-agentic";
const USER: &str = "demo-user";
const SESSION: &str = "session-1";

fn api_key() -> anyhow::Result<String> {
    std::env::var("META_API_KEY")
        .or_else(|_| std::env::var("MODEL_API_KEY"))
        .map_err(|_| anyhow::anyhow!("Set META_API_KEY (or MODEL_API_KEY) to run this example"))
}

fn weather_tool(calls: Arc<AtomicUsize>) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "get_weather",
        "Returns the current weather for a city. Args: {\"city\": string}.",
        move |_ctx: Arc<dyn ToolContext>, args: Value| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let city = args.get("city").and_then(Value::as_str).unwrap_or("unknown");
                Ok(json!({"city": city, "celsius": 21, "condition": "sunny"}))
            }
        },
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    dotenvy::dotenv().ok();

    println!("=== Meta Muse agentic example (muse-spark-1.3) ===");
    let key = api_key()?;
    let model = MuseClient::new(MuseConfig::spark_1_3(key))?;

    let calls = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new("muse_assistant")
        .description("Muse-powered assistant with a weather tool")
        .instruction("You answer weather questions. Always call get_weather for the city first, then summarize its result in one sentence.")
        .model(Arc::new(model))
        .tool(weather_tool(calls.clone()))
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()?;

    let question = "What is the weather in Paris?";
    println!("\nUser: {question}\n");
    let mut events = runner
        .run(UserId::new(USER)?, SessionId::new(session.id().to_string())?, Content::new("user").with_text(question))
        .await?;

    let mut final_text = String::new();
    while let Some(event) = events.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Text { text, .. } if !text.is_empty() => {
                        println!("[{}] {text}", event.author);
                        final_text.push_str(text);
                    }
                    Part::FunctionCall { name, args, .. } => {
                        println!("[{}] calls {name}({args})", event.author);
                    }
                    _ => {}
                }
            }
        }
    }

    let tool_calls = calls.load(Ordering::SeqCst);
    println!("\nTool calls: {tool_calls}");
    anyhow::ensure!(tool_calls >= 1, "expected the model to call get_weather at least once");
    anyhow::ensure!(!final_text.trim().is_empty(), "expected a non-empty final answer");
    println!("PASS: Muse Spark completed the agentic turn.");
    Ok(())
}
