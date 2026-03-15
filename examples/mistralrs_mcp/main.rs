//! mistral.rs MCP (Model Context Protocol) client integration example.
//!
//! This example demonstrates how to connect mistral.rs models to MCP servers
//! for external tool integration.
//!
//! # What is MCP?
//!
//! Model Context Protocol (MCP) is a standard for connecting AI models to
//! external tools and data sources. MCP servers provide tools that models
//! can discover and use during inference.
//!
//! # Prerequisites
//!
//! You need an MCP server running. Popular options:
//! - mcp-server-filesystem: File system operations
//! - mcp-server-fetch: HTTP requests
//! - mcp-server-sqlite: Database queries
//!
//! Install with: `npm install -g @modelcontextprotocol/server-filesystem`
//!
//! # Running
//!
//! ```bash
//! # With a process-based MCP server
//! cargo run --example mistralrs_mcp
//!
//! # With a config file
//! MCP_CONFIG=mcp-config.json cargo run --example mistralrs_mcp
//! ```

use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{
    McpClientConfig, McpServerConfig, MistralRsConfig, MistralRsModel, ModelSource,
};
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("ADK mistral.rs MCP Client Example");
    println!("==================================");
    println!();

    // Check for config file
    let mcp_config = if let Ok(config_path) = std::env::var("MCP_CONFIG") {
        println!("Loading MCP config from: {}", config_path);
        McpClientConfig::from_file(&config_path)?
    } else {
        // Create a default MCP configuration
        println!("Using default MCP configuration");
        println!();
        println!("To use a custom config, set MCP_CONFIG environment variable.");
        println!();

        create_example_config()
    };

    // Validate the configuration
    if let Err(e) = mcp_config.validate() {
        println!("MCP configuration validation failed: {}", e);
        println!();
        print_usage();
        return Ok(());
    }

    println!("MCP Servers configured: {}", mcp_config.servers.len());
    for server in &mcp_config.servers {
        println!("  - {} ({})", server.name, if server.enabled { "enabled" } else { "disabled" });
    }
    println!();

    // Get model ID
    let model_id = std::env::var("MISTRALRS_MODEL")
        .unwrap_or_else(|_| "microsoft/Phi-3.5-mini-instruct".to_string());

    println!("Loading model: {}", model_id);
    println!("This may take a few minutes on first run...");
    println!();

    // Create model configuration with MCP client
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface(&model_id))
        .mcp_client(mcp_config)
        .temperature(0.3) // Lower temperature for tool calling
        .max_tokens(1024)
        .build();

    // Load the model
    let model = MistralRsModel::new(config).await?;

    println!("Model loaded successfully!");
    println!();

    // Create an agent with the MCP-enabled model
    let agent = LlmAgentBuilder::new("mcp-assistant")
        .description("An assistant with MCP tool integration")
        .model(Arc::new(model))
        .instruction(
            "You are a helpful assistant with access to external tools via MCP. \
             Use the available tools to help users with their requests. \
             When a tool is available, prefer using it over making assumptions.",
        )
        .build()?;

    println!("MCP Integration Notes:");
    println!("----------------------");
    println!("- Tools from MCP servers are automatically discovered");
    println!("- The model can call these tools during inference");
    println!("- Tool results are incorporated into the response");
    println!();

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "mistralrs_mcp".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}

/// Create an example MCP configuration.
///
/// This demonstrates the different types of MCP server configurations.
fn create_example_config() -> McpClientConfig {
    // Example: Filesystem MCP server (process-based)
    // Requires: npm install -g @modelcontextprotocol/server-filesystem
    let filesystem_server = McpServerConfig::process("Filesystem", "mcp-server-filesystem")
        .with_args(vec!["--root".to_string(), "/tmp".to_string()])
        .with_tool_prefix("fs");

    // Example: HTTP-based MCP server
    // let http_server = McpServerConfig::http("API Tools", "https://api.example.com/mcp")
    //     .with_bearer_token("your-api-key")
    //     .with_timeout(30);

    // Example: WebSocket-based MCP server
    // let ws_server = McpServerConfig::websocket("Realtime", "wss://realtime.example.com/mcp")
    //     .with_timeout(60);

    McpClientConfig::new()
        .add_server(filesystem_server)
        // .add_server(http_server)
        // .add_server(ws_server)
        .with_tool_timeout(30)
        .with_max_concurrent_calls(3)
}

fn print_usage() {
    println!("MCP Client Usage");
    println!("================");
    println!();
    println!("This example requires MCP servers to be available.");
    println!();
    println!("Option 1: Install an MCP server");
    println!("  npm install -g @modelcontextprotocol/server-filesystem");
    println!();
    println!("Option 2: Use a config file");
    println!("  MCP_CONFIG=mcp-config.json cargo run --example mistralrs_mcp");
    println!();
    println!("Example config file (mcp-config.json):");
    println!(
        r#"{{
  "servers": [
    {{
      "name": "Filesystem",
      "source": {{
        "type": "Process",
        "command": "mcp-server-filesystem",
        "args": ["--root", "/tmp"]
      }},
      "enabled": true,
      "tool_prefix": "fs"
    }},
    {{
      "name": "HTTP API",
      "source": {{
        "type": "Http",
        "url": "https://api.example.com/mcp"
      }},
      "bearer_token": "your-api-key"
    }}
  ],
  "auto_register_tools": true,
  "tool_timeout_secs": 30,
  "max_concurrent_calls": 3
}}"#
    );
    println!();
    println!("Supported MCP server types:");
    println!("  - Process: Local command execution (stdin/stdout)");
    println!("  - Http: HTTP-based JSON-RPC");
    println!("  - WebSocket: Real-time bidirectional communication");
}
