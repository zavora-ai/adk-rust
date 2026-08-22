//! OpenAI-backed tool guardrail example.
//!
//! The model is asked to attempt a deployment outside the allowed workspace, observe the policy
//! denial, and retry inside it. The accepted retry is revised to `dry_run: true` before an
//! auto-approval handler sees it. The executable assertions prove that denied calls do not prompt,
//! confirmations see revised arguments, and the tool receives only confined paths.
//!
//! ```bash
//! OPENAI_API_KEY=... cargo run --manifest-path examples/tool_guardrails_openai/Cargo.toml
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{PathAllowList, ToolGuardrail, ToolGuardrailResult, ToolGuardrailSet};
use adk_core::{
    Agent, Content, Part, Result, RunConfig, Tool, ToolConfirmationDecision,
    ToolConfirmationHandler, ToolConfirmationRequest, ToolContext,
};
use adk_model::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

#[derive(Debug)]
struct ForceDryRun;

#[async_trait]
impl ToolGuardrail for ForceDryRun {
    fn name(&self) -> &str {
        "force-dry-run"
    }

    fn applies_to(&self, tool_name: &str) -> bool {
        tool_name == "deploy_config"
    }

    async fn validate_call(&self, _tool_name: &str, args: &Value) -> ToolGuardrailResult {
        let mut revised = args.clone();
        if let Some(object) = revised.as_object_mut() {
            object.insert("dry_run".to_string(), json!(true));
        }
        ToolGuardrailResult::revise(revised, "live demo permits dry-run deployments only")
    }
}

#[derive(Debug)]
struct RevisedArgsApprover {
    decisions: AtomicUsize,
}

#[async_trait]
impl ToolConfirmationHandler for RevisedArgsApprover {
    async fn decide(&self, request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        if request.args.get("dry_run") != Some(&json!(true)) {
            return Err(adk_core::AdkError::agent(
                "confirmation received arguments before the dry-run guardrail revision",
            ));
        }
        self.decisions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolConfirmationDecision::Approve)
    }
}

#[derive(Debug)]
struct RecordingDeployTool {
    calls: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for RecordingDeployTool {
    fn name(&self) -> &str {
        "deploy_config"
    }

    fn description(&self) -> &str {
        "Deploy a configuration file. Pass absolute `path` and boolean `dry_run`."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "dry_run": { "type": "boolean" }
            },
            "required": ["path", "dry_run"],
            "additionalProperties": false
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.calls.lock().unwrap_or_else(|error| error.into_inner()).push(args.clone());
        Ok(json!({ "accepted": true, "effective_args": args }))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("set OPENAI_API_KEY to run this example"))?;
    let model_id =
        std::env::var("TOOL_GUARDRAIL_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string());
    let allowed = tempfile::tempdir()?;
    let allowed_path = allowed.path().join("config.json");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let approver = Arc::new(RevisedArgsApprover { decisions: AtomicUsize::new(0) });

    let tool: Arc<dyn Tool> = Arc::new(RecordingDeployTool { calls: Arc::clone(&calls) });
    let guardrails = ToolGuardrailSet::new()
        .with(
            PathAllowList::new("workspace-only", ["path"], [allowed.path()])
                .on_tools(["deploy_config"]),
        )
        .with(ForceDryRun);
    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, &model_id))?);
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("guarded-deployer")
            .model(model)
            .instruction(
                "Always use deploy_config for deployment requests. If its policy rejects a path, \
                 retry once using the workspace path supplied by the user. Report the final result.",
            )
            .tool(tool)
            .tool_guardrails(guardrails)
            .require_tool_confirmation("deploy_config")
            .build()?,
    );

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "tool-guardrails-live".into(),
            user_id: "user".into(),
            session_id: Some("live".into()),
            state: HashMap::new(),
        })
        .await?;
    let run_config = RunConfig::builder()
        .tool_confirmation_handler(Arc::clone(&approver) as Arc<dyn ToolConfirmationHandler>)
        .build();
    let runner = Runner::builder()
        .app_name("tool-guardrails-live")
        .agent(agent)
        .session_service(sessions)
        .run_config(run_config)
        .build()?;

    println!("model: {model_id}");
    println!("allowed workspace: {}", allowed.path().display());
    let prompt = format!(
        "First call deploy_config with path /etc/adk/config.json and dry_run false. That should be \
         rejected by policy. Then retry with path {} and dry_run false.",
        allowed_path.display()
    );
    let mut events = runner.run_str("user", "live", Content::new("user").with_text(prompt)).await?;
    let mut denials = 0usize;
    let mut final_text = String::new();
    while let Some(event) = events.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                match part {
                    Part::FunctionResponse { function_response, .. }
                        if function_response.response.to_string().contains("workspace-only") =>
                    {
                        denials += 1;
                        println!("policy denial observed");
                    }
                    Part::Text { text } => final_text.push_str(text),
                    _ => {}
                }
            }
        }
    }

    let recorded = calls.lock().unwrap_or_else(|error| error.into_inner()).clone();
    anyhow::ensure!(denials >= 1, "the live model did not exercise the denial path");
    anyhow::ensure!(!recorded.is_empty(), "the live model never completed an allowed retry");
    anyhow::ensure!(
        approver.decisions.load(Ordering::SeqCst) == recorded.len(),
        "a denied call reached confirmation or an approved call did not execute"
    );
    for args in &recorded {
        anyhow::ensure!(
            args.get("dry_run") == Some(&json!(true)),
            "tool saw unrevised args: {args}"
        );
        let path = args.get("path").and_then(Value::as_str).unwrap_or_default();
        anyhow::ensure!(
            path.starts_with(&allowed.path().display().to_string()),
            "unsafe path: {path}"
        );
    }

    println!("confirmed executions: {}", recorded.len());
    println!("final response: {}", final_text.trim());
    Ok(())
}
