use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create artifact service and save TXT
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    let txt_content = std::fs::read_to_string("examples/artifacts/test_document.txt")?;
    
    // Save user-scoped artifact
    artifact_service.save(SaveRequest {
        app_name: "doc_analyst".to_string(),
        user_id: "user".to_string(),
        session_id: "init".to_string(), 
        file_name: "user:documentation.txt".to_string(),
        part: Part::Text { text: txt_content.clone() },
        version: None,
    }).await?;

    let agent = LlmAgentBuilder::new("doc_analyst")
        .description("Analyzes text documents")
        .instruction(
            "You are a document analyst. When users ask about documentation, \
             use the load_artifacts tool to retrieve 'user:documentation.txt'. \
             The document contains ADK-Rust information about session management, agents, tools, and state persistence."
        )
        .model(model)
        .tool(Arc::new(LoadArtifactsTool::new()))
        .build()?;

    if is_interactive_mode() {
        Launcher::new(Arc::new(agent))
            .with_artifact_service(artifact_service)
            .run()
            .await?;
    } else {
        print_validating("TXT Analysis Agent");
        println!("✓ TXT file loaded into artifact service: {} bytes", txt_content.len());
        println!("✓ Agent configured with LoadArtifactsTool");
        print_success("chat_txt");
        println!("\nTry: cargo run --example chat_txt -- chat");
        println!("Ask: 'Summarize the documentation file'");
    }

    Ok(())
}
