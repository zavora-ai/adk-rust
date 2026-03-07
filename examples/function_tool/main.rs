use adk_agent::LlmAgentBuilder;
use adk_core::{AdkError, Result, ToolContext};
use adk_model::gemini::GeminiModel;
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
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let calc_tool = FunctionTool::new(
        "calculator",
        "Performs basic arithmetic operations (add, subtract, multiply, divide)",
        calculator,
    )
    .with_parameters_schema::<CalculatorArgs>();

    let agent = LlmAgentBuilder::new("calculator_agent")
        .description("Agent that can perform calculations")
        .instruction("Use the calculator tool to perform arithmetic operations. Available operations are: add, subtract, multiply, divide.")
        .model(Arc::new(model))
        .tool(Arc::new(calc_tool))
        .build()?;

    adk_cli::console::run_console(
        Arc::new(agent),
        "calculator_app".to_string(),
        adk_core::types::UserId::new("user1").unwrap(),
    )
    .await?;

    Ok(())
}
