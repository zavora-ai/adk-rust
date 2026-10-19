//! Item 11: the console `Launcher` prints each tool call once.
//!
//! The launcher reads the user's lines from stdin, so it runs in a child process of this
//! binary, started with `--launcher <gemini | base URL>`, and its stdout is read back.

use std::process::Stdio;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Llm, Tool, ToolContext};
use adk_model::GeminiModel;
use adk_model::openai::OpenAIClient;
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::common::{LIVE_MODEL, Mode, Report, Verdict, skip, verdict};
use crate::upstream::{Reply, Upstream};

/// Runs this binary as a console launcher.
pub const LAUNCHER_FLAG: &str = "--launcher";

struct Weather;

#[async_trait]
impl Tool for Weather {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Returns the current weather for a city."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok(json!({"city": args["city"], "forecast": "sunny, 24 C"}))
    }
}

/// Runs `adk_runner::Launcher` over this process's stdin and stdout.
pub async fn serve(target: &str) -> anyhow::Result<()> {
    let model: Arc<dyn Llm> = if target == "gemini" {
        let key = std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))?;
        Arc::new(GeminiModel::new(key, LIVE_MODEL)?)
    } else {
        Arc::new(OpenAIClient::compatible("test-key", target, "scripted-model")?)
    };
    let agent = LlmAgentBuilder::new("weather")
        .instruction("Use get_weather to answer weather questions. Call it exactly once.")
        .model(model)
        .tool(Arc::new(Weather))
        .build()?;
    adk_runner::Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}

pub async fn run(gemini_key: Option<&str>, report: &mut Report) {
    report.record_result("launcher-tool-call-once-scripted", 11, Mode::Scripted, scripted().await);
    match gemini_key {
        Some(_) => report.record_result("launcher-tool-call-once-live", 11, Mode::Live, live().await),
        None => report.record("launcher-tool-call-once-live", 11, Mode::Live, skip("no Gemini key")),
    }
}

/// Sends one question to a launcher child and returns its stdout.
async fn ask(target: &str) -> anyhow::Result<String> {
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .args([LAUNCHER_FLAG, target])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;
    stdin.write_all(b"What is the weather in Paris?\nexit\n").await?;
    drop(stdin);
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output())
        .await??;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn check(stdout: &str) -> Verdict {
    let printed = stdout.matches("[tool] get_weather(").count();
    verdict(vec![(printed == 1, format!("tool call printed {printed} time(s); stdout {stdout:?}"))])
}

async fn scripted() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![
        Reply::ToolCall { name: "get_weather", args: json!({"city": "Paris"}) },
        Reply::Text("It is sunny in Paris."),
    ])
    .await?;
    Ok(check(&ask(&upstream.base).await?))
}

async fn live() -> anyhow::Result<Verdict> {
    Ok(check(&ask("gemini").await?))
}
