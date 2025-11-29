//! Validates: docs/official_docs/events/events.md
//!
//! This example demonstrates event inspection and handling.

use adk_rust::prelude::*;
use adk_rust_guide::{print_success, print_validating};

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("events/events.md");

    // Create a sample event
    let mut event = Event::new("invocation_123");
    event.author = "agent_1".to_string();
    event.content = Some(Content::new("model").with_text("Hello, world!"));

    // Inspect event properties (fields, not methods)
    println!("Event ID: {}", event.id);
    println!("Event author: {}", event.author);
    println!("Event timestamp: {:?}", event.timestamp);

    // Events form conversation history
    println!("\nEvents contain:");
    println!("  - id: Unique identifier");
    println!("  - timestamp: When the event occurred");
    println!("  - author: Who created the event (agent name or 'user')");
    println!("  - content: The message content");
    println!("  - actions: Optional actions like state_delta");

    print_success("event_inspection");
    Ok(())
}
