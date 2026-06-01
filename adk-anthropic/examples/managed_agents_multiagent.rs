//! Managed Agents: Multiagent Coordinator
//!
//! Shows creating multiple specialized agents, configuring a coordinator,
//! and observing thread activity in a multiagent session.
//!
//! # Usage
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run -p managed-agents-multiagent
//! ```

use adk_anthropic::managed_agents::{
    CreateAgentParams, CreateEnvironmentParams, CreateSessionParams, ManagedAgentsClient,
    Multiagent, SessionEvent, ToolConfig, UserEvent,
};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    eprintln!("=== Managed Agents: Multiagent Coordinator ===\n");

    let client = ManagedAgentsClient::from_env()?;
    eprintln!("✓ Client created");

    // 1. Create specialized worker agents
    let researcher = client
        .create_agent(CreateAgentParams {
            name: "Researcher".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some(
                "You are a research specialist. When given a topic, provide \
                 well-structured factual information with key points. \
                 Focus on accuracy and cite specific details. \
                 Keep your research concise but comprehensive."
                    .to_string(),
            ),
            description: Some(
                "Researches topics and provides structured factual information".to_string(),
            ),
            tools: vec![ToolConfig::agent_toolset()],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: None,
            metadata: None,
        })
        .await?;
    eprintln!("✓ Researcher agent created: {}", researcher.id);

    let writer = client
        .create_agent(CreateAgentParams {
            name: "Writer".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some(
                "You are a creative writer. Take research material and transform it \
                 into engaging, well-written prose. Use clear structure with headers, \
                 smooth transitions, and a compelling narrative voice. \
                 Output should be polished and ready to publish."
                    .to_string(),
            ),
            description: Some(
                "Transforms research into polished, engaging written content".to_string(),
            ),
            tools: vec![ToolConfig::agent_toolset()],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: None,
            metadata: None,
        })
        .await?;
    eprintln!("✓ Writer agent created: {}", writer.id);

    // 2. Create coordinator that references the workers
    let coordinator = client
        .create_agent(CreateAgentParams {
            name: "Content Coordinator".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some(
                "You are a content production coordinator. You manage a team of specialists:\n\
                 - Researcher: gathers factual information on topics\n\
                 - Writer: transforms research into polished content\n\n\
                 For content requests, first delegate research to the Researcher, \
                 then pass the research to the Writer for final output. \
                 Provide a brief summary of the coordination at the end."
                    .to_string(),
            ),
            description: Some("Coordinates researcher and writer agents".to_string()),
            tools: vec![ToolConfig::agent_toolset()],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: Some(Multiagent::coordinator(vec![
                Multiagent::agent_ref(&researcher.id),
                Multiagent::agent_ref(&writer.id),
            ])),
            metadata: None,
        })
        .await?;
    eprintln!("✓ Coordinator agent created: {}", coordinator.id);

    // 3. Create environment and session
    let env = client.create_environment(CreateEnvironmentParams::cloud("multiagent-env")).await?;
    eprintln!("✓ Environment created: {}", env.id);

    let session = client.create_session(CreateSessionParams::new(&coordinator.id, &env.id)).await?;
    eprintln!("✓ Session created: {}", session.id);

    // 4. Open stream and send a task
    let mut stream = client.stream_events(&session.id).await?;
    eprintln!("✓ Stream opened\n");

    client
        .send_event(
            &session.id,
            UserEvent::message(
                "Write a short article (3-4 paragraphs) about the history and \
                 significance of the Rust programming language. Have the researcher \
                 gather key facts first, then have the writer craft the final piece.",
            ),
        )
        .await?;
    eprintln!("→ Sent coordination task\n");

    // 5. Stream events and observe coordination
    eprintln!("← Processing events:");
    while let Some(event) = stream.next().await {
        match event? {
            SessionEvent::AgentMessage { content, .. } => {
                if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            println!("{text}");
                        }
                    }
                }
            }
            SessionEvent::AgentToolUse { name, input, .. } => {
                let tool_name = name.as_deref().unwrap_or("unknown");
                // Agent delegation shows up as tool use
                eprintln!("  ⚙️  Tool: {tool_name}");
                if let Some(inp) = &input {
                    if let Some(agent_name) = inp.get("agent_name").and_then(|v| v.as_str()) {
                        eprintln!("    → Delegating to: {agent_name}");
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
            SessionEvent::StatusRunning {} => {
                eprintln!("  ▶ Session running...");
            }
            _ => {}
        }
    }

    // 6. List threads to observe multiagent activity
    eprintln!("\n── Session Threads ──\n");
    let threads = client.list_threads(&session.id).await?;
    for thread in &threads {
        let agent_info = thread
            .agent
            .as_ref()
            .and_then(|a| a.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown");
        let status = thread.status.as_deref().unwrap_or("unknown");
        let parent = thread.parent_thread_id.as_deref().unwrap_or("(root)");
        eprintln!(
            "  🧵 {} — agent: {}, status: {}, parent: {}",
            thread.id, agent_info, status, parent
        );
    }

    // 7. Cleanup
    eprintln!("\nCleaning up...");
    client.archive_session(&session.id).await?;
    eprintln!("  ✓ Session archived");
    client.archive_agent(&coordinator.id).await?;
    eprintln!("  ✓ Coordinator deleted");
    client.archive_agent(&researcher.id).await?;
    eprintln!("  ✓ Researcher deleted");
    client.archive_agent(&writer.id).await?;
    eprintln!("  ✓ Writer deleted");
    client.archive_environment(&env.id).await?;
    eprintln!("  ✓ Environment archived");

    eprintln!("\n=== Done ===");
    Ok(())
}
