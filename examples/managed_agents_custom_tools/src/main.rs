//! Managed Agents: Custom Tool Flow
//!
//! Shows how to define a custom tool, handle `AgentCustomToolUse` events,
//! execute the tool locally, and send results back to the agent.
//!
//! # Usage
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run -p managed-agents-custom-tools
//! ```

use adk_anthropic::managed_agents::{
    CreateAgentParams, CreateEnvironmentParams, CreateSessionParams, ManagedAgentsClient,
    SessionEvent, ToolConfig, UserEvent,
};
use futures::StreamExt;

/// Simulate a weather lookup. In production this would call a real API.
fn get_weather(input: &serde_json::Value) -> String {
    let city = input
        .get("city")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let unit = input
        .get("unit")
        .and_then(|v| v.as_str())
        .unwrap_or("celsius");

    let (temp, symbol) = match unit {
        "fahrenheit" => ("72°F", "🌤️"),
        _ => ("22°C", "🌤️"),
    };

    format!("{symbol} Weather in {city}: {temp}, partly cloudy, humidity 65%")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    eprintln!("=== Managed Agents: Custom Tool Flow ===\n");

    let client = ManagedAgentsClient::from_env()?;
    eprintln!("✓ Client created");

    // Define a custom tool with JSON schema
    let weather_tool = ToolConfig::custom(
        "get_weather",
        "Get the current weather for a city. Returns temperature, conditions, and humidity.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "The city name (e.g., 'San Francisco')"
                },
                "unit": {
                    "type": "string",
                    "enum": ["celsius", "fahrenheit"],
                    "description": "Temperature unit"
                }
            },
            "required": ["city"]
        }),
    );

    // Create agent with both standard toolset and custom tool
    let agent = client
        .create_agent(CreateAgentParams {
            name: "Weather Agent".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some(
                "You are a helpful weather assistant. Use the get_weather tool \
                 to look up weather information when asked."
                    .to_string(),
            ),
            description: Some("Agent with a custom weather tool".to_string()),
            tools: vec![ToolConfig::agent_toolset(), weather_tool],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: None,
            metadata: None,
        })
        .await?;
    eprintln!("✓ Agent created: {}", agent.id);

    let env = client
        .create_environment(CreateEnvironmentParams::cloud("tools-env"))
        .await?;
    eprintln!("✓ Environment created: {}", env.id);

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await?;
    eprintln!("✓ Session created: {}", session.id);

    // Open stream before sending events
    let mut stream = client.stream_events(&session.id).await?;
    eprintln!("✓ Stream opened\n");

    // Send a message that should trigger the custom tool
    client
        .send_event(
            &session.id,
            UserEvent::message("What's the weather like in San Francisco and Tokyo?"),
        )
        .await?;
    eprintln!("→ Sent: \"What's the weather like in San Francisco and Tokyo?\"\n");

    // Process events, handling custom tool calls
    eprintln!("← Processing events:");
    loop {
        let Some(event) = stream.next().await else {
            eprintln!("  Stream ended unexpectedly");
            break;
        };

        match event? {
            SessionEvent::AgentCustomToolUse { id, name, input } => {
                let tool_name = name.as_deref().unwrap_or("unknown");
                let tool_use_id = id.as_deref().unwrap_or("");
                let tool_input = input.as_ref().cloned().unwrap_or_default();

                eprintln!("  🔧 Custom tool call: {tool_name}({tool_input})");

                // Execute the tool locally
                let result = match tool_name {
                    "get_weather" => get_weather(&tool_input),
                    other => format!("Unknown tool: {other}"),
                };

                eprintln!("  📤 Sending result: {result}");

                // Send the result back to the agent
                client
                    .send_event(
                        &session.id,
                        UserEvent::custom_tool_result(tool_use_id, &result),
                    )
                    .await?;
            }
            SessionEvent::AgentMessage { content, .. } => {
                if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            println!("\n{text}");
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
            SessionEvent::AgentToolUse { name, .. } => {
                eprintln!("  ⚙️  Built-in tool: {name:?}");
            }
            _ => {}
        }
    }

    // Cleanup
    eprintln!("\nCleaning up...");
    client.archive_session(&session.id).await?;
    client.archive_agent(&agent.id).await?;
    client.archive_environment(&env.id).await?;
    eprintln!("✓ All resources cleaned up");

    eprintln!("\n=== Done ===");
    Ok(())
}
