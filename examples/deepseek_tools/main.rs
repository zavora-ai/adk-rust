//! DeepSeek Function Calling (Tool Use) Example
//!
//! This example demonstrates using DeepSeek with function calling / tool use.
//! The agent can call tools to get real-time information like weather data.
//!
//! Set DEEPSEEK_API_KEY environment variable before running:
//! ```bash
//! cargo run --example deepseek_tools --features deepseek
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_tool::FunctionTool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

/// Arguments for the weather tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city and country, e.g. 'Tokyo, Japan'
    location: String,
}

/// Arguments for the calculator tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CalculatorArgs {
    /// The arithmetic operation to perform
    operation: CalculatorOperation,
    /// First operand
    a: f64,
    /// Second operand
    b: f64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum CalculatorOperation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// Get weather for a location (mock data).
async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let args: WeatherArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    let location = args.location.to_lowercase();

    // Mock weather data based on location
    let (temp, condition) = if location.contains("tokyo") {
        (22, "Partly cloudy")
    } else if location.contains("london") {
        (15, "Rainy")
    } else if location.contains("new york") {
        (18, "Sunny")
    } else if location.contains("paris") {
        (17, "Cloudy")
    } else if location.contains("sydney") {
        (25, "Clear")
    } else {
        (20, "Unknown")
    };

    Ok(json!({
        "location": args.location,
        "temperature_celsius": temp,
        "condition": condition,
        "humidity": 65,
        "wind_speed_kmh": 12
    }))
}

/// Perform basic arithmetic.
async fn calculate(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let args: CalculatorArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    let result = match args.operation {
        CalculatorOperation::Add => args.a + args.b,
        CalculatorOperation::Subtract => args.a - args.b,
        CalculatorOperation::Multiply => args.a * args.b,
        CalculatorOperation::Divide => {
            if args.b == 0.0 {
                return Err(adk_core::AdkError::Tool("Division by zero".to_string()));
            }
            args.a / args.b
        }
    };

    Ok(json!({
        "a": args.a,
        "b": args.b,
        "result": result
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set");

    // Create DeepSeek client
    let model = DeepSeekClient::new(DeepSeekConfig::chat(api_key))?;

    // Create weather tool with schema
    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get current weather information for a given location",
        get_weather,
    )
    .with_parameters_schema::<WeatherArgs>();

    // Create calculator tool with schema
    let calculator_tool = FunctionTool::new(
        "calculate",
        "Perform basic arithmetic calculations. Supports add, subtract, multiply, divide.",
        calculate,
    )
    .with_parameters_schema::<CalculatorArgs>();

    // Build agent with tools
    let agent = LlmAgentBuilder::new("tool_assistant")
        .model(Arc::new(model))
        .instruction(
            "You are a helpful assistant with access to weather and calculator tools. \
             Use them when appropriate to answer user questions accurately.",
        )
        .tool(Arc::new(weather_tool))
        .tool(Arc::new(calculator_tool))
        .build()?;

    println!("=== DeepSeek Function Calling Demo ===\n");
    println!("This agent can use tools to get weather and do calculations.\n");

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "deepseek_tools".to_string(),
        "user_1".to_string(),
    )
    .await?;

    Ok(())
}
