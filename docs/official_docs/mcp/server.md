# Publish an MCP server

ADK-Rust consumes MCP servers through `McpToolset`, but it does not wrap every
server-side SDK API. Server authors use the official `rmcp` SDK re-exported by
`adk_tool::mcp::rmcp` so client and server protocol types stay aligned.

For a standalone server crate, depending directly on the same `rmcp` release is
also appropriate:

```toml
[dependencies]
rmcp = { version = "2.2", features = ["transport-io", "schemars"] }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
```

## Minimal stdio server

```rust
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
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
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Read-only order investigation tools")
    }
}

let service = rmcp::ServiceExt::serve(
    OrderServer { tool_router: OrderServer::tool_router() },
    rmcp::transport::io::stdio(),
).await?;
service.waiting().await?;
```

The deterministic `examples/mcp_manager` fixture uses this server shape and is
compiled and executed by the MCP verification gates.

## Publish honest capabilities

Only advertise a capability when the handler implements it. A client uses the
initialization response to decide whether it may call resources, prompts,
completion, elicitation, subscriptions, or tasks.

For task-capable tools, declare the tool's task support and advertise
`tasks.requests.tools.call`. The client may reject a required task tool when the
server did not negotiate task support.

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

See the official [`rmcp` documentation](https://docs.rs/rmcp/2.2.0/rmcp/) for
server routers, resources, prompts, custom handlers, transports, authorization,
and extension APIs.
