//! MCP Tools Filtered Example
//!
//! Demonstrates filtering MCP tools to only expose specific ones to the agent.
//!
//! Prerequisites:
//! - Node.js and npm installed
//! - GOOGLE_API_KEY environment variable set
//!
//! Run:
//!   cd doc-test/tools/mcp_test
//!   GOOGLE_API_KEY=your_key cargo run --bin filtered

use adk_core::types::UserId;
use adk_core::{Content, ReadonlyContext, Toolset};
use adk_rust::prelude::*;
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use std::sync::Arc;
use tokio::process::Command;

/// Minimal context for tool discovery
#[derive(Default)]
struct SimpleContext {
    identity: adk_core::types::AdkIdentity,
}

impl ReadonlyContext for SimpleContext {
    fn identity(&self) -> &adk_core::types::AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::user().with_text("init"))
    }

    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        static METADATA: std::sync::OnceLock<std::collections::HashMap<String, String>> =
            std::sync::OnceLock::new();
        METADATA.get_or_init(std::collections::HashMap::new)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("MCP Tools Filtered Example");
    println!("==========================\n");

    // 1. Start MCP server
    println!("Starting MCP server...");
    let mut cmd = Command::new("npx");
    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");

    let client = ().serve(TokioChildProcess::new(cmd)?).await?;
    println!("MCP server connected!\n");

    // 2. Create toolset with filtering
    // Method 1: Filter by exact names
    let toolset = McpToolset::new(client).with_name("math-tools").with_tools(&["echo", "add"]);

    // Method 2: Filter by predicate (alternative)
    // let toolset = McpToolset::new(client)
    //     .with_filter(|name| matches!(name, "echo" | "add" | "printEnv"));

    // 3. Get cancellation token
    let cancel_token = toolset.cancellation_token().await;

    // 4. Discover filtered tools
    let ctx = Arc::new(SimpleContext::default()) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await?;

    println!("Filtered to {} tools:", tools.len());
    for tool in &tools {
        println!("  - {}: {}", tool.name(), tool.description());
    }
    println!();

    // 5. Build agent
    let mut builder = LlmAgentBuilder::new("mcp_filtered").model(model).instruction(
        "You have access to two MCP tools:\n\
             - echo: Repeats a message back to you\n\
             - add: Adds two numbers (parameters: a and b)\n\n\
             Use these tools to help the user.",
    );

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    // 6. Run console
    let result = adk_cli::console::run_console(
        Arc::new(agent),
        "mcp_filtered".to_string(),
        UserId::new("user").unwrap(),
    )
    .await;

    // 7. Cleanup
    println!("\nShutting down MCP server...");
    cancel_token.cancel();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    result?;
    Ok(())
}
