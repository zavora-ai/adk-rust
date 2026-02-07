// MCP Integration Example
//
// This example demonstrates how to use the McpToolset to connect
// to MCP servers and expose their tools to an ADK agent.
//
// Two modes are demonstrated:
// 1. Local MCP server via stdio (requires Node.js)
// 2. Remote MCP server via HTTP (no dependencies)
//
// Features demonstrated:
// - Basic MCP toolset creation
// - Tool filtering
// - Task support for long-running operations (SEP-1686)
// - Graceful shutdown with cancellation tokens
//
// Usage:
//   # Local server mode (requires Node.js):
//   GEMINI_API_KEY=your_key cargo run --example mcp
//
//   # Remote server mode (no Node.js needed):
//   GEMINI_API_KEY=your_key cargo run --example mcp -- --remote

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, InvocationContext, Part, ReadonlyContext, RunConfig, Session, State, Toolset,
};
use adk_model::GeminiModel;
use adk_tool::{McpTaskConfig, McpToolset};
use async_trait::async_trait;
use futures::StreamExt;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

// Mock session for the example
struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "mcp-session"
    }
    fn app_name(&self) -> &str {
        "mcp-example"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct MockState;
impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct MockContext {
    session: MockSession,
    user_content: Content,
}

impl MockContext {
    fn new(text: &str) -> Self {
        Self {
            session: MockSession,
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: text.to_string() }],
            },
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "mcp-inv"
    }
    fn agent_name(&self) -> &str {
        "mcp-agent"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "mcp-example"
    }
    fn session_id(&self) -> &str {
        "mcp-session"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        unimplemented!()
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load environment variables from .env file
    dotenvy::dotenv().ok();

    println!("MCP Integration Example (rmcp 0.14)");
    println!("====================================\n");

    // Check for API key
    let api_key = match env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("❌ GOOGLE_API_KEY not set");
            println!("\nTo run this example:");
            println!("  GOOGLE_API_KEY=your_key cargo run --example mcp");
            println!("\nShowing MCP usage patterns instead...\n");
            print_usage_patterns();
            return Ok(());
        }
    };

    // Try to connect to a local MCP server via stdio
    println!("Attempting to connect to local MCP server...\n");

    // Try to spawn the MCP server
    let mut cmd = Command::new("npx");
    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");

    let mcp_result = TokioChildProcess::new(cmd);

    match mcp_result {
        Ok(transport) => {
            println!("✅ MCP transport created, connecting...");

            match ().serve(transport).await {
                Ok(client) => {
                    println!("✅ Connected to local MCP server!\n");

                    // Create toolset with task support for long-running operations
                    let toolset = McpToolset::new(client)
                        .with_name("everything-tools")
                        .with_task_support(
                            McpTaskConfig::enabled()
                                .poll_interval(Duration::from_secs(1))
                                .timeout(Duration::from_secs(60)),
                        )
                        .with_filter(|name| {
                            // Only expose specific tools for this demo
                            matches!(name, "echo" | "add" | "longRunningOperation" | "getTime")
                        });

                    // Get cancellation token for graceful shutdown
                    let cancel_token = toolset.cancellation_token().await;

                    // Create model and agent
                    let model = Arc::new(GeminiModel::new(&api_key, "gemini-1.5-flash")?);

                    // Get tools from toolset
                    let ctx_for_tools =
                        Arc::new(MockContext::new("init")) as Arc<dyn ReadonlyContext>;
                    let tools = toolset.tools(ctx_for_tools).await?;

                    println!("Discovered {} tools:", tools.len());
                    for tool in &tools {
                        println!("  - {}: {}", tool.name(), tool.description());
                    }
                    println!();

                    let mut agent_builder = LlmAgentBuilder::new("mcp-demo-agent")
                        .description("Agent with MCP tools from the 'everything' server")
                        .model(model)
                        .instruction(
                            "You are a helpful assistant with access to MCP tools. \
                             Use the 'echo' tool to repeat messages, 'add' to add numbers, \
                             and 'getTime' to get the current time.",
                        );

                    for tool in tools {
                        agent_builder = agent_builder.tool(tool);
                    }
                    let agent = agent_builder.build()?;

                    println!("✅ Agent created with MCP tools\n");

                    // Run a query that uses MCP tools
                    let ctx = Arc::new(MockContext::new(
                        "Use the echo tool to say 'Hello from MCP!' and then tell me the current time.",
                    ));
                    let mut stream: std::pin::Pin<
                        Box<dyn futures::Stream<Item = adk_core::Result<adk_core::Event>> + Send>,
                    > = agent.run(ctx).await?;

                    println!("Agent response:");
                    println!("--------------");
                    while let Some(result) = stream.next().await {
                        if let Ok(event) = result
                            && let Some(content) = event.llm_response.content
                        {
                            for part in content.parts {
                                if let Part::Text { text } = part {
                                    print!("{}", text);
                                }
                            }
                        }
                    }
                    println!("\n");

                    // Graceful shutdown
                    println!("Shutting down MCP connection...");
                    cancel_token.cancel();
                    println!("✅ Done!\n");
                }
                Err(e) => {
                    println!("❌ Failed to connect to MCP server: {}", e);
                    println!("\nMake sure you have Node.js installed and can run:");
                    println!("  npx -y @modelcontextprotocol/server-everything\n");
                    print_usage_patterns();
                }
            }
        }
        Err(e) => {
            println!("❌ Failed to create MCP transport: {}", e);
            println!("\nMake sure you have Node.js and npx installed.\n");
            print_usage_patterns();
        }
    }

    Ok(())
}

fn print_usage_patterns() {
    println!(
        r#"
MCP Toolset Usage Patterns (rmcp 0.14)
======================================

1. LOCAL MCP SERVER - Via stdio (requires Node.js)
--------------------------------------------------

use rmcp::{{ServiceExt, transport::TokioChildProcess}};
use tokio::process::Command;
use adk_tool::McpToolset;

// Connect to a local MCP server
let mut cmd = Command::new("npx");
cmd.arg("-y").arg("@modelcontextprotocol/server-filesystem").arg("/path");

let client = ().serve(TokioChildProcess::new(cmd)?).await?;
let toolset = McpToolset::new(client);


2. REMOTE MCP SERVER - Via HTTP (no Node.js needed)
---------------------------------------------------

// Requires: adk-tool = {{ features = ["http-transport"] }}

use adk_tool::McpHttpClientBuilder;

// Connect to remote MCP servers
let fetch_toolset = McpHttpClientBuilder::new("https://remote.mcpservers.org/fetch/mcp")
    .timeout(Duration::from_secs(30))
    .connect()
    .await?;

let thinking_toolset = McpHttpClientBuilder::new("https://remote.mcpservers.org/sequentialthinking/mcp")
    .connect()
    .await?;


3. TASK SUPPORT - Long-Running Operations (SEP-1686)
----------------------------------------------------

use adk_tool::{{McpToolset, McpTaskConfig}};
use std::time::Duration;

let toolset = McpToolset::new(client)
    .with_task_support(
        McpTaskConfig::enabled()
            .poll_interval(Duration::from_secs(2))
            .timeout(Duration::from_secs(300))
    );


4. TOOL FILTERING
-----------------

// Filter by predicate
let toolset = McpToolset::new(client)
    .with_filter(|name| name.starts_with("file_"));

// Or filter by exact names
let toolset = McpToolset::new(client)
    .with_tools(&["fetch", "read_file", "write_file"]);


5. AUTHENTICATION (for remote servers)
--------------------------------------

use adk_tool::{{McpHttpClientBuilder, McpAuth, OAuth2Config}};

// Bearer token
let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
    .with_auth(McpAuth::bearer("your-token"))
    .connect()
    .await?;

// OAuth2
let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
    .with_auth(McpAuth::oauth2(
        OAuth2Config::new("client-id", "https://auth.example.com/token")
            .with_secret("secret")
    ))
    .connect()
    .await?;


AVAILABLE REMOTE MCP SERVERS
============================

- https://remote.mcpservers.org/fetch/mcp
  Web content fetching, converts HTML to markdown

- https://remote.mcpservers.org/sequentialthinking/mcp
  Structured problem-solving through step-by-step thinking

Run with HTTP transport:
  cargo run --example mcp_http --features http-transport

"#
    );
}
