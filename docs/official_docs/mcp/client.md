# Build an MCP client

An ADK-Rust application is an MCP client when it connects to a server, reads its
published catalog, and makes selected capabilities available to an agent or
workflow.

## Install

```toml
[dependencies]
adk-tool = { version = "2.0.0", features = ["mcp"] }
```

For remote Streamable HTTP:

```toml
adk-tool = { version = "2.0.0", features = ["mcp", "http-transport"] }
```

## Local stdio connection

```rust
use adk_tool::{
    McpToolset,
    mcp::rmcp::{ServiceExt, transport::TokioChildProcess},
};
use std::sync::Arc;
use tokio::process::Command;

let command = Command::new("./target/release/company-mcp");
let client = ().serve(TokioChildProcess::new(command)?).await?;

let toolset = McpToolset::new(client)
    .with_name("company_tools")
    .with_tools(&["find_customer", "read_order", "request_refund"]);

let shutdown = toolset.cancellation_token().await;

let agent = LlmAgentBuilder::new("support")
    .model(model)
    .toolset(Arc::new(toolset))
    .build()?;

// Run the agent, then close the client-owned MCP session.
shutdown.cancel();
```

Use an absolute binary path in production. Avoid package tags such as `latest`
in deployment configuration because they make builds and incident recovery
non-reproducible.

## Tool discovery and filtering

`McpToolset` converts each published MCP tool into an ADK-Rust `Tool`. It keeps
the server's input and output schemas unchanged. The selected model provider
normalizes a copy of the schema when it builds its request.

```rust
let reviewed = McpToolset::new(client).with_filter(|name| {
    matches!(name, "read_order" | "read_policy" | "request_replacement")
});
```

Filtering controls model visibility. It does not replace authorization at tool
execution time.

## Resources, prompts, and completion

```rust
use serde_json::json;

let resources = toolset.list_resources().await?;
let templates = toolset.list_resource_templates().await?;
let policy = toolset.read_resource("company://policy/refunds").await?;

let prompts = toolset.list_prompts().await?;
let prompt = toolset
    .get_prompt(
        "investigate_order",
        Some(serde_json::Map::from_iter([
            ("order_id".to_string(), json!("ORD-1042")),
        ])),
    )
    .await?;

let suggestions = toolset
    .complete_prompt_argument("investigate_order", "order_id", "ORD-", None)
    .await?;
```

Resource-template completion uses `complete_resource_argument`. A server that
does not implement list operations returns an empty list when it responds with
MCP `MethodNotFound`; other protocol and transport failures remain errors.

## Resource subscriptions

```rust
toolset.subscribe_resource("company://inventory/sku-42").await?;
// Handle the server notification in the ClientHandler used for this connection.
toolset.unsubscribe_resource("company://inventory/sku-42").await?;
```

Subscribing creates the protocol subscription. Receiving the notification
requires a client handler that implements the relevant notification callback.

## Elicitation

Elicitation lets a server request information while handling a tool call. The
application decides how to present the request and whether to accept, decline,
or cancel it.

```rust
let toolset = McpToolset::with_elicitation_handler(
    transport,
    Arc::new(MyElicitationHandler),
).await?;
```

ADK-Rust advertises form and URL elicitation. A handler failure or panic becomes
a decline, preserving the MCP session. The application must still validate
accepted values and apply consent policy.

See `examples/mcp_elicitation` for a complete client and server pair.

## Negotiated tasks

```rust
use adk_tool::McpTaskConfig;
use std::time::Duration;

let toolset = McpToolset::new(client).with_task_support(
    McpTaskConfig::enabled()
        .poll_interval(Duration::from_secs(1))
        .timeout(Duration::from_secs(120))
        .max_attempts(120),
);
```

Task mode is selected from two negotiated facts:

1. the server advertises `tasks.requests.tools.call`; and
2. the tool declares task support as required or optional.

ADK-Rust sends task metadata with `tools/call`, receives the created task, polls
`tasks/get`, reads `tasks/result`, and calls `tasks/cancel` when local bounds are
exceeded. `input_required` is returned as a typed error because an ordinary ADK
tool call does not yet provide a protocol-neutral task-resume input channel.

## Remote Streamable HTTP

```rust
use adk_tool::{McpAuth, McpHttpClientBuilder};
use std::time::Duration;

let toolset = McpHttpClientBuilder::new("https://mcp.example.com/mcp")
    .with_auth(McpAuth::bearer(std::env::var("MCP_TOKEN")?))
    .header("X-Tenant-ID", "tenant-42")
    .timeout(Duration::from_secs(30))
    .reinit_on_expired_session(true)
    .connect()
    .await?;
```

The builder supports bearer tokens, a custom API-key header, and fixed OAuth
2.0 client credentials. See [Security and authorization](security.md) before
choosing an authentication flow.
