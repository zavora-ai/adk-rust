# ACP Tools (Agent Client Protocol)

The `adk-acp` crate implements the Agent Client Protocol, enabling ADK-Rust agents to connect to external ACP-compatible agents (Claude Code, Codex, Kiro CLI) as tools, and to expose ADK-Rust agents as ACP-compatible servers for consumption by other clients.

## Overview

ACP is a protocol for agent-to-agent communication over stdio or network transports. It allows:

- **Consuming** remote agents as tools inside your ADK agent
- **Exposing** your ADK agent as a tool for external ACP clients
- **IDE integration** via stdio transport (Kiro CLI, Claude Code, Codex)

## Installation

```toml
[dependencies]
adk-acp = "1.0.0"

# Or via the umbrella crate
adk-rust = { version = "1.0.0", features = ["acp"] }
```

## AcpAgentTool — Remote Agent as a Tool

Wrap any ACP-compatible remote agent as a tool your agent can call:

```rust
use adk_acp::{AcpAgentTool, AcpTransport, StdioTransport};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Connect to a remote ACP agent via stdio
    let transport = StdioTransport::spawn("claude-code", &["--acp"])?;

    // Wrap as a tool
    let code_agent = AcpAgentTool::new(transport)
        .with_name("code_assistant")
        .with_description("A coding assistant that can write and edit code")
        .with_auto_approve(&["read_file", "write_file"]);

    // Use in your agent
    let agent = LlmAgentBuilder::new("orchestrator")
        .model(model)
        .instruction("Use code_assistant for coding tasks.")
        .tool(Arc::new(code_agent))
        .build()?;

    Ok(())
}
```

### How It Works

1. Your agent decides to call the `code_assistant` tool
2. `AcpAgentTool` serializes the request and sends it over the ACP transport
3. The remote agent processes the request (potentially making its own tool calls)
4. The response is returned to your agent as a tool result

### Configuration

```rust
let tool = AcpAgentTool::new(transport)
    .with_name("assistant")                    // Tool name visible to LLM
    .with_description("Does X")               // Description for LLM routing
    .with_auto_approve(&["safe_tool"])         // Auto-approve these tool calls
    .with_timeout(Duration::from_secs(120))   // Request timeout
    .with_max_turns(10);                       // Max conversation turns
```

## AcpToolset — Multiple Agent Connections

Manage connections to multiple ACP agents simultaneously:

```rust
use adk_acp::{AcpToolset, AcpAgentConfig};

let toolset = AcpToolset::new()
    .add_agent(AcpAgentConfig {
        name: "coder".into(),
        command: "claude-code".into(),
        args: vec!["--acp".into()],
        description: "Writes and edits code".into(),
        auto_approve: vec!["read_file".into()],
    })
    .add_agent(AcpAgentConfig {
        name: "researcher".into(),
        command: "research-agent".into(),
        args: vec![],
        description: "Searches documentation and web".into(),
        auto_approve: vec![],
    });

// Start all connections
toolset.connect_all().await?;

// Use as a toolset with an agent
let agent = LlmAgentBuilder::new("coordinator")
    .model(model)
    .toolset(Arc::new(toolset))
    .build()?;

// Cleanup
toolset.disconnect_all().await?;
```

## AcpServer — Expose ADK Agents as ACP

Make your ADK-Rust agent available to external ACP clients:

```rust
use adk_acp::{AcpServer, AcpServerConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

// Build your agent
let agent = LlmAgentBuilder::new("my_agent")
    .model(model)
    .instruction("You are a helpful assistant.")
    .tool(Arc::new(my_tool))
    .build()?;

// Expose over stdio (for IDE integration)
let server = AcpServer::new(Arc::new(agent))
    .with_config(AcpServerConfig {
        name: "my-agent".into(),
        description: "A helpful assistant with domain tools".into(),
        version: "1.0.0".into(),
    });

// Blocks until the client disconnects
server.serve_stdio().await?;
```

This enables your agent to be used from Kiro CLI, Claude Code, or any ACP-compatible client.

## StdioTransport — IDE Connections

The `StdioTransport` spawns a child process and communicates over stdin/stdout using the ACP protocol:

```rust
use adk_acp::StdioTransport;

// Spawn and connect to an external agent
let transport = StdioTransport::spawn("kiro", &["--agent", "my-agent"])?;

// Or connect to an already-running process
let transport = StdioTransport::from_child(child_process);
```

### Supported Clients

| Client | Command | Notes |
|--------|---------|-------|
| Kiro CLI | `kiro --acp` | Full ACP support |
| Claude Code | `claude-code --acp` | Code-focused capabilities |
| Codex | `codex --acp` | OpenAI Codex agent |
| Custom | Any binary | Must implement ACP protocol |

## Auto-Approve Permissions

By default, tool calls made by remote agents require confirmation. Use `auto_approve` to skip confirmation for trusted operations:

```rust
let tool = AcpAgentTool::new(transport)
    .with_auto_approve(&[
        "read_file",      // Safe read operations
        "list_directory", // Directory listing
        "search",         // Search operations
    ]);
```

Tool calls not in the auto-approve list will be declined unless a custom approval handler is configured:

```rust
let tool = AcpAgentTool::new(transport)
    .with_approval_handler(|tool_name, args| {
        // Custom logic to approve/deny
        Ok(tool_name.starts_with("safe_"))
    });
```

## Error Handling

```rust
use adk_acp::AcpError;

match tool.execute(ctx, args).await {
    Ok(result) => println!("Success: {result}"),
    Err(e) => match e {
        AcpError::ConnectionFailed(msg) => eprintln!("Connection lost: {msg}"),
        AcpError::Timeout => eprintln!("Request timed out"),
        AcpError::ProtocolError(msg) => eprintln!("Protocol error: {msg}"),
        AcpError::AgentError(msg) => eprintln!("Remote agent error: {msg}"),
        _ => eprintln!("ACP error: {e}"),
    }
}
```

## Related

- [MCP Tools](mcp-tools.md) — MCP protocol for tool servers (complementary to ACP)
- [Multi-Agent Systems](../agents/multi-agent.md) — In-process agent composition
- [A2A Protocol](../deployment/a2a.md) — HTTP-based agent-to-agent communication

---

**Previous**: [← Benchmarking](benchmarking.md) | **Next**: [Retry & Reflect →](retry-reflect.md)
