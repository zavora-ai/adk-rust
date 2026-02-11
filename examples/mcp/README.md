# MCP Integration Examples

These examples demonstrate how to integrate MCP (Model Context Protocol) servers with ADK agents.

## Examples

### 1. Local MCP Server (`mcp`)

Demonstrates connecting to a local MCP server via stdio transport:

```bash
# Requires Node.js for the MCP server
GEMINI_API_KEY=your_key cargo run --example mcp
```

Features:
- Local MCP server connection via `TokioChildProcess`
- Tool filtering (by predicate or exact names)
- Task support for long-running operations (SEP-1686)
- Graceful shutdown with cancellation tokens

### 2. Remote MCP Servers (`mcp_http`)

Demonstrates connecting to real remote MCP servers via HTTP:

```bash
# No Node.js required!
GEMINI_API_KEY=your_key cargo run --example mcp_http --features http-transport
```

Features:
- Streamable HTTP transport
- Real remote MCP servers:
  - **Fetch** (`https://remote.mcpservers.org/fetch/mcp`) - Web content fetching
  - **Sequential Thinking** (`https://remote.mcpservers.org/sequentialthinking/mcp`) - Structured problem-solving
- Task support for long-running operations
- Authentication options (Bearer, API Key, OAuth2)

## Quick Start

### Remote MCP Servers (Recommended - No Dependencies)

```rust
use adk_tool::McpHttpClientBuilder;
use std::time::Duration;

// Connect to Fetch MCP server
let fetch_toolset = McpHttpClientBuilder::new("https://remote.mcpservers.org/fetch/mcp")
    .timeout(Duration::from_secs(30))
    .connect()
    .await?;

// Connect to Sequential Thinking MCP server
let thinking_toolset = McpHttpClientBuilder::new("https://remote.mcpservers.org/sequentialthinking/mcp")
    .connect()
    .await?;
```

### Local MCP Server (Requires Node.js)

```rust
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;
use adk_tool::McpToolset;

// Connect to local MCP server
let mut cmd = Command::new("npx");
cmd.arg("-y").arg("@modelcontextprotocol/server-everything");

let client = ().serve(TokioChildProcess::new(cmd)?).await?;
let toolset = McpToolset::new(client);
```

## Available Remote MCP Servers

| Server | URL | Description |
|--------|-----|-------------|
| Fetch | `https://remote.mcpservers.org/fetch/mcp` | Web content fetching, converts HTML to markdown |
| Sequential Thinking | `https://remote.mcpservers.org/sequentialthinking/mcp` | Structured problem-solving through step-by-step thinking |

## Task Support (SEP-1686)

For long-running operations, enable task support:

```rust
use adk_tool::{McpToolset, McpTaskConfig};
use std::time::Duration;

let toolset = McpToolset::new(client)
    .with_task_support(
        McpTaskConfig::enabled()
            .poll_interval(Duration::from_secs(2))
            .timeout(Duration::from_secs(300))
    );
```

## Authentication (for private servers)

```rust
use adk_tool::{McpHttpClientBuilder, McpAuth, OAuth2Config};

// Bearer token
let toolset = McpHttpClientBuilder::new("https://private-mcp.example.com")
    .with_auth(McpAuth::bearer("your-token"))
    .connect()
    .await?;

// OAuth2
let toolset = McpHttpClientBuilder::new("https://private-mcp.example.com")
    .with_auth(McpAuth::oauth2(
        OAuth2Config::new("client-id", "https://auth.example.com/token")
            .with_secret("secret")
    ))
    .connect()
    .await?;
```

## Cargo.toml Configuration

```toml
[dependencies]
# For local MCP servers (stdio)
adk-tool = "0.3.0"
rmcp = { version = "0.14", features = ["client", "transport-child-process"] }

# For remote MCP servers (HTTP)
adk-tool = { version = "0.3.0", features = ["http-transport"] }
```

## Related Documentation

- [MCP Specification](https://modelcontextprotocol.io/)
- [rmcp Crate](https://crates.io/crates/rmcp)
- [Remote MCP Servers](https://remote.mcpservers.org/)
