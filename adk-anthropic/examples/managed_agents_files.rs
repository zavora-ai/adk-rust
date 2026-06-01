//! Managed Agents: File Upload and Processing
//!
//! Shows uploading a file via the Files API, mounting it in a session,
//! and asking the agent to analyze the file contents.
//!
//! # Usage
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run -p managed-agents-files
//! ```

use adk_anthropic::managed_agents::{
    CreateAgentParams, CreateEnvironmentParams, CreateSessionParams, ManagedAgentsClient,
    SessionEvent, SessionResource, ToolConfig, UserEvent,
};
use futures::StreamExt;

/// Generate sample CSV data for the demo.
fn sample_csv() -> Vec<u8> {
    let csv = "\
month,product,units_sold,revenue,region
Jan,Widget A,150,4500.00,North
Jan,Widget B,89,2670.00,South
Feb,Widget A,175,5250.00,North
Feb,Widget B,102,3060.00,South
Mar,Widget A,200,6000.00,North
Mar,Widget B,95,2850.00,South
Mar,Widget C,50,2500.00,East
Apr,Widget A,180,5400.00,North
Apr,Widget B,110,3300.00,South
Apr,Widget C,75,3750.00,East
May,Widget A,220,6600.00,North
May,Widget B,130,3900.00,South
May,Widget C,90,4500.00,East
Jun,Widget A,195,5850.00,North
Jun,Widget B,115,3450.00,South
Jun,Widget C,110,5500.00,East
";
    csv.as_bytes().to_vec()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    eprintln!("=== Managed Agents: File Upload and Processing ===\n");

    // Create client
    let client = ManagedAgentsClient::from_env()?;
    eprintln!("✓ Client created");

    // 1. Upload a CSV file (uses managed-agents beta header)
    let csv_data = sample_csv();
    let file_resp: serde_json::Value = client.upload_file("sales_data.csv", csv_data).await?;
    let file_id = file_resp["id"].as_str().unwrap().to_string();
    let file_size = file_resp["size_bytes"].as_u64().unwrap_or(0);
    eprintln!("✓ File uploaded: {} ({} bytes)", file_id, file_size);

    // 2. Create agent
    let agent = client
        .create_agent(CreateAgentParams {
            name: "Data Analyst".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some(
                "You are a data analyst. When given files, analyze them thoroughly \
                 and provide insights with specific numbers from the data."
                    .to_string(),
            ),
            description: Some("Analyzes uploaded data files".to_string()),
            tools: vec![ToolConfig::agent_toolset()],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: None,
            metadata: None,
        })
        .await?;
    eprintln!("✓ Agent created: {}", agent.id);

    // 3. Create environment
    let env = client.create_environment(CreateEnvironmentParams::cloud("files-env")).await?;
    eprintln!("✓ Environment created: {}", env.id);

    // 4. Create session with the file mounted
    let mut session_params = CreateSessionParams::new(&agent.id, &env.id);
    session_params.resources = vec![SessionResource::file(&file_id)];

    let session = client.create_session(session_params).await?;
    eprintln!("✓ Session created with file mounted: {}", session.id);

    // 5. Open stream and send analysis request
    let mut stream = client.stream_events(&session.id).await?;
    eprintln!("✓ Stream opened\n");

    client
        .send_event(
            &session.id,
            UserEvent::message(
                "I've uploaded a file called sales_data.csv. Find it and analyze it. \
                 Provide: 1) Total revenue by product, 2) Month-over-month growth trends, \
                 3) Which region is performing best. Be specific with numbers.",
            ),
        )
        .await?;
    eprintln!("→ Sent analysis request\n");

    // 6. Stream the response
    eprintln!("← Agent analysis:");
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
            SessionEvent::AgentToolUse { name, .. } => {
                eprintln!("  ⚙️  Tool: {name:?}");
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

    // 7. Cleanup
    eprintln!("\nCleaning up...");
    client.archive_session(&session.id).await?;
    eprintln!("  ✓ Session archived");
    client.archive_agent(&agent.id).await?;
    eprintln!("  ✓ Agent archived");
    client.archive_environment(&env.id).await?;
    eprintln!("  ✓ Environment archived");

    eprintln!("\n=== Done ===");
    Ok(())
}
