use adk_agent::{LlmAgentBuilder, LoopAgent};
use adk_model::gemini::GeminiModel;
use adk_tool::ExitLoopTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let refiner = LlmAgentBuilder::new("refiner")
        .description("Refines and improves content iteratively")
        .instruction(
            "Refine the given content to make it better. \
             If the content is good enough (clear, concise, well-structured), \
             call the exit_loop tool with the final result. \
             Otherwise, provide an improved version.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(ExitLoopTool::new()))
        .build()?;

    let loop_agent =
        LoopAgent::new("loop_workflow", vec![Arc::new(refiner)]).with_max_iterations(5);

    adk_cli::console::run_console(
        Arc::new(loop_agent),
        "loop_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
