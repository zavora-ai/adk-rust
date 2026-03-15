// Sequential Code Generation Workflow Example
//
// This example demonstrates a multi-stage code generation workflow
// using sequential agents: designer → implementer → reviewer

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_model::gemini::GeminiModel;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let designer = LlmAgentBuilder::new("designer")
        .description("Code design agent")
        .instruction("Design the code structure and architecture. Output a detailed design plan.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .build()?;

    let implementer = LlmAgentBuilder::new("implementer")
        .description("Code implementation agent")
        .instruction("Implement the code based on the design. Write clean, working code.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .build()?;

    let reviewer = LlmAgentBuilder::new("reviewer")
        .description("Code review agent")
        .instruction("Review the code for bugs, improvements, and best practices. Provide final polished code.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .build()?;

    let _workflow = SequentialAgent::new(
        "code_workflow",
        vec![Arc::new(designer), Arc::new(implementer), Arc::new(reviewer)],
    );

    println!("Sequential Code Generation Workflow");
    println!("====================================\n");
    println!("✅ Created 3-stage workflow:");
    println!("   1. Designer  - Creates architecture plan");
    println!("   2. Implementer - Writes code");
    println!("   3. Reviewer  - Reviews and polishes\n");
    println!("Note: Full runner integration pending artifact/memory service setup.");
    println!("See sequential.rs for working workflow example.");

    Ok(())
}
