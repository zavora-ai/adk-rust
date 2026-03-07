// MCP HTTP Transport Example
//
// This example demonstrates connecting to real remote MCP servers
// using the streamable HTTP transport.
//
// Features demonstrated:
// - HTTP transport for remote MCP servers
// - Fetch MCP server (web content fetching)
// - Sequential Thinking MCP server (structured problem-solving)
//
// To run this example:
//   cargo run --example mcp_http --features http-transport
//
// Remote MCP servers used:
// - https://remote.mcpservers.org/fetch/mcp - Web content fetching
// - https://remote.mcpservers.org/sequentialthinking/mcp - Structured thinking

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, ReadonlyContext, Toolset, types::AdkIdentity};
use adk_model::GeminiModel;
use adk_tool::{McpHttpClientBuilder, McpTaskConfig};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

/// Minimal context for tool discovery (no unimplemented! methods)
struct SimpleContext {
    identity: AdkIdentity,
    metadata: std::collections::HashMap<String, String>,
}

impl SimpleContext {
    fn new() -> Self {
        Self { identity: AdkIdentity::default(), metadata: std::collections::HashMap::new() }
    }
}

#[async_trait::async_trait]
#[async_trait::async_trait]
impl ReadonlyContext for SimpleContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::new("user").with_text("init"))
    }

    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        &self.metadata
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables from .env file
    dotenvy::dotenv().ok();

    println!("MCP HTTP Transport Example");
    println!("==========================\n");

    // Check for API key
    let api_key = std::env::var("GOOGLE_API_KEY").expect(
        "GOOGLE_API_KEY must be set. Run with:\n  \
         GOOGLE_API_KEY=your_key cargo run --example mcp_http --features http-transport",
    );

    // Remote MCP server endpoints
    let fetch_server = "https://remote.mcpservers.org/fetch/mcp";
    let thinking_server = "https://remote.mcpservers.org/sequentialthinking/mcp";

    println!("Connecting to remote MCP servers...\n");

    // Connect to Fetch MCP server
    println!("1. Connecting to Fetch MCP server: {}", fetch_server);
    let fetch_toolset = match McpHttpClientBuilder::new(fetch_server)
        .timeout(Duration::from_secs(30))
        .connect()
        .await
    {
        Ok(toolset) => {
            println!("   ✅ Connected to Fetch server!\n");
            Some(toolset)
        }
        Err(e) => {
            println!("   ❌ Failed to connect: {}\n", e);
            None
        }
    };

    // Connect to Sequential Thinking MCP server
    println!("2. Connecting to Sequential Thinking MCP server: {}", thinking_server);
    let thinking_toolset = match McpHttpClientBuilder::new(thinking_server)
        .timeout(Duration::from_secs(30))
        .connect()
        .await
    {
        Ok(toolset) => {
            // Add task support for long-running thinking operations
            let toolset_with_tasks = toolset.with_task_support(
                McpTaskConfig::enabled()
                    .poll_interval(Duration::from_secs(1))
                    .timeout(Duration::from_secs(120)),
            );
            println!("   ✅ Connected to Sequential Thinking server!\n");
            Some(toolset_with_tasks)
        }
        Err(e) => {
            println!("   ❌ Failed to connect: {}\n", e);
            None
        }
    };

    // Create a simple context for tool discovery
    let ctx = Arc::new(SimpleContext::new()) as Arc<dyn ReadonlyContext>;

    // Collect tools from connected servers
    let mut all_tools = Vec::new();

    if let Some(ref toolset) = fetch_toolset {
        match toolset.tools(ctx.clone()).await {
            Ok(tools) => {
                println!("Fetch server tools:");
                for tool in &tools {
                    let desc = tool.description();
                    let short_desc = if desc.len() > 60 {
                        format!("{}...", &desc[..60])
                    } else {
                        desc.to_string()
                    };
                    println!("  - {}: {}", tool.name(), short_desc);
                }
                all_tools.extend(tools);
            }
            Err(e) => println!("Failed to list Fetch tools: {}", e),
        }
    }

    if let Some(ref toolset) = thinking_toolset {
        match toolset.tools(ctx.clone()).await {
            Ok(tools) => {
                println!("\nSequential Thinking server tools:");
                for tool in &tools {
                    let desc = tool.description();
                    let short_desc = if desc.len() > 60 {
                        format!("{}...", &desc[..60])
                    } else {
                        desc.to_string()
                    };
                    println!("  - {}: {}", tool.name(), short_desc);
                }
                all_tools.extend(tools);
            }
            Err(e) => println!("Failed to list Sequential Thinking tools: {}", e),
        }
    }

    if all_tools.is_empty() {
        println!("\n❌ No tools available. Check your network connection.");
        return Ok(());
    }

    println!("\n✅ Total tools available: {}\n", all_tools.len());

    // Create model
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Build agent with MCP tools
    let mut agent_builder = LlmAgentBuilder::new("mcp-http-agent")
        .description("Agent with remote MCP tools for web fetching and structured thinking")
        .model(model)
        .instruction(
            "You are a helpful assistant with access to remote MCP tools:\n\n\
             1. **fetch** - Retrieve and process web content from URLs\n\
                - Converts HTML to markdown for easier reading\n\
                - Use this when asked to fetch or read web pages\n\n\
             2. **sequentialthinking** - Structured problem-solving\n\
                - Use for complex reasoning that benefits from step-by-step thinking\n\
                - Good for planning, analysis, and multi-step problems\n\n\
             When asked to fetch web content, use the fetch tool with the URL.\n\
             When asked to solve complex problems, use sequential thinking.",
        );

    for tool in all_tools {
        agent_builder = agent_builder.tool(tool);
    }

    let agent = agent_builder.build()?;

    println!("✅ Agent created with remote MCP tools");
    println!("\nStarting interactive console...");
    println!("Try: 'Fetch the content from https://example.com'\n");

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "mcp_http_example".to_string(),
        "user".to_string(),
    )
    .await?;

    Ok(())
}
