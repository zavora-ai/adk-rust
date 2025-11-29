//! Validates: docs/official_docs/tools/function-tools.md
//!
//! This example demonstrates creating custom function tools as documented
//! in the Function Tools documentation page.
//!
//! Run modes:
//!   cargo run --example function_tool -p adk-rust-guide              # Validation mode
//!   cargo run --example function_tool -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example function_tool -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// Parameter schema for the calculator tool
#[derive(JsonSchema, Serialize, Deserialize)]
struct CalculatorParams {
    /// The arithmetic operation to perform: add, subtract, multiply, or divide
    operation: String,
    /// First number
    a: f64,
    /// Second number
    b: f64,
}

/// Parameter schema for the temperature converter tool
#[derive(JsonSchema, Serialize, Deserialize)]
struct TemperatureParams {
    /// The temperature value to convert
    value: f64,
    /// Source unit: celsius, fahrenheit, or kelvin
    from: String,
    /// Target unit: celsius, fahrenheit, or kelvin
    to: String,
}

/// Calculator tool handler - demonstrates parameter handling
async fn calculator(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    // Extract parameters from JSON args
    let operation = args
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("add");
    let a = args
        .get("a")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| AdkError::Tool("Parameter 'a' is required".into()))?;
    let b = args
        .get("b")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| AdkError::Tool("Parameter 'b' is required".into()))?;

    // Perform the calculation
    let result = match operation {
        "add" => a + b,
        "subtract" => a - b,
        "multiply" => a * b,
        "divide" => {
            if b == 0.0 {
                return Err(AdkError::Tool("Cannot divide by zero".into()));
            }
            a / b
        }
        _ => return Err(AdkError::Tool(format!("Unknown operation: {}", operation))),
    };

    // Return structured JSON response
    Ok(json!({
        "result": result,
        "expression": format!("{} {} {} = {}", a, operation, b, result)
    }))
}

/// Unit converter tool handler - demonstrates another tool pattern
async fn convert_temperature(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    let value = args
        .get("value")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| AdkError::Tool("Parameter 'value' is required".into()))?;
    let from = args
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AdkError::Tool("Parameter 'from' is required".into()))?;
    let to = args
        .get("to")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AdkError::Tool("Parameter 'to' is required".into()))?;

    let result = match (from, to) {
        ("celsius", "fahrenheit") => value * 9.0 / 5.0 + 32.0,
        ("fahrenheit", "celsius") => (value - 32.0) * 5.0 / 9.0,
        ("celsius", "kelvin") => value + 273.15,
        ("kelvin", "celsius") => value - 273.15,
        _ => {
            return Err(AdkError::Tool(format!(
                "Cannot convert from {} to {}",
                from, to
            )))
        }
    };

    Ok(json!({
        "original": { "value": value, "unit": from },
        "converted": { "value": result, "unit": to }
    }))
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment
    let api_key = init_env();

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create function tools with parameter schemas for better LLM understanding
    let calc_tool = FunctionTool::new(
        "calculator",
        "Perform arithmetic operations (add, subtract, multiply, divide) on two numbers",
        calculator,
    )
    .with_parameters_schema::<CalculatorParams>();

    let temp_tool = FunctionTool::new(
        "convert_temperature",
        "Convert temperature between celsius, fahrenheit, and kelvin",
        convert_temperature,
    )
    .with_parameters_schema::<TemperatureParams>();

    // Build agent with multiple custom tools
    let agent = LlmAgentBuilder::new("math_helper")
        .description("A helpful assistant for math and unit conversions")
        .instruction(
            "You are a math helper. Use the calculator tool for arithmetic operations \
             and the convert_temperature tool for temperature conversions. \
             Always show your work and explain the results.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(calc_tool))
        .tool(Arc::new(temp_tool))
        .build()?;

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        // Validation mode - verify the tools and agent were created correctly
        print_validating("tools/function-tools.md");

        println!("Agent name: {}", agent.name());
        println!("Agent description: {}", agent.description());

        // Verify the agent was built successfully with tools
        assert_eq!(agent.name(), "math_helper");
        assert!(!agent.description().is_empty());

        print_success("function_tool");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example function_tool -p adk-rust-guide -- chat");
        println!(
            "\nTry asking: 'What is 15 multiplied by 7?' or 'Convert 100 celsius to fahrenheit'"
        );
    }

    Ok(())
}
