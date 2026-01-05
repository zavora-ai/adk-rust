//! Loop Refiner - Critic → Refine → Repeat until quality

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Critic agent evaluates content
    let critic = LlmAgentBuilder::new("critic")
        .instruction("Review the content for quality. Score it 1-10 and list \
                     specific improvements needed. Be constructive but critical.")
        .model(model.clone())
        .build()?;

    // Refiner agent improves based on critique
    let refiner = LlmAgentBuilder::new("refiner")
        .instruction("Apply the critique to improve the content. \
                     If the score is 8 or higher, call exit_loop to finish. \
                     Otherwise, provide an improved version.")
        .model(model.clone())
        .tool(Arc::new(ExitLoopTool::new()))
        .build()?;

    // Create inner sequential: critic → refiner
    let critique_refine = SequentialAgent::new(
        "critique_refine_step",
        vec![Arc::new(critic), Arc::new(refiner)],
    );

    // Wrap in loop with max 3 iterations
    let iterative_improver = LoopAgent::new(
        "iterative_improver",
        vec![Arc::new(critique_refine)],
    ).with_max_iterations(3)
     .with_description("Critique-refine loop (max 3 passes)");

    println!("🔄 Iterative Improvement Loop");
    println!("   critic → refiner → repeat (max 3x or until quality >= 8)");
    println!();
    println!("Try: 'Write a tagline for a coffee shop'");
    println!();

    Launcher::new(Arc::new(iterative_improver)).run().await?;
    Ok(())
}
