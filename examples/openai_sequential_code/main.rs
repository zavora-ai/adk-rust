//! OpenAI Sequential Code Generation Workflow Example
//!
//! This example demonstrates a multi-stage code generation workflow
//! using sequential agents with OpenAI: designer -> implementer -> reviewer
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_sequential_code --features openai
//! ```

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_cli::Launcher;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let designer = LlmAgentBuilder::new("designer")
        .description("Code design agent")
        .instruction(
            "You are a software architect. Design the code structure and architecture. \
             Output a detailed design plan including:\n\
             - Module structure\n\
             - Key classes/functions\n\
             - Data flow\n\
             - Error handling strategy\n\
             Be thorough but concise.",
        )
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    let implementer = LlmAgentBuilder::new("implementer")
        .description("Code implementation agent")
        .instruction(
            "You are an expert programmer. Based on the design provided, implement the code. \
             Write clean, working code with:\n\
             - Clear variable names\n\
             - Proper error handling\n\
             - Comments for complex logic\n\
             Output the complete implementation.",
        )
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    let reviewer = LlmAgentBuilder::new("reviewer")
        .description("Code review agent")
        .instruction(
            "You are a senior code reviewer. Review the code for:\n\
             - Bugs and edge cases\n\
             - Performance issues\n\
             - Best practices violations\n\
             - Security concerns\n\
             Provide the final polished code with any necessary fixes.",
        )
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?))
        .build()?;

    let workflow = SequentialAgent::new(
        "code_workflow",
        vec![Arc::new(designer), Arc::new(implementer), Arc::new(reviewer)],
    );

    println!("OpenAI Sequential Code Generation Workflow");
    println!("==========================================\n");
    println!("This workflow uses 3 specialized agents:");
    println!("  1. Designer   - Creates architecture plan");
    println!("  2. Implementer - Writes code based on design");
    println!("  3. Reviewer   - Reviews and polishes final code\n");
    println!("Try asking: 'Create a simple REST API client in Rust'\n");

    Launcher::new(Arc::new(workflow)).run().await?;

    Ok(())
}
