use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
use adk_model::gemini::GeminiModel;
use adk_tool::FunctionTool;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;

async fn calculator(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let operation = args["operation"].as_str().unwrap_or("add");
    let a = args["a"].as_f64().unwrap_or(0.0);
    let b = args["b"].as_f64().unwrap_or(0.0);

    let result = match operation {
        "add" => a + b,
        "subtract" => a - b,
        "multiply" => a * b,
        "divide" => a / b,
        _ => return Err(adk_core::AdkError::Tool(format!("Unknown operation: {}", operation))),
    };

    Ok(json!({ "result": result }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    let calc_tool = FunctionTool::new(
        "calculator",
        "Performs basic arithmetic operations (add, subtract, multiply, divide)",
        calculator,
    );

    let agent = LlmAgentBuilder::new("calculator_agent")
        .description("Agent that can perform calculations")
        .instruction("Use the calculator tool to perform arithmetic operations.")
        .model(Arc::new(model))
        .tool(Arc::new(calc_tool))
        .build()?;

    adk_cli::console::run_console(
        Arc::new(agent),
        "calculator_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
