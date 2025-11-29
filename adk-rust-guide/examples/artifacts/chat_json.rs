use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create artifact service and save JSON
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    let json_content = std::fs::read_to_string("examples/artifacts/test_config.json")?;
    
    // Save user-scoped artifact
    artifact_service.save(SaveRequest {
        app_name: "config_analyst".to_string(),
        user_id: "user".to_string(),
        session_id: "init".to_string(), 
        file_name: "user:config.json".to_string(),
        part: Part::InlineData {
            data: json_content.as_bytes().to_vec(),
            mime_type: "application/json".to_string(),
        },
        version: None,
    }).await?;

    let agent = LlmAgentBuilder::new("config_analyst")
        .description("Analyzes JSON configuration files")
        .instruction(
            "You are a configuration analyst. When users ask about config settings, \
             use the load_artifacts tool to retrieve 'user:config.json'. \
             The config contains app settings, model configurations, and feature flags."
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
        print_validating("JSON Analysis Agent");
        println!("✓ JSON file loaded into artifact service: {} bytes", json_content.len());
        println!("✓ Agent configured with LoadArtifactsTool");
        print_success("chat_json");
        println!("\nTry: cargo run --example chat_json -- chat");
        println!("Ask: 'What settings are in the config file?'");
    }

    Ok(())
}
