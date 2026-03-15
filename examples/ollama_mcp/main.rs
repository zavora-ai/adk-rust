//! Ollama MCP Integration Example
//!
//! This example demonstrates how to use the McpToolset with a local Ollama model.
//! It connects to the MCP "everything" server and uses its tools.
//!
//! Requirements:
//! 1. Ollama running locally: ollama serve
//! 2. A model pulled: ollama pull qwen2.5:7b (or llama3.2)
//! 3. Node.js/npm installed (for the MCP server)
//!
//! Usage:
//!   OLLAMA_MODEL=qwen2.5:7b cargo run --example ollama_mcp --features ollama

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, ReadonlyContext, Toolset};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_tool::McpToolset;
use async_trait::async_trait;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use std::sync::Arc;
use tokio::process::Command;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Simple context for getting tools from the toolset
struct SimpleContext;

#[async_trait]
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
        "ollama-mcp"
    }
    fn session_id(&self) -> &str {
        "init"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "init".to_string() }],
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing (only show warnings by default to reduce noise)
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "warn,adk_agent=info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("Ollama + MCP Integration Example");
    println!("=================================\n");

    let model_name = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen2.5:7b".to_string());
    println!("Using Ollama model: {}", model_name);
    println!("Make sure Ollama is running: ollama serve");
    println!("And the model is pulled: ollama pull {}\n", model_name);

    let config = OllamaConfig::new(&model_name);
    let model = Arc::new(OllamaModel::new(config)?);

    println!("Starting MCP server (@modelcontextprotocol/server-everything)...");

    let mut cmd = Command::new("npx");
    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");

    let client = ().serve(TokioChildProcess::new(cmd)?).await?;
    println!("MCP server connected!");

    let toolset = McpToolset::new(client)
        .with_name("everything-tools")
        .with_filter(|name| matches!(name, "echo" | "add" | "printEnv"));

    // Get cancellation token to cleanly shutdown the MCP server later
    let mcp_cancel = toolset.cancellation_token().await;

    let ctx = Arc::new(SimpleContext) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await?;

    println!("Discovered {} MCP tools:", tools.len());
    for tool in &tools {
        println!("  - {}: {}", tool.name(), tool.description());
    }
    println!();

    let mut builder = LlmAgentBuilder::new("ollama-mcp-agent")
        .description("An assistant with MCP tools running on local Ollama")
        .model(model)
        .instruction(
            "You are a helpful assistant running locally via Ollama. \
             You have access to MCP tools. Use them when appropriate to help the user.",
        );

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;
    println!("Agent created with MCP tools!\n");

    let result = adk_cli::console::run_console(
        Arc::new(agent),
        "ollama_mcp".to_string(),
        "user".to_string(),
    )
    .await;

    // Cleanly shutdown the MCP server
    println!("\nShutting down MCP server...");
    mcp_cancel.cancel();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    println!("Done.");

    result?;
    Ok(())
}
