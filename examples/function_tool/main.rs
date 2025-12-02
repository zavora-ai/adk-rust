use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
#[allow(unused_imports)]
use adk_model::gemini::GeminiModel;
use adk_model::openai::OpenaiModel;
use adk_tool::FunctionTool;
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum CalculatorOperation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl Default for CalculatorOperation {
    fn default() -> Self {
        Self::Add
    }
}

impl CalculatorOperation {
    fn symbol(&self) -> &'static str {
        match self {
            CalculatorOperation::Add => "+",
            CalculatorOperation::Subtract => "-",
            CalculatorOperation::Multiply => "*",
            CalculatorOperation::Divide => "/",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CalculatorArgs {
    #[serde(default)]
    operation: CalculatorOperation,
    a: f64,
    b: f64,
}

async fn calculator(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let args: CalculatorArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid calculator arguments: {}", e)))?;

    let result = match args.operation {
        CalculatorOperation::Add => args.a + args.b,
        CalculatorOperation::Subtract => args.a - args.b,
        CalculatorOperation::Multiply => args.a * args.b,
        CalculatorOperation::Divide => args.a / args.b,
    };

    let symbol = args.operation.symbol();

    Ok(json!({
        "result": result,
        "expression": format!("{} {} {}", args.a, symbol, args.b),
        "formatted": format!("{} {} {} = {}", args.a, symbol, args.b, result),
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
    let model = OpenaiModel::new("http://127.0.0.1:8317/v1", "123456", "glm-4.6")?;

    let calc_tool = FunctionTool::new(
        "calculator",
        "Performs basic arithmetic operations (add, subtract, multiply, divide)",
        calculator,
    )
    .with_parameters_schema::<CalculatorArgs>();

    let agent = LlmAgentBuilder::new("calculator_agent")
        .description("Agent that can perform calculations")
        .instruction(
            "Always solve math requests by calling the calculator tool with explicit 'operation', 'a', and 'b' arguments. After the tool responds, reply to the user using the tool's 'formatted' field (e.g., '1 * 19 = 19'). Never guess or perform arithmetic without the tool.",
        )
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
