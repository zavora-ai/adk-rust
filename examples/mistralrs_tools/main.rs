//! mistral.rs function calling example.
//!
//! This example demonstrates how to use tool/function calling with mistral.rs
//! for local LLM inference.
//!
//! # Prerequisites
//!
//! Add adk-mistralrs to your Cargo.toml via git dependency:
//! ```toml
//! adk-mistralrs = { git = "https://github.com/zavora-ai/adk-rust" }
//! ```
//!
//! # Running
//!
//! ```bash
//! cargo run --example mistralrs_tools
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{Tool, ToolContext};
use adk_mistralrs::{MistralRsConfig, MistralRsModel, ModelSource};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Weather tool input parameters
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct WeatherInput {
    /// The city to get weather for
    location: String,
    /// Temperature unit (celsius or fahrenheit)
    #[serde(default = "default_unit")]
    unit: String,
}

fn default_unit() -> String {
    "celsius".to_string()
}

/// Calculator tool input parameters
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CalculatorInput {
    /// Mathematical expression to evaluate
    expression: String,
}

/// Simple weather tool that returns mock weather data
struct WeatherTool;

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Get the current weather for a location"
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "The city to get weather for"
                },
                "unit": {
                    "type": "string",
                    "enum": ["celsius", "fahrenheit"],
                    "description": "Temperature unit"
                }
            },
            "required": ["location"]
        }))
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn ToolContext>,
        input: serde_json::Value,
    ) -> adk_core::Result<serde_json::Value> {
        let params: WeatherInput = serde_json::from_value(input)?;

        // Mock weather data
        let temp = match params.unit.as_str() {
            "fahrenheit" => "72°F",
            _ => "22°C",
        };

        Ok(serde_json::json!({
            "location": params.location,
            "temperature": temp,
            "condition": "Partly cloudy",
            "humidity": "65%",
            "wind": "10 km/h"
        }))
    }
}

/// Simple calculator tool
struct CalculatorTool;

#[async_trait]
impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Evaluate a mathematical expression"
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "Mathematical expression to evaluate (e.g., '2 + 2', '10 * 5')"
                }
            },
            "required": ["expression"]
        }))
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn ToolContext>,
        input: serde_json::Value,
    ) -> adk_core::Result<serde_json::Value> {
        let params: CalculatorInput = serde_json::from_value(input)?;

        // Simple expression evaluation (in production, use a proper math parser)
        let result = eval_simple_expression(&params.expression);

        Ok(serde_json::json!({
            "expression": params.expression,
            "result": result
        }))
    }
}

/// Simple expression evaluator for basic arithmetic
fn eval_simple_expression(expr: &str) -> String {
    // Very basic evaluation - in production use a proper math library
    let expr = expr.trim();

    // Try to parse as simple binary operation
    for op in ['+', '-', '*', '/'] {
        if let Some(pos) = expr.find(op) {
            let left: f64 = expr[..pos].trim().parse().unwrap_or(0.0);
            let right: f64 = expr[pos + 1..].trim().parse().unwrap_or(0.0);

            let result = match op {
                '+' => left + right,
                '-' => left - right,
                '*' => left * right,
                '/' => {
                    if right != 0.0 {
                        left / right
                    } else {
                        return "Error: Division by zero".to_string();
                    }
                }
                _ => return "Error: Unknown operator".to_string(),
            };

            return result.to_string();
        }
    }

    // If no operator found, try to parse as number
    expr.parse::<f64>()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "Error: Invalid expression".to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("ADK mistral.rs Tools Example");
    println!("============================");
    println!();

    // Get model ID - use an instruction-tuned model for better tool calling
    let model_id = std::env::var("MISTRALRS_MODEL")
        .unwrap_or_else(|_| "microsoft/Phi-3.5-mini-instruct".to_string());

    println!("Loading model: {}", model_id);
    println!("This may take a few minutes on first run...");
    println!();

    // Create model configuration
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface(&model_id))
        .temperature(0.3) // Lower temperature for more deterministic tool calls
        .max_tokens(1024)
        .build();

    // Load the model
    let model = MistralRsModel::new(config).await?;

    println!("Model loaded successfully!");
    println!();

    // Create tools
    let weather_tool: Arc<dyn Tool> = Arc::new(WeatherTool);
    let calculator_tool: Arc<dyn Tool> = Arc::new(CalculatorTool);

    // Create an agent with tools
    let agent = LlmAgentBuilder::new("tool-assistant")
        .description("An assistant with weather and calculator tools")
        .model(Arc::new(model))
        .instruction(
            "You are a helpful assistant with access to tools. \
             Use the get_weather tool to check weather conditions. \
             Use the calculator tool for mathematical calculations. \
             Always use tools when appropriate to provide accurate information.",
        )
        .tool(weather_tool)
        .tool(calculator_tool)
        .build()?;

    println!("Available tools:");
    println!("  - get_weather: Get current weather for a location");
    println!("  - calculator: Evaluate mathematical expressions");
    println!();
    println!("Try asking:");
    println!("  - What's the weather in Tokyo?");
    println!("  - Calculate 15 * 7 + 23");
    println!("  - What's the weather in Paris and what's 100 / 4?");
    println!();

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "mistralrs_tools".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
