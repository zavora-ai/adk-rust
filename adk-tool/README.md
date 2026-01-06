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
- **McpToolset** - Model Context Protocol integration
- **BasicToolset** - Group multiple tools together
- **ExitLoopTool** - Control flow for loop agents
- **LoadArtifactsTool** - Inject binary artifacts into context

## Installation

```toml
[dependencies]
adk-tool = "{{version}}"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "{{version}}", features = ["tools"] }
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

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
