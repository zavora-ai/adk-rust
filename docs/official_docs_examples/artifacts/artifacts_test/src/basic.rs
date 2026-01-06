//! Basic ArtifactService Example
//!
//! Demonstrates CRUD operations with InMemoryArtifactService.
//!
//! Run:
//!   cd doc-test/artifacts/artifacts_test
//!   cargo run --bin basic

use adk_artifact::{
    ArtifactService, DeleteRequest, InMemoryArtifactService, ListRequest, LoadRequest,
    SaveRequest, VersionsRequest,
};
use adk_core::Part;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("ArtifactService Basic Example");
    println!("==============================\n");

    let service = InMemoryArtifactService::new();

    // Save text artifact
    println!("1. Saving text artifact...");
    let response = service
        .save(SaveRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "notes.txt".to_string(),
            part: Part::Text { text: "First version of notes".to_string() },
            version: None, // Auto-increment
        })
        .await?;
    println!("   Saved as version: {}", response.version);

    // Save binary artifact
    println!("\n2. Saving binary artifact...");
    let image_data = vec![0xFF, 0xD8, 0xFF, 0xE0]; // JPEG header bytes
    let response = service
        .save(SaveRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "image.jpg".to_string(),
            part: Part::InlineData { mime_type: "image/jpeg".to_string(), data: image_data },
            version: None,
        })
        .await?;
    println!("   Saved as version: {}", response.version);

    // Save second version
    println!("\n3. Saving second version of notes...");
    let response = service
        .save(SaveRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "notes.txt".to_string(),
            part: Part::Text { text: "Updated notes - version 2".to_string() },
            version: None,
        })
        .await?;
    println!("   Saved as version: {}", response.version);

    // List artifacts
    println!("\n4. Listing artifacts...");
    let list_response = service
        .list(ListRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
        })
        .await?;
    for name in &list_response.file_names {
        println!("   - {}", name);
    }

    // Load latest version
    println!("\n5. Loading latest version of notes...");
    let load_response = service
        .load(LoadRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "notes.txt".to_string(),
            version: None, // Latest
        })
        .await?;
    if let Part::Text { text } = load_response.part {
        println!("   Content: {}", text);
    }

    // Load specific version
    println!("\n6. Loading version 1 of notes...");
    let load_response = service
        .load(LoadRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "notes.txt".to_string(),
            version: Some(1),
        })
        .await?;
    if let Part::Text { text } = load_response.part {
        println!("   Content: {}", text);
    }

    // Get versions
    println!("\n7. Getting all versions of notes...");
    let versions_response = service
        .versions(VersionsRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "notes.txt".to_string(),
        })
        .await?;
    println!("   Versions: {:?}", versions_response.versions);

    // Delete specific version
    println!("\n8. Deleting version 1...");
    service
        .delete(DeleteRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "notes.txt".to_string(),
            version: Some(1),
        })
        .await?;
    println!("   Deleted version 1");

    // Verify deletion
    let versions_response = service
        .versions(VersionsRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "notes.txt".to_string(),
        })
        .await?;
    println!("   Remaining versions: {:?}", versions_response.versions);

    println!("\n✓ All operations completed successfully!");

    Ok(())
}
