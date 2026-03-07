# MCP Tools

Model Context Protocol (MCP) is an open standard that enables LLMs to communicate with external applications, data sources, and tools. ADK-Rust provides full MCP support through the `McpToolset`, allowing you to connect to any MCP-compliant server and expose its tools to your agents.

## Overview

MCP follows a client-server architecture:
- **MCP Servers** expose tools, resources, and prompts
- **MCP Clients** (like ADK agents) connect to servers and use their capabilities

Benefits of MCP integration:
- **Universal connectivity** - Connect to any MCP-compliant server
- **Automatic discovery** - Tools are discovered dynamically from the server
- **Language agnostic** - Use tools written in any language
- **Growing ecosystem** - Access thousands of existing MCP servers

## Prerequisites

MCP servers are typically distributed as npm packages. You'll need:
- Node.js and npm installed
- An LLM API key (Gemini, OpenAI, etc.)

## Quick Start

Connect to an MCP server and use its tools:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, ReadonlyContext, Toolset};
use adk_model::GeminiModel;
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // 1. Start MCP server and connect
    let mut cmd = Command::new("npx");
    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");

    let client = ().serve(TokioChildProcess::new(cmd)?).await?;

    // 2. Create toolset from the client
    let toolset = McpToolset::new(client)
        .with_tools(&["echo", "add"]);  // Only expose these tools

    // 3. Get cancellation token for cleanup
    let cancel_token = toolset.cancellation_token().await;

    // 4. Discover tools and add to agent
    let ctx: Arc<dyn ReadonlyContext> = Arc::new(SimpleContext);
    let tools = toolset.tools(ctx).await?;

    let mut builder = LlmAgentBuilder::new("mcp_agent")
        .model(model)
        .instruction("You have MCP tools. Use 'echo' to repeat messages, 'add' to sum numbers.");

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    // 5. Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "mcp_demo".to_string(),
        "user".to_string(),
    ).await?;

    // 6. Cleanup: shutdown MCP server
    cancel_token.cancel();

    Ok(())
}

// Minimal context for tool discovery
struct SimpleContext;

#[async_trait::async_trait]
impl ReadonlyContext for SimpleContext {
    fn identity(&self) -> &adk_core::types::AdkIdentity {
        static IDENTITY: std::sync::OnceLock<adk_core::types::AdkIdentity> = std::sync::OnceLock::new();
        IDENTITY.get_or_init(|| {
            let mut id = adk_core::types::AdkIdentity::default();
            id.user_id = "user".to_string();
            id.app_name = "mcp".to_string();
            id
        })
    }
    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::new("user").with_text("init"))
    }
    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        static METADATA: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
        METADATA.get_or_init(std::collections::HashMap::new)
    }
}
```

Run with:
```bash
GOOGLE_API_KEY=your_key cargo run --bin basic
```

## McpToolset API

### Creating a Toolset

```rust
use adk_tool::McpToolset;

// Basic creation
let toolset = McpToolset::new(client);

// With custom name
let toolset = McpToolset::new(client)
    .with_name("filesystem-tools");
```

### Tool Filtering

Filter which tools to expose:

```rust
// Filter by predicate function
let toolset = McpToolset::new(client)
    .with_filter(|name| {
        matches!(name, "read_file" | "write_file" | "list_directory")
    });

// Filter by exact names (convenience method)
let toolset = McpToolset::new(client)
    .with_tools(&["echo", "add", "get_time"]);
```

### Cleanup with Cancellation Token

Always get a cancellation token to cleanly shutdown the MCP server:

```rust
let toolset = McpToolset::new(client);
let cancel_token = toolset.cancellation_token().await;

// ... use the toolset ...

// Before exiting, shutdown the MCP server
cancel_token.cancel();
```

This prevents EPIPE errors and ensures clean process termination.

## Connecting to MCP Servers

### Local Servers (Stdio)

Connect to a local MCP server via standard input/output:

```rust
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;

// NPM package server
let mut cmd = Command::new("npx");
cmd.arg("-y")
    .arg("@modelcontextprotocol/server-filesystem")
    .arg("/path/to/allowed/directory");
let client = ().serve(TokioChildProcess::new(cmd)?).await?;

// Local binary server
let mut cmd = Command::new("./my-mcp-server");
cmd.arg("--config").arg("config.json");
let client = ().serve(TokioChildProcess::new(cmd)?).await?;
```

### Remote Servers (SSE)

Connect to a remote MCP server via Server-Sent Events:

```rust
use rmcp::{ServiceExt, transport::SseClient};

let client = ().serve(
    SseClient::new("http://localhost:8080/sse")?
).await?;
```

## Tool Discovery

The `McpToolset` automatically discovers tools from the connected server:

```rust
use adk_core::{ReadonlyContext, Toolset};

// Get discovered tools
let tools = toolset.tools(ctx).await?;

println!("Discovered {} tools:", tools.len());
for tool in &tools {
    println!("  - {}: {}", tool.name(), tool.description());
}
```

Each discovered tool:
- Has its name and description from the MCP server
- Includes parameter schemas for LLM accuracy
- Executes via the MCP protocol when called

## Adding Tools to Agent

There are two patterns for adding MCP tools to an agent:

### Pattern 1: Add as Toolset

```rust
let toolset = McpToolset::new(client);

let agent = LlmAgentBuilder::new("agent")
    .model(model)
    .toolset(Arc::new(toolset))
    .build()?;
```

### Pattern 2: Add Individual Tools

This gives you more control over which tools are added:

```rust
let toolset = McpToolset::new(client)
    .with_tools(&["echo", "add"]);

let tools = toolset.tools(ctx).await?;

let mut builder = LlmAgentBuilder::new("agent")
    .model(model);

for tool in tools {
    builder = builder.tool(tool);
}

let agent = builder.build()?;
```

## Popular MCP Servers

Here are some commonly used MCP servers you can integrate:

### Everything Server (Testing)
```bash
npx -y @modelcontextprotocol/server-everything
```
Tools: `echo`, `add`, `longRunningOperation`, `sampleLLM`, `getAlerts`, `printEnv`

### Filesystem Server
```bash
npx -y @modelcontextprotocol/server-filesystem /path/to/directory
```
Tools: `read_file`, `write_file`, `list_directory`, `search_files`

### GitHub Server
```bash
npx -y @modelcontextprotocol/server-github
```
Tools: `search_repositories`, `get_file_contents`, `create_issue`

### Slack Server
```bash
npx -y @modelcontextprotocol/server-slack
```
Tools: `send_message`, `list_channels`, `search_messages`

### Memory Server
```bash
npx -y @modelcontextprotocol/server-memory
```
Tools: `store`, `retrieve`, `search`

Find more servers at the [MCP Server Registry](https://github.com/modelcontextprotocol/servers).

## Error Handling

Handle MCP connection and execution errors:

```rust
use adk_core::AdkError;

match toolset.tools(ctx).await {
    Ok(tools) => {
        println!("Discovered {} tools", tools.len());
    }
    Err(AdkError::Tool(msg)) => {
        eprintln!("MCP error: {}", msg);
    }
    Err(e) => {
        eprintln!("Other error: {}", e);
    }
}
```

Common errors:
- **Connection failed** - Server not running or wrong address
- **Tool execution failed** - MCP server returned an error
- **Invalid parameters** - Tool received incorrect arguments

## Best Practices

1. **Filter tools** - Only expose tools the agent needs to reduce confusion
2. **Use cancellation tokens** - Always call `cancel()` before exiting to cleanup
3. **Handle errors** - MCP servers may fail; implement appropriate error handling
4. **Use local servers** - For development, stdio transport is simpler than remote
5. **Check server status** - Verify MCP server is running before creating toolset

## Complete Example

Here's a full working example with proper cleanup:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, ReadonlyContext, Toolset};
use adk_model::GeminiModel;
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use std::sync::Arc;
use tokio::process::Command;

struct SimpleContext;

#[async_trait::async_trait]
impl ReadonlyContext for SimpleContext {
    fn identity(&self) -> &adk_core::types::AdkIdentity {
        static IDENTITY: std::sync::OnceLock<adk_core::types::AdkIdentity> = std::sync::OnceLock::new();
        IDENTITY.get_or_init(|| {
            let mut id = adk_core::types::AdkIdentity::default();
            id.user_id = "user".to_string();
            id.app_name = "mcp".to_string();
            id
        })
    }
    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::new("user").with_text("init"))
    }
    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        static METADATA: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
        METADATA.get_or_init(std::collections::HashMap::new)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("Starting MCP server...");
    let mut cmd = Command::new("npx");
    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");

    let client = ().serve(TokioChildProcess::new(cmd)?).await?;
    println!("MCP server connected!");

    // Create filtered toolset
    let toolset = McpToolset::new(client)
        .with_name("everything-tools")
        .with_filter(|name| matches!(name, "echo" | "add" | "printEnv"));

    // Get cancellation token for cleanup
    let cancel_token = toolset.cancellation_token().await;

    // Discover tools
    let ctx = Arc::new(SimpleContext) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await?;

    println!("Discovered {} tools:", tools.len());
    for tool in &tools {
        println!("  - {}: {}", tool.name(), tool.description());
    }

    // Build agent with tools
    let mut builder = LlmAgentBuilder::new("mcp_demo")
        .model(model)
        .instruction(
            "You have access to MCP tools:\n\
             - echo: Repeat a message back\n\
             - add: Add two numbers (a + b)\n\
             - printEnv: Print environment variables"
        );

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    // Run interactive console
    let result = adk_cli::console::run_console(
        Arc::new(agent),
        "mcp_demo".to_string(),
        "user".to_string(),
    ).await;

    // Cleanup
    println!("\nShutting down MCP server...");
    cancel_token.cancel();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    result?;
    Ok(())
}
```

## Advanced: Custom MCP Server

You can create your own MCP server in Rust using the `rmcp` SDK:

```rust
use rmcp::{tool, tool_router, handler::server::tool::ToolRouter, model::*};

#[derive(Clone)]
pub struct MyServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MyServer {
    fn new() -> Self {
        Self { tool_router: Self::tool_router() }
    }

    #[tool(description = "Add two numbers")]
    async fn add(&self, a: i32, b: i32) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![Content::text((a + b).to_string())]))
    }

    #[tool(description = "Multiply two numbers")]
    async fn multiply(&self, a: i32, b: i32) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![Content::text((a * b).to_string())]))
    }
}
```

See the [rmcp documentation](https://github.com/modelcontextprotocol/rust-sdk) for complete server implementation details.

## Related

- [Function Tools](function-tools.md) - Creating custom tools in Rust
- [Built-in Tools](built-in-tools.md) - Pre-built tools included with ADK
- [LlmAgent](../agents/llm-agent.md) - Adding tools to agents
- [rmcp SDK](https://github.com/modelcontextprotocol/rust-sdk) - Official Rust MCP SDK
- [MCP Specification](https://modelcontextprotocol.io/) - Protocol documentation

---

**Previous**: [← UI Tools](ui-tools.md) | **Next**: [Sessions →](../sessions/sessions.md)
