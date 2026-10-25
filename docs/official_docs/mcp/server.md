# Publish an MCP server

Two paths publish an MCP server:

| You have | Use | Feature |
|---|---|---|
| ADK-Rust `Tool`s, such as `FunctionTool`, `#[tool]` functions or AWP capabilities | `adk_tool::mcp::server::McpServer` with `McpRouterBuilder` | `mcp-server` (experimental) |
| A hand-written server with resources, prompts or custom handlers | The official `rmcp` SDK, re-exported as `adk_tool::mcp::rmcp` | `mcp` |

## Publish ADK-Rust tools (`mcp-server`)

```toml
[dependencies]
adk-tool = { version = "3.0.0", features = ["mcp-server"] }
axum = "0.8"
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

`McpServer` lists each tool from its own declaration and answers `tools/call`;
`McpRouterBuilder` serves it over MCP Streamable HTTP as an `axum::Router` that an
application, or the AWP agent edge, mounts.

```rust
use std::sync::Arc;

use adk_core::Url;
use adk_tool::FunctionTool;
use adk_tool::mcp::server::{McpRouterBuilder, McpServer, ServerCardHeader};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let quote = FunctionTool::new("get_quote", "Quotes a solar installation", |_ctx, args| async move {
        Ok(json!({ "kw": args["kw"], "price": 4200 }))
    })
    .with_read_only(true);
    let server = McpServer::builder("com.example/solar", "1.0.0")
        .description("Quotes for Example Solar installations")
        .title("Example Solar")
        .tool(Arc::new(quote))
        .build()?;
    let router = McpRouterBuilder::new(server)
        .public_url(Url::parse("http://127.0.0.1:8080/mcp")?)
        .card_header(ServerCardHeader::new("Authorization").required().secret())
        .build()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, router).await?;
    Ok(())
}
```

### What a client sees

| Tool declaration | MCP tool |
|---|---|
| `name()`, `description()` | `name`, `description` |
| `parameters_schema()` | `inputSchema`; none becomes `{"type": "object"}` |
| `response_schema()` of type `object` | `outputSchema`; a result object is also sent as `structuredContent` |
| `effect()` `ReadOnly` | `readOnlyHint: true`, `idempotentHint: true`, `destructiveHint: false` |
| `effect()` `Idempotent` | `readOnlyHint: false`, `idempotentHint: true`, `destructiveHint` from the tool, else `true` |
| `effect()` `NonIdempotent` | `readOnlyHint: false`, `idempotentHint: false`, `destructiveHint` from the tool, else `true` |
| `annotations()` `title`, `openWorldHint` | Kept; `openWorldHint` otherwise defaults to `McpServerBuilder::open_world` (`true`) |

`tools/call` checks the arguments against the parameters schema first: arguments that
do not match are answered with `isError: true` and the validation message, so a model
can correct them, and never reach the tool.

### Executor and caller hooks

| Hook | Default | Role |
|---|---|---|
| `McpCallerResolver` | `AnonymousCallers` | Runs for every request to the endpoint and returns the `McpCaller` (an id, whether it authenticated, typed extensions); an error refuses the request with its HTTP status, and `401` carries `WWW-Authenticate: Bearer` |
| `McpCallExecutor::execute` | `DirectExecutor`: `Tool::execute` with a `SimpleToolContext` | Receives every call to a visible tool with valid arguments, with its caller, JSON-RPC request id and session; returns `Completed(value)`, `Failed(error)` or a refusal |
| `McpCallExecutor::is_visible` | Every tool | Filters `tools/list` per caller; a call to a hidden tool is answered as a call to an unknown tool |

An executor is where calls enter a governed path, such as `adk_core::authorize_tool_call`
and the action ledger. The JSON-RPC request id is unique only within its session, so an
executor that needs an idempotency key builds one from the caller and session or reads one
the resolver attached to the caller.

### Errors

| Outcome | Client receives |
|---|---|
| Unknown or hidden tool | JSON-RPC `-32602`, `unknown tool '<name>'` |
| Arguments that do not match the schema | Result with `isError: true` and the validation message |
| The tool ran and failed (`Failed`) | Result with `isError: true`: the error's component, category and code, plus its message only for a rejection category |
| The executor refused (`Err`) | JSON-RPC `-32602` for `InvalidInput`, `NotFound` and `Unsupported`, otherwise `-32603`; the message adds the error's text only for a rejection category; `data` carries `category`, `code` and `retryable` |
| The call ran past the call timeout (60 s by default) | JSON-RPC `-32603` with category `timeout`; not marked retryable for a non-idempotent tool |

Rejection categories are those `ErrorCategory::is_rejection` names: `InvalidInput`,
`Forbidden`, `NotFound`, `Unsupported`, `Conflict` and `ResourceExhausted`. Other
messages can quote hosts, credentials or upstream bodies, so they stay on the server.

### Transport

| Request | Response |
|---|---|
| A `POST` body over `max_body_bytes` (1 MiB) | `413` with JSON-RPC `-32600` |
| A body that is not JSON | `400` with JSON-RPC `-32700` |
| JSON that is not one JSON-RPC message, such as a batch | `400` with JSON-RPC `-32600` |
| A request to a served method whose parameters do not fit it | `400` with JSON-RPC `-32602` |
| A `Host` outside `allowed_hosts` (loopback by default) | `403` |
| A session another caller opened | `404` |
| `initialize` while `max_sessions` sessions are live | `503` with JSON-RPC `-32603` |
| A server error inside the transport | JSON-RPC `-32603` `internal error`, with no detail |

`McpSessions::Stateful` (the default) issues an `Mcp-Session-Id` on `initialize`, binds it
to the caller that opened it, closes it after `idle_timeout` (5 minutes) without a request
and keeps at most `max_sessions` (1024) live. `McpSessions::Stateless` issues no sessions
and answers in JSON. Clients that negotiate MCP `2026-07-28` are served statelessly in both.

### Server Card and AI Catalog

With a public URL, the router serves three discovery routes:

| Route | Serves |
|---|---|
| `{endpoint}/server-card`, such as `/mcp/server-card` | The MCP Server Card ([SEP-2127](https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2127)) at the location SEP-2127 reserves |
| `/.well-known/ai-catalog.json` | An AI Catalog with one entry, `urn:air:{publisher}:mcp:{name}`, pointing at that card; `ai_catalog(host)` names its operator |
| `/.well-known/mcp/server-card.json` | The same card at the path AWP 2.0 site contracts link |

An application that publishes its own AI Catalog, listing an A2A card beside the MCP server,
calls `without_ai_catalog()` and adds `McpServer::ai_catalog_entry(card_url)` to its catalog.

```json
{
  "$schema": "https://static.modelcontextprotocol.io/schemas/v1/server-card.schema.json",
  "name": "com.example/solar",
  "version": "1.0.0",
  "description": "Quotes for Example Solar installations",
  "title": "Example Solar",
  "remotes": [
    {
      "type": "streamable-http",
      "url": "http://127.0.0.1:8080/mcp",
      "headers": [{ "name": "Authorization", "isRequired": true, "isSecret": true }],
      "supportedProtocolVersions": ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"]
    }
  ]
}
```

The card's name, version, title and description are the server's `serverInfo`, and
`supportedProtocolVersions` are the versions it negotiates. A card lists no tools: clients
read them from `tools/list`. Card responses carry `Content-Type:
application/mcp-server-card+json`, `Access-Control-Allow-Origin: *`,
`Cache-Control: public, max-age=3600` and an `ETag`, and answer `If-None-Match` with `304`.

## Hand-written servers with `rmcp`

Server authors who need resources, prompts or custom handlers use the official `rmcp` SDK
re-exported by `adk_tool::mcp::rmcp`, so client and server protocol types stay aligned.

For a standalone server crate, depending directly on the same `rmcp` release is
also appropriate:

```toml
[dependencies]
rmcp = { version = "3.4", features = ["transport-io", "schemars"] }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
```

### Minimal stdio server

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

### Publish honest capabilities

Only advertise a capability when the handler implements it. A client uses the
initialization response to decide whether it may call resources, prompts,
completion, elicitation, subscriptions, or tasks.

To run a slow call as an SEP-2663 task, declare the
`io.modelcontextprotocol/tasks` extension in the server's capabilities
(`ServerCapabilities::builder().enable_tools().enable_tasks()`), and answer
`tools/call` with a task only when the client declared the extension too
(`ClientCapabilities::supports_tasks()`). `examples/mcp_protocol_revisions`
contains a server that does both.

### Tools are security boundaries

Descriptions and JSON Schema help the model form a call; they are not input
validation or authorization. A server must:

- validate every input independently of the model;
- resolve identity and tenant scope at the server boundary;
- authorize the specific action and resource;
- separate read operations from consequential writes;
- avoid returning secrets or unbounded data;
- make retries and idempotency explicit for side effects; and
- record enough evidence to explain the outcome.

### Choose a transport

Use stdio when the client owns the local child process. Use Streamable HTTP when
the server is an independently deployed service. Remote deployments also need
authentication, request limits, session policy, observability, and an
application-level health probe.

See the official [`rmcp` documentation](https://docs.rs/rmcp/3.5.1/rmcp/) for
server routers, resources, prompts, custom handlers, transports, authorization,
and extension APIs.
