//! Live LLM demo showcasing all developer ergonomics features.
//!
//! This demo builds a real agent with multiple tools and runs it against
//! a live LLM to demonstrate:
//!
//! 1. **RunnerConfigBuilder** — typestate builder for Runner construction
//! 2. **ToolExecutionStrategy::Auto** — read-only tools run concurrently
//! 3. **Tool metadata** — `with_read_only(true)` on lookup tools
//! 4. **StatefulTool<S>** — shared counter state across invocations
//! 5. **SimpleToolContext** — tool execution outside the agent loop
//! 6. **Runner::run_str()** — string convenience method
//!
//! Usage:
//!   export GOOGLE_API_KEY=your-key
//!   cargo run --manifest-path examples/developer_ergonomics/Cargo.toml --bin llm_demo
//!
//! Or with OpenAI:
//!   export OPENAI_API_KEY=sk-your-key
//!   cargo run --manifest-path examples/developer_ergonomics/Cargo.toml --bin llm_demo

use adk_core::{Content, Part, Tool, ToolContext, ToolExecutionStrategy};
use adk_runner::Runner;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_tool::{FunctionTool, SimpleToolContext, StatefulTool, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared state for the request counter tool.
struct RequestCounter {
    count: RwLock<u64>,
}

// ---------------------------------------------------------------------------
// #[tool] macro — works alongside FunctionTool and StatefulTool
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct ConvertArgs {
    /// Temperature in Celsius
    celsius: f64,
}

/// Convert a temperature from Celsius to Fahrenheit.
#[tool(read_only, concurrency_safe)]
async fn convert_temp(
    args: ConvertArgs,
) -> std::result::Result<serde_json::Value, adk_core::AdkError> {
    let f = args.celsius * 9.0 / 5.0 + 32.0;
    Ok(json!({"celsius": args.celsius, "fahrenheit": f}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run().await
}

async fn run() -> anyhow::Result<()> {
    load_dotenv();

    println!("=== Developer Ergonomics — Live LLM Demo ===\n");

    // ---------------------------------------------------------------
    // 1. Create tools showcasing metadata and StatefulTool
    // ---------------------------------------------------------------

    // Read-only tool: city info lookup (safe for concurrent dispatch)
    let city_info = FunctionTool::new(
        "get_city_info",
        "Get population and country for a city. Parameters: city (string)",
        |_ctx, args| async move {
            let city = args["city"].as_str().unwrap_or("unknown");
            let info = match city.to_lowercase().as_str() {
                "tokyo" => json!({"city": "Tokyo", "country": "Japan", "population": "14M"}),
                "paris" => json!({"city": "Paris", "country": "France", "population": "2.1M"}),
                "new york" => json!({"city": "New York", "country": "USA", "population": "8.3M"}),
                "london" => json!({"city": "London", "country": "UK", "population": "8.8M"}),
                _ => json!({"city": city, "country": "Unknown", "population": "N/A"}),
            };
            Ok(info)
        },
    )
    .with_read_only(true) // ← enables concurrent dispatch in Auto mode
    .with_concurrency_safe(true);

    // Read-only tool: weather lookup (also safe for concurrent dispatch)
    let weather = FunctionTool::new(
        "get_weather",
        "Get current weather for a city. Parameters: city (string)",
        |_ctx, args| async move {
            let city = args["city"].as_str().unwrap_or("unknown");
            let weather = match city.to_lowercase().as_str() {
                "tokyo" => json!({"city": "Tokyo", "temp": "18°C", "condition": "cloudy"}),
                "paris" => json!({"city": "Paris", "temp": "14°C", "condition": "rainy"}),
                "new york" => json!({"city": "New York", "temp": "22°C", "condition": "sunny"}),
                "london" => json!({"city": "London", "temp": "12°C", "condition": "foggy"}),
                _ => json!({"city": city, "temp": "20°C", "condition": "clear"}),
            };
            Ok(weather)
        },
    )
    .with_read_only(true)
    .with_concurrency_safe(true);

    // StatefulTool: request counter (mutable — runs after read-only batch)
    let counter_state = Arc::new(RequestCounter { count: RwLock::new(0) });
    let counter_state_ref = counter_state.clone();

    let request_counter = StatefulTool::new(
        "log_request",
        "Log this request and return the running request count. No parameters needed.",
        counter_state.clone(),
        |state: Arc<RequestCounter>, _ctx: Arc<dyn ToolContext>, _args| async move {
            let mut count = state.count.write().await;
            *count += 1;
            Ok(json!({"request_number": *count}))
        },
    );
    // Not read_only — this mutates state, so it runs sequentially after the read-only batch

    println!("Tools created:");
    println!("  • get_city_info  (read_only: {}, FunctionTool)", city_info.is_read_only());
    println!("  • get_weather    (read_only: {}, FunctionTool)", weather.is_read_only());
    println!("  • log_request    (read_only: {}, StatefulTool)", request_counter.is_read_only());
    println!("  • convert_temp   (read_only: {}, #[tool] macro)", ConvertTemp.is_read_only());

    // ---------------------------------------------------------------
    // 2. Detect LLM provider and build model
    // ---------------------------------------------------------------

    let model = provider_from_env()?;
    println!("\nModel: auto-detected via provider_from_env()");

    // ---------------------------------------------------------------
    // 3. Build agent with tool_execution_strategy
    // ---------------------------------------------------------------

    let agent = LlmAgentBuilder::new("travel_assistant")
        .description("A travel assistant that looks up city info and weather")
        .instruction(
            "You are a helpful travel assistant. When the user asks about a city, \
             use get_city_info AND get_weather to gather data, then use log_request \
             to track the query. If the user asks about temperature conversion, \
             use convert_temp. Combine all results into a concise answer. \
             Always call all relevant tools for each question.",
        )
        .model(model)
        .tool(Arc::new(city_info))
        .tool(Arc::new(weather))
        .tool(Arc::new(request_counter))
        .tool(Arc::new(ConvertTemp)) // #[tool] macro-generated struct
        .tool_execution_strategy(ToolExecutionStrategy::Auto) // ← read-only tools run concurrently
        .max_iterations(10)
        .build()?;

    println!("Agent: travel_assistant (strategy: Auto)");

    // ---------------------------------------------------------------
    // 4. Build Runner using the typestate builder
    // ---------------------------------------------------------------

    let session_service = Arc::new(InMemorySessionService::new());

    let runner = Runner::builder()
        .app_name("ergonomics-llm-demo")
        .agent(Arc::new(agent))
        .session_service(session_service.clone())
        .build()?;

    println!("Runner built via typestate builder ✓\n");

    // ---------------------------------------------------------------
    // 5. Pre-create session and run with run_str()
    // ---------------------------------------------------------------

    session_service
        .create(CreateRequest {
            app_name: "ergonomics-llm-demo".to_string(),
            user_id: "demo-user".to_string(),
            session_id: Some("demo-session".to_string()),
            state: Default::default(),
        })
        .await?;

    // First query
    println!("--- Query 1: \"Tell me about Tokyo\" ---\n");
    run_query(&runner, "Tell me about Tokyo").await?;
    println!("\n  [Request counter: {}]\n", *counter_state_ref.count.read().await);

    // Second query — counter should increment
    println!("--- Query 2: \"What about Paris?\" ---\n");
    run_query(&runner, "What about Paris?").await?;
    println!("\n  [Request counter: {}]\n", *counter_state_ref.count.read().await);

    // Third query — exercises the #[tool] macro-generated convert_temp
    println!("--- Query 3: \"Convert 25 Celsius to Fahrenheit\" ---\n");
    run_query(&runner, "Convert 25 degrees Celsius to Fahrenheit using the convert_temp tool")
        .await?;
    println!();

    // ---------------------------------------------------------------
    // 6. Demonstrate SimpleToolContext — call a tool outside the agent
    // ---------------------------------------------------------------

    println!("--- SimpleToolContext: direct tool call outside agent loop ---\n");

    let standalone_tool =
        FunctionTool::new("standalone_greet", "Greet someone", |ctx, args| async move {
            let name = args["name"].as_str().unwrap_or("world");
            Ok(json!({
                "greeting": format!("Hello, {}!", name),
                "called_by": ctx.agent_name(),
                "invocation_id": ctx.invocation_id().to_string(),
            }))
        });

    let simple_ctx: Arc<dyn ToolContext> =
        Arc::new(SimpleToolContext::new("direct-caller").with_function_call_id("manual-call-001"));
    let result = standalone_tool.execute(simple_ctx, json!({"name": "Developer"})).await?;
    println!("  Result: {}", serde_json::to_string_pretty(&result)?);

    println!("\n=== Demo complete ===");
    Ok(())
}

/// Run a single query through the runner using run_str() and print the response.
async fn run_query(runner: &Runner, question: &str) -> anyhow::Result<()> {
    use adk_rust::futures::StreamExt;

    let mut stream = runner
        .run_str("demo-user", "demo-session", Content::new("user").with_text(question))
        .await?;

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(ref content) = event.llm_response.content {
                    for part in &content.parts {
                        match part {
                            Part::Text { text } => print!("{text}"),
                            Part::FunctionCall { name, args, .. } => {
                                println!("  [tool call: {name}({args})]");
                            }
                            Part::FunctionResponse { function_response, .. } => {
                                println!(
                                    "  [tool result: {} → {}]",
                                    function_response.name, function_response.response
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
            Err(e) => eprintln!("  Error: {e}"),
        }
    }
    Ok(())
}

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
