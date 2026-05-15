# adk-acp

[![crates.io](https://img.shields.io/crates/v/adk-acp.svg)](https://crates.io/crates/adk-acp)
[![docs.rs](https://docs.rs/adk-acp/badge.svg)](https://docs.rs/adk-acp)

Agent Client Protocol (ACP) integration for ADK-Rust. Connect your ADK agents to external ACP agents like Claude Code, Codex, and other ACP-compatible coding agents.

## What is ACP?

The [Agent Client Protocol](https://agentclientprotocol.com/) standardizes communication between code editors and coding agents. This crate lets ADK agents delegate tasks to ACP agents and optionally expose themselves as ACP-compatible agents.

## Installation

```toml
[dependencies]
adk-acp = "0.8.2"

# Or via the umbrella crate:
adk-rust = { version = "0.8.2", features = ["acp"] }
```

## Quick Start

```rust,ignore
use adk_acp::AcpAgentTool;
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

// Wrap an ACP agent as a tool
let claude = AcpAgentTool::new("claude-code")
    .description("Delegate complex coding tasks to Claude Code")
    .working_dir("/path/to/project");

let agent = LlmAgentBuilder::new("orchestrator")
    .instruction("Use claude-code for complex refactoring. Use codex for quick edits.")
    .model(model)
    .tool(Arc::new(claude))
    .build()?;
```

## Multiple Agents

```rust,ignore
use adk_acp::{AcpToolset, AcpAgentTool};

let toolset = AcpToolset::new("coding-agents")
    .with_agent(AcpAgentTool::new("claude-code").description("Complex refactoring"))
    .with_agent(AcpAgentTool::new("codex --model o3").description("Quick code generation"));

let agent = LlmAgentBuilder::new("orchestrator")
    .toolset(Arc::new(toolset))
    .build()?;
```

## How It Works

1. When the ADK agent calls the ACP tool, it spawns the ACP agent process via stdio
2. Performs the ACP initialization handshake (protocol version negotiation)
3. Creates a session with the working directory context
4. Sends the prompt and collects the streaming response
5. Returns the response as the tool output

## Features

| Feature | Description |
|---------|-------------|
| (default) | Client-side: connect to ACP agents as tools |
| `server` | Server-side: expose ADK agents as ACP-compatible agents |

## Supported ACP Agents

Any agent implementing the ACP protocol works, including:

- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) (`claude-code`)
- [Codex](https://github.com/openai/codex) (`codex`)
- Custom agents built with the [ACP SDK](https://github.com/agentclientprotocol/rust-sdk)

## License

Apache-2.0
