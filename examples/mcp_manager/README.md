# MCP Server Manager Example

Demonstrates `McpServerManager` from the `adk-tool` crate — managing the full lifecycle of multiple MCP server child processes.

## What it shows

1. **JSON config loading** — Parse Kiro `mcp.json` format
2. **Start all servers** — Concurrent startup of non-disabled servers
3. **Tool aggregation** — Query tools from all running servers via the `Toolset` trait
4. **Status reporting** — `server_status()`, `all_statuses()`, `running_server_count()`
5. **Dynamic management** — Add and remove servers at runtime
6. **Graceful shutdown** — Stop all servers cleanly

## Prerequisites

- `npx` must be installed (comes with Node.js)
- The example uses `@playwright/mcp` and `@zavora-ai/computer-use-mcp` servers

## Running

```bash
cargo run -p mcp-manager-example
```

## Configuration

The example uses inline JSON config. In production, load from a file:

```rust
let manager = McpServerManager::from_json_file("mcp.json")?;
```

## Builder pattern

```rust
let manager = McpServerManager::new(configs)
    .with_elicitation_handler(handler)
    .with_health_check_interval(Duration::from_secs(15))
    .with_grace_period(Duration::from_secs(3))
    .with_name("my_manager");
```
