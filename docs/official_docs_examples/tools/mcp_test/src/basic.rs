//! MCP Tools Basic Example
//!
//! Demonstrates connecting to an MCP server and using its tools with an agent.
//!
//! Prerequisites:
//! - Node.js and npm installed
//! - GOOGLE_API_KEY environment variable set
//!
//! Run:
//!   cd doc-test/tools/mcp_test
//!   GOOGLE_API_KEY=your_key cargo run --bin basic

use adk_core::{Content, ReadonlyContext, Toolset};
use adk_rust::prelude::*;
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use std::sync::Arc;
use tokio::process::Command;

/// Minimal context for tool discovery
struct SimpleContext;

#[async_trait::async_trait]
impl ReadonlyContext for SimpleContext {
    fn invocation_id(&self) -> &str { "init" }
    fn agent_name(&self) -> &str { "init" }
    fn user_id(&self) -> &str { "user" }
    fn app_name(&self) -> &str { "mcp" }
    fn session_id(&self) -> &str { "init" }
    fn branch(&self) -> &str { "main" }
    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::new("user").with_text("init"))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    println!("MCP Tools Basic Example");
    println!("=======================\n");

    // 1. Start MCP server and connect
    println!("Starting MCP server (@modelcontextprotocol/server-everything)...");
    let mut cmd = Command::new("npx");
    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");

    let client = ().serve(TokioChildProcess::new(cmd)?).await?;
    println!("MCP server connected!\n");

    // 2. Create toolset (expose all tools)
    let toolset = McpToolset::new(client);

    // 3. Get cancellation token for cleanup
    let cancel_token = toolset.cancellation_token().await;

    // 4. Discover tools
    let ctx = Arc::new(SimpleContext) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await?;

    println!("Discovered {} tools:", tools.len());
    for tool in &tools {
        println!("  - {}: {}", tool.name(), tool.description());
    }
    println!();

    // 5. Build agent with all tools
    let mut builder = LlmAgentBuilder::new("mcp_basic")
        .model(model)
        .instruction(
            "You have access to MCP tools from the 'everything' server. \
             Use them to help the user. Available tools include echo, add, \
             longRunningOperation, and more."
        );

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    // 6. Run interactive console
    let result = adk_cli::console::run_console(
        Arc::new(agent),
        "mcp_basic".to_string(),
        "user".to_string(),
    ).await;

    // 7. Cleanup: shutdown MCP server
    println!("\nShutting down MCP server...");
    cancel_token.cancel();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    result?;
    Ok(())
}
