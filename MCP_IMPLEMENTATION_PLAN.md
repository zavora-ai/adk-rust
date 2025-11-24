# MCP Implementation Plan

## Analysis of Go Implementation

### Key Components

**1. McpToolset (set.go)**
- Holds MCP client, transport, and optional tool filter
- Lazy session initialization (created on first use)
- `Tools()` method: Lists tools from MCP server, converts to ADK tools, applies filter
- Pagination support via cursor

**2. McpTool (tool.go)**
- Wraps individual MCP tool
- Stores name, description, function declaration (schema)
- `Run()` method: Calls MCP server via session.CallTool()
- Handles both structured and text responses
- Error handling for tool execution failures

**3. Key Patterns**
- **Lazy initialization**: Session created on first tool request
- **Mutex protection**: Thread-safe session access
- **Tool conversion**: MCP tool → ADK tool with schema preservation
- **Response handling**: Structured content or text concatenation

## Rust Implementation Plan

### Phase 1: Core Structure (~30 min)

**File**: `adk-tool/src/mcp/toolset.rs`

```rust
pub struct McpToolset {
    transport: Arc<dyn Transport>,
    client: Arc<Mutex<Option<Client>>>,
    tool_filter: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
}

impl McpToolset {
    pub fn new(transport: impl Transport + 'static) -> Self
    pub fn with_filter<F>(self, filter: F) -> Self
    async fn get_client(&self) -> Result<Client>
}
```

### Phase 2: Toolset Implementation (~45 min)

**Implement Toolset trait**:
- `name()` → "mcp_toolset"
- `tools()` → List tools from MCP server with pagination
- Convert MCP tools to McpTool instances
- Apply optional filter

### Phase 3: Tool Wrapper (~30 min)

**File**: `adk-tool/src/mcp/tool.rs`

```rust
struct McpTool {
    name: String,
    description: String,
    client: Arc<Mutex<Option<Client>>>,
}

impl Tool for McpTool {
    async fn execute() -> Result<Value>
    // Call client.call_tool()
    // Handle structured vs text response
    // Error handling
}
```

### Phase 4: Example (~30 min)

**File**: `examples/mcp_example.rs`

```rust
// Connect to MCP server (e.g., npx @modelcontextprotocol/server-everything)
// Create McpToolset with transport
// Add to LlmAgent
// Demonstrate tool usage
```

## Implementation Details

### Transport Setup
```rust
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;

let transport = TokioChildProcess::new(
    Command::new("npx")
        .arg("-y")
        .arg("@modelcontextprotocol/server-everything")
)?;
```

### Client Creation
```rust
use rmcp::ServiceExt;

let client = ().serve(transport).await?;
```

### Tool Listing
```rust
let tools = client.list_all_tools().await?;
// Handles pagination automatically
```

### Tool Execution
```rust
use rmcp::model::CallToolRequestParam;

let result = client.call_tool(CallToolRequestParam {
    name: tool_name,
    arguments: Some(args),
}).await?;
```

### Response Handling
```rust
// Check result.is_error
// Extract from result.content (Vec<Content>)
// Handle TextContent vs other types
```

## Key Differences from Go

1. **Async/Await**: Rust uses async/await vs Go's goroutines
2. **Mutex**: `tokio::sync::Mutex` for async-safe locking
3. **Arc**: Shared ownership for client across tools
4. **Result**: Rust Result type vs Go error returns
5. **Traits**: Implement Tool and Toolset traits

## Dependencies

Already added:
```toml
rmcp = { version = "0.9", features = ["client"] }
```

## Testing Strategy

1. **Unit tests**: Mock MCP client responses
2. **Integration test**: Use rmcp test server
3. **Example**: Real MCP server (npx server)

## Estimated Time

- Phase 1: 30 min
- Phase 2: 45 min  
- Phase 3: 30 min
- Phase 4: 30 min
- Testing: 30 min
- **Total**: ~3 hours

## Success Criteria

✅ McpToolset implements Toolset trait
✅ Can connect to MCP server via transport
✅ Lists tools from MCP server
✅ Individual tools execute via MCP protocol
✅ Handles both structured and text responses
✅ Example demonstrates end-to-end usage
✅ Compiles without errors
