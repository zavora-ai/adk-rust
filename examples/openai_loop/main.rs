//! OpenAI Loop example with ADK.
//!
//! This example demonstrates using OpenAI with a loop agent that refines content iteratively.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_loop --features openai
//! ```

use adk_agent::{LlmAgentBuilder, LoopAgent};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_tool::ExitLoopTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    let refiner = LlmAgentBuilder::new("refiner")
        .description("Refines and improves content iteratively")
        .instruction(
            "Refine the given content to make it better. \
             If the content is good enough (clear, concise, well-structured), \
             call the exit_loop tool with the final result. \
             Otherwise, provide an improved version. \
             Maximum 3 refinement iterations.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(ExitLoopTool::new()))
        .build()?;

    let loop_agent =
        LoopAgent::new("openai_loop_workflow", vec![Arc::new(refiner)]).with_max_iterations(5);

    println!("OpenAI Loop Agent");
    println!("=================");
    println!("This agent refines content iteratively until it's good enough.");
    println!("It will call exit_loop when satisfied with the result.\n");

    adk_cli::console::run_console(
        Arc::new(loop_agent),
        "openai_loop_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
