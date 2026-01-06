//! Multi-tool agent example
//!
//! Run: cargo run --bin multi_tool

use adk_rust::prelude::*;
use adk_rust::Launcher;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize)]
struct CalcParams {
    /// The operation: add, subtract, multiply, divide
    operation: String,
    /// First number
    a: f64,
    /// Second number
    b: f64,
}

#[derive(JsonSchema, Serialize, Deserialize)]
struct ConvertParams {
    /// The value to convert
    value: f64,
    /// Source unit (celsius, fahrenheit, km, miles)
    from: String,
    /// Target unit (celsius, fahrenheit, km, miles)
    to: String,
}

#[derive(JsonSchema, Serialize, Deserialize)]
struct WeatherParams {
    /// The city or location
    location: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Tool 1: Calculator
    let calc_tool = FunctionTool::new(
        "calculator",
        "Perform arithmetic operations",
        |_ctx, args| async move {
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("add");
            let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let result = match op {
                "add" => a + b,
                "subtract" => a - b,
                "multiply" => a * b,
                "divide" if b != 0.0 => a / b,
                "divide" => return Err(adk_core::AdkError::Tool("Cannot divide by zero".into())),
                _ => return Err(adk_core::AdkError::Tool(format!("Unknown operation: {}", op))),
            };
            Ok(json!({ "result": result }))
        },
    )
    .with_parameters_schema::<CalcParams>();

    // Tool 2: Unit converter
    let convert_tool = FunctionTool::new(
        "convert_units",
        "Convert between temperature or distance units",
        |_ctx, args| async move {
            let value = args.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let from = args.get("from").and_then(|v| v.as_str()).unwrap_or("");
            let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let result = match (from, to) {
                ("celsius", "fahrenheit") => value * 9.0 / 5.0 + 32.0,
                ("fahrenheit", "celsius") => (value - 32.0) * 5.0 / 9.0,
                ("km", "miles") => value * 0.621371,
                ("miles", "km") => value / 0.621371,
                _ => return Err(adk_core::AdkError::Tool(format!("Cannot convert {} to {}", from, to))),
            };
            Ok(json!({ "result": result, "from": from, "to": to }))
        },
    )
    .with_parameters_schema::<ConvertParams>();

    // Tool 3: Weather
    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get current weather for a location",
        |_ctx, args| async move {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("unknown");
            Ok(json!({ "location": location, "temperature": "22°C", "conditions": "sunny" }))
        },
    )
    .with_parameters_schema::<WeatherParams>();

    let agent = LlmAgentBuilder::new("multi_tool_agent")
        .description("A helpful assistant with calculator, converter, and weather tools")
        .instruction("Help users with calculations, unit conversions, and weather. Always use the appropriate tool.")
        .model(Arc::new(model))
        .tool(Arc::new(calc_tool))
        .tool(Arc::new(convert_tool))
        .tool(Arc::new(weather_tool))
        .build()?;

    println!("✅ Multi-tool agent ready with 3 tools: calculator, convert_units, get_weather");
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
