//! Live end-to-end check of `adk-acp` against OpenAI.
//!
//! One binary plays both sides. Client mode spawns the same executable with `--serve-acp`,
//! which serves an OpenAI-backed ADK agent over ACP stdio. The client then drives it twice:
//!
//! 1. A one-shot `prompt_agent_with_policy` call reads a random marker from the workspace.
//! 2. An OpenAI-backed coordinator delegates through a persistent `AcpSession`, and the child
//!    records the marker with a confirmation-gated tool, which crosses the ACP permission bridge.
//!
//! Every outcome is asserted, so the process exits non-zero when any step fails.
//!
//! ```bash
//! OPENAI_API_KEY=... cargo run --manifest-path examples/acp_openai_e2e/Cargo.toml
//! ```

mod server;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_acp::{
    AcpAgentConfig, AcpSession, PermissionDecision, PermissionPolicy, prompt_agent_with_policy,
};
use adk_agent::LlmAgentBuilder;
use adk_core::{AdkError, Agent, Content, Part, Result, Tool, ToolContext};
use adk_model::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::time::timeout;
use tracing_subscriber::EnvFilter;

const SERVE_FLAG: &str = "--serve-acp";
const DEFAULT_MODEL: &str = "gpt-6-luna";
const WORKSPACE_VAR: &str = "ACP_E2E_WORKSPACE";
const ENV_PROBE_VAR: &str = "ACP_E2E_ENV_PROBE";
const SECRET_FILE: &str = "secret.txt";
const PROBE_FILE: &str = ".probe";
const FINDINGS_FILE: &str = "findings.log";
const READ_TOOL: &str = "read_workspace_file";
const RECORD_TOOL: &str = "record_finding";
const DELEGATE_TOOL: &str = "delegate_to_workspace_agent";
const PHASE_TIMEOUT: Duration = Duration::from_secs(180);

fn model_id() -> String {
    std::env::var("OPENAI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

/// Coordinator tool that forwards a task to the ACP agent over one persistent session.
struct DelegateToWorkspaceAgent {
    session: Arc<tokio::sync::Mutex<AcpSession>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for DelegateToWorkspaceAgent {
    fn name(&self) -> &str {
        DELEGATE_TOOL
    }

    fn description(&self) -> &str {
        "Send a task to the workspace agent, which can read workspace files and record \
         findings. Returns the workspace agent's reply."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "task": { "type": "string" } },
            "required": ["task"],
            "additionalProperties": false
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let task = args.get("task").and_then(Value::as_str).unwrap_or_default();
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut session = self.session.lock().await;
        let reply = timeout(PHASE_TIMEOUT, session.prompt(task))
            .await
            .map_err(|_| AdkError::tool("the ACP agent did not answer within the timeout"))?
            .map_err(|error| AdkError::tool(format!("ACP prompt failed: {error}")))?;
        Ok(json!({ "response": reply.text }))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // stderr only: in server mode stdout carries the ACP protocol.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    if std::env::args().any(|arg| arg == SERVE_FLAG) {
        return server::run().await;
    }

    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
        anyhow::anyhow!("set OPENAI_API_KEY (or add it to .env) to run this example")
    })?;
    let model_id = model_id();
    let workspace = tempfile::tempdir()?;
    let marker = format!("ACP-E2E-{}", uuid::Uuid::new_v4());
    let probe = uuid::Uuid::new_v4().to_string();
    std::fs::write(workspace.path().join(SECRET_FILE), &marker)?;

    let exe = std::env::current_exe()?;
    let command = format!("{} {SERVE_FLAG}", shell_words::quote(&exe.to_string_lossy()));
    let config = AcpAgentConfig::new(command)
        .working_dir(workspace.path())
        .env("OPENAI_API_KEY", &api_key)
        .env("OPENAI_MODEL", &model_id)
        .env(WORKSPACE_VAR, workspace.path().to_string_lossy())
        .env(ENV_PROBE_VAR, &probe);

    let permission_titles = Arc::new(Mutex::new(Vec::<String>::new()));
    let approvals = Arc::new(AtomicUsize::new(0));
    let policy = {
        let titles = Arc::clone(&permission_titles);
        let approvals = Arc::clone(&approvals);
        Arc::new(PermissionPolicy::Custom(Box::new(move |request| {
            titles.lock().unwrap_or_else(|error| error.into_inner()).push(request.title.clone());
            if request.title.contains(RECORD_TOOL) {
                approvals.fetch_add(1, Ordering::SeqCst);
                PermissionDecision::allow_once()
            } else {
                PermissionDecision::deny()
            }
        })))
    };

    println!("model: {model_id}");
    println!("workspace: {}", workspace.path().display());

    // Phase 1: one-shot prompt, a fresh agent process for this call.
    let one_shot = timeout(
        PHASE_TIMEOUT,
        prompt_agent_with_policy(
            &config,
            &format!(
                "Use {READ_TOOL} to read {SECRET_FILE} and reply with its exact contents only."
            ),
            Arc::clone(&policy),
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("phase 1 timed out after {PHASE_TIMEOUT:?}"))??;
    println!("phase 1 reply: {}", one_shot.trim());
    let probe_seen = std::fs::read_to_string(workspace.path().join(PROBE_FILE)).unwrap_or_default();

    // Phase 2: a coordinator agent delegates over one persistent session.
    let session = Arc::new(tokio::sync::Mutex::new(
        timeout(PHASE_TIMEOUT, AcpSession::start(config.clone(), Arc::clone(&policy)))
            .await
            .map_err(|_| anyhow::anyhow!("starting the ACP session timed out"))??,
    ));
    let delegate_calls = Arc::new(AtomicUsize::new(0));
    let coordinator: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("coordinator")
            .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(&api_key, &model_id))?))
            .instruction(format!(
                "You cannot read files yourself. Use {DELEGATE_TOOL} for every workspace task, \
                 then report the result to the user."
            ))
            .tool(Arc::new(DelegateToWorkspaceAgent {
                session: Arc::clone(&session),
                calls: Arc::clone(&delegate_calls),
            }))
            .build()?,
    );

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "acp-openai-e2e".into(),
            user_id: "user".into(),
            session_id: Some("live".into()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("acp-openai-e2e")
        .agent(coordinator)
        .session_service(sessions)
        .build()?;

    let request = format!(
        "Have the workspace agent read {SECRET_FILE} and record its exact contents as a finding \
         with {RECORD_TOOL}. Then tell me the exact contents."
    );
    let mut final_text = String::new();
    let phase_two = async {
        let mut events =
            runner.run_str("user", "live", Content::new("user").with_text(request)).await?;
        while let Some(event) = events.next().await {
            let event = event?;
            if event.author != "coordinator" {
                continue;
            }
            if let Some(content) = event.content() {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        final_text.push_str(text);
                    }
                }
            }
        }
        anyhow::Ok(())
    };
    timeout(PHASE_TIMEOUT * 2, phase_two)
        .await
        .map_err(|_| anyhow::anyhow!("phase 2 timed out"))??;
    println!("phase 2 reply: {}", final_text.trim());

    let prompt_count = {
        let mut session = session.lock().await;
        let count = session.prompt_count();
        session.close().await?;
        count
    };
    let findings =
        std::fs::read_to_string(workspace.path().join(FINDINGS_FILE)).unwrap_or_default();
    let finding_lines = findings.lines().filter(|line| !line.trim().is_empty()).count();
    let titles = permission_titles.lock().unwrap_or_else(|error| error.into_inner()).clone();
    let approved = approvals.load(Ordering::SeqCst);

    println!("permission requests: {titles:?}");
    println!("findings recorded: {finding_lines}");
    println!("persistent-session prompts: {prompt_count}");

    anyhow::ensure!(probe_seen == probe, "AcpAgentConfig::env did not reach the ACP agent process");
    anyhow::ensure!(one_shot.contains(&marker), "phase 1 reply is missing the workspace marker");
    anyhow::ensure!(delegate_calls.load(Ordering::SeqCst) >= 1, "the coordinator never delegated");
    anyhow::ensure!(prompt_count >= 1, "the persistent ACP session carried no prompt");
    anyhow::ensure!(!titles.is_empty(), "the gated tool never produced an ACP permission request");
    anyhow::ensure!(findings.contains(&marker), "no finding containing the marker was recorded");
    anyhow::ensure!(
        approved == finding_lines,
        "{approved} approvals but {finding_lines} findings: an unapproved call ran or an approved \
         call did not"
    );
    anyhow::ensure!(final_text.contains(&marker), "coordinator reply is missing the marker");

    println!("all checks passed");
    Ok(())
}
