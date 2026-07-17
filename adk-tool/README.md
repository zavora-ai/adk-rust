# adk-tool

Tool system for Rust Agent Development Kit (ADK-Rust) agents (FunctionTool, MCP, Google Search).

[![Crates.io](https://img.shields.io/crates/v/adk-tool.svg)](https://crates.io/crates/adk-tool)
[![Documentation](https://docs.rs/adk-tool/badge.svg)](https://docs.rs/adk-tool)
[![License](https://img.shields.io/crates/l/adk-tool.svg)](LICENSE)

## Overview

`adk-tool` provides the tool infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **FunctionTool** - Create tools from async Rust functions
- **StatefulTool\<S\>** - Wrap shared state (`Arc<S>`) with a tool handler
- **SimpleToolContext** - Lightweight `ToolContext` for non-agent callers (testing, MCP servers)
- **AgentTool** - Use agents as callable tools for composition (runs sub-agents in non-streaming mode for reliable response capture)
- **GoogleSearchTool** - Web search via Gemini's grounding
- **Provider-native wrappers** - Typed declarations for Gemini, Anthropic, and OpenAI built-in tools
- **McpToolset** - Model Context Protocol integration (local & remote servers)
- **McpServerManager** - Multi-server lifecycle management with health monitoring and auto-restart
- **BasicToolset** - Group multiple tools together
- **FilteredToolset** - Filter tools from any toolset by predicate
- **MergedToolset** - Combine multiple toolsets into one
- **PrefixedToolset** - Namespace tool names with a prefix
- **ExitLoopTool** - Control flow for loop agents
- **LoadArtifactsTool** - Inject binary artifacts into context
- **LoadMemoryTool** - Agent-callable tool for on-demand memory search (feature: `memory-tools`)
- **PreloadMemoryTool** - Auto-loads relevant memories at turn start (feature: `memory-tools`)

## Installation

```toml
[dependencies]
adk-tool = "2.0.0"

# For local MCP servers via stdio:
adk-tool = { version = "2.0.0", features = ["mcp"] }

# For remote MCP servers via HTTP:
adk-tool = { version = "2.0.0", features = ["mcp", "http-transport"] }
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "2.0.0", features = ["tools"] }
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

### Tool Metadata

Mark tools as read-only or concurrency-safe for smarter dispatch:

```rust
let lookup = FunctionTool::new("lookup", "Look up data", handler)
    .with_read_only(true)        // safe for concurrent dispatch in Auto mode
    .with_concurrency_safe(true);
```

### StatefulTool

Wrap shared state with a tool handler — the `Arc<S>` is cloned per invocation:

```rust
use adk_tool::StatefulTool;
use tokio::sync::RwLock;

struct Counter { count: RwLock<u64> }

let state = Arc::new(Counter { count: RwLock::new(0) });

let tool = StatefulTool::new("increment", "Increment counter", state, |s, _ctx, _args| async move {
    let mut count = s.count.write().await;
    *count += 1;
    Ok(json!({"count": *count}))
});
```

### SimpleToolContext

Call tools outside the agent loop (testing, MCP servers, sub-agent delegation):

```rust
use adk_tool::SimpleToolContext;

let ctx = SimpleToolContext::new("my-test-harness");
let result = my_tool.execute(Arc::new(ctx), json!({"key": "value"})).await?;
```

Defaults: `user_id()` → `"anonymous"`, `session_id()` → `""`, unique UUIDs for invocation and function call IDs.

### MCP Server Manager (Multi-Server Lifecycle)

Manage a changing registry of local MCP server processes with connection monitoring, bounded restart, configuration persistence, and tool aggregation:

```rust
use adk_tool::mcp::manager::McpServerManager;
use std::sync::Arc;
use std::time::Duration;

// Load from Kiro mcp.json format
let manager = Arc::new(McpServerManager::from_json(r#"{
    "mcpServers": {
        "workspace": {
            "command": "/opt/company/bin/workspace-mcp",
            "args": ["--stdio", "--root", "/srv/workspace"],
            "disabled": false
        }
    }
}"#)?
    .with_health_check_interval(Duration::from_secs(30))
    .with_grace_period(Duration::from_secs(5)));

// Start all non-disabled servers
let results = manager.start_all().await;

// Use as a Toolset — tools from all servers are aggregated
// Name collisions are resolved with {server_id}__{tool_name} prefixes
let agent = LlmAgentBuilder::new("agent")
    .model(model)
    .toolset(manager.clone())
    .build()?;

// Dynamic management at runtime
manager.add_server("github".into(), github_config).await?;
manager.start_server("github").await?;
manager.update_server("github", replacement_config).await?;
manager.disable_server("github").await?;
manager.enable_server("github").await?;
manager.save_json_file("mcp.json").await?;
manager.remove_server("github").await?;

// Graceful shutdown
manager.shutdown().await?;
```

Use absolute, versioned executable paths in deployment configuration. The
manager preserves `autoApprove` for configuration compatibility but does not
turn that field into authorization or human approval policy.

### MCP Tools (Local Server via stdio)

Connect to local MCP servers running as child processes:

```rust
use adk_tool::{
    McpToolset,
    mcp::rmcp::{ServiceExt, transport::TokioChildProcess},
};
use tokio::process::Command;

// Connect to a local MCP server
let cmd = Command::new("/opt/company/bin/workspace-mcp")
    .arg("--stdio")
    .arg("--root")
    .arg("/srv/workspace");

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

// Connect to a service owned by your organization or integration provider
let toolset = McpHttpClientBuilder::new("https://mcp.example.com/mcp")
    .timeout(Duration::from_secs(30))
    .connect()
    .await?;
```

### MCP Authentication

Connect to authenticated MCP servers:

```rust
use adk_tool::{McpHttpClientBuilder, McpAuth, OAuth2Config};
use std::time::Duration;

// Static bearer token supplied by your deployment identity system
let toolset = McpHttpClientBuilder::new("https://mcp.example.com/mcp")
    .with_auth(McpAuth::bearer(std::env::var("MCP_TOKEN")?))
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

Enable the negotiated MCP `2025-11-25` task lifecycle for long-running tool operations:

```rust
use adk_tool::{McpToolset, McpTaskConfig};
use std::time::Duration;

let toolset = McpToolset::new(client)
    .with_task_support(
        McpTaskConfig::enabled()
            .poll_interval(Duration::from_secs(2))
            .timeout(Duration::from_secs(300))
            .max_attempts(100)
    );
```

Task mode is used only when the server advertises task support and the selected
tool declares it. ADK-Rust sends task metadata with `tools/call`, polls
`tasks/get`, reads `tasks/result`, and requests `tasks/cancel` when the local
timeout or poll bound is reached.

### MCP Auto-Reconnect (Connection Resilience)

For one custom connection, `ConnectionRefresher` accepts a
`ConnectionFactory` that can create the same concrete `rmcp::RunningService`
again after a retryable failure. Configure bounded attempts and delay with
`RefreshConfig`. For a changing set of local stdio processes, prefer
`McpServerManager`, whose registry, monitoring, restart, and persistence model
is easier to operate.

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
| `mcp` | Local MCP clients via stdio, `McpToolset`, and `McpServerManager` |
| `http-transport` | Remote MCP servers via streamable HTTP |
| `mcp-sampling` | Deprecated upstream sampling compatibility |

## MCP examples and guides

`examples/mcp_manager` runs a real Rust stdio server locally and verifies
discovery, tool execution, dynamic registry changes, persistence, and shutdown
without a package download or network dependency. `examples/mcp_elicitation`
demonstrates a server asking its client application for additional information.

The complete official guide covers client construction, server authoring,
dynamic management, security, testing, resources, prompts, completion,
reconnect-safe subscriptions with `ResourceNotificationHandler`, elicitation,
and tasks in `docs/official_docs/mcp/`.

## Toolset Composition

Compose, filter, and namespace toolsets for complex agent configurations:

```rust
use adk_tool::{BasicToolset, FilteredToolset, MergedToolset, PrefixedToolset, string_predicate};
use std::sync::Arc;

// Group tools into named toolsets
let weather = Arc::new(BasicToolset::new("weather", vec![get_weather, get_forecast]));
let utils = Arc::new(BasicToolset::new("utils", vec![search, calculate]));

// Filter: expose only specific tools from a toolset
let filtered = FilteredToolset::new(weather.clone(), string_predicate(vec!["get_weather".into()]));

// Or use a custom predicate
let custom = FilteredToolset::with_name(
    weather.clone(),
    Box::new(|tool| tool.name().starts_with("get_")),
    "get_only",
);

// Merge: combine multiple toolsets (first-wins deduplication)
let merged = MergedToolset::new("all_tools", vec![weather.clone(), utils.clone()]);

// Prefix: namespace tool names to avoid collisions
let prefixed = PrefixedToolset::new(weather.clone(), "wx"); // wx_get_weather, wx_get_forecast

// Chain them: prefix → filter → merge
let composed = MergedToolset::new("composed", vec![
    Arc::new(PrefixedToolset::new(weather, "wx")) as Arc<dyn Toolset>,
    Arc::new(FilteredToolset::new(utils, string_predicate(vec!["search".into()]))),
]);

// Register with an agent
let agent = LlmAgentBuilder::new("agent")
    .model(model)
    .toolset(Arc::new(composed))
    .build()?;
```

All composition utilities implement `Toolset` and work with any `Toolset` implementation including `McpToolset` and `BrowserToolset`.

## rmcp compatibility

ADK-Rust 2 uses `rmcp 2.2`, the official Rust SDK aligned with MCP
`2025-11-25`. `McpToolset::new(client)` remains the primary adapter. Advanced
server authoring, transports, protocol extensions, and SDK types are available
through `adk_tool::mcp::rmcp`, keeping them on the same version used internally.

Sampling, roots, and logging are deprecated upstream by SEP-2577. The
`mcp-sampling` feature exists for compatible deployments and should not be the
default design for a new system.

When migrating code that imports `rmcp` types directly, align it to `rmcp 2.2`
or import the SDK through `adk_tool::mcp::rmcp`. MCP 2.2 renamed several public
content and elicitation types, so downstream type annotations may require
updates even when `McpToolset::new(client)` itself is unchanged.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Tool` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agents that use tools

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
