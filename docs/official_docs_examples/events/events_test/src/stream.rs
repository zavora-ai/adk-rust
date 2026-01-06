//! Event Stream Processing Example
//!
//! Demonstrates processing events from a session and identifying event types.
//!
//! Run:
//!   cd doc-test/events/events_test
//!   cargo run --bin stream

use adk_core::{Content, Event, FunctionResponseData, Part};
use serde_json::json;

fn main() {
    println!("Event Stream Processing Example");
    println!("================================\n");

    // Simulate a conversation event stream
    let events = create_sample_conversation();

    println!("Processing {} events:\n", events.len());

    for (i, event) in events.iter().enumerate() {
        println!("--- Event {} ---", i + 1);
        println!("Author: {}", event.author);
        println!("Invocation: {}", event.invocation_id);

        // Identify event type by content
        if let Some(content) = event.content() {
            let has_text = content.parts.iter().any(|p| matches!(p, Part::Text { .. }));
            let has_function_call =
                content.parts.iter().any(|p| matches!(p, Part::FunctionCall { .. }));
            let has_function_response =
                content.parts.iter().any(|p| matches!(p, Part::FunctionResponse { .. }));

            if has_function_call {
                println!("Type: Tool Call Request");
                for part in &content.parts {
                    if let Part::FunctionCall { name, args, .. } = part {
                        println!("  Tool: {}", name);
                        println!("  Args: {}", args);
                    }
                }
            } else if has_function_response {
                println!("Type: Tool Result");
                for part in &content.parts {
                    if let Part::FunctionResponse { function_response, .. } = part {
                        println!("  Tool: {}", function_response.name);
                        println!("  Result: {}", function_response.response);
                    }
                }
            } else if has_text {
                println!("Type: Text Message");
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        println!("  Content: {}", text);
                    }
                }
            }
        } else {
            println!("Type: Metadata Only (no content)");
        }

        // Check for state changes
        if !event.actions.state_delta.is_empty() {
            println!("State Changes: {:?}", event.actions.state_delta);
        }

        // Check for transfers
        if let Some(target) = &event.actions.transfer_to_agent {
            println!("Transfer To: {}", target);
        }

        println!("Final Response: {}", event.is_final_response());
        println!();
    }

    println!("✓ Event stream processing complete!");
}

fn create_sample_conversation() -> Vec<Event> {
    let invocation_id = "inv-conversation-1";

    // Event 1: User message
    let mut e1 = Event::new(invocation_id);
    e1.author = "user".to_string();
    e1.set_content(Content::new("user").with_text("What's the weather in Tokyo?"));

    // Event 2: Agent requests tool
    let mut e2 = Event::new(invocation_id);
    e2.author = "assistant".to_string();
    e2.set_content(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "get_weather".to_string(),
            args: json!({"city": "Tokyo"}),
            id: Some("call_weather".to_string()),
        }],
    });

    // Event 3: Tool response
    let mut e3 = Event::new(invocation_id);
    e3.author = "get_weather".to_string();
    e3.set_content(Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData {
                name: "get_weather".to_string(),
                response: json!({"temp": 22, "condition": "sunny"}),
            },
            id: Some("call_weather".to_string()),
        }],
    });

    // Event 4: Agent final response
    let mut e4 = Event::new(invocation_id);
    e4.author = "assistant".to_string();
    e4.set_content(Content::new("model").with_text("It's 22°C and sunny in Tokyo!"));

    vec![e1, e2, e3, e4]
}
