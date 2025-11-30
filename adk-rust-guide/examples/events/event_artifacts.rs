//! Validates: docs/official_docs/events/events.md
//!
//! This example demonstrates how events track artifact operations through artifact_delta.
//! It shows how artifact saves and modifications are recorded in the event stream.
//!
//! Run modes:
//!   cargo run --example event_artifacts -p adk-rust-guide              # Validation mode
//!   cargo run --example event_artifacts -p adk-rust-guide -- chat      # Interactive console

use adk_rust::prelude::*;
use adk_rust::runner::{Runner, RunnerConfig};
use adk_rust::session::{CreateRequest, GetRequest, SessionService};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create a tool that saves artifacts
    let save_note = FunctionTool::new(
        "save_note",
        "Saves a text note as an artifact. Parameters: filename (string), content (string)",
        |ctx, args| {
            Box::pin(async move {
                let filename = args["filename"].as_str().unwrap_or("note.txt");
                let content = args["content"].as_str().unwrap_or("");

                // Save artifact if service is available
                if let Some(artifacts) = ctx.artifacts() {
                    let part = Part::Text {
                        text: content.to_string(),
                    };
                    let version = artifacts.save(filename, &part).await?;

                    Ok(json!({
                        "success": true,
                        "filename": filename,
                        "version": version,
                        "message": format!("Saved {} (version {})", filename, version)
                    }))
                } else {
                    Ok(json!({
                        "success": false,
                        "message": "Artifact service not available"
                    }))
                }
            })
        },
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("artifact_demo")
            .model(Arc::new(model))
            .instruction(
                "You are a helpful assistant that can save notes. \
                 Use the save_note tool when users want to save information.",
            )
            .tool(Arc::new(save_note))
            .build()?,
    );

    if is_interactive_mode() {
        Launcher::new(agent).run().await?;
        return Ok(());
    }

    print_validating("events/events.md");

    println!("\n=== Artifact Tracking via Events ===\n");

    let session_service = Arc::new(InMemorySessionService::new());
    let artifact_service = Arc::new(InMemoryArtifactService::new());

    session_service
        .create(CreateRequest {
            app_name: "artifact_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: Some("artifact_session".to_string()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: "artifact_demo".to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: Some(artifact_service.clone()),
        memory_service: None,
    })?;

    // Run agent interaction that will save artifacts
    let user_input =
        Content::new("user").with_text("Save a note called 'meeting.txt' with the content 'Discuss Q4 goals'");

    let mut stream = runner
        .run(
            "user_123".to_string(),
            "artifact_session".to_string(),
            user_input,
        )
        .await?;

    println!("Processing events and tracking artifact operations...\n");

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                // Check for artifact changes in this event
                if !event.actions.artifact_delta.is_empty() {
                    println!("─────────────────────────────────────");
                    println!("Event with artifact changes detected!");
                    println!("Event ID: {}", event.id);
                    println!("Author: {}", event.author);
                    println!("Timestamp: {}", event.timestamp.format("%H:%M:%S"));
                    println!("\nArtifact delta:");
                    for (name, version) in &event.actions.artifact_delta {
                        println!("  {} → version {}", name, version);
                    }
                    println!("─────────────────────────────────────\n");
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    // Retrieve session and analyze artifact tracking
    let session = session_service
        .get(GetRequest {
            app_name: "artifact_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: "artifact_session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    let events = session.events();
    println!("=== Artifact Tracking Analysis ===");
    println!("Total events: {}", events.len());

    let mut events_with_artifacts = 0;
    let mut artifact_operations: HashMap<String, Vec<i64>> = HashMap::new();

    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            if !event.actions.artifact_delta.is_empty() {
                events_with_artifacts += 1;

                println!("\nEvent {} (by {}):", i + 1, event.author);
                for (name, version) in &event.actions.artifact_delta {
                    println!("  {} (v{})", name, version);
                    artifact_operations
                        .entry(name.clone())
                        .or_insert_with(Vec::new)
                        .push(*version);
                }
            }
        }
    }

    println!("\n=== Summary ===");
    println!("Events with artifact operations: {}", events_with_artifacts);
    println!("Unique artifacts tracked: {}", artifact_operations.len());

    for (name, versions) in &artifact_operations {
        println!("\nArtifact: {}", name);
        println!("  Versions: {:?}", versions);
        println!("  Total operations: {}", versions.len());
    }

    // Explain artifact_delta purpose
    println!("\n=== About artifact_delta ===");
    println!("The artifact_delta field in EventActions tracks:");
    println!("  • Which artifacts were created or modified");
    println!("  • The version number of each artifact");
    println!("  • When artifacts were saved (via event timestamp)");
    println!("  • Who saved the artifact (via event author)");
    println!("\nThis provides a complete audit trail of artifact operations.");

    print_success("event_artifacts");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example event_artifacts -p adk-rust-guide -- chat");

    Ok(())
}
