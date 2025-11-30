# MCP Tools

> **Status**: ✅ Implemented
> **Completed**: 2025

## Overview

Model Context Protocol (MCP) is an open standard that standardizes how Large Language Models communicate with external applications, data sources, and tools. MCP follows a client-server architecture where:

- **MCP Servers** expose resources, prompts, and tools
- **MCP Clients** (like ADK agents) consume these capabilities

ADK-Rust will support MCP integration through the `McpToolset` class, allowing agents to discover and use tools from any MCP-compliant server.

## What is MCP?

The Model Context Protocol provides:

- **Universal Connection**: A standard way for LLMs to interact with external systems
- **Tool Discovery**: Automatic discovery of available tools from MCP servers
- **Stateful Sessions**: Persistent connections between clients and servers
- **Protocol Flexibility**: Support for both local (stdio) and remote (SSE/HTTP) servers

## Planned Architecture

### McpToolset Integration

The `McpToolset` will act as a bridge between ADK agents and MCP servers:

```rust,ignore
use adk_tool::McpToolset;
use adk_agent::LlmAgentBuilder;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;

// Create MCP client connection
let peer = ().serve(TokioChildProcess::new(
    Command::new("npx")
        .arg("-y")
        .arg("@modelcontextprotocol/server-everything")
)?).await?;

// Create toolset from MCP server
let mcp_toolset = McpToolset::new(peer);

// Add to agent
let agent = LlmAgentBuilder::new("assistant")
    .model("gemini-2.0-flash-exp")
    .instruction("Help the user with various tasks using available tools")
    .toolset(Arc::new(mcp_toolset))
    .build()?;
```

### Connection Types

**Stdio Connection** (Local Process):
```rust,ignore
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;

// Start local MCP server as subprocess
let peer = ().serve(TokioChildProcess::new(
    Command::new("npx")
        .arg("-y")
        .arg("@modelcontextprotocol/server-filesystem")
        .arg("/path/to/directory")
)?).await?;

let toolset = McpToolset::new(peer);
```

**Remote Connection** (HTTP/SSE):
```rust,ignore
// Connect to remote MCP server
// Implementation details pending rmcp SDK API
let peer = connect_to_remote_server("https://mcp-server.example.com").await?;
let toolset = McpToolset::new(peer);
```

### Tool Discovery and Execution

The `McpToolset` will:

1. **Connect** to the MCP server on initialization
2. **Discover** available tools via `list_tools` MCP method
3. **Convert** MCP tool schemas to ADK-compatible `Tool` instances
4. **Proxy** tool calls from the agent to the MCP server
5. **Handle** responses and errors appropriately

### Tool Filtering

Filter which tools are exposed to your agent:

```rust,ignore
let toolset = McpToolset::new(peer)
    .with_filter(|tool_name| {
        // Only expose specific tools
        matches!(tool_name, "read_file" | "list_directory" | "search_files")
    });
```

## Implementation Status

### Current State - Fully Implemented ✅

The `McpToolset` in `adk-tool/src/mcp/toolset.rs` provides complete MCP integration:

```rust,ignore
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;
use adk_tool::McpToolset;

// Create MCP client connection
let client = ().serve(TokioChildProcess::new(
    Command::new("npx")
        .arg("-y")
        .arg("@modelcontextprotocol/server-everything")
)?).await?;

// Create toolset with filtering
let toolset = McpToolset::new(client)
    .with_name("my-mcp-tools")
    .with_filter(|name| matches!(name, "echo" | "add"));

// Or filter by specific tool names
let toolset = McpToolset::new(client)
    .with_tools(&["read_file", "write_file"]);
```

### What's Implemented

- ✅ `rmcp` SDK integration with `RunningService<RoleClient, S>`
- ✅ `McpToolset` struct implementing `Toolset` trait
- ✅ `McpTool` wrapper implementing ADK `Tool` trait
- ✅ Tool discovery via `list_all_tools()` with pagination
- ✅ Tool execution via `call_tool()` proxying
- ✅ Tool filtering with `with_filter()` and `with_tools()`
- ✅ Error handling for MCP responses
- ✅ Support for text, image, resource, and audio content
- ✅ Structured content handling
- ✅ Example code in `examples/mcp/main.rs`

### Future Enhancements

- [ ] Convenience constructors for common transports (stdio, SSE)
- [ ] Connection retry logic
- [ ] MCP resource access integration
- [ ] MCP prompt integration

## Use Cases

MCP integration enables:

### File System Operations
```rust,ignore
// Access file system through MCP
let fs_toolset = McpToolset::from_stdio(
    "npx", 
    &["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
).await?;
```

### External APIs
```rust,ignore
// Use Google Maps through MCP
let maps_toolset = McpToolset::from_stdio_with_env(
    "npx",
    &["-y", "@modelcontextprotocol/server-google-maps"],
    &[("GOOGLE_MAPS_API_KEY", api_key)]
).await?;
```

### Database Access
```rust,ignore
// Query databases through MCP
let db_toolset = McpToolset::from_remote(
    "https://mcp-db-server.example.com/sse"
).await?;
```

### Custom Tools
```rust,ignore
// Expose your own tools via MCP server
// Then connect from any MCP client
```

## Comparison with adk-go

ADK-Go has full MCP support with:
- `McpToolset` class for client integration
- Support for stdio and SSE connections
- Tool filtering capabilities
- Comprehensive examples (filesystem, Google Maps)
- Deployment patterns for Cloud Run, GKE, Vertex AI

ADK-Rust will achieve feature parity with these capabilities.

## Related Resources

- [MCP Specification](https://spec.modelcontextprotocol.io/)
- [rmcp Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [MCP Server Registry](https://github.com/modelcontextprotocol/servers)
- [ADK-Go MCP Documentation](../adk-go-docs/tools/mcp-tools.md)

## Implementation Complete

MCP integration has been fully implemented following the design patterns established in ADK-Go while leveraging Rust's type safety and async capabilities.

Completed milestones:
1. ✅ Complete `rmcp` SDK integration
2. ✅ Implement tool discovery and execution
3. ✅ Add connection management (via rmcp RunningService)
4. ✅ Create working example (`examples/mcp/main.rs`)
5. ✅ Comprehensive documentation
6. Ready for testing with popular MCP servers

## Contributing

If you're interested in contributing to MCP support in ADK-Rust, please:

1. Review the existing code in `adk-tool/src/mcp/`
2. Familiarize yourself with the `rmcp` SDK
3. Check the ADK-Go implementation for reference
4. Open an issue to discuss your approach

---

**Note**: This is a roadmap document. The APIs and examples shown here are illustrative and subject to change during implementation.
