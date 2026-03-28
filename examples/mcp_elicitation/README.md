# MCP Elicitation Example

Demonstrates ADK-Rust's MCP Elicitation support — the ability for MCP servers to request additional information from clients during tool execution.

## What's Inside

Two binaries:

- `elicitation-server` — A real MCP server (over stdio) with two tools that use elicitation:
  - `create_user` — collects name + email via form elicitation
  - `deploy_app` — asks for deployment confirmation via form elicitation

- `elicitation-client` — An LLM-powered agent that connects to the server with a custom `ElicitationHandler`, discovers tools, and runs an interactive console. When the agent calls a tool that triggers elicitation, you get prompted on stdin.

## Running

```bash
export GOOGLE_API_KEY=your_key

# Build both binaries
cargo build --manifest-path examples/mcp_elicitation/Cargo.toml

# Run the agent (spawns the server automatically)
cargo run --manifest-path examples/mcp_elicitation/Cargo.toml --bin elicitation-client
```

Then try prompts like:
- "Create a new user account"
- "Deploy my-app to production"

The agent will call the MCP tool, which triggers elicitation — you'll be prompted for input.

## How It Works

```
User ──→ Agent ──→ MCP Tool Call ──→ Server
                                       │
                                       │ peer.elicit::<UserProfile>(message)
                                       │
                              ←── ElicitationHandler called
                              (prompts user on stdin)
                              ──→ Accept { name, email }
                                       │
                              ←── Tool Result: "User created!"
```

The key difference from a standard MCP connection:

```rust
// Without elicitation (standard)
let client = ().serve(transport).await?;
let toolset = McpToolset::new(client);

// With elicitation
let handler = Arc::new(StdinElicitationHandler);
let toolset = McpToolset::with_elicitation_handler(transport, handler).await?;
```

## Key APIs

| API | Purpose |
|-----|---------|
| `ElicitationHandler` trait | Implement to handle server elicitation requests |
| `McpToolset::with_elicitation_handler()` | Connect with elicitation support |
| `peer.elicit::<T>(message)` | Server-side: request typed data from client |
| `peer.supported_elicitation_modes()` | Server-side: check client capability |
