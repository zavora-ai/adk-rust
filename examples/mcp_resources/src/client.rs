//! MCP resources, prompts, and notifications — agentic example.
//!
//! Connects to the `resources-server` with a [`ResourceNotificationHandler`],
//! then:
//!
//! 1. lists and reads the server's **resource** (the review policy),
//! 2. fetches the server's **prompt** (`review_pr`) and uses it as the agent's
//!    instruction — the server, not the client, owns the wording,
//! 3. **subscribes** to the policy resource, and
//! 4. runs an LLM agent whose only tool is `refresh_policy`. When the agent
//!    calls it, the server mutates the policy and pushes a
//!    `notifications/resources/updated`, which the handler prints live.
//!
//! # Usage
//!
//! ```bash
//! export GOOGLE_API_KEY=your_key
//! cargo build --manifest-path examples/mcp_resources/Cargo.toml
//! cargo run --manifest-path examples/mcp_resources/Cargo.toml --bin resources-client
//! ```
//!
//! Then ask the agent to "refresh the review policy" and watch the resource
//! update notification arrive.

use std::sync::Arc;

use adk_core::{ReadonlyContext, Toolset};
use adk_rust::prelude::*;
use adk_tool::{
    AutoDeclineElicitationHandler, McpToolset, ResourceNotificationHandler, SimpleToolContext,
};
use rmcp::model::{ContentBlock, ResourceContents};
use serde_json::Map;

const POLICY_URI: &str = "config://app/review-policy";

/// Prints resource-update notifications as they arrive. The same handler is
/// retained if ADK-Rust has to reconnect the transport.
struct PrintingResourceHandler;

#[async_trait::async_trait]
impl ResourceNotificationHandler for PrintingResourceHandler {
    async fn handle_resource_updated(
        &self,
        uri: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("\n[notification] resource updated: {uri}");
        println!("               re-read it with read_resource to get the new contents.\n");
        Ok(())
    }

    async fn handle_resource_list_changed(
        &self,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("\n[notification] the server's resource list changed\n");
        Ok(())
    }
}

fn server_binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("cannot determine executable path");
    path.pop();
    path.push("resources-server");
    path
}

/// Flatten an MCP resource's contents into plain text.
fn resource_text(contents: &[ResourceContents]) -> String {
    contents
        .iter()
        .filter_map(|content| match content {
            ResourceContents::TextResourceContents { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set");
    let model_name =
        std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());
    let model = Arc::new(GeminiModel::new(&api_key, &model_name)?);

    println!("MCP Resources & Prompts Example");
    println!("===============================\n");

    // 1. Start the MCP server with a resource-notification handler installed
    //    before the handshake, so updates arrive the moment we subscribe.
    let server_path = server_binary_path();
    if !server_path.exists() {
        anyhow::bail!(
            "Server binary not found at {}.\nBuild first: cargo build --manifest-path examples/mcp_resources/Cargo.toml",
            server_path.display()
        );
    }

    let cmd = tokio::process::Command::new(&server_path);
    let transport = rmcp::transport::TokioChildProcess::new(cmd)?;
    let toolset = McpToolset::with_handlers(
        transport,
        Arc::new(AutoDeclineElicitationHandler),
        Arc::new(PrintingResourceHandler),
    )
    .await?;
    println!("Connected to MCP server with a resource-notification handler.\n");

    let cancel_token = toolset.cancellation_token().await;

    // 2. Discover and read the server's resources.
    let resources = toolset.list_resources().await?;
    println!("Resources ({}):", resources.len());
    for resource in &resources {
        println!("  - {} ({})", resource.uri, resource.name);
    }
    let policy = resource_text(&toolset.read_resource(POLICY_URI).await?);
    println!("\nCurrent policy resource:\n  {policy}\n");

    // 3. Fetch the server-owned prompt and use it as the agent instruction.
    let prompts = toolset.list_prompts().await?;
    println!("Prompts ({}):", prompts.len());
    for prompt in &prompts {
        println!("  - {}", prompt.name);
    }
    let mut prompt_args = Map::new();
    prompt_args.insert("pr_title".into(), "Add OAuth login".into());
    let review_prompt = toolset.get_prompt("review_pr", Some(prompt_args)).await?;
    let instruction = review_prompt
        .messages
        .iter()
        .filter_map(|message| match &message.content {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    println!("\nUsing the server's prompt as the agent instruction.\n");

    // 4. Subscribe to the policy resource. The subscription (and the handler
    //    above) survive an automatic reconnect.
    toolset.subscribe_resource(POLICY_URI).await?;
    println!("Subscribed to {POLICY_URI}.\n");

    // 5. Build the agent from the MCP-supplied instruction and tools.
    let ctx = Arc::new(SimpleToolContext::new("mcp-resources")) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await?;
    let mut builder = LlmAgentBuilder::new("policy_reviewer").model(model).instruction(format!(
        "{instruction}\n\nYou can call the refresh_policy tool to rotate the review policy. \
         After rotating, tell the user the policy changed.",
    ));
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = builder.build()?;

    println!("Try: 'Refresh the review policy' (watch for the update notification)\n");
    let result = adk_cli::console::run_console(
        Arc::new(agent),
        "mcp-resources".to_string(),
        "user".to_string(),
    )
    .await;

    println!("\nShutting down MCP server...");
    cancel_token.cancel();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    result?;
    Ok(())
}
