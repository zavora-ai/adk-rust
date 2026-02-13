//! MCP Task Support Example (SEP-1686)
//!
//! Demonstrates using MCP task support for long-running operations.
//! Tasks allow tools to be queued and polled rather than blocking.
//!
//! Prerequisites:
//! - Node.js and npm installed
//! - GOOGLE_API_KEY environment variable set
//!
//! Run:
//!   cd doc-test/tools/mcp_test
//!   GOOGLE_API_KEY=your_key cargo run --bin task_support

use adk_core::{Content, ReadonlyContext, Toolset};
use adk_rust::prelude::*;
use adk_tool::{McpTaskConfig, McpToolset};
use rmcp::{ServiceExt, transport::TokioChildProcess};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Minimal context for tool discovery
struct SimpleContext;

#[async_trait::async_trait]
impl ReadonlyContext for SimpleContext {
    fn invocation_id(&self) -> &str {
        "init"
    }
    fn agent_name(&self) -> &str {
        "init"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "mcp"
    }
    fn session_id(&self) -> &str {
        "init"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::new("user").with_text("init"))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("MCP Task Support Example (SEP-1686)");
    println!("===================================\n");

    // 1. Start MCP server
    println!("Starting MCP server...");
    let mut cmd = Command::new("npx");
    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");

    let client = ().serve(TokioChildProcess::new(cmd)?).await?;
    println!("MCP server connected!\n");

    // 2. Create toolset with task support enabled
    // This enables async task lifecycle for long-running operations
    let task_config = McpTaskConfig::enabled()
        .poll_interval(Duration::from_secs(1)) // Poll every second
        .timeout(Duration::from_secs(120)) // 2 minute timeout
        .max_attempts(60); // Max 60 poll attempts

    let toolset = McpToolset::new(client)
        .with_name("task-enabled-tools")
        .with_task_support(task_config)
        .with_filter(|name| {
            // Include the longRunningOperation tool which benefits from task support
            matches!(name, "echo" | "add" | "longRunningOperation" | "getTime")
        });

    // 3. Get cancellation token
    let cancel_token = toolset.cancellation_token().await;

    // 4. Discover tools
    let ctx = Arc::new(SimpleContext) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await?;

    println!("Discovered {} tools with task support:", tools.len());
    for tool in &tools {
        let long_running = if tool.is_long_running() { " [LONG-RUNNING]" } else { "" };
        println!("  - {}{}: {}", tool.name(), long_running, tool.description());
    }
    println!();

    println!("Task Configuration:");
    println!("  - Poll interval: 1 second");
    println!("  - Timeout: 120 seconds");
    println!("  - Max attempts: 60");
    println!();

    // 5. Build agent
    let mut builder = LlmAgentBuilder::new("mcp_task_support").model(model).instruction(
        "You have access to MCP tools with task support enabled.\n\n\
             Available tools:\n\
             - echo: Repeats a message\n\
             - add: Adds two numbers\n\
             - longRunningOperation: Simulates a long operation (uses task polling)\n\
             - getTime: Gets the current time\n\n\
             When using longRunningOperation, the system will automatically \
             poll for completion rather than blocking.",
    );

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    // 6. Run console
    println!("Try asking the agent to run a long operation!");
    println!("Example: 'Run a long operation for 5 seconds'\n");

    let result = adk_cli::console::run_console(
        Arc::new(agent),
        "mcp_task_support".to_string(),
        "user".to_string(),
    )
    .await;

    // 7. Cleanup
    println!("\nShutting down MCP server...");
    cancel_token.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;

    result?;
    Ok(())
}
