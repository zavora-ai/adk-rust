//! Process-lifecycle tests against a scripted ACP agent subprocess: every
//! client entry point returns, rather than hangs, when the agent exits.

use std::sync::Arc;
use std::time::Duration;

use adk_acp::{
    AcpAgentConfig, AcpSession, PermissionPolicy, StatusTracker, prompt_agent,
    prompt_agent_with_policy, stream_prompt,
};

const LIMIT: Duration = Duration::from_secs(15);

fn agent(mode: &str) -> AcpAgentConfig {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_acp_agent.py");
    AcpAgentConfig::new(format!("python3 {script} {mode}"))
}

#[tokio::test]
async fn one_shot_prompt_returns_the_agent_text() {
    let text = tokio::time::timeout(LIMIT, prompt_agent(&agent("normal"), "hi"))
        .await
        .expect("prompt_agent hung")
        .expect("prompt succeeds");
    assert_eq!(text, "hello");
}

#[tokio::test]
async fn one_shot_prompt_fails_when_the_agent_exits_mid_prompt() {
    let result = tokio::time::timeout(LIMIT, prompt_agent(&agent("die_on_prompt"), "hi"))
        .await
        .expect("prompt_agent hung after the agent exited");
    assert!(result.is_err(), "got {result:?}");
}

#[tokio::test]
async fn permission_request_from_an_exiting_agent_does_not_hang() {
    let policy = Arc::new(PermissionPolicy::DenyAll);
    let result = tokio::time::timeout(
        LIMIT,
        prompt_agent_with_policy(&agent("permission_then_die"), "hi", policy),
    )
    .await
    .expect("prompt hung after a permission request from an exiting agent");
    assert!(result.is_err(), "got {result:?}");
}

#[tokio::test]
async fn persistent_session_fails_when_the_agent_exits_mid_prompt() {
    let mut session = tokio::time::timeout(
        LIMIT,
        AcpSession::start(agent("die_second_prompt"), Arc::new(PermissionPolicy::DenyAll)),
    )
    .await
    .expect("session start hung")
    .expect("session starts");
    let first =
        tokio::time::timeout(LIMIT, session.prompt("one")).await.expect("first prompt hung");
    assert_eq!(first.expect("first prompt succeeds").text, "hello");
    let second =
        tokio::time::timeout(LIMIT, session.prompt("two")).await.expect("second prompt hung");
    assert!(second.is_err(), "got {:?}", second.map(|r| r.text));
}

#[tokio::test]
async fn persistent_session_fails_when_the_agent_exits_while_idle() {
    let mut session = tokio::time::timeout(
        LIMIT,
        AcpSession::start(agent("die_idle"), Arc::new(PermissionPolicy::DenyAll)),
    )
    .await
    .expect("session start hung")
    .expect("session starts");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let result = tokio::time::timeout(LIMIT, session.prompt("hi"))
        .await
        .expect("prompt hung after the agent exited while idle");
    assert!(result.is_err(), "got {:?}", result.map(|r| r.text));
}

#[tokio::test]
async fn stream_ends_when_the_agent_exits_mid_prompt() {
    let mut stream = tokio::time::timeout(
        LIMIT,
        stream_prompt(
            &agent("die_on_prompt"),
            "hi",
            Arc::new(PermissionPolicy::DenyAll),
            StatusTracker::new(),
        ),
    )
    .await
    .expect("stream_prompt hung")
    .expect("stream starts");
    tokio::time::timeout(LIMIT, async { while stream.recv().await.is_some() {} })
        .await
        .expect("stream stayed open after the agent exited");
}
