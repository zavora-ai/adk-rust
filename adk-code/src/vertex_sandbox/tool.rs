//! [`VertexSandboxTool`] — sandbox code execution as an [`adk_core::Tool`].

use super::errors;
use super::executor::SandboxCodeExecutor;
use super::types::InputFile;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::debug;

/// MIME type assumed for input files that do not declare one.
const DEFAULT_FILE_MIME_TYPE: &str = "application/octet-stream";

/// A tool that executes model-written code in a managed Vertex AI Agent
/// Engine sandbox.
///
/// Arguments: `{"code": string, "files": [{"name", "mimeType", "dataBase64"}]}`
/// (files optional). The sandbox is resolved per session through a
/// [`SandboxCodeExecutor`], keyed by the calling context's app, user, and
/// session IDs. Returns `{"stdout", "stderr", "outputFiles": [{"name",
/// "mimeType", "dataBase64"}]}`.
///
/// # Example
///
/// ```rust,no_run
/// use adk_code::vertex_sandbox::{
///     SandboxCodeExecutor, VertexSandboxClient, VertexSandboxConfig, VertexSandboxTool,
/// };
/// use std::sync::Arc;
///
/// # fn build() -> adk_core::Result<VertexSandboxTool> {
/// let client = Arc::new(VertexSandboxClient::new_with_adc(VertexSandboxConfig::new(
///     "my-project",
///     "us-central1",
/// ))?);
/// let executor = Arc::new(SandboxCodeExecutor::for_engine(client, "4242"));
/// Ok(VertexSandboxTool::new(executor))
/// # }
/// ```
pub struct VertexSandboxTool {
    executor: Arc<SandboxCodeExecutor>,
}

impl VertexSandboxTool {
    /// Creates the tool over a shared executor.
    pub fn new(executor: Arc<SandboxCodeExecutor>) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl Tool for VertexSandboxTool {
    fn name(&self) -> &str {
        "vertex_sandbox_code_execution"
    }

    fn description(&self) -> &str {
        "Executes code in a managed Vertex AI Agent Engine sandbox. Pass the source in 'code' \
         and optional input files in 'files' (name, mimeType, dataBase64). Returns stdout, \
         stderr, and any files the code wrote. State persists across calls in the same session."
    }

    fn is_long_running(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Source code to execute in the sandbox.",
                },
                "files": {
                    "type": "array",
                    "description": "Input files made available to the code.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "Filename the code sees." },
                            "mimeType": { "type": "string", "description": "MIME type of the file." },
                            "dataBase64": { "type": "string", "description": "Base64-encoded file bytes." },
                        },
                        "required": ["name", "dataBase64"],
                    },
                },
            },
            "required": ["code"],
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let errors = errors();
        let code = args.get("code").and_then(Value::as_str).ok_or_else(|| {
            errors.invalid_input(
                "vertex sandbox tool requires a string 'code' argument with the source to execute",
            )
        })?;

        let mut files = Vec::new();
        if let Some(entries) = args.get("files").filter(|value| !value.is_null()) {
            let entries = entries.as_array().ok_or_else(|| {
                errors.invalid_input(
                    "vertex sandbox tool 'files' must be an array of {name, mimeType, dataBase64} objects",
                )
            })?;
            for entry in entries {
                let name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| {
                        errors.invalid_input(
                            "every vertex sandbox input file needs a non-empty 'name'",
                        )
                    })?;
                let mime_type =
                    entry.get("mimeType").and_then(Value::as_str).unwrap_or(DEFAULT_FILE_MIME_TYPE);
                let data = entry.get("dataBase64").and_then(Value::as_str).ok_or_else(|| {
                    errors.invalid_input(format!(
                        "vertex sandbox input file '{name}' needs a base64 string 'dataBase64'",
                    ))
                })?;
                let bytes = BASE64.decode(data).map_err(|error| {
                    errors.invalid_input(format!(
                        "vertex sandbox input file '{name}' dataBase64 is not valid base64: {error}",
                    ))
                })?;
                files.push(InputFile::new(name, mime_type, bytes));
            }
        }

        let session_key = format!("{}/{}/{}", ctx.app_name(), ctx.user_id(), ctx.session_id());
        debug!(session.key = session_key.as_str(), "executing code in vertex sandbox");
        let result = self.executor.execute_for_session(&session_key, code, &files).await?;

        let output_files: Vec<Value> = result
            .output_files
            .iter()
            .map(|file| {
                json!({
                    "name": file.name,
                    "mimeType": file.mime_type,
                    "dataBase64": BASE64.encode(&file.data),
                })
            })
            .collect();
        Ok(json!({
            "stdout": result.stdout,
            "stderr": result.stderr,
            "outputFiles": output_files,
        }))
    }
}
