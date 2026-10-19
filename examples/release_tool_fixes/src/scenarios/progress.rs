//! Item 6: a client that stalls while a tool floods progress still receives the truncation
//! marker.

use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, Tool, ToolContext, UserId};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::common::{APP, Mode, Report, USER, Verdict, verdict};
use crate::upstream::{Reply, Upstream};

const MARKER: &str = "[adk: tool progress truncated]";
/// More chunks than the agent's progress queue holds.
const CHUNKS: usize = 1_000;

/// Streams build output from a worker task while the tool waits for it, like a shell tool.
struct BuildTool;

#[async_trait]
impl Tool for BuildTool {
    fn name(&self) -> &str {
        "run_build"
    }

    fn description(&self) -> &str {
        "Runs the build and streams its output."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({"type": "object", "properties": {}}))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        let reporter = ctx.clone();
        let worker = tokio::spawn(async move {
            for line in 0..CHUNKS {
                reporter.emit_progress("stdout", &format!("compiling unit {line}\n")).await;
            }
        });
        let _ = worker.await;
        Ok(json!({"status": "ok"}))
    }
}

pub async fn run(report: &mut Report) {
    report.record_result("progress-marker-survives-full-queue", 6, Mode::Scripted, stall().await);
}

async fn stall() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![
        Reply::ToolCall { name: "run_build", args: json!({}) },
        Reply::Text("Build finished."),
    ])
    .await?;
    let agent = LlmAgentBuilder::new("builder")
        .instruction("Run the build.")
        .model(Arc::new(upstream.client()?))
        .tool(Arc::new(BuildTool))
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: None,
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .build()?;
    let mut stream = runner
        .run_with_config(
            UserId::new(USER)?,
            SessionId::new(session.id())?,
            Content::new("user").with_text("Build it."),
            None,
        )
        .await?;

    let mut progress = 0;
    let mut markers = 0;
    let mut error = None;
    while let Some(item) = stream.next().await {
        let event = match item {
            Ok(event) => event,
            Err(err) => {
                error = Some(err.to_string());
                break;
            }
        };
        if event.tool_progress_stream().is_none() {
            continue;
        }
        let text: String = event
            .llm_response
            .content
            .iter()
            .flat_map(|content| &content.parts)
            .filter_map(Part::text)
            .collect();
        if text == MARKER {
            markers += 1;
        } else {
            progress += 1;
        }
        if progress == 1 {
            // A UI busy rendering: the worker keeps emitting into a full queue.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(verdict(vec![
        (error.is_none(), format!("run error: {error:?}")),
        (progress < CHUNKS, format!("{progress} of {CHUNKS} chunks forwarded")),
        (markers == 1, format!("{markers} truncation marker(s)")),
    ]))
}
