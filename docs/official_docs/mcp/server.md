# Publish an MCP server

ADK-Rust consumes MCP servers through `McpToolset`, but it does not wrap every
server-side SDK API. Server authors use the official `rmcp` SDK re-exported by
`adk_tool::mcp::rmcp` so client and server protocol types stay aligned.

For a standalone server crate, depending directly on the same `rmcp` release is
also appropriate:

```toml
[dependencies]
rmcp = { version = "3.4", features = ["transport-io", "schemars"] }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
```

## Minimal stdio server

```rust
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LookupInput {
    order_id: String,
}

#[derive(Debug, Clone)]
struct OrderServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl OrderServer {
    #[tool(description = "Read one order by its public order ID")]
    async fn read_order(
        &self,
        Parameters(input): Parameters<LookupInput>,
    ) -> String {
        format!("Order {} is ready for investigation", input.order_id)
    }
}

#[tool_handler]
impl ServerHandler for OrderServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Read-only order investigation tools")
    }
}

let service = rmcp::ServiceExt::serve(
    OrderServer { tool_router: OrderServer::tool_router() },
    rmcp::transport::io::stdio(),
).await?;
service.waiting().await?;
```

The deterministic `examples/mcp_manager` fixture uses this server shape. CI
compiles it with the other standalone examples; run it with
`cargo run --manifest-path examples/mcp_manager/Cargo.toml`. `ServerConfig` is the name
rmcp 3.4 gives the initialize result; earlier releases call it `ServerInfo`,
which rmcp 3.4 and later keep as a deprecated alias.

## Publish honest capabilities

Only advertise a capability when the handler implements it. A client uses the
initialization response to decide whether it may call resources, prompts,
completion, elicitation, subscriptions, or tasks.

To run a slow call as an SEP-2663 task, declare the
`io.modelcontextprotocol/tasks` extension in the server's capabilities
(`ServerCapabilities::builder().enable_tools().enable_tasks()`), and answer
`tools/call` with a task only when the client declared the extension too
(`ClientCapabilities::supports_tasks()`). `examples/mcp_protocol_revisions`
contains a server that does both.

## Tools are security boundaries

Descriptions and JSON Schema help the model form a call; they are not input
validation or authorization. A server must:

- validate every input independently of the model;
- resolve identity and tenant scope at the server boundary;
- authorize the specific action and resource;
- separate read operations from consequential writes;
- avoid returning secrets or unbounded data;
- make retries and idempotency explicit for side effects; and
- record enough evidence to explain the outcome.

## Choose a transport

Use stdio when the client owns the local child process. Use Streamable HTTP when
the server is an independently deployed service. Remote deployments also need
authentication, request limits, session policy, observability, and an
application-level health probe.

See the official [`rmcp` documentation](https://docs.rs/rmcp/3.5.1/rmcp/) for
server routers, resources, prompts, custom handlers, transports, authorization,
and extension APIs.
