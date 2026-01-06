//! A2A Client Example
//!
//! Connects to a remote A2A agent and sends a message.
//!
//! First start the server:
//!   GOOGLE_API_KEY=your_key cargo run --bin server
//!
//! Then run the client:
//!   cargo run --bin client

use adk_server::a2a::{A2aClient, Message, Part, Role};

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    println!("A2A Client Example");
    println!("==================\n");

    // Connect to remote agent
    println!("1. Fetching agent card from http://localhost:8090...");
    let client = A2aClient::from_url("http://localhost:8090").await?;

    let card = client.agent_card();
    println!("   Agent: {}", card.name);
    println!("   Description: {}", card.description);
    println!("   Streaming: {}", card.capabilities.streaming);

    // Build message using helper
    println!("\n2. Sending message...");
    let message = Message {
        role: Role::User,
        parts: vec![Part::text("What is 7 * 8?".to_string())],
        message_id: uuid::Uuid::new_v4().to_string(),
        context_id: None,
        task_id: None,
        metadata: None,
    };

    // Send message (blocking)
    let response = client.send_message(message).await?;

    println!("\n3. Response received:");
    if let Some(result) = response.result {
        println!("   {}", serde_json::to_string_pretty(&result).unwrap_or_default());
    }
    if let Some(error) = response.error {
        println!("   Error: {} (code: {})", error.message, error.code);
    }

    println!("\n✓ A2A communication complete!");
    Ok(())
}
