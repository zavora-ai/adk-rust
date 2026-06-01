//! Integration tests for file mounting in Managed Agents sessions.
//!
//! These tests require a real `ANTHROPIC_API_KEY` and exercise the full flow:
//! upload file → create session with resources → agent reads file → cleanup.
//!
//! Run with:
//! ```bash
//! cargo test -p adk-anthropic --features "managed-agents,files" --test managed_agents_files_integration -- --ignored --test-threads=1
//! ```

#![cfg(all(feature = "managed-agents", feature = "files"))]

use adk_anthropic::files::FilesClient;
use adk_anthropic::managed_agents::{
    CreateAgentParams, CreateEnvironmentParams, CreateSessionParams, ManagedAgentsClient,
    SessionEvent, SessionResource, ToolConfig, UserEvent,
};
use futures::StreamExt;

fn test_clients() -> Option<(ManagedAgentsClient, FilesClient)> {
    let ma = ManagedAgentsClient::from_env().ok()?;
    let files = FilesClient::from_env().ok()?;
    Some((ma, files))
}

#[tokio::test]
#[ignore]
async fn test_mount_file_at_session_creation() {
    let Some((client, files_client)) = test_clients() else { return };

    // Upload a CSV file
    let csv_data = b"name,age,city\nAlice,30,NYC\nBob,25,SF\nCharlie,35,LA".to_vec();
    let file =
        files_client.upload_file("people.csv", csv_data).await.expect("failed to upload file");
    eprintln!("Uploaded file: {}", file.id);

    // Create agent
    let agent = client
        .create_agent(CreateAgentParams {
            name: "File Reader Agent".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some(
                "You are a data analyst. Read files and answer questions about them.".to_string(),
            ),
            description: None,
            tools: vec![ToolConfig::agent_toolset()],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: None,
            metadata: None,
        })
        .await
        .expect("failed to create agent");

    let env = client
        .create_environment(CreateEnvironmentParams::cloud("file-test-env"))
        .await
        .expect("failed to create environment");

    // Create session with the file mounted
    let mut params = CreateSessionParams::new(&agent.id, &env.id);
    params.resources = vec![SessionResource::file_at(&file.id, "/workspace/people.csv")];

    let session =
        client.create_session(params).await.expect("failed to create session with resources");
    eprintln!("Created session: {}", session.id);

    // Open stream and ask about the file
    let mut stream = client.stream_events(&session.id).await.expect("failed to open stream");

    client
        .send_event(
            &session.id,
            UserEvent::message("Read /workspace/people.csv and tell me how many people are listed. Just say the number."),
        )
        .await
        .expect("failed to send message");

    let mut got_response = false;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(SessionEvent::AgentMessage { .. }) => got_response = true,
            Ok(SessionEvent::StatusIdle { .. }) => break,
            Ok(_) => continue,
            Err(e) => {
                eprintln!("Stream error: {e}");
                break;
            }
        }
    }

    assert!(got_response, "agent should have responded about the file");

    // Cleanup
    let _ = client.interrupt(&session.id).await;
    let _ = client.archive_session(&session.id).await;
    let _ = client.archive_environment(&env.id).await;
    let _ = client.archive_agent(&agent.id).await;
    let _ = files_client.delete_file(&file.id).await;
}

#[tokio::test]
#[ignore]
async fn test_add_resource_to_running_session() {
    let Some((client, files_client)) = test_clients() else { return };

    // Upload a file
    let file = files_client
        .upload_file("config.json", br#"{"version": "1.0", "debug": true}"#.to_vec())
        .await
        .expect("failed to upload file");

    // Create agent, env, session
    let agent = client
        .create_agent(CreateAgentParams {
            name: "Resource Test Agent".to_string(),
            model: serde_json::json!("claude-sonnet-4-6"),
            system: Some("You are a helpful assistant.".to_string()),
            description: None,
            tools: vec![ToolConfig::agent_toolset()],
            mcp_servers: vec![],
            skills: vec![],
            multiagent: None,
            metadata: None,
        })
        .await
        .expect("failed to create agent");

    let env = client
        .create_environment(CreateEnvironmentParams::cloud("resource-test-env"))
        .await
        .expect("failed to create environment");

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");

    // Add resource to the session after creation
    let resource = client
        .add_session_resource(&session.id, SessionResource::file(&file.id))
        .await
        .expect("failed to add resource");
    eprintln!("Added resource: {}", resource.id);

    // List resources
    let resources =
        client.list_session_resources(&session.id).await.expect("failed to list resources");
    assert!(resources.iter().any(|r| r.id == resource.id), "added resource should appear in list");

    // Delete the resource
    client
        .delete_session_resource(&session.id, &resource.id)
        .await
        .expect("failed to delete resource");

    // Cleanup
    let _ = client.archive_session(&session.id).await;
    let _ = client.archive_environment(&env.id).await;
    let _ = client.archive_agent(&agent.id).await;
    let _ = files_client.delete_file(&file.id).await;
}
