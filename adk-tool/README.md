# adk-tool

Tool system for Rust Agent Development Kit (ADK-Rust) agents (FunctionTool, MCP, Google Search).

[![Crates.io](https://img.shields.io/crates/v/adk-tool.svg)](https://crates.io/crates/adk-tool)
[![Documentation](https://docs.rs/adk-tool/badge.svg)](https://docs.rs/adk-tool)
[![License](https://img.shields.io/crates/l/adk-tool.svg)](LICENSE)

## Overview

`adk-tool` provides the tool infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **FunctionTool** - Create tools from async Rust functions
- **AgentTool** - Use agents as callable tools for composition
- **GoogleSearchTool** - Web search via Gemini's grounding
- **McpToolset** - Model Context Protocol integration (local & remote servers)
- **BasicToolset** - Group multiple tools together
- **ExitLoopTool** - Control flow for loop agents
- **LoadArtifactsTool** - Inject binary artifacts into context

## Installation

```toml
[dependencies]
adk-tool = "0.3.0"

# For remote MCP servers via HTTP:
adk-tool = { version = "0.3.0", features = ["http-transport"] }
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.0", features = ["tools"] }
```

## Quick Start

### Function Tool

```rust
use adk_tool::FunctionTool;
use adk_core::{ToolContext, Result};
use serde_json::{json, Value};
use std::sync::Arc;

async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    let city = args["city"].as_str().unwrap_or("Unknown");
    Ok(json!({
        "city": city,
        "temperature": 72,
        "condition": "sunny"
    }))
}

let tool = FunctionTool::new(
    "get_weather",
    "Get current weather for a city",
    get_weather,
);
```

### With Parameter Schema (Recommended)

Always add a schema so the LLM knows what parameters to pass:

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Serialize, Deserialize)]
struct WeatherParams {
    /// The city to get weather for
    city: String,
}

let tool = FunctionTool::new("get_weather", "Get weather", get_weather)
    .with_parameters_schema::<WeatherParams>();
```

### MCP Tools (Local Server via stdio)

Connect to local MCP servers running as child processes:

```rust
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;

// Connect to a local MCP server
let cmd = Command::new("npx")
    .arg("-y")
    .arg("@modelcontextprotocol/server-filesystem")
    .arg("/path/to/files");

let client = ().serve(TokioChildProcess::new(cmd)?).await?;

let toolset = McpToolset::new(client)
    .with_name("filesystem-tools")
    .with_filter(|name| matches!(name, "read_file" | "write_file"));

// Get cancellation token for graceful shutdown
let cancel_token = toolset.cancellation_token().await;

// ... use toolset with agent ...

// Cleanup before exit
cancel_token.cancel();
```

### MCP Tools (Remote Server via HTTP)

Connect to remote MCP servers using HTTP transport (requires `http-transport` feature):

```rust
use adk_tool::McpHttpClientBuilder;
use std::time::Duration;

// Connect to a public remote MCP server
let toolset = McpHttpClientBuilder::new("https://remote.mcpservers.org/fetch/mcp")
    .timeout(Duration::from_secs(30))
    .connect()
    .await?;
```

### MCP Authentication

Connect to authenticated MCP servers:

```rust
use adk_tool::{McpHttpClientBuilder, McpAuth, OAuth2Config};
use std::time::Duration;

// Bearer token (e.g., GitHub Copilot MCP)
let toolset = McpHttpClientBuilder::new("https://api.githubcopilot.com/mcp/")
    .with_auth(McpAuth::bearer(std::env::var("GITHUB_TOKEN")?))
    .timeout(Duration::from_secs(60))
    .connect()
    .await?;

// API key in custom header
let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
    .with_auth(McpAuth::api_key("X-API-Key", "your-api-key"))
    .connect()
    .await?;

// OAuth2 client credentials flow
let oauth_config = OAuth2Config::new(
    "your-client-id",
    "https://auth.example.com/oauth/token"
)
.with_secret("your-client-secret")
.with_scopes(vec!["mcp:read".into(), "mcp:write".into()]);

let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
    .with_auth(McpAuth::oauth2(oauth_config))
    .connect()
    .await?;
```

### MCP Task Support (Long-Running Operations)

Enable async task lifecycle for long-running MCP operations (SEP-1686):

```rust
use adk_tool::{McpToolset, McpTaskConfig};
use std::time::Duration;

let toolset = McpToolset::new(client)
    .with_task_support(
        McpTaskConfig::enabled()
            .poll_interval(Duration::from_secs(2))
            .timeout(Duration::from_secs(300))
            .max_poll_attempts(100)
    );
```

### MCP Auto-Reconnect (Connection Resilience)

For long-running agents, use `ConnectionRefresher` to automatically reconnect when connections fail:

```rust
use adk_tool::mcp::{ConnectionRefresher, ConnectionFactory, RefreshConfig};
use rmcp::{RoleClient, ServiceExt, service::RunningService, transport::TokioChildProcess};
use std::sync::Arc;
use tokio::process::Command;

// Define a factory that can create new connections
struct MyConnectionFactory {
    command: String,
    args: Vec<String>,
}

#[async_trait::async_trait]
impl<S> ConnectionFactory<S> for MyConnectionFactory
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    async fn create_connection(&self) -> Result<RunningService<RoleClient, S>, String> {
        let cmd = Command::new(&self.command)
            .args(&self.args)
            .spawn()
            .map_err(|e| e.to_string())?;
        
        ().serve(TokioChildProcess::new(cmd).map_err(|e| e.to_string())?)
            .await
            .map_err(|e| e.to_string())
    }
}

// Create initial connection
let cmd = Command::new("npx")
    .arg("-y")
    .arg("@modelcontextprotocol/server-filesystem")
    .arg("/path/to/files");
let client = ().serve(TokioChildProcess::new(cmd)?).await?;

// Wrap with auto-reconnect
let factory = Arc::new(MyConnectionFactory {
    command: "npx".to_string(),
    args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into(), "/path".into()],
});

let refresher = ConnectionRefresher::new(client, factory)
    .with_config(RefreshConfig::default()
        .with_max_attempts(5)
        .with_retry_delay_ms(2000));

// Operations automatically retry on connection failure
let tools = refresher.list_tools().await?;
if tools.reconnected {
    println!("Connection was refreshed during operation");
}
```

The refresher handles these error conditions automatically:
- Connection closed / EOF
- Broken pipe / transport errors
- Session not found (server restart)
- Connection reset

### Google Search

```rust
use adk_tool::GoogleSearchTool;

let search = GoogleSearchTool::new();
// Add to agent - enables grounded web search
```

## Features

| Feature | Description |
|---------|-------------|
| (default) | Local MCP servers via stdio transport |
| `http-transport` | Remote MCP servers via streamable HTTP |

## MCP Server Examples

### Available Public MCP Servers

- `https://remote.mcpservers.org/fetch/mcp` - Web content fetching
- `https://remote.mcpservers.org/sequentialthinking/mcp` - Step-by-step reasoning

### GitHub Copilot MCP (40+ tools)

```rust
// Requires GITHUB_TOKEN with Copilot access
let toolset = McpHttpClientBuilder::new("https://api.githubcopilot.com/mcp/")
    .with_auth(McpAuth::bearer(std::env::var("GITHUB_TOKEN")?))
    .connect()
    .await?;

// Discovered tools include:
// - search_repositories, search_code, search_issues
// - create_pull_request, merge_pull_request
// - get_file_contents, create_or_update_file
// - issue_read, issue_write, add_issue_comment
// - and 30+ more GitHub operations
```

## Migration from rmcp 0.9

**No changes required!** The rmcp 0.14 breaking changes were handled internally:

| What Changed | Impact |
|--------------|--------|
| `CallToolRequestParam` → `CallToolRequestParams` | Internal only |
| Added `meta: None` field | Internal only |
| HTTP transport API | Internal only |

Your existing code using `McpToolset::new(client)` continues to work unchanged.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Tool` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agents that use tools

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
