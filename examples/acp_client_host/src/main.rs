//! ACP client/host example.
//!
//! This application starts an external ACP coding agent, provides a bounded
//! read-only view of the current workspace, applies an explicit permission
//! policy, and renders the agent's updates as they arrive.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_acp::agent_client_protocol::Error as ProtocolError;
use adk_acp::agent_client_protocol::schema::v1::{
    ReadTextFileRequest, ReadTextFileResponse, WriteTextFileRequest, WriteTextFileResponse,
};
use adk_acp::agent_client_protocol::util::internal_error;
use adk_acp::{
    AcpAgentConfig, AcpFileSystem, OutputChunk, PermissionDecision, PermissionPolicy,
    StatusTracker, stream_prompt,
};
use async_trait::async_trait;

#[derive(Debug)]
struct ReadOnlyWorkspace {
    root: PathBuf,
}

impl ReadOnlyWorkspace {
    fn new(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        Ok(Self { root: root.as_ref().canonicalize()? })
    }

    fn resolve_existing(&self, requested: &Path) -> Result<PathBuf, ProtocolError> {
        let resolved = requested
            .canonicalize()
            .map_err(|error| internal_error(format!("cannot resolve requested file: {error}")))?;
        if !resolved.starts_with(&self.root) {
            return Err(internal_error("requested file is outside the approved workspace"));
        }
        Ok(resolved)
    }

    async fn read(&self, request: &ReadTextFileRequest) -> Result<String, ProtocolError> {
        let path = self.resolve_existing(&request.path)?;
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|error| internal_error(format!("cannot read requested file: {error}")))?;

        let start = request.line.unwrap_or(1).saturating_sub(1) as usize;
        let limit = request.limit.map_or(usize::MAX, |value| value as usize);
        Ok(content.lines().skip(start).take(limit).collect::<Vec<_>>().join("\n"))
    }
}

#[async_trait]
impl AcpFileSystem for ReadOnlyWorkspace {
    fn supports_read(&self) -> bool {
        true
    }

    fn supports_write(&self) -> bool {
        false
    }

    async fn read_text_file(
        &self,
        request: ReadTextFileRequest,
    ) -> Result<ReadTextFileResponse, ProtocolError> {
        Ok(ReadTextFileResponse::new(self.read(&request).await?))
    }

    async fn write_text_file(
        &self,
        _request: WriteTextFileRequest,
    ) -> Result<WriteTextFileResponse, ProtocolError> {
        Err(ProtocolError::method_not_found())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let command = std::env::var("ACP_AGENT_COMMAND").map_err(|_| {
        anyhow::anyhow!(
            "ACP_AGENT_COMMAND is required; copy .env.example and set it to your agent's ACP command"
        )
    })?;
    let prompt = std::env::var("ACP_PROMPT").unwrap_or_else(|_| {
        "Read README.md and explain what this project provides in five bullets.".into()
    });
    let workspace = std::env::current_dir()?.canonicalize()?;

    let policy = PermissionPolicy::async_custom(|request| async move {
        let operation = request.title.to_ascii_lowercase();
        if ["read", "list", "search", "inspect"].iter().any(|word| operation.contains(word)) {
            PermissionDecision::allow_once()
        } else {
            PermissionDecision::deny()
        }
    });

    let config = AcpAgentConfig::new(command)
        .working_dir(&workspace)
        .filesystem(Arc::new(ReadOnlyWorkspace::new(&workspace)?));
    let status = StatusTracker::new();
    let mut output = stream_prompt(&config, &prompt, Arc::new(policy), status.clone()).await?;

    println!("ACP agent status: {}", status.get());
    while let Some(chunk) = output.recv().await {
        match chunk {
            OutputChunk::Text(text) => print!("{text}"),
            OutputChunk::Thought(_) => {}
            OutputChunk::ToolCall { title } => println!("\n[tool started] {title}"),
            OutputChunk::ToolCallComplete { title } => println!("\n[tool completed] {title}"),
            OutputChunk::PermissionRequested { title, approved } => {
                println!(
                    "\n[permission] {title}: {}",
                    if approved { "allowed once" } else { "denied" }
                );
            }
            OutputChunk::Done => {
                println!("\n\nACP turn completed.");
                break;
            }
            OutputChunk::Error(error) => return Err(anyhow::anyhow!(error)),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workspace_reads_a_requested_line_range() {
        let workspace = ReadOnlyWorkspace::new(env!("CARGO_MANIFEST_DIR")).unwrap();
        let request = ReadTextFileRequest::new(
            "session-test",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        )
        .line(1)
        .limit(2);

        let content = workspace.read(&request).await.unwrap();

        assert_eq!(content, "[package]\nname = \"acp-client-host-example\"");
    }

    #[tokio::test]
    async fn workspace_rejects_a_file_outside_the_approved_root() {
        let workspace = ReadOnlyWorkspace::new(env!("CARGO_MANIFEST_DIR")).unwrap();
        let request = ReadTextFileRequest::new("session-test", "/etc/hosts");

        assert!(workspace.read(&request).await.is_err());
    }
}
