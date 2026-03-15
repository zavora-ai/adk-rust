//! Anthropic Tools Example
//!
//! This example demonstrates function calling with Anthropic's Claude models.
//!
//! Set ANTHROPIC_API_KEY environment variable before running:
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_tools --features anthropic
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_tool::FunctionTool;
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to get weather for
    city: String,
    /// Temperature unit (celsius or fahrenheit)
    #[serde(default = "default_unit")]
    unit: String,
}

fn default_unit() -> String {
    "celsius".to_string()
}

async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let args: WeatherArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    // Simulate weather data
    let temp = match args.city.to_lowercase().as_str() {
        "tokyo" => 22,
        "london" => 15,
        "new york" => 18,
        "sydney" => 25,
        "paris" => 17,
        _ => 20,
    };

    let temp_display = if args.unit == "fahrenheit" {
        format!("{}°F", (temp * 9 / 5) + 32)
    } else {
        format!("{}°C", temp)
    };

    Ok(json!({
        "city": args.city,
        "temperature": temp_display,
        "conditions": "Partly cloudy",
        "humidity": "65%",
        "wind": "10 km/h"
    }))
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CalculatorArgs {
    /// The arithmetic operation (add, subtract, multiply, divide)
    operation: String,
    /// First number
    a: f64,
    /// Second number
    b: f64,
}

async fn calculator(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let args: CalculatorArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    let result = match args.operation.as_str() {
        "add" => args.a + args.b,
        "subtract" => args.a - args.b,
        "multiply" => args.a * args.b,
        "divide" => {
            if args.b == 0.0 {
                return Err(adk_core::AdkError::Tool("Division by zero".to_string()));
            }
            args.a / args.b
        }
        _ => {
            return Err(adk_core::AdkError::Tool(format!("Unknown operation: {}", args.operation)));
        }
    };

    Ok(json!({
        "operation": args.operation,
        "a": args.a,
        "b": args.b,
        "result": result
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-5-20250929"))?;

    // Create weather tool with schema
    let weather_tool =
        FunctionTool::new("get_weather", "Get current weather information for a city", get_weather)
            .with_parameters_schema::<WeatherArgs>();

    // Create calculator tool with schema
    let calc_tool = FunctionTool::new(
        "calculator",
        "Perform basic arithmetic operations (add, subtract, multiply, divide)",
        calculator,
    )
    .with_parameters_schema::<CalculatorArgs>();

    let agent = LlmAgentBuilder::new("claude_with_tools")
        .description("Claude assistant with weather and calculator tools")
        .model(Arc::new(model))
        .instruction(
            "You are Claude, a helpful AI assistant with access to tools. \
             Use the get_weather tool to check weather in cities. \
             Use the calculator tool for math operations. \
             Always use the appropriate tool when the user asks about weather or math.",
        )
        .tool(Arc::new(weather_tool))
        .tool(Arc::new(calc_tool))
        .build()?;

    println!("=== Anthropic Claude Tools Example ===");
    println!("Model: claude-sonnet-4-5-20250929");
    println!();
    println!("Available tools:");
    println!("  - get_weather: Get weather for a city");
    println!("  - calculator: Basic math operations");
    println!();
    println!("Try asking:");
    println!("  - 'What's the weather in Tokyo?'");
    println!("  - 'Calculate 25 * 4'");
    println!();

    adk_cli::console::run_console(
        Arc::new(agent),
        "anthropic_tools_example".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
