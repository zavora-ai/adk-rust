//! OpenAI Agent Tool Example
//!
//! This example demonstrates how to use AgentTool with OpenAI to wrap specialized agents
//! as callable tools. A coordinator agent can then invoke these specialist agents
//! dynamically based on user requests.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_agent_tool --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_tool::{AgentTool, FunctionTool};
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CalculatorArgs {
    /// The arithmetic operation to perform
    operation: String,
    /// First operand
    a: f64,
    /// Second operand (optional for sqrt)
    #[serde(default)]
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
        "power" => args.a.powf(args.b),
        "sqrt" => args.a.sqrt(),
        "percent" => args.a * (args.b / 100.0),
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
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?);

    // Create the calculator tool with schema
    let calc_tool = FunctionTool::new(
        "calculator",
        "Performs arithmetic operations: add, subtract, multiply, divide, power, sqrt, percent",
        calculator,
    )
    .with_parameters_schema::<CalculatorArgs>();

    // Create the Math Expert agent with calculator tool
    let math_agent = LlmAgentBuilder::new("math_expert")
        .description("A math expert that can perform calculations and solve mathematical problems.")
        .instruction(
            "You are a math expert. When asked to perform calculations, use the calculator tool. \
             Be precise with numbers and show your work.",
        )
        .model(model.clone())
        .tool(Arc::new(calc_tool))
        .build()?;

    // Create the Trivia Expert agent (no tools, just LLM knowledge)
    let trivia_agent = LlmAgentBuilder::new("trivia_expert")
        .description("A trivia and general knowledge expert.")
        .instruction(
            "You are a trivia expert. Answer questions accurately and concisely. \
             If you're unsure, say so.",
        )
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    // Wrap agents as tools
    let math_tool = AgentTool::new(Arc::new(math_agent)).skip_summarization(false);
    let trivia_tool = AgentTool::new(Arc::new(trivia_agent)).skip_summarization(false);

    // Create the Coordinator agent that uses the specialist agents as tools
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Smart assistant that routes questions to specialist agents")
        .instruction(
            "You are a helpful coordinator. Route requests to the appropriate specialist:\n\
             - For math problems -> use math_expert\n\
             - For trivia/facts -> use trivia_expert\n\n\
             Summarize the specialist's response for the user.",
        )
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?))
        .tool(Arc::new(math_tool))
        .tool(Arc::new(trivia_tool))
        .build()?;

    println!("=== OpenAI Agent Tool Example ===");
    println!();
    println!("This coordinator agent can delegate to:");
    println!("  - math_expert: for calculations and math problems");
    println!("  - trivia_expert: for general knowledge questions");
    println!();
    println!("Try questions like:");
    println!("  - 'What is 15% of 250?'");
    println!("  - 'Who invented the telephone?'");
    println!();

    adk_cli::console::run_console(
        Arc::new(coordinator),
        "openai_agent_tool_example".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
