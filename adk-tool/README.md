# adk-tool

Tool system for ADK agents (FunctionTool, MCP, Google Search).

[![Crates.io](https://img.shields.io/crates/v/adk-tool.svg)](https://crates.io/crates/adk-tool)
[![Documentation](https://docs.rs/adk-tool/badge.svg)](https://docs.rs/adk-tool)
[![License](https://img.shields.io/crates/l/adk-tool.svg)](LICENSE)

## Overview

`adk-tool` provides the tool infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **FunctionTool** - Create tools from async Rust functions
- **GoogleSearchTool** - Web search via Gemini's grounding
- **McpToolset** - Model Context Protocol integration
- **BasicToolset** - Group multiple tools together
- **ExitLoopTool** - Control flow for loop agents
- **LoadArtifactsTool** - Inject binary artifacts into context

## Installation

```toml
[dependencies]
adk-tool = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["tools"] }
```

## Quick Start

### Function Tool

```rust
use adk_tool::FunctionTool;
use serde_json::{json, Value};

async fn get_weather(ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
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

### MCP Tools

```rust
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};

let client = ().serve(TokioChildProcess::new(
    Command::new("npx")
        .arg("-y")
        .arg("@modelcontextprotocol/server-filesystem")
        .arg("/path/to/files")
)?).await?;

let toolset = McpToolset::new(client)
    .with_filter(|name| matches!(name, "read_file" | "write_file"));
```

### Google Search

```rust
use adk_tool::GoogleSearchTool;

let search = GoogleSearchTool::new();
// Add to agent - enables grounded web search
```

## Features

- Type-safe parameter schemas with `schemars`
- Long-running tool support with progress tracking
- MCP protocol for external tool integration
- Tool filtering and composition

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Tool` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agents that use tools

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
