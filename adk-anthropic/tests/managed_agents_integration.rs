//! Integration tests for the Managed Agents API.
//!
//! These tests require a real `ANTHROPIC_API_KEY` environment variable and are
//! marked `#[ignore]` so they don't run in CI by default.
//!
//! Run with:
//! ```bash
//! cargo test -p adk-anthropic --features managed-agents --test managed_agents_integration -- --ignored
//! ```

#![cfg(feature = "managed-agents")]

use adk_anthropic::managed_agents::{
    CreateAgentParams, CreateEnvironmentParams, CreateSessionParams, ManagedAgentsClient,
    SessionEvent, SessionStatus, ToolConfig, UserEvent,
};
use futures::StreamExt;

// ─── Test Infrastructure ─────────────────────────────────────────────────────

struct TestFixture {
    client: ManagedAgentsClient,
    agent_ids: Vec<String>,
    environment_ids: Vec<String>,
    session_ids: Vec<String>,
}

impl TestFixture {
    fn new(client: ManagedAgentsClient) -> Self {
        Self { client, agent_ids: Vec::new(), environment_ids: Vec::new(), session_ids: Vec::new() }
    }

    /// Explicitly clean up all resources. Call this at the end of each test
    /// to ensure sessions are archived (stopping billing) before the test exits.
    async fn cleanup(&mut self) {
        // Archive/delete sessions first (stops billing immediately)
        for id in self.session_ids.drain(..) {
            // First try to interrupt if running, then archive
            let _ = self.client.interrupt(&id).await;
            let _ = self.client.archive_session(&id).await;
        }
        for id in self.environment_ids.drain(..) {
            let _ = self.client.archive_environment(&id).await;
        }
        for id in self.agent_ids.drain(..) {
            let _ = self.client.archive_agent(&id).await;
        }
    }
}

impl Drop for TestFixture {
    fn drop(&mut self) {
        // Best-effort fallback if cleanup() wasn't called explicitly
        let client = self.client.clone();
        let sessions = std::mem::take(&mut self.session_ids);
        let environments = std::mem::take(&mut self.environment_ids);
        let agents = std::mem::take(&mut self.agent_ids);

        if sessions.is_empty() && environments.is_empty() && agents.is_empty() {
            return;
        }

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                for id in sessions {
                    let _ = client.interrupt(&id).await;
                    let _ = client.archive_session(&id).await;
                }
                for id in environments {
                    let _ = client.archive_environment(&id).await;
                }
                for id in agents {
                    let _ = client.archive_agent(&id).await;
                }
            });
        }
    }
}

fn test_client() -> Option<ManagedAgentsClient> {
    match ManagedAgentsClient::from_env() {
        Ok(client) => Some(client),
        Err(_) => {
            eprintln!("ANTHROPIC_API_KEY not set, skipping integration test");
            None
        }
    }
}

fn default_agent_params() -> CreateAgentParams {
    CreateAgentParams {
        name: "ADK Test Agent".to_string(),
        model: serde_json::json!("claude-sonnet-4-6"),
        system: Some(
            "You are a helpful test assistant. Keep responses to one sentence.".to_string(),
        ),
        description: None,
        tools: vec![ToolConfig::agent_toolset()],
        mcp_servers: vec![],
        skills: vec![],
        multiagent: None,
        metadata: None,
    }
}

fn default_environment_params() -> CreateEnvironmentParams {
    CreateEnvironmentParams::cloud("adk-test-env")
}

// ─── Integration Tests ───────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_create_and_delete_agent() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    assert!(!agent.id.is_empty());
    assert_eq!(agent.model.id, "claude-sonnet-4-6");
    fixture.agent_ids.push(agent.id.clone());

    let retrieved = client.get_agent(&agent.id).await.expect("failed to get agent");
    assert_eq!(retrieved.id, agent.id);

    // Archive instead of delete (API uses archive for agents)
    client.archive_agent(&agent.id).await.expect("failed to archive agent");
    fixture.agent_ids.clear();
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_create_and_delete_environment() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    assert!(!env.id.is_empty());
    fixture.environment_ids.push(env.id.clone());

    let retrieved = client.get_environment(&env.id).await.expect("failed to get environment");
    assert_eq!(retrieved.id, env.id);

    // Archive instead of delete
    client.archive_environment(&env.id).await.expect("failed to archive environment");
    fixture.environment_ids.clear();
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_create_session_idle_status() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    assert!(!session.id.is_empty());
    assert_eq!(session.status, SessionStatus::Idle);
    fixture.session_ids.push(session.id);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_send_message_transitions_to_running() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    fixture.session_ids.push(session.id.clone());

    client
        .send_event(&session.id, UserEvent::message("Say hello in one word."))
        .await
        .expect("failed to send message");

    let updated = client.get_session(&session.id).await.expect("failed to get session");
    assert!(
        updated.status == SessionStatus::Running || updated.status == SessionStatus::Idle,
        "session should be running or idle, got: {:?}",
        updated.status
    );
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_stream_agent_message_events() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    fixture.session_ids.push(session.id.clone());

    // Open stream FIRST (docs: open stream before sending events)
    let mut stream = client.stream_events(&session.id).await.expect("failed to open SSE stream");

    // Then send message
    client
        .send_event(&session.id, UserEvent::message("Say hello in one word."))
        .await
        .expect("failed to send message");

    let mut received_agent_message = false;
    let mut received_status_idle = false;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(SessionEvent::AgentMessage { .. }) => received_agent_message = true,
            Ok(SessionEvent::StatusIdle { .. }) => {
                received_status_idle = true;
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("Stream error: {e}");
                break;
            }
        }
    }

    assert!(received_agent_message, "should have received at least one AgentMessage event");
    assert!(received_status_idle, "should have received a StatusIdle event");
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_custom_tool_flow() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent_params = CreateAgentParams {
        name: "Custom Tool Test Agent".to_string(),
        model: serde_json::json!("claude-sonnet-4-6"),
        system: Some("You are a helpful assistant. When asked about weather, ALWAYS use the get_weather tool. Do not answer without using the tool.".to_string()),
        description: None,
        tools: vec![
            ToolConfig::agent_toolset(),
            ToolConfig::custom(
                "get_weather",
                "Get current weather for a location. Always use this when asked about weather.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string", "description": "City name"}
                    },
                    "required": ["location"]
                }),
            ),
        ],
        mcp_servers: vec![],
        skills: vec![],
        multiagent: None,
            metadata: None,
    };

    let agent = client.create_agent(agent_params).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    fixture.session_ids.push(session.id.clone());

    // Open stream first
    let mut stream = client.stream_events(&session.id).await.expect("failed to open SSE stream");

    // Send message that triggers custom tool
    client
        .send_event(&session.id, UserEvent::message("What is the weather in San Francisco?"))
        .await
        .expect("failed to send message");

    let mut custom_tool_event_id = String::new();
    let mut received_custom_tool_use = false;

    // Wait for AgentCustomToolUse or StatusIdle with requires_action
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(SessionEvent::AgentCustomToolUse { id, name, .. }) => {
                if let Some(event_id) = id {
                    custom_tool_event_id = event_id;
                    received_custom_tool_use = true;
                }
                eprintln!("Got custom tool use: {:?}", name);
                break;
            }
            Ok(SessionEvent::StatusIdle { stop_reason, .. }) => {
                // Check if it's requires_action
                if let Some(reason) = &stop_reason {
                    if reason.get("type").and_then(|v| v.as_str()) == Some("requires_action") {
                        if let Some(ids) = reason.get("event_ids").and_then(|v| v.as_array()) {
                            if let Some(first_id) = ids.first().and_then(|v| v.as_str()) {
                                custom_tool_event_id = first_id.to_string();
                                received_custom_tool_use = true;
                            }
                        }
                    }
                }
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("Stream error: {e}");
                break;
            }
        }
    }

    assert!(
        received_custom_tool_use,
        "should have received a custom tool use event or requires_action idle"
    );

    // Send tool result
    client
        .custom_tool_result(
            &session.id,
            &custom_tool_event_id,
            r#"{"temperature": "72°F", "condition": "sunny"}"#,
        )
        .await
        .expect("failed to send tool result");

    // Wait for idle after tool result
    let mut received_idle = false;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(SessionEvent::StatusIdle { .. }) => {
                received_idle = true;
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("Stream error after tool result: {e}");
                break;
            }
        }
    }

    assert!(received_idle, "session should return to idle after custom tool result");
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_interrupt_running_session() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    fixture.session_ids.push(session.id.clone());

    // Open stream first
    let mut stream = client.stream_events(&session.id).await.expect("failed to open SSE stream");

    // Send a long task
    client
        .send_event(
            &session.id,
            UserEvent::message("Write a 2000 word essay about the history of computing."),
        )
        .await
        .expect("failed to send message");

    // Wait for running
    let mut received_running = false;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(SessionEvent::StatusRunning {}) => {
                received_running = true;
                break;
            }
            Ok(SessionEvent::StatusIdle { .. }) => break,
            Ok(_) => continue,
            Err(e) => {
                eprintln!("Stream error: {e}");
                break;
            }
        }
    }

    if !received_running {
        eprintln!("Agent completed before running status observed; skipping interrupt");
        return;
    }

    // Interrupt
    client.interrupt(&session.id).await.expect("failed to interrupt");

    // Wait for idle
    let mut received_idle = false;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(SessionEvent::StatusIdle { .. }) => {
                received_idle = true;
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("Stream error after interrupt: {e}");
                break;
            }
        }
    }

    assert!(received_idle, "session should transition to idle after interrupt");
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_resume_idle_session() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    fixture.session_ids.push(session.id.clone());

    // First interaction
    let mut stream = client.stream_events(&session.id).await.expect("failed to open stream");
    client.send_event(&session.id, UserEvent::message("Say hi.")).await.expect("failed to send");

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(SessionEvent::StatusIdle { .. }) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    drop(stream);

    // Verify idle
    let session_state = client.get_session(&session.id).await.expect("failed to get session");
    assert_eq!(session_state.status, SessionStatus::Idle);

    // Resume with new message
    client
        .send_event(&session.id, UserEvent::message("Say goodbye."))
        .await
        .expect("failed to resume");

    let resumed =
        client.get_session(&session.id).await.expect("failed to get session after resume");
    assert!(
        resumed.status == SessionStatus::Running || resumed.status == SessionStatus::Idle,
        "session should be running or idle after resume, got: {:?}",
        resumed.status
    );
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_archive_session() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    fixture.session_ids.push(session.id.clone());

    client.archive_session(&session.id).await.expect("failed to archive session");

    let archived = client.get_session(&session.id).await.expect("failed to get archived session");
    assert_eq!(archived.status, SessionStatus::Terminated);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_delete_session() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    let session_id = session.id.clone();

    client.delete_session(&session_id).await.expect("failed to delete session");

    let result = client.get_session(&session_id).await;
    assert!(result.is_err(), "session should not exist after deletion");
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_usage_tracking() {
    let Some(client) = test_client() else { return };
    let mut fixture = TestFixture::new(client.clone());

    let agent = client.create_agent(default_agent_params()).await.expect("failed to create agent");
    fixture.agent_ids.push(agent.id.clone());

    let env = client
        .create_environment(default_environment_params())
        .await
        .expect("failed to create environment");
    fixture.environment_ids.push(env.id.clone());

    let session = client
        .create_session(CreateSessionParams::new(&agent.id, &env.id))
        .await
        .expect("failed to create session");
    fixture.session_ids.push(session.id.clone());

    // Open stream, send message, wait for idle
    let mut stream = client.stream_events(&session.id).await.expect("failed to open stream");
    client
        .send_event(&session.id, UserEvent::message("What is 2 + 2?"))
        .await
        .expect("failed to send");

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(SessionEvent::StatusIdle { .. }) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    drop(stream);

    let completed = client.get_session(&session.id).await.expect("failed to get session");
    assert!(
        completed.usage.input_tokens > 0,
        "input_tokens should be > 0, got: {}",
        completed.usage.input_tokens
    );
    assert!(
        completed.usage.output_tokens > 0,
        "output_tokens should be > 0, got: {}",
        completed.usage.output_tokens
    );
    fixture.cleanup().await;
}

// ─── Cleanup Utility ─────────────────────────────────────────────────────────

/// Run this test manually to clean up any orphaned resources from failed test runs.
/// It lists all sessions and archives any that are still idle/running.
///
/// ```bash
/// cargo test -p adk-anthropic --features managed-agents --test managed_agents_integration -- --ignored test_cleanup_orphaned_resources
/// ```
#[tokio::test]
#[ignore]
async fn test_cleanup_orphaned_resources() {
    let Some(client) = test_client() else { return };

    // List all sessions and archive any that are idle or running
    let sessions = client.list_sessions(None).await.unwrap_or_default();
    let mut archived = 0;
    for session in &sessions {
        if session.status == SessionStatus::Idle || session.status == SessionStatus::Running {
            let _ = client.interrupt(&session.id).await;
            if client.archive_session(&session.id).await.is_ok() {
                archived += 1;
                eprintln!("Archived session: {}", session.id);
            }
        }
    }
    eprintln!("Cleaned up {archived} sessions out of {} total", sessions.len());
}
