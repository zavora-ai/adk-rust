# MCP Apps Tool Result Example

Demonstrates the framework-owned constructor path for MCP Apps tool responses.

Run:

```bash
cargo run -p adk-examples --example mcp_apps_tool_result
```

Test:

```bash
cargo test -p adk-examples --example mcp_apps_tool_result
```

What it shows:

- `McpUiBridgeSnapshot` as the typed bridge/session state source
- `build_tool_result(...)` as the canonical constructor path
- additive `resourceUri` and inline `html` fallbacks
- typed bridge metadata under `toolResult.bridge`
