# MCP Resources, Prompts & Notifications Example

Demonstrates ADK-Rust's MCP **resource**, **prompt**, and **resource-update
notification** support with an LLM-driven agent.

## What's Inside

Two binaries:

- `resources-server` — a real MCP server (over stdio) that publishes:
  - a **resource** `config://app/review-policy` (the current PR review policy),
  - a **prompt** `review_pr` (a policy-aware review instruction template), and
  - a **tool** `refresh_policy` that rotates the policy and sends
    `notifications/resources/updated` to subscribers.

- `resources-client` — an LLM agent that connects with a custom
  `ResourceNotificationHandler`, reads the resource, uses the server's prompt as
  its instruction, subscribes to the resource, and then runs an interactive
  console. When the agent calls `refresh_policy`, the update notification is
  printed live.

## Running

```bash
export GOOGLE_API_KEY=your_key

# Build both binaries
cargo build --manifest-path examples/mcp_resources/Cargo.toml

# Run the agent (spawns the server automatically)
cargo run --manifest-path examples/mcp_resources/Cargo.toml --bin resources-client
```

Then try:

- "Refresh the review policy" — the agent calls `refresh_policy`; the server
  rotates the policy and pushes a resource-update notification, which the client
  prints.

## How It Works

```
Client                                   Server
  │  with_handlers(elicit, resource)  →   connect
  │  list_resources / read_resource   →   returns the policy text
  │  get_prompt("review_pr")          →   returns a policy-aware instruction
  │  subscribe_resource(policy)       →   subscription accepted
  │                                        (agent instruction = server prompt)
  │  agent calls refresh_policy        →   rotate policy
  │  ← notifications/resources/updated ──  handle_resource_updated(uri) printed
```

The prompt text comes from the server, so the *server* owns the review wording;
the client just runs it. The `ResourceNotificationHandler` and the subscription
are both retained if ADK-Rust reconnects the transport.

## Key APIs

| API | Purpose |
|-----|---------|
| `ResourceNotificationHandler` trait | Handle `resources/updated` + `resources/list_changed` |
| `McpToolset::with_handlers()` | Connect with elicitation **and** resource-notification handlers |
| `McpToolset::subscribe_resource()` / `unsubscribe_resource()` | Manage resource subscriptions (reconnect-safe) |
| `McpToolset::list_resources()` / `read_resource()` | Discover and read resources |
| `McpToolset::list_prompts()` / `get_prompt()` | Discover and resolve server prompts |

For managing many servers at once, `McpServerManager` exposes the same surface
per server: `with_resource_notification_handler()`, `list_server_resources()`,
`read_server_resource()`, `subscribe_server_resource()`, `get_server_prompt()`,
and so on. See `docs/official_docs/mcp/manager.md`.
