//! Integration tests for `adk-enterprise` against a real platform instance.
//!
//! These tests are marked `#[ignore]` because they require:
//! - A running platform at the configured base URL
//! - A valid `ADK_API_KEY` environment variable
//!
//! Run with: `cargo test -p adk-enterprise --test integration_tests -- --ignored`
//!
//! **Validates: Requirements 15.1, 15.2, 15.3**

use adk_enterprise::{ContentBlock, CreateAgentParams, EnterpriseClient, SessionEvent, ToolConfig};
use futures::StreamExt;

/// Helper: create a client from the environment, panicking with guidance if missing.
fn create_client() -> EnterpriseClient {
    EnterpriseClient::from_env().expect(
        "ADK_API_KEY or ADK_ENTERPRISE_KEY must be set to run integration tests. \
         Set the environment variable and ensure a platform instance is running.",
    )
}

/// Full lifecycle integration test.
///
/// Exercises the complete happy path:
/// 1. Create an agent
/// 2. Create a session
/// 3. Open an SSE event stream
/// 4. Send a message
/// 5. Collect the response (wait for `StatusIdle`)
/// 6. Archive the session
///
/// **Validates: Requirements 15.1, 15.2, 15.3**
#[tokio::test]
#[ignore]
async fn test_full_lifecycle() {
    let client = create_client();

    // 1. Create an agent with a simple system prompt
    let agent = client
        .create_agent(CreateAgentParams {
            name: "Integration Test Agent".into(),
            model: "gemini-2.5-flash".into(),
            system: Some("You are a helpful assistant. Keep responses brief.".into()),
            ..Default::default()
        })
        .await
        .expect("failed to create agent");

    assert!(!agent.id.is_empty(), "agent ID should be non-empty");
    assert_eq!(agent.name, "Integration Test Agent");
    assert_eq!(agent.version, 1);

    // 2. Create a session bound to the agent
    let session = client.create_session(&agent.id, None).await.expect("failed to create session");

    assert!(!session.id.is_empty(), "session ID should be non-empty");
    assert_eq!(session.agent_id, agent.id);

    // 3. Open SSE stream BEFORE sending (required ordering per design)
    let mut stream = client.stream_events(&session.id).await.expect("failed to open event stream");

    // 4. Send a message
    client
        .send_message(&session.id, "What is 2 + 2? Reply with just the number.")
        .await
        .expect("failed to send message");

    // 5. Collect events until StatusIdle
    let mut received_message = false;
    let mut received_idle = false;
    let timeout = tokio::time::Duration::from_secs(60);

    let result = tokio::time::timeout(timeout, async {
        while let Some(event) = stream.next().await {
            match event.expect("stream error") {
                SessionEvent::Message { content, .. } => {
                    // Verify we got at least one text content block
                    let has_text =
                        content.iter().any(|block| matches!(block, ContentBlock::Text { .. }));
                    if has_text {
                        received_message = true;
                    }
                }
                SessionEvent::StatusIdle { .. } => {
                    received_idle = true;
                    break;
                }
                SessionEvent::StatusRunning { .. } => {
                    // Expected — agent is processing
                }
                SessionEvent::Error { message, .. } => {
                    panic!("received error event from agent: {message}");
                }
                _ => {
                    // Other events (tool use, etc.) are fine to ignore here
                }
            }
        }
    })
    .await;

    assert!(result.is_ok(), "timed out waiting for agent response");
    assert!(received_message, "expected at least one Message event");
    assert!(received_idle, "expected StatusIdle event");

    // 6. Archive the session (cleanup)
    let archived = client.archive_session(&session.id).await.expect("failed to archive session");

    assert_eq!(archived.status, adk_enterprise::SessionStatus::Archived);

    // Cleanup: delete the agent
    client.delete_agent(&agent.id).await.expect("failed to delete agent");
}

/// Custom tool round-trip integration test.
///
/// Exercises the custom tool flow:
/// 1. Create an agent with a custom tool definition
/// 2. Start a session
/// 3. Open an SSE stream
/// 4. Send a message that should trigger the custom tool
/// 5. Receive `CustomToolUse` event
/// 6. Send `custom_tool_result` back
/// 7. Receive final message incorporating the tool result
///
/// **Validates: Requirements 15.1, 15.2, 15.3**
#[tokio::test]
#[ignore]
async fn test_custom_tool_round_trip() {
    let client = create_client();

    // 1. Create an agent with a custom tool
    let agent = client
        .create_agent(CreateAgentParams {
            name: "Custom Tool Test Agent".into(),
            model: "gemini-2.5-flash".into(),
            system: Some(
                "You are an assistant with access to a get_weather tool. \
                 When asked about weather, ALWAYS use the get_weather tool. \
                 After receiving the tool result, report it to the user."
                    .into(),
            ),
            tools: vec![ToolConfig::custom(
                "get_weather",
                "Get current weather for a city. Returns temperature and conditions.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "city": {
                            "type": "string",
                            "description": "The city name"
                        }
                    },
                    "required": ["city"]
                }),
            )],
            ..Default::default()
        })
        .await
        .expect("failed to create agent with custom tool");

    assert!(!agent.tools.is_empty(), "agent should have tools");

    // 2. Create a session
    let session = client.create_session(&agent.id, None).await.expect("failed to create session");

    // 3. Open SSE stream
    let mut stream = client.stream_events(&session.id).await.expect("failed to open event stream");

    // 4. Send a message that triggers the tool
    client
        .send_message(&session.id, "What is the weather in Tokyo?")
        .await
        .expect("failed to send message");

    // 5. Wait for CustomToolUse event
    let timeout = tokio::time::Duration::from_secs(60);
    let mut tool_use_id: Option<String> = None;
    let mut final_message_received = false;

    let result = tokio::time::timeout(timeout, async {
        while let Some(event) = stream.next().await {
            match event.expect("stream error") {
                SessionEvent::CustomToolUse { custom_tool_use_id, name, input, .. } => {
                    // Verify the tool call
                    assert_eq!(name, "get_weather", "expected get_weather tool call");
                    assert!(input.get("city").is_some(), "expected 'city' in tool input");

                    // 6. Send the custom tool result back
                    let result_content =
                        format!("22°C, sunny in {}", input["city"].as_str().unwrap_or("unknown"));
                    client
                        .custom_tool_result(
                            &session.id,
                            &custom_tool_use_id,
                            vec![adk_enterprise::ContentBlock::text(result_content)],
                        )
                        .await
                        .expect("failed to send custom tool result");

                    tool_use_id = Some(custom_tool_use_id);
                }
                SessionEvent::Message { content, .. }
                    // 7. After tool result is sent, agent should produce a final message
                    if tool_use_id.is_some() => {
                        let has_text =
                            content.iter().any(|block| matches!(block, ContentBlock::Text { .. }));
                        if has_text {
                            final_message_received = true;
                        }
                    }
                SessionEvent::StatusIdle { .. } => {
                    break;
                }
                SessionEvent::Error { message, .. } => {
                    panic!("received error event: {message}");
                }
                _ => {}
            }
        }
    })
    .await;

    assert!(result.is_ok(), "timed out waiting for custom tool flow");
    assert!(tool_use_id.is_some(), "expected a CustomToolUse event from the agent");
    assert!(final_message_received, "expected a final Message after tool result");

    // Cleanup
    client.archive_session(&session.id).await.expect("failed to archive session");
    client.delete_agent(&agent.id).await.expect("failed to delete agent");
}

/// Self-hosted client compatibility test.
///
/// Verifies that `EnterpriseClient::self_hosted()` produces a client that
/// behaves identically to the production client, just targeting a different URL.
/// This confirms Requirement 15.3: no code path is conditional on the base URL.
///
/// **Validates: Requirements 15.1, 15.2, 15.3**
#[tokio::test]
#[ignore]
async fn test_self_hosted_client() {
    // Read the API key from environment
    let api_key = std::env::var("ADK_API_KEY")
        .or_else(|_| std::env::var("ADK_ENTERPRISE_KEY"))
        .expect("ADK_API_KEY or ADK_ENTERPRISE_KEY must be set");

    // Read optional self-hosted URL (defaults to production if not set)
    let base_url = std::env::var("ADK_ENTERPRISE_URL")
        .unwrap_or_else(|_| "https://enterprise.adk-rust.com/managed/v1".into());

    // Create client via self_hosted() constructor
    let client = EnterpriseClient::self_hosted(&api_key, &base_url)
        .expect("failed to create self-hosted client");

    // Verify client configuration
    assert_eq!(client.config().api_key, api_key);
    assert_eq!(client.config().base_url, base_url);

    // Exercise the same lifecycle as the production client
    let agent = client
        .create_agent(CreateAgentParams {
            name: "Self-Hosted Test Agent".into(),
            model: "gemini-2.5-flash".into(),
            system: Some("You are brief.".into()),
            ..Default::default()
        })
        .await
        .expect("self-hosted: failed to create agent");

    assert!(!agent.id.is_empty());

    let session = client
        .create_session(&agent.id, None)
        .await
        .expect("self-hosted: failed to create session");

    assert!(!session.id.is_empty());
    assert_eq!(session.agent_id, agent.id);

    // Open stream and send a message
    let mut stream =
        client.stream_events(&session.id).await.expect("self-hosted: failed to open stream");

    client
        .send_message(&session.id, "Say hello in one word.")
        .await
        .expect("self-hosted: failed to send message");

    // Collect response
    let timeout = tokio::time::Duration::from_secs(60);
    let mut got_response = false;

    let result = tokio::time::timeout(timeout, async {
        while let Some(event) = stream.next().await {
            match event.expect("stream error") {
                SessionEvent::Message { .. } => {
                    got_response = true;
                }
                SessionEvent::StatusIdle { .. } => break,
                SessionEvent::Error { message, .. } => {
                    panic!("self-hosted: error event: {message}");
                }
                _ => {}
            }
        }
    })
    .await;

    assert!(result.is_ok(), "self-hosted: timed out");
    assert!(got_response, "self-hosted: expected a response message");

    // Cleanup
    client.archive_session(&session.id).await.expect("self-hosted: failed to archive");
    client.delete_agent(&agent.id).await.expect("self-hosted: failed to delete agent");
}
