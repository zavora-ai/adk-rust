//! Validates: docs/official_docs/artifacts/artifacts.md
//!
//! This example demonstrates using artifacts with CSV data files.
//! It shows how to save CSV data as user-scoped artifacts and load them
//! using the LoadArtifactsTool for data analysis.
//!
//! Run modes:
//!   cargo run --example chat_csv -p adk-rust-guide              # Validation mode
//!   cargo run --example chat_csv -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example chat_csv -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create artifact service and save CSV
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    let csv_content = std::fs::read_to_string("examples/artifacts/test_data.csv")?;
    
    artifact_service.save(SaveRequest {
        app_name: "csv_analyst".to_string(),
        user_id: "user".to_string(), // Default user in Launcher
        session_id: "session".to_string(), // We don't know the session ID yet, but user-scoped works
        file_name: "data.csv".to_string(),
        part: Part::InlineData {
            data: csv_content.as_bytes().to_vec(),
            mime_type: "text/csv".to_string(),
        },
        version: None,
    }).await?;

    // Note: Launcher creates a random session ID, so we should save as user-scoped artifact
    // to ensure it's accessible.
    artifact_service.save(SaveRequest {
        app_name: "csv_analyst".to_string(),
        user_id: "user".to_string(),
        session_id: "init".to_string(), 
        file_name: "user:data.csv".to_string(), // User-scoped!
        part: Part::InlineData {
            data: csv_content.as_bytes().to_vec(),
            mime_type: "text/csv".to_string(),
        },
        version: None,
    }).await?;

    let agent = LlmAgentBuilder::new("csv_analyst")
        .description("Analyzes CSV data files")
        .instruction(
            "You are a CSV data analyst. When users ask about data, \
             use the load_artifacts tool to retrieve 'user:data.csv'. \
             The CSV contains information with columns."
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
        print_validating("CSV Analysis Agent");
        println!("✓ CSV file loaded into artifact service: {} bytes", csv_content.len());
        println!("✓ Agent configured with LoadArtifactsTool");
        print_success("chat_csv");
        println!("\nTry: cargo run --example chat_csv -- chat");
        println!("Ask: 'What employees are in the CSV file?'");
    }

    Ok(())
}
