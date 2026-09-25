//! Server mode: an OpenAI-backed ADK agent served over ACP stdio.
//!
//! The client spawns this mode as a child process and passes its configuration through the
//! environment, so every value read here proves `AcpAgentConfig::env` reached the child.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_acp::server::{AcpServer, AcpServerConfigBuilder, TransportConfig};
use adk_agent::LlmAgentBuilder;
use adk_core::{AdkError, Agent, Result, Tool, ToolContext};
use adk_model::{OpenAIClient, OpenAIConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    ENV_PROBE_VAR, FINDINGS_FILE, PROBE_FILE, READ_TOOL, RECORD_TOOL, WORKSPACE_VAR, model_id,
};

/// Reads a file that sits directly inside the workspace.
struct ReadWorkspaceFile {
    workspace: PathBuf,
}

#[async_trait]
impl Tool for ReadWorkspaceFile {
    fn name(&self) -> &str {
        READ_TOOL
    }

    fn description(&self) -> &str {
        "Read a file from the workspace. Pass a bare file `name` such as `secret.txt`."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"],
            "additionalProperties": false
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        let path = confined_path(&self.workspace, name)?;
        let contents = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| AdkError::tool(format!("failed to read '{name}': {error}")))?;
        Ok(json!({ "name": name, "contents": contents }))
    }
}

/// Appends a finding to the workspace log; gated by tool confirmation.
struct RecordFinding {
    workspace: PathBuf,
}

#[async_trait]
impl Tool for RecordFinding {
    fn name(&self) -> &str {
        RECORD_TOOL
    }

    fn description(&self) -> &str {
        "Record a finding in the workspace findings log. Pass the finding as `text`."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
            "additionalProperties": false
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let text = args.get("text").and_then(Value::as_str).unwrap_or_default().trim();
        if text.is_empty() {
            return Err(AdkError::tool("`text` must not be empty"));
        }
        let line = text.replace('\n', " ");
        let path = self.workspace.join(FINDINGS_FILE);
        tokio::task::spawn_blocking(move || {
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(file, "{line}")
        })
        .await
        .map_err(|error| AdkError::tool(format!("findings writer panicked: {error}")))?
        .map_err(|error| AdkError::tool(format!("failed to record finding: {error}")))?;
        Ok(json!({ "recorded": true }))
    }
}

/// Rejects anything but a bare file name so the model cannot leave the workspace.
fn confined_path(workspace: &Path, name: &str) -> Result<PathBuf> {
    let is_bare = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\'])
        && Path::new(name).components().count() == 1;
    if !is_bare {
        return Err(AdkError::tool(format!(
            "'{name}' is not a bare file name; pass a name such as `secret.txt`"
        )));
    }
    Ok(workspace.join(name))
}

pub async fn run() -> anyhow::Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY was not passed to the ACP agent process"))?;
    let workspace = PathBuf::from(
        std::env::var(WORKSPACE_VAR)
            .map_err(|_| anyhow::anyhow!("{WORKSPACE_VAR} was not passed to the ACP agent"))?,
    );
    let probe = std::env::var(ENV_PROBE_VAR).unwrap_or_default();
    std::fs::write(workspace.join(PROBE_FILE), probe)?;

    let model_id = model_id();
    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, &model_id))?);
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("workspace_agent")
            .description("Reads workspace files and records findings")
            .model(model)
            .instruction(format!(
                "You operate on a local workspace. Use {READ_TOOL} to read files and \
                 {RECORD_TOOL} to record findings. Quote file contents exactly."
            ))
            .tool(Arc::new(ReadWorkspaceFile { workspace: workspace.clone() }))
            .tool(Arc::new(RecordFinding { workspace }))
            .require_tool_confirmation(RECORD_TOOL)
            .build()?,
    );

    let config = AcpServerConfigBuilder::new()
        .agent(agent)
        .session_service(Arc::new(InMemorySessionService::new()))
        .agent_name("acp-openai-e2e-workspace-agent")
        .agent_description("OpenAI-backed ADK agent for the ACP end-to-end example")
        .transport(TransportConfig::Stdio)
        .build()?;

    tracing::info!(model = %model_id, "acp agent serving on stdio");
    AcpServer::run(config).await?.wait().await?;
    Ok(())
}
