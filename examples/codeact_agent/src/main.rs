//! # CodeAct Agent example
//!
//! Runs the ADK-Rust [`CodeActAgent`] — the agent that *acts by writing and running
//! code* — end-to-end against a self-contained [`LineScriptRuntime`] (see
//! [`runtime`]). No API key or native interpreter is required: a small
//! deterministic model (`DemoLlm`) drives the loop so the example always runs.
//!
//! It demonstrates the whole CodeAct loop:
//!
//! 1. the model writes a script,
//! 2. the script calls a tool (`add`) exposed as a function,
//! 3. the result is fed back as an observation, and
//! 4. a second turn returns the final result.
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/codeact_agent/Cargo.toml
//! ```
//!
//! To use a real model and a real interpreter in production, swap `DemoLlm` for
//! an `adk-model` provider and `LineScriptRuntime` for a Monty-backed Python
//! runtime — the rest of the wiring is unchanged.

mod runtime;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::codeact::CodeActAgent;
use adk_core::{
    Agent, Content, Llm, LlmRequest, LlmResponseStream, Part, SessionId, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::runtime::LineScriptRuntime;

const APP_NAME: &str = "codeact-agent-example";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    println!("=== ADK-Rust CodeAct Agent example ===\n");

    let agent: Arc<dyn Agent> = Arc::new(
        CodeActAgent::builder()
            .name("calculator")
            .model(Arc::new(DemoLlm::new()))
            .runtime(Arc::new(LineScriptRuntime))
            .instruction("You add numbers by writing a line script.")
            .tool(Arc::new(AddTool))
            .build()?,
    );

    let session_id = "session-1";
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await?;
    let runner =
        Runner::builder().app_name(APP_NAME).agent(agent).session_service(sessions).build()?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text("What is 40 + 2?"),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            let text: String = content.parts.iter().filter_map(Part::text).collect();
            if !text.is_empty() {
                println!("[{}] {text}", event.author);
            }
        }
    }

    println!("\nDone.");
    Ok(())
}

/// A deterministic model that emits line scripts, so the example runs offline.
///
/// Turn 1 calls the `add` tool and observes the result; turn 2 returns the final
/// answer. A real deployment would use an `adk-model` provider instead.
struct DemoLlm {
    turn: AtomicUsize,
}

impl DemoLlm {
    fn new() -> Self {
        Self { turn: AtomicUsize::new(0) }
    }
}

#[async_trait]
impl Llm for DemoLlm {
    fn name(&self) -> &str {
        "demo-line-script"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let script = match self.turn.fetch_add(1, Ordering::SeqCst) {
            0 => "```\nCALL add {\"a\": 40, \"b\": 2}\nOBSERVE $last\n```",
            _ => "```\nFINAL {\"answer\": 42, \"explanation\": \"40 + 2 = 42\"}\n```",
        };
        let response =
            adk_core::model::LlmResponse::new(Content::new("model").with_text(script.to_string()));
        Ok(Box::pin(futures::stream::once(async move { Ok(response) })))
    }
}

/// A read-only, concurrency-safe tool that adds two numbers.
struct AddTool;

#[async_trait]
impl adk_core::Tool for AddTool {
    fn name(&self) -> &str {
        "add"
    }

    fn description(&self) -> &str {
        "adds two numbers `a` and `b`, returning {\"sum\": a + b}"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let a = args.get("a").and_then(Value::as_f64).unwrap_or(0.0);
        let b = args.get("b").and_then(Value::as_f64).unwrap_or(0.0);
        Ok(json!({ "sum": a + b }))
    }
}
