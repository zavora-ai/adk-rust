//! OpenAI Tools example with ADK.
//!
//! This example demonstrates using OpenAI with function calling (tools).
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_tools --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_tool::FunctionTool;
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CalculatorArgs {
    /// The arithmetic operation to perform
    operation: Operation,
    /// First operand
    a: f64,
    /// Second operand
    b: f64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to get weather for
    city: String,
}

async fn calculator(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let args: CalculatorArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    let result = match args.operation {
        Operation::Add => args.a + args.b,
        Operation::Subtract => args.a - args.b,
        Operation::Multiply => args.a * args.b,
        Operation::Divide => {
            if args.b == 0.0 {
                return Err(adk_core::AdkError::Tool("Division by zero".to_string()));
            }
            args.a / args.b
        }
    };

    Ok(json!({ "result": result }))
}

async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let args: WeatherArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    // Mock weather data
    let weather = match args.city.to_lowercase().as_str() {
        "paris" => json!({ "city": "Paris", "temp": 18, "condition": "Partly cloudy" }),
        "london" => json!({ "city": "London", "temp": 14, "condition": "Rainy" }),
        "tokyo" => json!({ "city": "Tokyo", "temp": 22, "condition": "Sunny" }),
        "new york" => json!({ "city": "New York", "temp": 20, "condition": "Clear" }),
        _ => json!({ "city": args.city, "temp": 15, "condition": "Unknown" }),
    };

    Ok(weather)
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    // Create calculator tool with schema
    let calc_tool = FunctionTool::new(
        "calculator",
        "Performs basic arithmetic operations (add, subtract, multiply, divide)",
        calculator,
    )
    .with_parameters_schema::<CalculatorArgs>();

    // Create weather tool with schema
    let weather_tool =
        FunctionTool::new("get_weather", "Gets the current weather for a city", get_weather)
            .with_parameters_schema::<WeatherArgs>();

    let agent = LlmAgentBuilder::new("tools_agent")
        .description("Agent that can perform calculations and get weather")
        .instruction("You are a helpful assistant with access to a calculator and weather information. Use the tools when appropriate.")
        .model(Arc::new(model))
        .tool(Arc::new(calc_tool))
        .tool(Arc::new(weather_tool))
        .build()?;

    adk_cli::console::run_console(
        Arc::new(agent),
        "openai_tools_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
