//! MCP Elicitation client — agentic example.
//!
//! Connects to the `elicitation-server` with a custom ElicitationHandler,
//! discovers its tools, injects them into an LLM agent, and runs an
//! interactive console. When the agent calls a tool that triggers
//! elicitation, the handler prompts you on stdin.
//!
//! # Usage
//!
//! ```bash
//! export GOOGLE_API_KEY=your_key
//! cargo build --manifest-path examples/mcp_elicitation/Cargo.toml
//! cargo run --manifest-path examples/mcp_elicitation/Cargo.toml --bin elicitation-client
//! ```

use adk_core::{Content, ReadonlyContext, Toolset};
use adk_rust::prelude::*;
use adk_tool::{ElicitationHandler, McpToolset};
use rmcp::model::{ElicitationAction, ElicitationSchema};
use serde_json::Value;
use std::sync::Arc;

type ElicitResult = std::result::Result<
    rmcp::model::CreateElicitationResult,
    Box<dyn std::error::Error + Send + Sync>,
>;

// ---------------------------------------------------------------------------
// ElicitationHandler: prompts the user on stdin for each field
// ---------------------------------------------------------------------------

struct StdinElicitationHandler;

#[async_trait::async_trait]
impl ElicitationHandler for StdinElicitationHandler {
    async fn handle_form_elicitation(
        &self,
        message: &str,
        schema: &ElicitationSchema,
        _metadata: Option<&Value>,
    ) -> ElicitResult {
        println!("\n--- MCP Server requests input ---");
        println!("{message}\n");

        let mut response = serde_json::Map::new();
        let required = schema.required.clone().unwrap_or_default();

        for (field_name, _) in &schema.properties {
            let marker = if required.contains(field_name) {
                "*"
            } else {
                " "
            };
            print!("  {marker} {field_name}: ");
            std::io::Write::flush(&mut std::io::stdout())
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            let input = input.trim();

            if !input.is_empty() {
                response.insert(field_name.clone(), Value::String(input.to_string()));
            }
        }

        println!("--- End of input ---\n");
        Ok(
            rmcp::model::CreateElicitationResult::new(ElicitationAction::Accept)
                .with_content(Value::Object(response)),
        )
    }

    async fn handle_url_elicitation(
        &self,
        message: &str,
        url: &str,
        _elicitation_id: &str,
        _metadata: Option<&Value>,
    ) -> ElicitResult {
        println!("\n--- MCP Server requests URL visit ---");
        println!("{message}");
        println!("URL: {url}");
        print!("Press Enter when done: ");
        std::io::Write::flush(&mut std::io::stdout())
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

        let mut buf = String::new();
        std::io::stdin()
            .read_line(&mut buf)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

        Ok(rmcp::model::CreateElicitationResult::new(
            ElicitationAction::Accept,
        ))
    }
}

// ---------------------------------------------------------------------------
// Minimal ReadonlyContext for tool discovery
// ---------------------------------------------------------------------------

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
        "mcp-elicitation"
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

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn server_binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("cannot determine executable path");
    path.pop();
    path.push("elicitation-server");
    path
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY must be set");
    let model_name = std::env::var("GEMINI_MODEL")
        .unwrap_or_else(|_| "gemini-2.5-flash".to_string());

    let model = Arc::new(GeminiModel::new(&api_key, &model_name)?);

    println!("MCP Elicitation Example");
    println!("=======================\n");

    // 1. Start MCP server with elicitation support
    let server_path = server_binary_path();
    if !server_path.exists() {
        anyhow::bail!(
            "Server binary not found at {}.\nBuild first: cargo build --manifest-path examples/mcp_elicitation/Cargo.toml",
            server_path.display()
        );
    }

    println!("Starting elicitation MCP server...");
    let cmd = tokio::process::Command::new(&server_path);
    let transport = rmcp::transport::TokioChildProcess::new(cmd)?;

    // Key difference: with_elicitation_handler instead of ().serve()
    let handler = Arc::new(StdinElicitationHandler);
    let toolset = McpToolset::with_elicitation_handler(transport, handler).await?;
    println!("MCP server connected with elicitation support!\n");

    // 2. Get cancellation token for cleanup
    let cancel_token = toolset.cancellation_token().await;

    // 3. Discover tools
    let ctx = Arc::new(SimpleContext) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await?;
    println!("Discovered {} tools:", tools.len());
    for tool in &tools {
        println!("  - {}: {}", tool.name(), tool.description());
    }
    println!();

    // 4. Build agent with MCP tools
    let mut builder = LlmAgentBuilder::new("elicitation_agent")
        .model(model)
        .instruction(
            "You have access to MCP tools that support elicitation. \
             When you call these tools, the server may ask the user for \
             additional input (like their name, email, or a confirmation). \
             This happens automatically through the elicitation protocol. \
             Available tools: create_user (creates a user account), \
             deploy_app (deploys an app to production — requires confirmation).",
        );

    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = builder.build()?;

    // 5. Run interactive console
    println!("Try: 'Create a new user account' or 'Deploy my-app to production'\n");
    let result = adk_cli::console::run_console(
        Arc::new(agent),
        "mcp-elicitation".to_string(),
        "user".to_string(),
    )
    .await;

    // 6. Cleanup
    println!("\nShutting down MCP server...");
    cancel_token.cancel();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    result?;
    Ok(())
}
