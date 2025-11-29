//! Validates: docs/official_docs/artifacts/artifacts.md
//!
//! This example demonstrates artifact save and load operations.

use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest, LoadRequest, ListRequest};
use adk_rust_guide::{print_success, print_validating};

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("artifacts/artifacts.md");

    // Create an in-memory artifact service
    let artifact_service = InMemoryArtifactService::new();

    // Create artifact data
    let data = b"Hello, this is artifact content!".to_vec();
    let part = Part::InlineData {
        data,
        mime_type: "text/plain".to_string(),
    };

    // Save artifact
    let resp = artifact_service
        .save(SaveRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_1".to_string(),
            file_name: "my_artifact".to_string(),
            part: part.clone(),
            version: None,
        })
        .await?;

    println!("Saved artifact with version: {}", resp.version);

    // Load artifact
    let loaded_resp = artifact_service
        .load(LoadRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_1".to_string(),
            file_name: "my_artifact".to_string(),
            version: None,
        })
        .await?;

    println!("Loaded artifact successfully: {} bytes", 
        match loaded_resp.part {
            Part::InlineData { ref data, .. } => data.len(),
            Part::Text { ref text } => text.len(),
            _ => 0,
        }
    );

    // List artifacts
    let list_resp = artifact_service
        .list(ListRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_1".to_string(),
        })
        .await?;

    println!("Artifacts in session: {:?}", list_resp.file_names);

    print_success("artifact_ops");
    Ok(())
}
