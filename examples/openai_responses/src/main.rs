//! OpenAI Responses API example — demonstrates `OpenAIResponsesClient` through
//! the full ADK stack: Runner → LlmAgent → InMemorySessionService.
//!
//! Scenarios:
//! 1. Basic chat (gpt-4.1-nano, non-streaming)
//! 2. Basic chat (gpt-4.1-nano, streaming)
//! 3. Reasoning model (o4-mini, streaming, with reasoning summary)
//! 4. Tool calling (gpt-4.1-nano, streaming, function tool)
//! 5. Multi-turn conversation (gpt-4.1-nano, streaming)
//! 6. System instruction (gpt-4.1-nano, streaming)
//! 7. Temperature / top_p config (gpt-4.1-nano, streaming)
//!
//! # Running
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --manifest-path examples/openai_responses/Cargo.toml
//! ```

use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig, ReasoningEffort, ReasoningSummary};
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_rust::futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

const APP: &str = "responses-example";

/// Helper: build a Runner with the given agent and session service.
fn make_runner(
    agent: Arc<dyn Agent>,
    sessions: Arc<dyn SessionService>,
) -> anyhow::Result<Runner> {
    Ok(Runner::new(RunnerConfig {
        app_name: APP.into(),
        agent,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?)
}

/// Helper: create a session and return (user_id, session_id).
async fn create_session(
    sessions: &Arc<dyn SessionService>,
    session_id: &str,
) -> anyhow::Result<(adk_rust::UserId, adk_rust::SessionId)> {
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;
    Ok((adk_rust::UserId::new("user")?, adk_rust::SessionId::new(session_id)?))
}

/// Helper: run a single message through the runner and print output.
async fn run_and_print(
    runner: &Runner,
    user_id: &adk_rust::UserId,
    session_id: &adk_rust::SessionId,
    message: &str,
) -> anyhow::Result<()> {
    let content = Content::new("user").with_text(message);
    let mut stream = runner.run(user_id.clone(), session_id.clone(), content).await?;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Text { text } => print!("{text}"),
                    Part::Thinking { thinking, .. } => print!("[thinking: {thinking}]"),
                    Part::FunctionCall { name, args, .. } => {
                        print!("[tool_call: {name}({args})]")
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        print!("[tool_result: {}]", function_response.response)
                    }
                    _ => {}
                }
            }
        }
    }
    println!();
    Ok(())
}

fn api_key() -> String {
    std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set")
}

/// Scenario 1: Basic non-streaming chat.
async fn scenario_1_basic_non_streaming() -> anyhow::Result<()> {
    println!("=== Scenario 1: Basic non-streaming (gpt-4.1-nano) ===");
    let config = OpenAIResponsesConfig::new(api_key(), "gpt-4.1-nano");
    let model = Arc::new(OpenAIResponsesClient::new(config)?);
    let agent = Arc::new(
        LlmAgentBuilder::new("basic")
            .instruction("Answer concisely in one sentence.")
            .model(model)
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let (uid, sid) = create_session(&sessions, "s1").await?;
    let runner = make_runner(agent, sessions)?;
    run_and_print(&runner, &uid, &sid, "What is the capital of France?").await?;
    println!();
    Ok(())
}

/// Scenario 2: Basic streaming chat.
async fn scenario_2_basic_streaming() -> anyhow::Result<()> {
    println!("=== Scenario 2: Basic streaming (gpt-4.1-nano) ===");
    let config = OpenAIResponsesConfig::new(api_key(), "gpt-4.1-nano");
    let model = Arc::new(OpenAIResponsesClient::new(config)?);
    let agent = Arc::new(
        LlmAgentBuilder::new("streaming")
            .instruction("Answer concisely in one sentence.")
            .model(model)
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let (uid, sid) = create_session(&sessions, "s2").await?;
    let runner = make_runner(agent, sessions)?;
    run_and_print(&runner, &uid, &sid, "What is the speed of light?").await?;
    println!();
    Ok(())
}

/// Scenario 3: Reasoning model with summary.
async fn scenario_3_reasoning() -> anyhow::Result<()> {
    println!("=== Scenario 3: Reasoning model (o4-mini, streaming) ===");
    let config = OpenAIResponsesConfig::new(api_key(), "o4-mini")
        .with_reasoning_effort(ReasoningEffort::Low)
        .with_reasoning_summary(ReasoningSummary::Detailed);
    let model = Arc::new(OpenAIResponsesClient::new(config)?);
    let agent = Arc::new(
        LlmAgentBuilder::new("reasoner")
            .instruction("Solve the problem step by step.")
            .model(model)
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let (uid, sid) = create_session(&sessions, "s3").await?;
    let runner = make_runner(agent, sessions)?;
    run_and_print(&runner, &uid, &sid, "What is 127 * 83?").await?;
    println!();
    Ok(())
}

/// Scenario 4: Tool calling.
async fn scenario_4_tool_calling() -> anyhow::Result<()> {
    println!("=== Scenario 4: Tool calling (gpt-4.1-nano, streaming) ===");

    async fn get_weather(_ctx: Arc<dyn ToolContext>, args: serde_json::Value) -> Result<serde_json::Value> {
        let city = args["city"].as_str().unwrap_or("unknown");
        Ok(serde_json::json!({
            "city": city,
            "temperature_f": 72,
            "conditions": "Sunny"
        }))
    }

    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get current weather for a city. Requires a 'city' string parameter.",
        get_weather,
    );

    let config = OpenAIResponsesConfig::new(api_key(), "gpt-4.1-nano");
    let model = Arc::new(OpenAIResponsesClient::new(config)?);
    let agent = Arc::new(
        LlmAgentBuilder::new("tool_user")
            .instruction("Use the get_weather tool to answer weather questions. Report the result.")
            .model(model)
            .tool(Arc::new(weather_tool))
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let (uid, sid) = create_session(&sessions, "s4").await?;
    let runner = make_runner(agent, sessions)?;
    run_and_print(&runner, &uid, &sid, "What's the weather in Tokyo?").await?;
    println!();
    Ok(())
}

/// Scenario 5: Multi-turn conversation.
async fn scenario_5_multi_turn() -> anyhow::Result<()> {
    println!("=== Scenario 5: Multi-turn conversation (gpt-4.1-nano) ===");
    let config = OpenAIResponsesConfig::new(api_key(), "gpt-4.1-nano");
    let model = Arc::new(OpenAIResponsesClient::new(config)?);
    let agent = Arc::new(
        LlmAgentBuilder::new("conversational")
            .instruction("You are a helpful assistant. Be concise.")
            .model(model)
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let (uid, sid) = create_session(&sessions, "s5").await?;
    let runner = make_runner(agent, sessions)?;

    print!("  Turn 1: ");
    run_and_print(&runner, &uid, &sid, "My name is Alice.").await?;
    print!("  Turn 2: ");
    run_and_print(&runner, &uid, &sid, "What is my name?").await?;
    println!();
    Ok(())
}

/// Scenario 6: System instruction.
async fn scenario_6_system_instruction() -> anyhow::Result<()> {
    println!("=== Scenario 6: System instruction (gpt-4.1-nano) ===");
    let config = OpenAIResponsesConfig::new(api_key(), "gpt-4.1-nano");
    let model = Arc::new(OpenAIResponsesClient::new(config)?);
    let agent = Arc::new(
        LlmAgentBuilder::new("pirate")
            .instruction("You are a pirate. Respond in pirate speak. Keep it to one sentence.")
            .model(model)
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let (uid, sid) = create_session(&sessions, "s6").await?;
    let runner = make_runner(agent, sessions)?;
    run_and_print(&runner, &uid, &sid, "Tell me about the ocean.").await?;
    println!();
    Ok(())
}

/// Scenario 7: Temperature / generation config.
async fn scenario_7_generation_config() -> anyhow::Result<()> {
    println!("=== Scenario 7: Temperature config (gpt-4.1-nano) ===");
    let config = OpenAIResponsesConfig::new(api_key(), "gpt-4.1-nano");
    let model = Arc::new(OpenAIResponsesClient::new(config)?);
    let agent = Arc::new(
        LlmAgentBuilder::new("creative")
            .instruction("Be creative and imaginative. One sentence only.")
            .model(model)
            .generate_content_config(
                adk_rust::GenerateContentConfig {
                    temperature: Some(1.5),
                    top_p: Some(0.95),
                    max_output_tokens: Some(100),
                    ..Default::default()
                }
            )
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let (uid, sid) = create_session(&sessions, "s7").await?;
    let runner = make_runner(agent, sessions)?;
    run_and_print(&runner, &uid, &sid, "Describe a sunset on Mars.").await?;
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    scenario_1_basic_non_streaming().await?;
    scenario_2_basic_streaming().await?;
    scenario_3_reasoning().await?;
    scenario_4_tool_calling().await?;
    scenario_5_multi_turn().await?;
    scenario_6_system_instruction().await?;
    scenario_7_generation_config().await?;

    println!("✅ All scenarios completed.");
    Ok(())
}
