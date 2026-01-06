//! Complete Example - Production-ready agent with multiple tools

use adk_rust::prelude::*;
use adk_rust::Launcher;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Weather tool
    let weather = FunctionTool::new(
        "get_weather",
        "Get weather for a city. Parameters: city (string)",
        |_ctx, args| async move {
            let city = args.get("city").and_then(|v| v.as_str()).unwrap_or("unknown");
            Ok(json!({
                "city": city,
                "temperature": "22°C",
                "humidity": "65%",
                "condition": "partly cloudy"
            }))
        },
    );

    // Calculator tool
    let calc = FunctionTool::new(
        "calculate",
        "Math operations. Parameters: expression (string like '2 + 2')",
        |_ctx, args| async move {
            let expr = args.get("expression").and_then(|v| v.as_str()).unwrap_or("0");
            // Simple expression parsing for demo
            let result = if expr.contains('+') {
                let parts: Vec<&str> = expr.split('+').collect();
                if parts.len() == 2 {
                    let a: f64 = parts[0].trim().parse().unwrap_or(0.0);
                    let b: f64 = parts[1].trim().parse().unwrap_or(0.0);
                    a + b
                } else { 0.0 }
            } else if expr.contains('*') {
                let parts: Vec<&str> = expr.split('*').collect();
                if parts.len() == 2 {
                    let a: f64 = parts[0].trim().parse().unwrap_or(0.0);
                    let b: f64 = parts[1].trim().parse().unwrap_or(0.0);
                    a * b
                } else { 0.0 }
            } else {
                0.0
            };
            Ok(json!({ "expression": expr, "result": result }))
        },
    );

    // Build the full agent
    let agent = LlmAgentBuilder::new("assistant")
        .description("A helpful assistant with weather and calculation abilities")
        .instruction("You are a helpful assistant. \
                     Use the weather tool for weather questions. \
                     Use the calculator for math. \
                     Be concise and friendly.")
        .model(Arc::new(model))
        .tool(Arc::new(weather))
        .tool(Arc::new(calc))
        // .tool(Arc::new(GoogleSearchTool::new()))  // Currently unsupported with FunctionTool
        .output_key("last_response")
        .build()?;

    println!("✅ Agent '{}' ready!", agent.name());
    println!();
    println!("Try these prompts:");
    println!("  - What's 25 times 4?");
    println!("  - How's the weather in New York?");
    println!("  - Calculate 15% tip on $85");
    println!();

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
