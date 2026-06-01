//! Managed Agents: Hello World
//!
//! The simplest possible Anthropic Managed Agents session.
//! Creates an agent, environment, session, sends a message, streams the response,
//! and cleans up all resources.
//!
//! # Usage
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run -p managed-agents-hello
//! ```

use adk_anthropic::managed_agents::{
    CreateAgentParams, CreateEnvironmentParams, CreateSessionParams, ManagedAgentsClient,
    SessionEvent, ToolConfig, UserEvent,
};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    eprintln!("=== Managed Agents: Hello World ===\n");

    // 1. Create client from ANTHROPIC_API_KEY env var
    let client = ManagedAgentsClient::from_env()?;
    eprintln!("✓ Client created");

    // 2. Create an agent
    let agent = client
        .create_agent(CreateAgentParams {
            name: "Hello Agent".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some("You are a friendly assistant. Keep responses brief.".to_string()),
            description: Some("A minimal hello-world agent".to_string()),
            tools: vec![ToolConfig::agent_toolset()],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: None,
            metadata: None,
        })
        .await?;
    eprintln!("✓ Agent created: {}", agent.id);

    // 3. Create a cloud environment
    let env = client.create_environment(CreateEnvironmentParams::cloud("hello-env")).await?;
    eprintln!("✓ Environment created: {}", env.id);

    // 4. Create a session
    let session = client.create_session(CreateSessionParams::new(&agent.id, &env.id)).await?;
    eprintln!("✓ Session created: {} (status: {:?})", session.id, session.status);

    // 5. Open SSE stream BEFORE sending events (required by the API)
    let mut stream = client.stream_events(&session.id).await?;
    eprintln!("✓ Stream opened\n");

    // 6. Send a message
    client.send_event(&session.id, UserEvent::message("Hello! What is 2 + 2?")).await?;
    eprintln!("→ Sent: \"Hello! What is 2 + 2?\"\n");

    // 7. Stream and print agent responses
    eprintln!("← Agent response:");
    while let Some(event) = stream.next().await {
        match event? {
            SessionEvent::AgentMessage { content, .. } => {
                // content is a JSON array of content blocks
                if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            println!("{text}");
                        }
                    }
                }
            }
            SessionEvent::StatusIdle { stop_reason } => {
                eprintln!("\n✓ Session idle (stop_reason: {stop_reason:?})");
                break;
            }
            SessionEvent::Error { error, message } => {
                eprintln!("\n✗ Error: {error:?} {message:?}");
                break;
            }
            _ => {}
        }
    }

    // 8. Cleanup
    eprintln!("\nCleaning up...");
    client.archive_session(&session.id).await?;
    eprintln!("  ✓ Session archived");
    let _ = client.archive_agent(&agent.id).await;
    eprintln!("  ✓ Agent archived");
    let _ = client.archive_environment(&env.id).await;
    eprintln!("  ✓ Environment archived");

    eprintln!("\n=== Done ===");
    Ok(())
}
