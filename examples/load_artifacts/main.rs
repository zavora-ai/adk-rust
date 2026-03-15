use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_tool::LoadArtifactsTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let _agent = LlmAgentBuilder::new("artifact_agent")
        .description("Agent that can load and analyze artifacts")
        .instruction(
            "You have access to a load_artifacts tool that can load artifacts by name. \
             Use it when asked to load or access artifacts.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(LoadArtifactsTool::new()))
        .build()?;

    println!("LoadArtifactsTool Example");
    println!("=========================");
    println!();
    println!("This example demonstrates the LoadArtifactsTool.");
    println!("The tool allows agents to load artifacts from the artifact service.");
    println!();
    println!("To use this in a real scenario:");
    println!("1. Set up an ArtifactService (InMemory or Database)");
    println!("2. Pre-populate it with artifacts");
    println!("3. Add LoadArtifactsTool to your agent");
    println!("4. The agent can then load artifacts by name");
    println!();
    println!("Agent created successfully with LoadArtifactsTool!");

    Ok(())
}
