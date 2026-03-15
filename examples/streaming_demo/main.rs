//! Streaming Updates Demo
//!
//! This example demonstrates how to use UiUpdate to perform
//! real-time incremental UI updates, such as progress bars
//! that update as a task progresses.
//!
//! Run with: cargo run --example streaming_demo

use adk_ui::{
    Alert, AlertVariant, Component, Progress, Text, TextVariant, Theme, UiResponse, UiUpdate,
};

fn main() {
    println!("=== Streaming UI Updates Demo ===\n");

    // Step 1: Initial UI with a progress bar at 0%
    println!("Step 1: Sending initial progress bar at 0%...\n");

    let initial_response = UiResponse::new(vec![
        Component::Text(Text {
            id: Some("status-text".to_string()),
            content: "Starting process...".to_string(),
            variant: TextVariant::H3,
        }),
        Component::Progress(Progress {
            id: Some("main-progress".to_string()), // Important: ID is required for updates
            value: 0,
            label: Some("Processing: 0%".to_string()),
        }),
    ])
    .with_theme(Theme::System);

    println!("Initial UiResponse:");
    println!("{}\n", serde_json::to_string_pretty(&initial_response).unwrap());

    // Step 2: Simulate progress updates
    let progress_steps = [25, 50, 75, 100];

    for progress in progress_steps {
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!("Step {}: Updating progress to {}%...", progress / 25 + 1, progress);

        let status_text = match progress {
            25 => "Downloading files...",
            50 => "Processing data...",
            75 => "Finalizing...",
            100 => "Complete!",
            _ => "Working...",
        };

        // Update the progress bar component by ID
        let progress_update = UiUpdate::replace(
            "main-progress", // Target the component by its ID
            Component::Progress(Progress {
                id: Some("main-progress".to_string()),
                value: progress,
                label: Some(format!("Processing: {}%", progress)),
            }),
        );

        // Update the status text
        let text_update = UiUpdate::replace(
            "status-text",
            Component::Text(Text {
                id: Some("status-text".to_string()),
                content: status_text.to_string(),
                variant: TextVariant::H3,
            }),
        );

        println!("Progress UiUpdate:");
        println!("{}", serde_json::to_string_pretty(&progress_update).unwrap());
        println!("Text UiUpdate:");
        println!("{}\n", serde_json::to_string_pretty(&text_update).unwrap());
    }

    // Step 3: Final state - replace with success message
    println!("Step 6: Final state with success alert...\n");

    let final_response = UiResponse::new(vec![
        Component::Text(Text {
            id: Some("status-text".to_string()),
            content: "Process Complete!".to_string(),
            variant: TextVariant::H2,
        }),
        Component::Progress(Progress {
            id: Some("main-progress".to_string()),
            value: 100,
            label: Some("Done!".to_string()),
        }),
        Component::Alert(Alert {
            id: Some("success-alert".to_string()),
            title: "Success".to_string(),
            description: Some("All files have been processed successfully.".to_string()),
            variant: AlertVariant::Success,
        }),
    ]);

    println!("Final UiResponse:");
    println!("{}\n", serde_json::to_string_pretty(&final_response).unwrap());

    println!("=== Demo Complete ===");
    println!("\nKey points:");
    println!("1. Give components an `id` field to enable targeted updates");
    println!("2. Use UiUpdate::replace(id, component) to update specific components");
    println!("3. Use UiUpdate::remove(id) to remove components");
    println!("4. Use UiUpdate::append(container_id, component) to add to containers");
    println!("5. Send updates as SSE events with MIME type: application/vnd.adk.ui.update+json");
}
