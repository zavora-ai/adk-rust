//! Agent Tool Example
//!
//! This example demonstrates how to use AgentTool to wrap specialized agents
//! as callable tools. A coordinator agent can then invoke these specialist
//! agents dynamically based on user requests.
//!
//! Architecture:
//! - Coordinator Agent: Routes requests to appropriate specialists
//! - Math Expert Agent: Handles mathematical calculations
//! - Trivia Expert Agent: Answers trivia and general knowledge questions
//!
//! Run with:
//! ```
//! GOOGLE_API_KEY=your-key cargo run --example agent_tool
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
use adk_model::gemini::GeminiModel;
use adk_tool::{AgentTool, FunctionTool};
use anyhow::Result;
use serde_json::{Value, json};
use std::sync::Arc;

/// Calculator tool for the math agent
async fn calculator(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, adk_core::AdkError> {
    let operation = args["operation"].as_str().unwrap_or("add");
    let a = args["a"].as_f64().unwrap_or(0.0);
    let b = args["b"].as_f64().unwrap_or(0.0);

    let result = match operation {
        "add" => a + b,
        "subtract" => a - b,
        "multiply" => a * b,
        "divide" => {
            if b == 0.0 {
                return Err(adk_core::AdkError::Tool("Division by zero".to_string()));
            }
            a / b
        }
        "power" => a.powf(b),
        "sqrt" => a.sqrt(),
        "percent" => a * (b / 100.0),
        _ => return Err(adk_core::AdkError::Tool(format!("Unknown operation: {}", operation))),
    };

    Ok(json!({
        "operation": operation,
        "a": a,
        "b": b,
        "result": result
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing for debugging (optional)
    tracing_subscriber::fmt::init();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create the calculator tool
    let calc_tool = FunctionTool::new(
        "calculator",
        "Performs arithmetic operations: add, subtract, multiply, divide, power, sqrt, percent. \
         Args: operation (string), a (number), b (number - optional for sqrt)",
        calculator,
    );

    // Create the Math Expert agent with calculator tool
    let math_agent = LlmAgentBuilder::new("math_expert")
        .description(
            "A math expert that can perform calculations and solve mathematical problems. \
             Use this agent for any math-related questions, calculations, or numerical analysis.",
        )
        .instruction(
            "You are a math expert. When asked to perform calculations, use the calculator tool. \
             Always show your work step by step. For complex problems, break them down into \
             smaller calculations. Be precise with numbers.",
        )
        .model(model.clone())
        .tool(Arc::new(calc_tool))
        .build()?;

    // Create the Trivia Expert agent (no tools, just LLM knowledge)
    let trivia_agent = LlmAgentBuilder::new("trivia_expert")
        .description(
            "A trivia and general knowledge expert. Use this agent for questions about \
             history, science, geography, pop culture, sports, and other factual topics.",
        )
        .instruction(
            "You are a trivia expert with vast knowledge across many domains. Answer questions \
             accurately and provide interesting related facts when appropriate. If you're unsure, \
             say so rather than guessing.",
        )
        .model(model.clone())
        .build()?;

    // Wrap agents as tools
    let math_tool =
        AgentTool::new(Arc::new(math_agent)).skip_summarization(false).forward_artifacts(true);

    let trivia_tool = AgentTool::new(Arc::new(trivia_agent)).skip_summarization(false);

    // Create the Coordinator agent that uses the specialist agents as tools
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Smart assistant that routes questions to specialist agents")
        .instruction(
            "You are a helpful coordinator assistant. Analyze user requests and delegate them \
             to the appropriate specialist:\n\
             - For math problems, calculations, or numerical questions -> use math_expert\n\
             - For trivia, facts, or general knowledge questions -> use trivia_expert\n\n\
             After receiving a response from a specialist, summarize it for the user. \
             If a question spans multiple domains, you can call multiple specialists.",
        )
        .model(model)
        .tool(Arc::new(math_tool))
        .tool(Arc::new(trivia_tool))
        .build()?;

    println!("=== Agent Tool Example ===");
    println!();
    println!("This coordinator agent can delegate to:");
    println!("  - math_expert: for calculations and math problems");
    println!("  - trivia_expert: for general knowledge questions");
    println!();
    println!("Try questions like:");
    println!("  - 'What is 15% of 250?'");
    println!("  - 'Who was the first person to walk on the moon?'");
    println!("  - 'Calculate 2^10 and tell me a fact about that number'");
    println!();

    adk_cli::console::run_console(
        Arc::new(coordinator),
        "agent_tool_example".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
