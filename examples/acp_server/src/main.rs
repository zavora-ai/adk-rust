//! # ACP Server Example
//!
//! Demonstrates exposing an ADK-Rust agent as an ACP-compatible server that
//! IDEs (Kiro, VS Code, etc.) can connect to via the Agent Client Protocol.
//!
//! ## Protocol Overview
//!
//! The ACP protocol uses newline-delimited JSON over stdio:
//!
//! ```text
//! Client (IDE) ──stdin──► ACP Server ──► ADK Agent (Gemini)
//!              ◄─stdout──             ◄──
//! ```
//!
//! ## Message Flow
//!
//! 1. Client sends `initialize` → Server responds with capabilities
//! 2. Client sends `session/new` → Server creates a session
//! 3. Client sends `session/prompt` → Server runs the agent, returns notifications
//! 4. Client sends `session/close` → Server cleans up the session
//!
//! ## Run
//!
//! ```bash
//! cd examples/acp_server
//! cp .env.example .env   # add your GOOGLE_API_KEY
//! cargo run
//! ```
//!
//! Configure an ACP-compatible editor or SDK client to start the binary. See
//! the README for tested client configuration and protocol verification.

use std::sync::Arc;

use adk_acp::server::{AcpServer, AcpServerConfigBuilder, TransportConfig};
use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm, Tool, ToolContext, async_trait};
use adk_model::GeminiModel;
use adk_session::{InMemorySessionService, SessionService};
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

// ═══════════════════════════════════════════════════════════════════════════════
// Tools — Simple file system tools for the coding assistant
// ═══════════════════════════════════════════════════════════════════════════════

/// A tool that reads file contents from the local filesystem.
///
struct ReadFileTool {
    workspace: Arc<std::path::PathBuf>,
}

async fn resolve_workspace_path(
    workspace: &std::path::Path,
    requested: &str,
) -> adk_core::Result<std::path::PathBuf> {
    let requested = std::path::Path::new(requested);
    let candidate =
        if requested.is_absolute() { requested.to_path_buf() } else { workspace.join(requested) };
    let resolved = tokio::fs::canonicalize(candidate).await.map_err(|error| {
        adk_core::AdkError::tool(format!("cannot resolve requested path: {error}"))
    })?;
    if !resolved.starts_with(workspace) {
        return Err(adk_core::AdkError::tool("requested path is outside the server workspace"));
    }
    Ok(resolved)
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file at the given path. Returns the file content as text."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to read (relative to workspace root)"
                }
            },
            "required": ["path"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("unknown");
        tracing::info!(path = %path, "read_file tool called");
        let resolved = resolve_workspace_path(&self.workspace, path).await?;

        match tokio::fs::read_to_string(&resolved).await {
            Ok(content) => Ok(json!({
                "path": resolved,
                "content": content,
                "size_bytes": content.len()
            })),
            Err(e) => Ok(json!({
                "path": path,
                "error": format!("Could not read file: {e}"),
                "hint": "File may not exist or is not accessible"
            })),
        }
    }
}

/// A tool that lists directory contents.
struct ListDirectoryTool {
    workspace: Arc<std::path::PathBuf>,
}

#[async_trait]
impl Tool for ListDirectoryTool {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List the files and subdirectories in a given directory path."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory path to list (relative to workspace root)"
                }
            },
            "required": ["path"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        tracing::info!(path = %path, "list_directory tool called");
        let resolved = resolve_workspace_path(&self.workspace, path).await?;

        match tokio::fs::read_dir(&resolved).await {
            Ok(mut entries) => {
                let mut items = Vec::new();
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let file_type = entry.file_type().await.ok();
                    let is_dir = file_type.map(|ft| ft.is_dir()).unwrap_or(false);
                    items.push(json!({
                        "name": entry.file_name().to_string_lossy(),
                        "type": if is_dir { "directory" } else { "file" }
                    }));
                }
                Ok(json!({
                    "path": resolved,
                    "entries": items,
                    "count": items.len()
                }))
            }
            Err(e) => Ok(json!({
                "path": path,
                "error": format!("Could not list directory: {e}"),
                "hint": "Directory may not exist or is not accessible"
            })),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Main — Configure and start the ACP Server
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // ── Step 1: Initialize tracing ───────────────────────────────────────────
    // Tracing goes to stderr so it doesn't interfere with the JSON protocol on stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,adk_acp=debug")),
        )
        .with_writer(std::io::stderr)
        .init();

    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set — see .env.example");

    // ── Step 2: Create the LLM model ────────────────────────────────────────
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    tracing::info!(model = %model.name(), "model initialized");

    // ── Step 3: Create tools ─────────────────────────────────────────────────
    let workspace = Arc::new(std::env::current_dir()?.canonicalize()?);
    let read_file_tool: Arc<dyn Tool> = Arc::new(ReadFileTool { workspace: workspace.clone() });
    let list_dir_tool: Arc<dyn Tool> = Arc::new(ListDirectoryTool { workspace });

    // ── Step 4: Build the agent ──────────────────────────────────────────────
    // This is the agent that will handle prompts from the IDE.
    let agent = LlmAgentBuilder::new("coding-assistant")
        .description("A coding assistant that can read files and list directories")
        .model(model)
        .instruction(
            "You are a helpful coding assistant connected to an IDE via the Agent Client Protocol (ACP). \
             You can read files and list directories in the user's workspace. \
             When asked about code, use the available tools to inspect the relevant files. \
             Provide concise, actionable answers."
        )
        .tool(read_file_tool)
        .tool(list_dir_tool)
        .build()?;

    let agent: Arc<dyn Agent> = Arc::new(agent);

    // ── Step 5: Create the session service ───────────────────────────────────
    // InMemorySessionService is fine for local IDE connections.
    let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());

    // ── Step 6: Configure the ACP Server ─────────────────────────────────────
    // The server exposes the agent via the ACP protocol over stdio.
    // IDEs connect by spawning this process and communicating via stdin/stdout.
    let config = AcpServerConfigBuilder::new()
        .agent(agent)
        .session_service(session_service)
        .agent_name("coding-assistant")
        .agent_description(
            "ADK-Rust coding assistant with file reading and directory listing tools",
        )
        .transport(TransportConfig::Stdio)
        .build()?;

    // ── Step 7: Print instructions to stderr ─────────────────────────────────
    // These go to stderr so they don't interfere with the protocol on stdout.
    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║  ACP Server — ADK-Rust Coding Assistant                     ║");
    eprintln!("╚══════════════════════════════════════════════════════════════╝");
    eprintln!();
    eprintln!("  Agent: coding-assistant");
    eprintln!("  Tools: read_file, list_directory");
    eprintln!("  Transport: stdio (newline-delimited JSON)");
    eprintln!();
    eprintln!("  The server is now listening on stdin for ACP messages.");
    eprintln!("  Send JSON messages (one per line) to interact.");
    eprintln!();
    eprintln!("  Protocol flow:");
    eprintln!("    1. initialize (protocolVersion: 1)");
    eprintln!("    2. session/new (absolute cwd, MCP server list)");
    eprintln!("    3. session/prompt (typed content blocks)");
    eprintln!("       ← live session/update notifications");
    eprintln!("    4. session/cancel or session/close");
    eprintln!();
    eprintln!("  Press Ctrl+C or close stdin (Ctrl+D) to stop.");
    eprintln!("──────────────────────────────────────────────────────────────────");

    // ── Step 8: Start the server and wait ────────────────────────────────────
    // AcpServer::run() spawns a background task that reads from stdin and
    // writes responses to stdout. It returns a handle for lifecycle control.
    let handle = AcpServer::run(config).await?;

    tracing::info!("ACP server running — waiting for messages on stdin");

    // Wait for the server to stop (stdin closes or Ctrl+C)
    handle.wait().await?;

    tracing::info!("ACP server shut down cleanly");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_a_file_inside_the_server_workspace() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap();

        let resolved = resolve_workspace_path(&workspace, "Cargo.toml").await.unwrap();

        assert_eq!(resolved, workspace.join("Cargo.toml"));
    }

    #[tokio::test]
    async fn rejects_a_file_outside_the_server_workspace() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap();

        assert!(resolve_workspace_path(&workspace, "/etc/hosts").await.is_err());
    }
}
