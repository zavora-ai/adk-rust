# Build an MCP client

An ADK-Rust application is an MCP client when it connects to a server, reads its
published catalog, and makes selected capabilities available to an agent or
workflow.

## Install

```toml
[dependencies]
adk-tool = { version = "3.0.0", features = ["mcp"] }
```

For remote Streamable HTTP:

```toml
adk-tool = { version = "3.0.0", features = ["mcp", "http-transport"] }
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

let agent = LlmAgentBuilder::new("support")
    .model(model)
    .toolset(Arc::new(toolset))
    .build()?;

// Run the agent through a Runner, then end the MCP session and the server process.
runner.close().await;
```

Use an absolute binary path in production. Avoid package tags such as `latest`
in deployment configuration because they make builds and incident recovery
non-reproducible.

Give each toolset its own `with_name`. Its tools report the origin
`mcp:{toolset name}`, which tool-call approvals are bound to, so two servers
sharing the default name `mcp_toolset` would share one identity in approvals.
See [Security](security.md#tool-exposure-and-execution).

## Tool discovery and filtering

`McpToolset` converts each published MCP tool into an ADK-Rust `Tool`. It keeps
the server's input and output schemas unchanged, provided they are within the
[schema size limits](#schema-size-limits). The selected model provider
normalizes a copy of the schema when it builds its request.

The adapter also retains MCP tool annotations and maps them to the tool's
safety signals:

| Annotation | `Tool::effect()` | `is_read_only()` | `is_concurrency_safe()` |
|------------|------------------|------------------|-------------------------|
| `readOnlyHint: true` | `ReadOnly` | `true` | `true` |
| `idempotentHint: true` (not read-only) | `Idempotent` | `false` | `true` |
| Neither | `NonIdempotent` | `false` | `false` |

An `Idempotent` or `ReadOnly` effect permits safe replay after reconnecting and
retries under a [`RetryBudget`](../tools/tool-effects.md#retries); a
`NonIdempotent` call is recorded in the action ledger when one is configured.
`with_tool_call_retries()` allows replay after a reconnect for every tool of the
toolset; enable it only when the server's tools are safe to repeat. `Tool::annotations()` returns the server's
`annotations` object unchanged, so a policy or callback can read `title`,
`destructiveHint`, and `openWorldHint` as well:

```rust
use adk_tool::mcp::rmcp::model::ToolAnnotations;

let hints = tool
    .annotations()
    .and_then(|hints| serde_json::from_value::<ToolAnnotations>(hints.clone()).ok());
let destructive = hints.as_ref().is_none_or(ToolAnnotations::is_destructive);
```

> **Important:** MCP annotations are server-published hints. Use automatic
> replay and dispatch metadata only with servers inside the application's trust
> boundary.

### Tool names

Discovery admits each tool name once per server and skips, with a `warn!`
naming the toolset and the escaped name, any tool that no model can call:

| Skipped tool | Why |
|--------------|-----|
| A later tool with the name of an earlier one on the same server | The first definition is kept, so one server cannot fail the agent's turn with a duplicate |
| An empty name, or one containing control characters | No model provider accepts it, and it would corrupt log lines |
| A name longer than 64 bytes | Every model adapter truncates names to 64 bytes and nothing maps a truncated name back to its tool |

Names from different toolsets are not renamed: an agent whose toolsets publish
the same name still fails the turn. Use `PrefixedToolset`, or
`McpServerManager`, which prefixes names shared by its servers.

A tool call's result reaches the model as `{"output": ...}`; a call that
succeeds with no content is `{"output": ""}`, not a failure. A result with
`isError: true` is a tool error, whose text `LlmAgent` keeps out of the model
request by default. See [what the model receives](../tools/mcp-tools.md#connect-one-local-server).

```rust
let reviewed = McpToolset::new(client).with_filter(|name| {
    matches!(name, "read_order" | "read_policy" | "request_replacement")
});
```

Filtering controls model visibility. `call_tool_value` applies the same filter
and refuses an excluded tool without contacting the server. Filtering does not
replace authorization at tool execution time.

### Discovery bounds

`McpToolset` follows the server's `nextCursor` itself, so a server that never
stops paging cannot hold an agent turn. Each of `tools/list`, `resources/list`,
`resources/templates/list`, and `prompts/list` fails with `AdkError::Tool` when:

| Condition | Default bound |
|-----------|---------------|
| The server still returns a `nextCursor` after `max_pages` pages | 64 pages |
| The server returns a cursor it already returned | Always |
| The listing, reconnects included, takes longer than `timeout` | 30 seconds |

A toolset that fails discovery is skipped for that turn with a warning, or
fails the turn promptly with `strict_toolsets(true)`.

```rust
use adk_tool::mcp::{McpDiscoveryLimits, McpToolset};
use std::time::Duration;

let toolset = McpToolset::new(client).with_discovery_limits(
    McpDiscoveryLimits::default()
        .with_max_pages(256)
        .with_timeout(Duration::from_secs(10)),
);
```

`McpServerManager::with_discovery_limits`, `ConnectionRefresher::with_discovery_limits`,
and `SimpleClient::with_discovery_limits` apply the same bounds.

### Discovery cache

An agent resolves its toolsets before every model turn. `McpToolset` reuses a
`tools/list` result for 60 seconds, so a turn does not cost one round trip per
MCP server. The cached list is dropped before the TTL elapses when:

| Event | Applies to |
|-------|------------|
| The connection is refreshed | Every toolset with a connection factory |
| The server sends `notifications/tools/list_changed` | Connections served by `AdkClientHandler`, such as those built with `with_elicitation_handler` |
| `invalidate_tool_list_cache()` is called | Every toolset |

`invalidate_tool_list_cache()` also clears the connection's rmcp response cache,
which keeps pages the server marked fresh with `ttlMs`, and keeps a `tools/list`
already in flight from caching its result. With a TTL of `Duration::ZERO`,
rmcp's response cache still answers a page the server marked fresh.

```rust
use std::time::Duration;

// Send `tools/list` on every resolution.
let toolset = McpToolset::new(client).with_tool_list_cache_ttl(Duration::ZERO);

// Or drop the cached list from a custom handler's `on_tool_list_changed`.
toolset.invalidate_tool_list_cache().await;
```

### Schema size limits

Discovery measures each tool's input and output schema before copying or
logging it. A tool whose schema exceeds a limit is skipped with a `warn!` that
names the toolset and the tool, and the remaining tools are still registered.
Accepted tools log `schema.bytes` and `schema.nodes` at `debug` instead of the
schema itself.

| Limit | Measures | Default |
|-------|----------|---------|
| `max_bytes` | Approximate compact-JSON size of one schema | 256 KiB |
| `max_nodes` | JSON values in one schema, the root object included | 10 000 |
| `max_description_bytes` | UTF-8 bytes of one tool description | 8 KiB |

A tool's description is text the server chooses and every model request carries.
A description over `max_description_bytes` is cut at a character boundary with a
`warn!` giving both sizes, and the tool is kept.

```rust
use adk_tool::mcp::{McpSchemaLimits, McpToolset};

let toolset = McpToolset::new(client).with_schema_limits(
    McpSchemaLimits::default()
        .with_max_bytes(1024 * 1024)
        .with_max_nodes(40_000)
        .with_max_description_bytes(32 * 1024),
);
```

The defaults admit every schema a model can reasonably use: 256 KiB is about
64 000 tokens for a single tool. `McpServerManager::with_schema_limits` applies
one set of limits to every managed server. Raise the limits only for servers
inside the application's trust boundary.

## Concurrency and cancellation

A toolset, its clones, and every tool it produced share one MCP connection.
Requests on it run concurrently: a slow tool call does not delay discovery or
another call.

- **Reconnects** — when a connection fails and a connection factory is
  configured, the first failing request replaces it and concurrent failures
  retry on the replacement, so one failure reconnects once. The old connection
  closes when the last request still using it finishes.
- **Exited servers** — when a stdio server process exits, its transport
  closes, `is_closed()` returns `true`, and in-flight requests fail with
  `Transport closed`. With a connection factory, discovery reconnects and
  retries, and the next request replaces the closed connection before it is
  sent. A `tools/call` that was in flight is not resent unless replay is
  allowed.
- **What reconnects** — the decision comes from the error's variant, never
  from text the server chose. A closed transport, a broken pipe or reset
  connection, an interrupted HTTP stream, an expired Streamable HTTP session,
  and an HTTP 401 reconnect. An HTTP 5xx or a JSON-RPC error is an answer from
  a live session and is returned as is, whatever its body says.
  `should_refresh_connection` applies the same rule to an `rmcp::ServiceError`.
- **Dropped calls** — when a `tools/call` future is dropped before the response
  arrives, for example by the agent's tool timeout, the toolset sends
  `notifications/cancelled` for that request so the server can stop the work.
  When the server already turned the call into a task, the toolset sends
  `tasks/cancel` for it; the task stays tracked for `cancel_pending_tasks`
  until the server reports it finished.

`ConnectionRefresher` and `SimpleClient` share their connection the same way:
neither holds a lock across a request.

## Close the connection

`Runner::close` closes the toolsets of every agent in the runner's tree, and
`Toolset::close` closes one toolset directly. Closing an `McpToolset` runs these
steps and returns when the transport has closed or the close timeout elapses:

| Step | stdio server | Streamable HTTP server |
|------|--------------|------------------------|
| 1 | The toolset is marked closed: no new request starts, and its connection factory is not used again | Same |
| 2 | Remote tasks the toolset started are cancelled, within half of the close timeout | Same |
| 3 | The rmcp service is cancelled, and its transport closes the server's input and waits for the process to exit; rmcp kills a process still running 3 seconds later | The transport ends the session with `DELETE` |

```rust
use adk_tool::McpToolset;
use std::sync::Arc;
use std::time::Duration;

let toolset = McpToolset::new(client)
    .with_name("company_tools")
    .with_close_timeout(Duration::from_secs(2));
let agent = LlmAgentBuilder::new("support")
    .model(model)
    .toolset(Arc::new(toolset))
    .build()?;
// ... run the agent through `runner` ...
runner.close().await;
```

- **Final** — a closed toolset does not reconnect. `tools()`, tool calls, and the
  resource and prompt methods fail with an `AdkError` whose code is
  `tool.mcp.closed`. Build a new toolset to reach the server again.
- **Shared** — clones of the toolset and every tool it produced share its
  connection, so they close with it.
- **Bounded** — the close timeout is 5 seconds by default. A transport still
  closing when it elapses keeps closing in the background.
- **Idempotent** — closing again only waits for the first close to finish.
- **Composed toolsets** — `FilteredToolset` and `PrefixedToolset` close their
  inner toolset, and `MergedToolset` closes every member concurrently.
- **`McpServerManager`** — closing the manager as a toolset leaves its servers
  running; call [`shutdown`](manager.md#shutdown-behavior) to stop them.

The `mcp_close` example checks each of these against a stdio server, a
Streamable HTTP server, and an `McpServerManager`:

```bash
cargo run --manifest-path examples/mcp_close/Cargo.toml
```

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
use adk_tool::{AutoDeclineElicitationHandler, McpToolset, ResourceNotificationHandler};
use std::sync::Arc;

struct ResourceUpdates;

#[async_trait::async_trait]
impl ResourceNotificationHandler for ResourceUpdates {
    async fn handle_resource_updated(
        &self,
        uri: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("Resource changed: {uri}");
        Ok(())
    }

    async fn handle_resource_list_changed(
        &self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("The resource catalog changed");
        Ok(())
    }
}

let toolset = McpToolset::with_handlers(
    transport,
    Arc::new(AutoDeclineElicitationHandler),
    Arc::new(ResourceUpdates),
).await?;

toolset.subscribe_resource("company://inventory/sku-42").await?;
toolset.unsubscribe_resource("company://inventory/sku-42").await?;
```

`McpToolset` restores active subscriptions after its bounded connection refresh.
`McpServerManager` also retains subscriptions across managed process restarts.
Handler errors and panics are logged without terminating the MCP connection.
For Streamable HTTP, configure the same handler with
`McpHttpClientBuilder::with_resource_notification_handler` before calling
`connect_with_elicitation`.

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

Under the SEP-2663 tasks extension the server decides per call whether to
answer `tools/call` with a task, and only for a client that declared the
extension:

```rust
use adk_tool::mcp::rmcp::{ServiceExt, transport::TokioChildProcess};
use adk_tool::mcp::{AdkClientHandler, AutoDeclineElicitationHandler, McpTaskConfig, McpToolset};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

let client = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler))
    .with_tasks() // declares the extension; without it the server answers inline
    .serve(TokioChildProcess::new(Command::new("/opt/company/bin/report-mcp"))?)
    .await?;
let toolset = McpToolset::new(client).with_task_support(
    McpTaskConfig::enabled()
        .poll_interval(Duration::from_secs(1))
        .timeout(Duration::from_secs(120))
        .max_attempts(120),
);
```

| `McpTaskConfig` | Default | Effect |
|-----------------|---------|--------|
| `enable_tasks` | `false` (`enabled()` sets it) | Poll a task the server returns; with it off, the task is cancelled and the call fails |
| `poll_interval_ms` | 1000 | Used when the server's task carries no `pollIntervalMs` |
| `timeout_ms` | 300 000 (`no_timeout()` removes it) | `tasks/cancel`, then the call fails |
| `max_poll_attempts` | Unlimited | `tasks/cancel`, then the call fails |
| `max_input_rounds` | 10 | Most `input_required` rounds answered for one call |

The completed task's result comes back inline from `tasks/get`. An
`input_required` round — in a task or on a plain `tools/call` — is answered
through the connection's handler: elicitation always, and sampling or roots only
when the client declared them. `McpServerManager` declares the extension for a
server whose configuration sets `taskConfig.enableTasks`.

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

`connect()` returns an `McpToolset` with a connection factory attached, so a
failed connection is re-established. The request timeout defaults to 30 seconds,
and `reinit_on_expired_session` is on by default. Name the returned toolset
with `with_name`.

The builder supports bearer tokens (`McpAuth::bearer`), a custom API-key header
(`McpAuth::api_key`), fixed OAuth 2.0 client credentials (`McpAuth::oauth2`), and
tokens minted per request by a credential broker
(`with_credential_broker(broker, acting, scopes)`), which are sent only to the
endpoint they were minted for. See [Security and authorization](security.md)
before choosing an authentication flow.

With OAuth 2.0, each request carries the current access token rather than the
one fetched at connect time, so a long-lived connection outlives its first
token:

| Situation | Behavior |
|-----------|----------|
| The token is near the `expires_in` its token response declared | A new token is fetched before the request is sent |
| The server answers a request with HTTP 401 | A new token is fetched and the request is sent once more |
| The new token is also rejected | The 401 reaches the toolset, which reconnects through its connection factory |

A request rejected with 401 never ran, so it is resent even for tools whose
calls are not otherwise replayed after a connection failure. A token that cannot
be fetched at connect time fails `connect()`, so bad credentials surface before
the first tool call.

Reconnects follow `RefreshConfig` (`with_refresh_config`): three attempts, one
second apart, by default.
