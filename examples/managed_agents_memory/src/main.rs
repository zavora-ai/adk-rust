//! Managed Agents: Persistent Memory Across Sessions
//!
//! Shows creating a memory store, seeding it, running a session that writes to it,
//! then running a second session that reads from it.
//!
//! # Usage
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run -p managed-agents-memory
//! ```

use adk_anthropic::managed_agents::{
    CreateAgentParams, CreateEnvironmentParams, CreateMemoryParams, CreateMemoryStoreParams,
    CreateSessionParams, ManagedAgentsClient, MemoryStoreResource, SessionEvent, ToolConfig,
    UserEvent,
};
use futures::StreamExt;

/// Run a session, send a message, and stream until idle.
/// Returns the concatenated agent text output.
async fn run_session_turn(
    client: &ManagedAgentsClient,
    session_id: &str,
    message: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut stream = client.stream_events(session_id).await?;
    client
        .send_event(session_id, UserEvent::message(message))
        .await?;

    let mut output = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            SessionEvent::AgentMessage { content, .. } => {
                if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            output.push_str(text);
                        }
                    }
                }
            }
            SessionEvent::AgentToolUse { name, .. } => {
                eprintln!("    ⚙️  Tool: {name:?}");
            }
            SessionEvent::StatusIdle { .. } => break,
            SessionEvent::Error { error, message } => {
                return Err(format!("Session error: {error:?} {message:?}").into());
            }
            _ => {}
        }
    }
    Ok(output)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    eprintln!("=== Managed Agents: Persistent Memory ===\n");

    let client = ManagedAgentsClient::from_env()?;
    eprintln!("✓ Client created");

    // 1. Create a memory store
    let store = client
        .create_memory_store(CreateMemoryStoreParams {
            name: "User Preferences".to_string(),
            description: Some(
                "Stores user preferences and learned information across sessions. \
                 Read this at the start of each session to personalize responses. \
                 Write new preferences when the user shares them."
                    .to_string(),
            ),
        })
        .await?;
    eprintln!("✓ Memory store created: {}", store.id);

    // 2. Seed the store with initial memories
    let mem1 = client
        .create_memory(
            &store.id,
            CreateMemoryParams {
                path: "/preferences/language.md".to_string(),
                content: "# Language Preferences\n\n- Preferred language: English\n- Tone: casual and friendly\n- Avoid jargon unless asked".to_string(),
            },
        )
        .await?;
    eprintln!("✓ Memory seeded: {} (language prefs)", mem1.id);

    let mem2 = client
        .create_memory(
            &store.id,
            CreateMemoryParams {
                path: "/preferences/topics.md".to_string(),
                content: "# Topic Interests\n\n- Rust programming\n- Systems design\n- Coffee brewing methods".to_string(),
            },
        )
        .await?;
    eprintln!("✓ Memory seeded: {} (topic interests)", mem2.id);

    // 3. Create agent and environment
    let agent = client
        .create_agent(CreateAgentParams {
            name: "Memory Agent".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some(
                "You are a personalized assistant with persistent memory. \
                 At the start of each conversation, check /mnt/memory/ for user preferences. \
                 When the user shares new preferences, save them to memory. \
                 Always acknowledge what you remember about the user."
                    .to_string(),
            ),
            description: Some("Agent with persistent memory across sessions".to_string()),
            tools: vec![ToolConfig::agent_toolset()],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: None,
            metadata: None,
        })
        .await?;
    eprintln!("✓ Agent created: {}", agent.id);

    let env = client
        .create_environment(CreateEnvironmentParams::cloud("memory-env"))
        .await?;
    eprintln!("✓ Environment created: {}", env.id);

    // 4. Session 1: Tell the agent something new to remember
    eprintln!("\n── Session 1: Teaching the agent ──\n");

    // Build session params with memory store resource via raw JSON
    // (SessionResource only supports files, so we construct the full params)
    let memory_resource = MemoryStoreResource::read_write(&store.id)
        .with_instructions("Check this store for user preferences at the start of each session.");

    let mut session1_params = CreateSessionParams::new(&agent.id, &env.id);
    // Add memory store as a raw JSON resource in the resources array
    let memory_resource_json = serde_json::to_value(&memory_resource)?;
    // We need to serialize the full params and inject the memory store resource
    let mut params_json = serde_json::to_value(&session1_params)?;
    params_json["resources"] = serde_json::json!([memory_resource_json]);
    // Deserialize back to CreateSessionParams
    session1_params = serde_json::from_value(params_json)?;

    let session1 = client.create_session(session1_params).await?;
    eprintln!("✓ Session 1 created: {}", session1.id);

    let output1 = run_session_turn(
        &client,
        &session1.id,
        "Hey! I want you to remember that my favorite programming language is Rust, \
         and I prefer dark mode in all my tools. Also, I'm working on a project called \
         'Starlight' that's a distributed database.",
    )
    .await?;

    eprintln!("← Agent (Session 1):");
    println!("{output1}");

    client.archive_session(&session1.id).await?;
    eprintln!("\n✓ Session 1 archived");

    // 5. Session 2: Verify the agent remembers
    eprintln!("\n── Session 2: Testing recall ──\n");

    let mut session2_params = CreateSessionParams::new(&agent.id, &env.id);
    let mut params_json = serde_json::to_value(&session2_params)?;
    params_json["resources"] = serde_json::json!([memory_resource_json]);
    session2_params = serde_json::from_value(params_json)?;

    let session2 = client.create_session(session2_params).await?;
    eprintln!("✓ Session 2 created: {}", session2.id);

    let output2 = run_session_turn(
        &client,
        &session2.id,
        "What do you remember about me and my preferences?",
    )
    .await?;

    eprintln!("← Agent (Session 2):");
    println!("{output2}");

    client.archive_session(&session2.id).await?;
    eprintln!("\n✓ Session 2 archived");

    // 6. List memories to verify persistence
    eprintln!("\n── Memory Store Contents ──\n");
    let memories = client.list_memories(&store.id).await?;
    for mem in &memories {
        eprintln!(
            "  📝 {} — {} bytes",
            mem.path.as_deref().unwrap_or("(no path)"),
            mem.content.as_deref().map(|c| c.len()).unwrap_or(0)
        );
    }

    // 7. Cleanup
    eprintln!("\nCleaning up...");
    for mem in &memories {
        client.delete_memory(&store.id, &mem.id).await?;
    }
    eprintln!("  ✓ Memories deleted");
    client.delete_memory_store(&store.id).await?;
    eprintln!("  ✓ Memory store deleted");
    client.archive_agent(&agent.id).await?;
    eprintln!("  ✓ Agent archived");
    client.archive_environment(&env.id).await?;
    eprintln!("  ✓ Environment archived");

    eprintln!("\n=== Done ===");
    Ok(())
}
