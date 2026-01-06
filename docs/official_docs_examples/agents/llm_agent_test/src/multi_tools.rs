//! Multi Tools - Agent with weather and calculator tools

use adk_rust::prelude::*;
use adk_rust::Launcher;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Tool 1: Weather lookup
    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get the current weather for a city. Parameters: city (string)",
        |_ctx, args| async move {
            let city = args.get("city").and_then(|v| v.as_str()).unwrap_or("unknown");
            Ok(json!({ "city": city, "temperature": "22°C", "condition": "sunny" }))
        },
    );

    // Tool 2: Calculator
    let calculator = FunctionTool::new(
        "calculate",
        "Perform arithmetic. Parameters: a (number), b (number), operation (add/subtract/multiply/divide)",
        |_ctx, args| async move {
            let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("add");
            let result = match op {
                "add" => a + b,
                "subtract" => a - b,
                "multiply" => a * b,
                "divide" => if b != 0.0 { a / b } else { 0.0 },
                _ => 0.0,
            };
            Ok(json!({ "result": result }))
        },
    );

    // Build agent with weather and calculator tools
    let agent = LlmAgentBuilder::new("multi_tool_agent")
        .instruction("You are a helpful assistant. Use tools when needed: \
                     - get_weather for weather questions \
                     - calculate for math")
        .model(Arc::new(model))
        .tool(Arc::new(weather_tool))
        .tool(Arc::new(calculator))
        .build()?;

    println!("🔧 Multi-Tool Agent Ready!");
    println!();
    println!("Try these prompts:");
    println!("  - What's 15% of 250?");
    println!("  - What's the weather in Tokyo?");
    println!();

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
