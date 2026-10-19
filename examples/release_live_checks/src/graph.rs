//! Item 6: a `GraphAgent` turn run by `Runner` takes as many super-steps as the graph's own
//! recursion limit allows, past the 50 a run used to be capped at.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{Content, SessionId, UserId};
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::node::NodeOutput;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::json;

use crate::report::{Mode, Report, Verdict, check, fail};

const STEPS: i64 = 70;

pub async fn run(report: &mut Report) {
    report.record_result("graph_agent_turn_past_fifty_steps", 6, Mode::Scripted, turn().await);
}

async fn turn() -> anyhow::Result<Verdict> {
    let agent = GraphAgent::builder("counter")
        .channels(&["count", "result"])
        .node_fn("tick", |ctx| async move {
            let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
            Ok(NodeOutput::new()
                .with_update("count", json!(count))
                .with_update("result", json!(format!("counted to {count}"))))
        })
        .edge(START, "tick")
        .conditional_edge(
            "tick",
            |state| {
                let count = state.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                if count >= STEPS { END.to_string() } else { "tick".to_string() }
            },
            [("tick", "tick"), (END, END)],
        )
        .recursion_limit(100)
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "release-live-checks".to_string(),
            user_id: "user-1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("release-live-checks")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .build()?;
    let mut stream = runner
        .run(
            UserId::new("user-1")?,
            SessionId::new(session.id())?,
            Content::new("user").with_text("count"),
        )
        .await?;
    let mut answer = String::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => {
                if let Some(content) = event.llm_response.content {
                    answer = content.parts.iter().filter_map(|part| part.text()).collect();
                }
            }
            Err(error) => return Ok(fail(format!("{}: {}", error.code, error.message))),
        }
    }
    Ok(check(answer == format!("counted to {STEPS}"), format!("final answer {answer:?}")))
}
