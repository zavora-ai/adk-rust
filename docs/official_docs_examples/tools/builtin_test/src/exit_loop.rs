//! ExitLoopTool example - control flow for LoopAgent
//!
//! Run: cargo run --bin exit_loop

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Create refiner agent with exit capability
    let refiner = LlmAgentBuilder::new("refiner")
        .instruction(
            "Improve the text. Check for clarity and grammar.\n\
             If the text is already good, call exit_loop.\n\
             Otherwise, provide an improved version."
        )
        .model(Arc::new(model))
        .tool(Arc::new(ExitLoopTool::new()))
        .build()?;

    // Wrap in LoopAgent
    let loop_agent = LoopAgent::new("iterative_refiner", vec![Arc::new(refiner)])
        .with_max_iterations(3);

    println!("✅ Loop agent with exit capability");
    println!("   Try: 'The quick brown fox jump over lazy dog'");
    Launcher::new(Arc::new(loop_agent)).run().await?;
    Ok(())
}
