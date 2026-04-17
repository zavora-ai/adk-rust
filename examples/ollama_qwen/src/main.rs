//! Qwen on Ollama — Thinking + Tool Calling test.
//!
//! Tests two capabilities with Qwen models (3.5, 3.6, Coder):
//! 1. Thinking/reasoning traces (Qwen supports `<think>` blocks)
//! 2. Tool calling with function execution
//!
//! Set `OLLAMA_MODEL` to switch models (default: `qwen3.5`):
//! ```bash
//! OLLAMA_MODEL=qwen3.6:35b-a3b cargo run --manifest-path examples/ollama_qwen/Cargo.toml
//! ```

use adk_core::{Content, Part, SessionId, UserId};
use adk_model::OllamaModel;
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const APP: &str = "ollama-qwen-test";

fn model_name() -> String {
    std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen3.5".to_string())
}

async fn make_runner(agent: Arc<dyn Agent>, sid: &str) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "user".into(),
            session_id: Some(sid.into()),
            state: HashMap::new(),
        })
        .await?;
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

fn sep(title: &str) {
    println!("\n{}", "=".repeat(60));
    println!("  {title}");
    println!("{}\n", "=".repeat(60));
}

// ---------------------------------------------------------------------------
// Scenario 1: Thinking / Reasoning
// ---------------------------------------------------------------------------

async fn test_thinking() -> anyhow::Result<()> {
    sep("Scenario 1: Thinking / Reasoning");

    let model = Arc::new(OllamaModel::from_model(&model_name())?);
    let agent = Arc::new(
        LlmAgentBuilder::new("thinker")
            .instruction("You are a reasoning assistant. Think step by step.")
            .model(model)
            .build()?,
    );
    let runner: Runner = make_runner(agent, "think-test").await?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new("think-test")?,
            Content::new("user")
                .with_text("What is 17 * 23? Think step by step before answering."),
        )
        .await?;

    let mut saw_thinking = false;
    let mut saw_text = false;
    let mut in_thinking = false;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, .. } => {
                        if !in_thinking {
                            print!("  💭 ");
                            in_thinking = true;
                        }
                        print!("{thinking}");
                        saw_thinking = true;
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        if in_thinking {
                            println!("\n");
                            in_thinking = false;
                        }
                        print!("{text}");
                        saw_text = true;
                    }
                    _ => {}
                }
            }
        }
    }
    if in_thinking {
        println!();
    }
    println!();

    if saw_thinking {
        println!("\n  ✅ Thinking traces received!");
    } else {
        println!("\n  ⚠ No thinking traces (model may not emit them for this prompt)");
    }
    if saw_text {
        println!("  ✅ Text response received!");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 2: Tool Calling
// ---------------------------------------------------------------------------

async fn test_tool_calling() -> anyhow::Result<()> {
    sep("Scenario 2: Tool Calling");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct WeatherArgs {
        /// City name
        city: String,
    }

    async fn get_weather(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let city = args["city"].as_str().unwrap_or("unknown");
        println!("  🌤️  [get_weather] called with city={city}");
        Ok(json!({
            "city": city,
            "temperature": "22°C",
            "conditions": "sunny",
            "humidity": "45%"
        }))
    }

    let weather_tool: Arc<dyn Tool> = Arc::new(
        FunctionTool::new("get_weather", "Get current weather for a city.", get_weather)
            .with_parameters_schema::<WeatherArgs>(),
    );

    let model = Arc::new(OllamaModel::from_model(&model_name())?);
    let agent = Arc::new(
        LlmAgentBuilder::new("tool-caller")
            .instruction(
                "You are a weather assistant. Use the get_weather tool to answer weather questions. \
                 Always use the tool, never guess.",
            )
            .model(model)
            .tool(weather_tool)
            .build()?,
    );
    let runner: Runner = make_runner(agent, "tool-test").await?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new("tool-test")?,
            Content::new("user").with_text("What's the weather in Tokyo?"),
        )
        .await?;

    let mut saw_tool_call = false;
    let mut saw_tool_response = false;
    let mut saw_final_text = false;
    let mut in_thinking = false;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, .. } => {
                        if !in_thinking {
                            print!("  💭 ");
                            in_thinking = true;
                        }
                        print!("{thinking}");
                    }
                    Part::FunctionCall { name, args, .. } => {
                        if in_thinking {
                            println!();
                            in_thinking = false;
                        }
                        println!("  → FunctionCall: {name}({args})");
                        saw_tool_call = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.name);
                        saw_tool_response = true;
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        if in_thinking {
                            println!("\n");
                            in_thinking = false;
                        }
                        print!("{text}");
                        saw_final_text = true;
                    }
                    _ => {}
                }
            }
        }
    }
    if in_thinking {
        println!();
    }
    println!();

    println!();
    println!(
        "  {} Tool call",
        if saw_tool_call { "✅" } else { "❌" }
    );
    println!(
        "  {} Tool response",
        if saw_tool_response { "✅" } else { "❌" }
    );
    println!(
        "  {} Final text answer",
        if saw_final_text { "✅" } else { "❌" }
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("{} on Ollama — Thinking + Tool Calling Test", model_name());
    println!("==================================================\n");

    if let Err(e) = test_thinking().await {
        eprintln!("✗ Thinking test failed: {e:#}");
    }

    if let Err(e) = test_tool_calling().await {
        eprintln!("✗ Tool calling test failed: {e:#}");
    }

    if let Err(e) = test_openai_compat_tool_calling().await {
        eprintln!("✗ OpenAI-compat tool calling test failed: {e:#}");
    }

    println!("\nDone.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 3: Tool Calling via OpenAI-compatible endpoint
// Tests the text-based tool call parser (user issue: Qwen on HuggingFace)
// ---------------------------------------------------------------------------

async fn test_openai_compat_tool_calling() -> anyhow::Result<()> {
    sep("Scenario 3: Tool Calling via OpenAI-compat endpoint (text parser)");
    println!("  This scenario uses the OpenAI-compatible /v1 endpoint instead of");
    println!("  Ollama's native API. When the model emits <tool_call> XML tags in");
    println!("  text, ADK-Rust's text-based tool call parser automatically detects");
    println!("  and converts them to Part::FunctionCall — no user code changes needed.");
    println!("  Supports: Qwen, Llama, DeepSeek, Gemma 4, Mistral Nemo, action tags.\n");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct WeatherArgs {
        city: String,
    }

    async fn get_weather(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let city = args["city"].as_str().unwrap_or("unknown");
        println!("  🌤️  [get_weather] called with city={city}");
        Ok(json!({
            "city": city,
            "temperature": "18°C",
            "conditions": "cloudy"
        }))
    }

    let weather_tool: Arc<dyn Tool> = Arc::new(
        FunctionTool::new("get_weather", "Get current weather for a city.", get_weather)
            .with_parameters_schema::<WeatherArgs>(),
    );

    // Use OpenAI-compatible endpoint (Ollama exposes this at /v1)
    use adk_model::openai::{OpenAIClient, OpenAIConfig};
    let config = OpenAIConfig::compatible("ollama", "http://localhost:11434/v1", &model_name());
    let model = Arc::new(OpenAIClient::new(config)?);

    let agent = Arc::new(
        LlmAgentBuilder::new("openai-compat-tool-caller")
            .instruction(
                "You are a weather assistant. Use the get_weather tool to answer weather questions. \
                 Always use the tool, never guess.",
            )
            .model(model)
            .tool(weather_tool)
            .build()?,
    );
    let runner: Runner = make_runner(agent, "openai-compat-test").await?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new("openai-compat-test")?,
            Content::new("user").with_text("What's the weather in Paris?"),
        )
        .await?;

    let mut saw_tool_call = false;
    let mut saw_tool_response = false;
    let mut saw_final_text = false;
    let mut in_thinking = false;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, .. } => {
                        if !in_thinking {
                            print!("  💭 ");
                            in_thinking = true;
                        }
                        print!("{thinking}");
                    }
                    Part::FunctionCall { name, args, .. } => {
                        if in_thinking {
                            println!();
                            in_thinking = false;
                        }
                        println!("  → FunctionCall: {name}({args})");
                        saw_tool_call = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.name);
                        saw_tool_response = true;
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        if in_thinking {
                            println!("\n");
                            in_thinking = false;
                        }
                        print!("{text}");
                        saw_final_text = true;
                    }
                    _ => {}
                }
            }
        }
    }
    if in_thinking {
        println!();
    }
    println!();

    println!();
    println!(
        "  {} Tool call (via text parser)",
        if saw_tool_call { "✅" } else { "❌" }
    );
    println!(
        "  {} Tool response",
        if saw_tool_response { "✅" } else { "❌" }
    );
    println!(
        "  {} Final text answer",
        if saw_final_text { "✅" } else { "❌" }
    );

    Ok(())
}
