//! Basic Events Example
//!
//! Demonstrates Event structure, creation, and inspection.
//!
//! Run:
//!   cd doc-test/events/events_test
//!   cargo run --bin basic

use adk_core::{Content, Event, Part};
use serde_json::json;

fn main() {
    println!("Events Basic Example");
    println!("====================\n");

    // 1. Create a basic event
    println!("1. Creating a basic event:");
    let mut event = Event::new("inv-123");
    event.author = "assistant".to_string();
    event.set_content(Content::new("model").with_text("Hello! How can I help?"));

    println!("   ID: {}", event.id);
    println!("   Author: {}", event.author);
    println!("   Invocation ID: {}", event.invocation_id);
    println!("   Timestamp: {}", event.timestamp);

    // 2. Access content
    println!("\n2. Accessing content:");
    if let Some(content) = event.content() {
        println!("   Role: {}", content.role);
        for part in &content.parts {
            if let Part::Text { text } = part {
                println!("   Text: {}", text);
            }
        }
    }

    // 3. Check if final response
    println!("\n3. Checking is_final_response:");
    println!("   Text-only event: {}", event.is_final_response());

    // 4. Event with function call (not final)
    println!("\n4. Event with function call:");
    let mut tool_event = Event::new("inv-456");
    tool_event.author = "assistant".to_string();
    tool_event.set_content(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "get_weather".to_string(),
            args: json!({"city": "Tokyo"}),
            id: Some("call_1".to_string()),
            thought_signature: None,
        }],
    });
    println!("   Has function call: true");
    println!("   is_final_response: {} (needs tool execution)", tool_event.is_final_response());

    // 5. Event with state delta
    println!("\n5. Event with state changes:");
    let mut state_event = Event::new("inv-789");
    state_event.author = "assistant".to_string();
    state_event.actions.state_delta.insert("user_name".to_string(), json!("Alice"));
    state_event.actions.state_delta.insert("temp:step".to_string(), json!(1));
    state_event.actions.state_delta.insert("app:counter".to_string(), json!(42));

    println!("   State delta:");
    for (key, value) in &state_event.actions.state_delta {
        println!("     {} = {}", key, value);
    }

    // 6. Event with artifact delta
    println!("\n6. Event with artifact changes:");
    let mut artifact_event = Event::new("inv-abc");
    artifact_event.actions.artifact_delta.insert("report.pdf".to_string(), 1);
    artifact_event.actions.artifact_delta.insert("chart.png".to_string(), 2);

    println!("   Artifact delta:");
    for (name, version) in &artifact_event.actions.artifact_delta {
        println!("     {} (v{})", name, version);
    }

    // 7. Event with transfer
    println!("\n7. Event with agent transfer:");
    let mut transfer_event = Event::new("inv-def");
    transfer_event.author = "router".to_string();
    transfer_event.actions.transfer_to_agent = Some("specialist".to_string());
    transfer_event.set_content(Content::new("model").with_text("Transferring to specialist..."));

    if let Some(target) = &transfer_event.actions.transfer_to_agent {
        println!("   Transfer to: {}", target);
    }

    // 8. Partial streaming event
    println!("\n8. Partial streaming event:");
    let mut partial_event = Event::new("inv-stream");
    partial_event.llm_response.partial = true;
    partial_event.set_content(Content::new("model").with_text("Hello..."));

    println!("   partial: {}", partial_event.llm_response.partial);
    println!("   is_final_response: {}", partial_event.is_final_response());

    // 9. Function call IDs extraction
    println!("\n9. Extracting function call IDs:");
    let ids = tool_event.function_call_ids();
    println!("   Function calls: {:?}", ids);

    println!("\n✓ All event operations demonstrated!");
}
