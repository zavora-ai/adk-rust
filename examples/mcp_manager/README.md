# Dynamic MCP server manager

This example runs without Node.js, a model API key, or network access. The
executable starts a real Rust MCP server as its child process, then proves the
manager lifecycle against that server.

It demonstrates:

- loading `mcp.json`-compatible configuration;
- starting and monitoring a local stdio MCP server;
- discovering and calling a real MCP tool;
- adding, enabling, updating, disabling, and removing servers at runtime;
- resolving tool-name collisions across servers;
- saving the current registry; and
- closing all managed MCP sessions.

Run it from the repository root:

```bash
cargo run --manifest-path examples/mcp_manager/Cargo.toml
```

Expected output includes:

```text
ADK-Rust dynamic MCP server manager

1. Loaded local-tools from mcp.json-compatible configuration
   status: Running
2. Discovered 1 tool: echo
3. Called the real child server
   response: {"output":"MCP server replied: dynamic MCP is running"}
4. Added standby-tools at runtime: Disabled
   enabled and started: Running
5. Reconfigured the running server and restarted it safely
6. Saved the live registry to .../adk-rust-mcp-manager-example.json
7. Disabled, removed, and shut down every child server
```

`autoApprove` is preserved when reading and writing compatible configuration,
but it does not bypass ADK-Rust tool authorization. Apply approval policy in
the agent or application that owns the tool call.
