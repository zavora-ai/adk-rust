// MCP Integration Example
//
// This example demonstrates how to use the McpToolset to connect
// to an MCP server and expose its tools to an ADK agent.
//
// To run this example, you'll need an MCP server. For testing,
// you can use the "everything" server from the MCP reference implementation:
//
//   npx -y @modelcontextprotocol/server-everything
//
// Usage:
//   GEMINI_API_KEY=your_key cargo run --example mcp

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, InvocationContext, Part, ReadonlyContext, RunConfig, Session, State,
};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

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
    println!("MCP Integration Example");
    println!("=======================\n");

    // Check for API key
    let api_key = match env::var("GEMINI_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("❌ GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  GEMINI_API_KEY=your_key cargo run --example mcp");
            println!("\nShowing MCP usage pattern instead...\n");
            print_usage_pattern();
            return Ok(());
        }
    };

    // Try to connect to an MCP server
    println!("Attempting to connect to MCP server...\n");

    // For this example, we'll show the pattern without requiring an actual server
    // In a real application, you would use:
    //
    // use rmcp::{ServiceExt, transport::TokioChildProcess};
    // use tokio::process::Command;
    //
    // let client = ().serve(TokioChildProcess::new(
    //     Command::new("npx")
    //         .arg("-y")
    //         .arg("@modelcontextprotocol/server-everything")
    // )?).await?;
    //
    // let toolset = McpToolset::new(client)
    //     .with_filter(|name| matches!(name, "echo" | "add"));

    println!("⚠️  No MCP server available for this demo");
    println!("\nTo test with a real MCP server:");
    println!("1. Install the MCP server:");
    println!("   npm install -g @modelcontextprotocol/server-everything");
    println!("\n2. Run it:");
    println!("   npx @modelcontextprotocol/server-everything");
    println!("\n3. Connect from your code:");
    print_usage_pattern();

    // Show we can still create an agent without MCP tools
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-1.5-flash")?);

    let agent = LlmAgentBuilder::new("mcp-demo-agent")
        .description("Agent demonstrating MCP integration pattern")
        .model(model)
        .instruction("You are a helpful assistant. When MCP tools are available, you can use them.")
        .build()?;

    println!("\n✅ Agent created successfully (without MCP tools for this demo)");

    // Run a simple query
    let ctx = Arc::new(MockContext::new("Say hello briefly"));
    let mut stream = agent.run(ctx).await?;

    println!("\nAgent response:");
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

    Ok(())
}

fn print_usage_pattern() {
    println!(
        r#"
// MCP Toolset Usage Pattern
// =========================

use rmcp::{{ServiceExt, transport::TokioChildProcess}};
use tokio::process::Command;
use adk_tool::McpToolset;
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {{
    // 1. Create MCP client connection to a local server
    let client = ().serve(TokioChildProcess::new(
        Command::new("npx")
            .arg("-y")
            .arg("@modelcontextprotocol/server-everything")
    )?).await?;

    // 2. Create toolset from the client
    let toolset = McpToolset::new(client)
        .with_name("everything-tools")
        .with_filter(|name| {{
            // Only expose specific tools
            matches!(name, "echo" | "add" | "longRunningOperation")
        }});

    // 3. Add to agent
    let agent = LlmAgentBuilder::new("mcp-agent")
        .model(model)
        .instruction("Use MCP tools to help the user.")
        .toolset(Arc::new(toolset))
        .build()?;

    // 4. Run agent - it will automatically discover and use MCP tools
    let stream = agent.run(ctx).await?;

    Ok(())
}}

// Alternative: Filter by tool names
let toolset = McpToolset::new(client)
    .with_tools(&["read_file", "write_file", "list_directory"]);
"#
    );
}
