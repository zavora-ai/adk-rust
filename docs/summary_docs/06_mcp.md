# MCP Integration Guide

Model Context Protocol (MCP) allows ADK-Rust agents to connect to external MCP servers and use their tools dynamically.

## What is MCP?

Model Context Protocol is an open protocol that standardizes how applications provide context to Large Language Models (LLMs). MCP servers expose:

- **Tools**: Functions that agents can call
- **Resources**: Data that agents can access
- **Prompts**: Templates that agents can use

## Quick Start

### 1. Add MCP Toolset

```rust
use adk_tool::McpToolset;
use adk_agent::LlmAgentBuilder;

// Create MCP toolset
let mcp_toolset = McpToolset::new("filesystem", mcp_config)?;

// Add to agent
let agent = LlmAgentBuilder::new("assistant")
    .model(Arc::new(model))
    .toolset(Arc::new(mcp_toolset))
    .build()?;
```

### 2. Configure MCP Server

MCP servers can be:
- **Stdio**: Communicate via stdin/stdout
- **HTTP**: REST API endpoints
- **SSE**: Server-sent events

Example configuration for stdio server:

```rust
use adk_tool::mcp::McpConfig;

let config = McpConfig {
    command: "npx".to_string(),
    args: vec!["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"],
    env: HashMap::new(),
};
```

## Built-in MCP Servers

### Filesystem Server

Access local files:

```rust
let fs_config = McpConfig::stdio(
    "npx",
    vec!["-y", "@modelcontextprotocol/server-filesystem", "/data"],
);

let mcp = McpToolset::new("filesystem", fs_config)?;
```

**Tools provided**:
- `read_file`: Read file contents
- `write_file`: Write to file
- `list_directory`: List directory contents
- `search_files`: Search for files

### Database Server

Query databases:

```rust
let db_config = McpConfig::stdio(
    "npx",
    vec!["-y", "@modelcontextprotocol/server-postgres", "postgresql://..."],
);

let mcp = McpToolset::new("database", db_config)?;
```

**Tools provided**:
- `query`: Execute SQL queries
- `list_tables`: List database tables
- `describe_table`: Get table schema

### Web Server

Fetch web content:

```rust
let web_config = McpConfig::stdio(
    "npx",
    vec!["-y", "@modelcontextprotocol/server-fetch"],
);

let mcp = McpToolset::new("web", web_config)?;
```

**Tools provided**:
- `fetch`: Fetch URL content
- `fetch_html`: Extract HTML content

## Custom MCP Servers

### Creating a Custom Server

Implement an MCP server in any language. Here's a simple Python example:

```python
#!/usr/bin/env python3
import json
import sys

def handle_initialize(params):
    return {
        "protocolVersion": "1.0",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "my-custom-server",
            "version": "1.0.0"
        }
    }

def handle_list_tools(params):
    return {
        "tools": [
            {
                "name": "greet",
                "description": "Greet a person",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"}
                    },
                    "required": ["name"]
                }
            }
        ]
    }

def handle_call_tool(params):
    name = params["name"]
    args = params.get("arguments", {})
    
    if name == "greet":
        person = args.get("name", "World")
        return {
            "content": [
                {
                    "type": "text",
                    "text": f"Hello, {person}!"
                }
            ]
        }

# Main message loop
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    params = request.get("params", {})
    
    if method == "initialize":
        result = handle_initialize(params)
    elif method == "tools/list":
        result = handle_list_tools(params)
    elif method == "tools/call":
        result = handle_call_tool(params)
    else:
        result = {"error": "Unknown method"}
    
    response = {
        "jsonrpc": "2.0",
        "id": request.get("id"),
        "result": result
    }
    print(json.dumps(response))
    sys.stdout.flush()
```

### Using Custom Server

```rust
let custom_config = McpConfig::stdio(
    "python3",
    vec!["./my_custom_server.py"],
);

let mcp = McpToolset::new("custom", custom_config)?;
```

## Advanced Usage

### Multiple MCP Servers

Use multiple MCP toolsets in one agent:

```rust
let fs_mcp = McpToolset::new("filesystem", fs_config)?;
let db_mcp = McpToolset::new("database", db_config)?;
let web_mcp = McpToolset::new("web", web_config)?;

let agent = LlmAgentBuilder::new("super-agent")
    .model(Arc::new(model))
    .toolset(Arc::new(fs_mcp))
    .toolset(Arc::new(db_mcp))
    .toolset(Arc::new(web_mcp))
    .build()?;
```

### Tool Filtering

Only expose certain MCP tools to agents:

```rust
use adk_tool::string_predicate;

let mcp = McpToolset::new("filesystem", config)?
    .with_predicate(string_predicate(vec!["read_file", "list_directory"]));
```

### Error Handling

Handle MCP server failures gracefully:

```rust
match McpToolset::new("filesystem", config) {
    Ok(mcp) => {
        // Use MCP toolset
    }
    Err(e) => {
        eprintln!("MCP server failed to start: {}", e);
        // Fallback to non-MCP tools
    }
}
```

## MCP Protocol Details

### Message Format

MCP uses JSON-RPC 2.0 over stdio:

**Request**:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/list",
  "params": {}
}
```

**Response**:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "tools": [...]
  }
}
```

### Methods

#### initialize

Handshake and capability negotiation:

```json
{
  "method": "initialize",
  "params": {
    "protocolVersion": "1.0",
    "capabilities": {}
  }
}
```

#### tools/list

List available tools:

```json
{
  "method": "tools/list",
  "params": {}
}
```

#### tools/call

Execute a tool:

```json
{
  "method": "tools/call",
  "params": {
    "name": "read_file",
    "arguments": {
      "path": "/path/to/file.txt"
    }
  }
}
```

## Best Practices

### 1. Server Lifecycle Management

MCP servers are processes that need management:

```rust
// McpToolset automatically manages the subprocess
let mcp = McpToolset::new("fs", config)?;

// Server is started automatically
// Server is stopped when mcp is dropped
```

### 2. Timeouts

Set reasonable timeouts for MCP calls:

```rust
let config = McpConfig::stdio("npx", args)
    .with_timeout(Duration::from_secs(30));
```

### 3. Resource Limits

Limit what MCP servers can access:

```rust
// Restrict filesystem access
let config = McpConfig::stdio(
    "npx",
    vec!["-y", "@modelcontextprotocol/server-filesystem", "/safe/dir"],
);
```

### 4. Testing

Test MCP integration separately:

```rust
#[tokio::test]
async fn test_mcp_filesystem() {
    let config = McpConfig::stdio(...);
    let mcp = McpToolset::new("test", config).unwrap();
    
    // Verify tools are available
    let tools = mcp.tools();
    assert!(!tools.is_empty());
}
```

## Troubleshooting

### Server Won't Start

Check:
- Command is in PATH
- Arguments are correct
- Dependencies are installed

```bash
# Test manually
npx -y @modelcontextprotocol/server-filesystem /tmp
```

### Tool Not Found

Ensure the MCP server exposes the tool:

```rust
// List available tools
let tools = mcp_toolset.tools();
for tool in tools {
    println!("Tool: {}", tool.name());
}
```

### Timeout Errors

Increase timeout or optimize server:

```rust
let config = McpConfig::stdio("command", args)
    .with_timeout(Duration::from_secs(60));  // Longer timeout
```

## Example: Full MCP Integration

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use adk_tool::mcp::{McpConfig, McpToolset};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Configure MCP servers
    let fs_config = McpConfig::stdio(
        "npx",
        vec!["-y", "@modelcontextprotocol/server-filesystem", "/workspace"],
    );
    
    let db_config = McpConfig::stdio(
        "npx",
        vec!["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/mydb"],
    );
    
    // 2. Create MCP toolsets
    let fs_mcp = Arc::new(McpToolset::new("filesystem", fs_config)?);
    let db_mcp = Arc::new(McpToolset::new("database", db_config)?);
    
    // 3. Create model
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);
    
    // 4. Build agent with MCP tools
    let agent = LlmAgentBuilder::new("mcp-agent")
        .description("Agent with filesystem and database access")
        .model(model)
        .toolset(fs_mcp)
        .toolset(db_mcp)
        .build()?;
    
    // 5. Create runner and execute
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::new("mcp-app", Arc::new(agent), session_service);
    
    let query = Content::text("Read the README.md file and summarize it");
    let mut events = runner.run("user1".into(), "session1".into(), query).await?;
    
    use futures::StreamExt;
    while let Some(event) = events.next().await {
        let evt = event?;
        if let Some(content) = evt.content {
            println!("{:?}", content);
        }
    }
    
    Ok(())
}
```

## Additional Resources

- [MCP Specification](https://modelcontextprotocol.io/)
- [Official MCP Servers](https://github.com/modelcontextprotocol/servers)
- [MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [ADK-Rust MCP Example](../examples/mcp/)

---

**Previous**: [Workflow Patterns](07_workflows.md) | **Next**: [CLI Usage](09_cli.md)
