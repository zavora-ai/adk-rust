//! Item 7: `AcpServerHandle::shutdown` lets a running prompt finish within
//! `shutdown_timeout`.
//!
//! The server is this binary, started by the real ACP client (`AcpSession`) with
//! `--acp-server <shutdown timeout ms>`. It requests shutdown as soon as the agent starts
//! the prompt, which takes `AGENT_DELAY`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_acp::server::{AcpServer, AcpServerConfigBuilder};
use adk_acp::{AcpAgentConfig, AcpSession, PermissionPolicy};
use adk_core::{Agent, Content, Event, EventStream, InvocationContext};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use tokio::sync::Notify;

use crate::common::{Mode, Report, Verdict, verdict};

/// Runs this binary as an ACP server over stdio.
pub const SERVER_FLAG: &str = "--acp-server";

const AGENT_DELAY: Duration = Duration::from_millis(1_500);
const ANSWER: &str = "the slow answer";

/// Answers after `AGENT_DELAY`, and signals when a prompt starts.
struct SlowAgent {
    started: Arc<Notify>,
}

#[async_trait]
impl Agent for SlowAgent {
    fn name(&self) -> &str {
        "slow-agent"
    }

    fn description(&self) -> &str {
        "Answers after a pause"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        self.started.notify_one();
        let invocation = ctx.invocation_id().to_string();
        Ok(Box::pin(futures::stream::once(async move {
            tokio::time::sleep(AGENT_DELAY).await;
            let mut event = Event::new(&invocation);
            event.author = "slow-agent".to_string();
            event.set_content(Content::new("model").with_text(ANSWER));
            Ok(event)
        })))
    }
}

/// Serves `SlowAgent` and shuts down once the first prompt starts.
pub async fn serve_stdio(shutdown_timeout_ms: u64) -> anyhow::Result<()> {
    let started = Arc::new(Notify::new());
    let config = AcpServerConfigBuilder::new()
        .agent(Arc::new(SlowAgent { started: started.clone() }))
        .session_service(Arc::new(InMemorySessionService::new()))
        .agent_name("slow-agent")
        .shutdown_timeout(Duration::from_millis(shutdown_timeout_ms))
        .build()?;
    let handle = AcpServer::run(config).await?;
    started.notified().await;
    handle.shutdown();
    handle.wait().await?;
    Ok(())
}

pub async fn run(report: &mut Report) {
    report.record_result("acp-shutdown-lets-prompt-finish", 7, Mode::Local, graceful().await);
    report.record_result("acp-shutdown-timeout-bounds-wait", 7, Mode::Local, bounded().await);
}

async fn prompt_during_shutdown(
    shutdown_timeout_ms: u64,
) -> anyhow::Result<(Result<String, String>, Duration)> {
    let exe = std::env::current_exe()?;
    let command = format!("'{}' {SERVER_FLAG} {shutdown_timeout_ms}", exe.display());
    let mut session =
        AcpSession::start(AcpAgentConfig::new(command), Arc::new(PermissionPolicy::DenyAll))
            .await?;
    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(20), session.prompt("hello")).await;
    let elapsed = started.elapsed();
    let _ = session.close().await;
    let outcome = match outcome {
        Ok(Ok(result)) => Ok(result.text),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("no answer within 20 s".to_string()),
    };
    Ok((outcome, elapsed))
}

/// Shutdown requested mid-prompt: the prompt still completes within the 5 s timeout.
async fn graceful() -> anyhow::Result<Verdict> {
    let (outcome, elapsed) = prompt_during_shutdown(5_000).await?;
    Ok(verdict(vec![(
        outcome.as_deref().is_ok_and(|text| text.contains(ANSWER)),
        format!("prompt outcome {outcome:?} after {elapsed:?}"),
    )]))
}

/// A prompt longer than a 200 ms timeout is cut off at about the timeout.
async fn bounded() -> anyhow::Result<Verdict> {
    let (outcome, elapsed) = prompt_during_shutdown(200).await?;
    Ok(verdict(vec![
        (
            !outcome.as_deref().is_ok_and(|text| text.contains(ANSWER)),
            format!("prompt outcome {outcome:?}"),
        ),
        (elapsed < AGENT_DELAY, format!("ended after {elapsed:?}")),
    ]))
}
