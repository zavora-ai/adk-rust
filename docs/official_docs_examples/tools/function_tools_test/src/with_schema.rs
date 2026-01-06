//! FunctionTool with JSON Schema
//!
//! Run: cargo run --bin with_schema

use adk_rust::prelude::*;
use adk_rust::Launcher;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize)]
struct CalculatorParams {
    /// The arithmetic operation to perform
    operation: Operation,
    /// First operand
    a: f64,
    /// Second operand
    b: f64,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(JsonSchema, Serialize)]
struct CalculatorResult {
    /// The computed result
    result: f64,
    /// Human-readable expression
    expression: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Calculator with typed schema
    let calculator = FunctionTool::new(
        "calculator",
        "Perform arithmetic operations (add, subtract, multiply, divide)",
        |_ctx, args| async move {
            let params: CalculatorParams = serde_json::from_value(args)?;
            let result = match params.operation {
                Operation::Add => params.a + params.b,
                Operation::Subtract => params.a - params.b,
                Operation::Multiply => params.a * params.b,
                Operation::Divide if params.b != 0.0 => params.a / params.b,
                Operation::Divide => return Err(adk_core::AdkError::Tool("Cannot divide by zero".into())),
            };
            let op_str = match params.operation {
                Operation::Add => "+",
                Operation::Subtract => "-",
                Operation::Multiply => "*",
                Operation::Divide => "/",
            };
            Ok(json!({
                "result": result,
                "expression": format!("{} {} {} = {}", params.a, op_str, params.b, result)
            }))
        },
    )
    .with_parameters_schema::<CalculatorParams>()
    .with_response_schema::<CalculatorResult>();

    let agent = LlmAgentBuilder::new("math_agent")
        .instruction("You help with math. Use the calculator tool for arithmetic.")
        .model(Arc::new(model))
        .tool(Arc::new(calculator))
        .build()?;

    println!("✅ Math agent ready with typed calculator tool");
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
