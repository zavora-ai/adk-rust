//! Groq Tools Example
//!
//! Demonstrates function calling with Groq's ultra-fast inference.
//!
//! Run: GROQ_API_KEY=your_key cargo run --example groq_tools --features groq

use adk_agent::LlmAgentBuilder;
use adk_core::{AdkError, Result, ToolContext};
use adk_model::groq::{GroqClient, GroqConfig};
use adk_tool::FunctionTool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CalculatorArgs {
    /// The mathematical operation to perform
    operation: String,
    /// The first number
    a: f64,
    /// The second number
    b: f64,
}

async fn calculator(_ctx: Arc<dyn ToolContext>, input: Value) -> Result<Value> {
    let args: CalculatorArgs = serde_json::from_value(input)
        .map_err(|e| AdkError::Tool(format!("Invalid arguments: {}", e)))?;

    let result = match args.operation.as_str() {
        "add" => args.a + args.b,
        "subtract" => args.a - args.b,
        "multiply" => args.a * args.b,
        "divide" => {
            if args.b == 0.0 {
                return Err(AdkError::Tool("Division by zero".to_string()));
            }
            args.a / args.b
        }
        _ => return Err(AdkError::Tool(format!("Unsupported operation: {}", args.operation))),
    };

    Ok(json!({ "result": result }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file
    let _ = dotenvy::dotenv();

    println!("Groq Tools Example");
    println!("==================\n");

    let api_key = std::env::var("GROQ_API_KEY").expect("GROQ_API_KEY must be set");

    // Use llama-3.3-70b which supports tool calling well
    let model_name =
        std::env::var("GROQ_MODEL").unwrap_or_else(|_| "llama-3.3-70b-versatile".to_string());

    println!("Using model: {}\n", model_name);

    let config = GroqConfig::new(&api_key, &model_name);
    let model = GroqClient::new(config)?;

    let calc_tool = FunctionTool::new(
        "calculator",
        "Performs basic arithmetic operations (add, subtract, multiply, divide)",
        calculator,
    )
    .with_parameters_schema::<CalculatorArgs>();

    let agent = LlmAgentBuilder::new("groq-calculator")
        .description("A calculator assistant powered by Groq")
        .instruction(
            "You are a math assistant. Use the calculator tool for arithmetic operations. \
             Explain your calculations clearly.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(calc_tool))
        .build()?;

    adk_cli::console::run_console(Arc::new(agent), "groq_tools".to_string(), "user1".to_string())
        .await?;

    Ok(())
}
