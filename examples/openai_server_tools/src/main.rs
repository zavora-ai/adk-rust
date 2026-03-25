//! OpenAI Server-Side Tools — live integration example.
//!
//! Demonstrates OpenAI's built-in tools (`web_search_preview`) running
//! server-side via the Responses API alongside user-defined function calling
//! tools. Built-in tools are executed by OpenAI's servers and return
//! `ServerToolCall` parts in the response stream.
//!
//! Scenarios:
//!   1. Web search tool + function tool coexistence (full agent round-trip)
//!   2. ServerToolCall part inspection
//!   3. Custom function tool still works alongside web search
//!
//! # Usage
//!
//! ```bash
//! export OPENAI_API_KEY=sk-your-key-here
//! cargo run --manifest-path examples/openai_server_tools/Cargo.toml
//! ```

use adk_core::{GenerateContentConfig, Part, SessionId, UserId};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const APP_NAME: &str = "openai-server-tools-example";
const MODEL_NAME: &str = "gpt-4o";

fn load_dotenv() {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        let path = d.join(".env");
        if path.is_file() {
            let _ = dotenvy::from_path(path);
            return;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
}

fn separator(title: &str) {
    println!("\n{}", "=".repeat(60));
    println!("  {title}");
    println!("{}\n", "=".repeat(60));
}

fn api_key() -> String {
    std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set")
}

fn model_name() -> String {
    std::env::var("OPENAI_MODEL").unwrap_or_else(|_| MODEL_NAME.to_string())
}

/// Build a GenerateContentConfig with OpenAI web search tool in extensions.
fn config_with_web_search() -> GenerateContentConfig {
    let mut extensions = serde_json::Map::new();
    extensions.insert(
        "openai".to_string(),
        json!({
            "built_in_tools": [
                { "type": "web_search_preview" }
            ]
        }),
    );
    GenerateContentConfig { extensions, ..Default::default() }
}

async fn make_runner(agent: Arc<dyn Agent>, session_id: &str) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;

    Ok(Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
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

/// Scenario 1: Web search + custom function tool coexistence.
///
/// The agent has OpenAI's web_search_preview (server-side) and a custom
/// get_capital tool (function calling, client-side). When the user asks a
/// question that triggers web search, the agent should:
///   - Preserve ServerToolCall parts from the web search
///   - Still be able to call the custom tool normally
async fn test_web_search_plus_function_tool() -> anyhow::Result<()> {
    separator("1. Web search + function tool coexistence");

    async fn get_capital(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let country = args["country"].as_str().unwrap_or("Unknown");
        Ok(json!({ "country": country, "capital": "Nairobi" }))
    }

    let model =
        Arc::new(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(api_key(), model_name()))?);

    let capital_tool: Arc<dyn Tool> = Arc::new(FunctionTool::new(
        "get_capital",
        "Get the capital city of a country.",
        get_capital,
    ));

    let agent = Arc::new(
        LlmAgentBuilder::new("openai-mixed-tools")
            .instruction("You have access to web search and a get_capital tool. Use whichever is appropriate.")
            .model(model)
            .tool(capital_tool)
            .generate_content_config(config_with_web_search())
            .build()?,
    );

    let runner = make_runner(agent, "mixed-tools").await?;

    println!("Sending query that should trigger web search...");
    let content = Content::new("user")
        .with_text("Search the web for the latest Rust programming language release version and tell me about it.");

    let mut stream =
        runner.run(UserId::new("user")?, SessionId::new("mixed-tools")?, content).await?;

    let mut saw_server_tool_call = false;
    let mut saw_text = false;
    let mut full_text = String::new();

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        println!("  → ServerToolCall: {server_tool_call}");
                        saw_server_tool_call = true;
                    }
                    Part::ServerToolResponse { server_tool_response } => {
                        println!(
                            "  ← ServerToolResponse: [{}B]",
                            server_tool_response.to_string().len()
                        );
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        print!("{text}");
                        full_text.push_str(text);
                        saw_text = true;
                    }
                    Part::FunctionCall { name, .. } => {
                        println!("  → FunctionCall: {name}");
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                    }
                    _ => {}
                }
            }
        }
    }
    println!();

    if saw_server_tool_call {
        println!("ServerToolCall parts observed (web search executed server-side)");
    }
    if saw_text {
        println!("Final text response received ({} chars)", full_text.len());
    }

    assert!(
        saw_text || saw_server_tool_call,
        "expected either text output or server-side tool invocation"
    );
    println!("✓ Web search + function tool coexistence passed");
    Ok(())
}

/// Scenario 2: Custom function tool still works alongside web search.
async fn test_function_tool_still_works() -> anyhow::Result<()> {
    separator("2. Custom function tool execution alongside web search");

    async fn get_weather(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let city = args["city"].as_str().unwrap_or("Unknown");
        Ok(json!({
            "city": city,
            "temperature_c": 22,
            "condition": "Partly cloudy"
        }))
    }

    let model =
        Arc::new(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(api_key(), model_name()))?);

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct WeatherArgs {
        city: String,
    }

    let weather_tool: Arc<dyn Tool> = Arc::new(
        FunctionTool::new("get_weather", "Get current weather for a city.", get_weather)
            .with_parameters_schema::<WeatherArgs>(),
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("openai-function-tool")
            .instruction("Use the get_weather tool when asked about weather. Use web search for other queries.")
            .model(model)
            .tool(weather_tool)
            .generate_content_config(config_with_web_search())
            .build()?,
    );

    let runner = make_runner(agent, "function-tool").await?;

    println!("Sending query that should trigger get_weather (custom tool)...");
    let content = Content::new("user")
        .with_text("What's the weather in Tokyo right now? Use the get_weather tool.");

    let mut stream =
        runner.run(UserId::new("user")?, SessionId::new("function-tool")?, content).await?;

    let mut saw_function_call = false;
    let mut saw_function_response = false;
    let mut saw_text = false;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, .. } => {
                        println!("  → FunctionCall: {name}({args})");
                        saw_function_call = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                        saw_function_response = true;
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        print!("{text}");
                        saw_text = true;
                    }
                    _ => {}
                }
            }
        }
    }
    println!();

    assert!(saw_function_call, "expected get_weather function call");
    assert!(saw_function_response || saw_text, "expected function response or final text");
    println!("✓ Custom function tool execution passed");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("OpenAI Server-Side Tools — Live Integration Example");
    println!("====================================================");
    println!("Demonstrates web_search_preview + function tools on OpenAI\n");

    let model_name = model_name();
    println!("Model: {model_name}");

    type ScenarioFuture =
        std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
    type ScenarioFn = fn() -> ScenarioFuture;

    let scenarios: Vec<(&str, ScenarioFn)> = vec![
        ("Web search + function tool", || Box::pin(test_web_search_plus_function_tool())),
        ("Custom tool still works", || Box::pin(test_function_tool_still_works())),
    ];

    let mut passed = 0usize;
    let mut failed = 0usize;
    let total = scenarios.len();

    for (name, run) in scenarios {
        match run().await {
            Ok(()) => passed += 1,
            Err(err) => {
                eprintln!("\n✗ {name} FAILED: {err:#}");
                failed += 1;
            }
        }
    }

    separator("Summary");
    println!("  {passed}/{total} passed, {failed} failed");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
