//! # MCP Sampling Example
//!
//! Demonstrates ADK-Rust's MCP Sampling support — the ability for MCP servers
//! to request LLM inference from the client via `sampling/createMessage`.
//!
//! ## What This Shows
//!
//! - Configuring `McpToolset` with a `SamplingHandler` so MCP servers can
//!   use the client's LLM for inference
//! - Using `LlmSamplingHandler` to route sampling requests through Gemini
//! - The full MCP sampling flow: agent calls tool → server sends
//!   `sampling/createMessage` → client routes to LLM → response flows back
//!
//! ## MCP Sampling Flow
//!
//! ```text
//! User ──→ Agent ──→ MCP Tool Call ──→ Server
//!                                        │
//!                                        │ peer.create_message(params)
//!                                        │ (sampling/createMessage)
//!                                        │
//!                               ←── LlmSamplingHandler called
//!                               (routes to Gemini LLM)
//!                               ──→ SamplingResponse { text, model }
//!                                        │
//!                               ←── Tool Result with LLM-generated content
//! ```
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set
//!
//! ## Run
//!
//! ```bash
//! cargo build --manifest-path examples/mcp_sampling/Cargo.toml
//! cargo run --manifest-path examples/mcp_sampling/Cargo.toml --bin sampling-client
//! ```

use adk_core::{Content, ReadonlyContext, Toolset};
use adk_rust::prelude::*;
use adk_tool::sampling::LlmSamplingHandler;
use adk_tool::{AutoDeclineElicitationHandler, McpToolset};
use std::sync::Arc;

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
        "mcp-sampling"
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

/// Locate the sampling-server binary next to the current executable.
fn server_binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("cannot determine executable path");
    path.pop();
    path.push("sampling-server");
    path
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Environment Setup ---
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set");
    let model_name = std::env::var("GEMINI_MODEL")
        .unwrap_or_else(|_| "gemini-2.5-flash".to_string());

    println!("╔══════════════════════════════════════════╗");
    println!("║  MCP Sampling — ADK-Rust v0.7.0         ║");
    println!("╚══════════════════════════════════════════╝\n");

    // --- Create the LLM provider ---
    // This LLM will be used both by the agent AND by the sampling handler
    // to service sampling/createMessage requests from the MCP server.
    let model = Arc::new(GeminiModel::new(&api_key, &model_name)?);

    // --- Start the MCP server as a child process ---
    let server_path = server_binary_path();
    if !server_path.exists() {
        anyhow::bail!(
            "Server binary not found at {}.\n\
             Build first: cargo build --manifest-path examples/mcp_sampling/Cargo.toml",
            server_path.display()
        );
    }

    println!("Starting MCP sampling server...");
    let cmd = tokio::process::Command::new(&server_path);
    let transport = rmcp::transport::TokioChildProcess::new(cmd)?;

    // --- Connect with sampling support ---
    //
    // Key difference from a standard MCP connection:
    //
    //   Standard:    McpToolset::new(client)
    //   Elicitation: McpToolset::with_elicitation_handler(transport, handler)
    //   Sampling:    McpToolset::with_sampling_handler(transport, elicitation, sampling)
    //
    // The LlmSamplingHandler wraps our Gemini model. When the MCP server
    // calls peer.create_message(), the request flows through this handler,
    // which converts it to an LlmRequest, calls Gemini, and returns the
    // response as a SamplingResponse.
    let elicitation_handler = Arc::new(AutoDeclineElicitationHandler);
    let sampling_handler = Arc::new(LlmSamplingHandler::new(model.clone()));
    let toolset =
        McpToolset::with_sampling_handler(transport, elicitation_handler, sampling_handler)
            .await?;
    println!("MCP server connected with sampling support!\n");

    // --- Get cancellation token for cleanup ---
    let cancel_token = toolset.cancellation_token().await;

    // --- Discover tools ---
    let ctx = Arc::new(SimpleContext) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await?;
    println!("Discovered {} tools:", tools.len());
    for tool in &tools {
        println!("  - {}: {}", tool.name(), tool.description());
    }
    println!();

    // --- Build agent with MCP tools ---
    //
    // The agent uses the same Gemini model for its own reasoning.
    // When the agent calls an MCP tool (e.g. "summarize"), the server
    // sends a sampling/createMessage request back, which the
    // LlmSamplingHandler routes through Gemini again.
    let mut builder = LlmAgentBuilder::new("sampling_agent")
        .model(model)
        .instruction(
            "You have access to MCP tools that use sampling. When you call these \
             tools, the MCP server will ask YOUR LLM to generate content via the \
             MCP sampling protocol (sampling/createMessage). This means the server \
             has no LLM of its own — it relies on yours.\n\n\
             Available tools:\n\
             - summarize: Summarize a piece of text (server asks your LLM to summarize)\n\
             - translate: Translate text to another language (server asks your LLM to translate)",
        );

    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = builder.build()?;

    // --- Run interactive console ---
    println!("Try: 'Summarize this: Rust is a systems programming language...'");
    println!("  or 'Translate hello world to French'\n");
    let result = adk_cli::console::run_console(
        Arc::new(agent),
        "mcp-sampling".to_string(),
        "user".to_string(),
    )
    .await;

    // --- Cleanup ---
    println!("\nShutting down MCP server...");
    cancel_token.cancel();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    result?;

    println!("\n✅ MCP Sampling example completed successfully.");
    Ok(())
}
