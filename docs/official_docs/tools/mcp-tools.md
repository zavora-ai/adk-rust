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

## Basic Usage

Connect to an MCP server and use its tools:

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // 1. Create MCP client connection to a local server
    let client = ().serve(TokioChildProcess::new(
        Command::new("npx")
            .arg("-y")
            .arg("@modelcontextprotocol/server-everything")
    )?).await?;

    // 2. Create toolset from the client
    let toolset = McpToolset::new(client);

    // 3. Add to agent
    let agent = LlmAgentBuilder::new("mcp_agent")
        .description("Agent with MCP tools")
        .model(Arc::new(model))
        .toolset(Arc::new(toolset))
        .build()?;

    Ok(())
}
```

## McpToolset Configuration

### Custom Name

Set a custom name for the toolset:

```rust
let toolset = McpToolset::new(client)
    .with_name("filesystem-tools");
```

### Tool Filtering

Filter which tools to expose using a predicate function:

```rust
// Only expose specific tools
let toolset = McpToolset::new(client)
    .with_filter(|name| {
        matches!(name, "read_file" | "write_file" | "list_directory")
    });
```

Or use the convenience method for exact name matching:

```rust
let toolset = McpToolset::new(client)
    .with_tools(&["echo", "add", "get_time"]);
```

## Connecting to MCP Servers

### Local Servers (Stdio)

Connect to a local MCP server via standard input/output:

```rust
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;

// NPM package server
let client = ().serve(TokioChildProcess::new(
    Command::new("npx")
        .arg("-y")
        .arg("@modelcontextprotocol/server-filesystem")
        .arg("/path/to/allowed/directory")
)?).await?;

// Local binary server
let client = ().serve(TokioChildProcess::new(
    Command::new("./my-mcp-server")
        .arg("--config")
        .arg("config.json")
)?).await?;
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

for tool in &tools {
    println!("Tool: {} - {}", tool.name(), tool.description());
}
```

Each discovered tool:
- Has its name and description from the MCP server
- Includes parameter schemas for LLM accuracy
- Executes via the MCP protocol when called

## Complete Example

Here's a full example using the MCP "everything" test server:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, InvocationContext, Part, ReadonlyContext, RunConfig, Session, State};
use adk_model::GeminiModel;
use adk_tool::McpToolset;
use async_trait::async_trait;
use futures::StreamExt;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;

// ... (Session and Context implementations)

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Connect to MCP server
    let client = ().serve(TokioChildProcess::new(
        Command::new("npx")
            .arg("-y")
            .arg("@modelcontextprotocol/server-everything")
    )?).await?;

    // Create filtered toolset
    let toolset = McpToolset::new(client)
        .with_name("everything-tools")
        .with_filter(|name| {
            matches!(name, "echo" | "add" | "getAlerts")
        });

    // Build agent
    let agent = LlmAgentBuilder::new("mcp_demo")
        .description("Demo agent with MCP tools")
        .instruction(
            "You have access to MCP tools. Use 'echo' to repeat messages, \
             'add' to add numbers, and 'getAlerts' for weather alerts."
        )
        .model(Arc::new(model))
        .toolset(Arc::new(toolset))
        .build()?;

    // Run agent
    let ctx = Arc::new(MockContext::new("Echo 'Hello MCP!' and then add 5 + 3"));
    let mut stream = agent.run(ctx).await?;

    while let Some(result) = stream.next().await {
        if let Ok(event) = result {
            if let Some(content) = event.llm_response.content {
                for part in content.parts {
                    if let Part::Text { text } = part {
                        print!("{}", text);
                    }
                }
            }
        }
    }
    println!();

    Ok(())
}
```

## Popular MCP Servers

Here are some commonly used MCP servers you can integrate:

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
2. **Handle errors** - MCP servers may fail; implement appropriate error handling
3. **Use local servers** - For development, stdio transport is simpler than remote
4. **Check server status** - Verify MCP server is running before creating toolset
5. **Resource cleanup** - The client connection is dropped when toolset is dropped

## Advanced: Custom MCP Server

You can create your own MCP server in Rust using the `rmcp` SDK:

```rust
use rmcp::{tool, tool_router, handler::server::tool::ToolRouter, model::*};
use tokio::sync::Mutex;
use std::sync::Arc;

#[derive(Clone)]
pub struct MyServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MyServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Add two numbers")]
    async fn add(&self, a: i32, b: i32) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![Content::text(
            (a + b).to_string()
        )]))
    }

    #[tool(description = "Multiply two numbers")]
    async fn multiply(&self, a: i32, b: i32) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![Content::text(
            (a * b).to_string()
        )]))
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
