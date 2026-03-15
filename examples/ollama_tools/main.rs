//! Ollama with function tool example
//!
//! This example demonstrates using Ollama with function tools.
//! Requires Ollama running locally with a model that supports tool calling
//! (e.g., llama3.1, llama3.2, qwen2.5).

use adk_agent::LlmAgentBuilder;
use adk_core::{AdkError, Result, ToolContext};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_tool::FunctionTool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

/// Calculator input parameters
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CalculatorArgs {
    /// The operation to perform: add, subtract, multiply, divide
    operation: String,
    /// First number
    a: f64,
    /// Second number
    b: f64,
}

/// Simple calculator tool handler
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
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("ADK Ollama Tools Example");
    println!("---------------------------");
    println!("Demonstrates function calling with a local Ollama model.\n");

    // Create Ollama model
    // Models that support tools: llama3.1, llama3.2, qwen2.5, mistral, etc.
    let model_name = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".to_string());
    println!("Using model: {}", model_name);
    println!("Make sure Ollama is running: ollama serve");
    println!("And the model is pulled: ollama pull {}\n", model_name);

    let config = OllamaConfig::new(&model_name);
    let model = OllamaModel::new(config)?;

    // Create calculator tool
    let calc_tool = FunctionTool::new(
        "calculator",
        "Performs basic arithmetic operations (add, subtract, multiply, divide)",
        calculator,
    )
    .with_parameters_schema::<CalculatorArgs>();

    // Create agent with tool
    let agent = LlmAgentBuilder::new("math-assistant")
        .description("A math assistant that can perform calculations")
        .model(Arc::new(model))
        .instruction(
            "You are a helpful math assistant. When asked to perform calculations, \
             use the calculator tool. Always explain your work.",
        )
        .tool(Arc::new(calc_tool))
        .build()?;

    // Run agent in console mode
    adk_cli::console::run_console(Arc::new(agent), "ollama_tools".to_string(), "user1".to_string())
        .await?;

    Ok(())
}
