# adk-tool

Tool system for Rust Agent Development Kit (ADK-Rust) agents (FunctionTool, MCP, Google Search).

[![Crates.io](https://img.shields.io/crates/v/adk-tool.svg)](https://crates.io/crates/adk-tool)
[![Documentation](https://docs.rs/adk-tool/badge.svg)](https://docs.rs/adk-tool)
[![License](https://img.shields.io/crates/l/adk-tool.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

## Overview

`adk-tool` provides the tool infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **`#[tool]`** - Attribute macro that turns an async function into a `Tool` with a derived, enforced schema
- **FunctionTool** - Create tools from async Rust functions
- **StatefulTool\<S\>** - Wrap shared state (`Arc<S>`) with a tool handler
- **SimpleToolContext** - Lightweight `ToolContext` for non-agent callers (testing, MCP servers)
- **AgentTool** - Use an agent as a callable tool; the delegate runs in non-streaming mode and shares the caller's budget, governance, policy, approvals and ledgers, but not its handoff targets, context cache or invocation toolsets
- **GoogleSearchTool** - Web search via Gemini's grounding
- **Provider-native wrappers** - Typed declarations for Gemini, Anthropic, and OpenAI built-in tools, each declaring a `ToolEffect`: search, URL context, Maps and file search are `ReadOnly`; code execution, computer use, image generation, shell, apply-patch and MCP tools are `NonIdempotent`
- **McpToolset** - Model Context Protocol integration (local and remote servers)
- **McpServerManager** - Multi-server lifecycle management with health monitoring and auto-restart
- **BasicToolset / FilteredToolset / MergedToolset / PrefixedToolset** - Group, filter, combine, and namespace toolsets
- **ExitLoopTool** - Control flow for loop agents
- **LoadArtifactsTool** - Inject binary artifacts into context
- **ToolArgumentValidator** - Check hand-written tools' arguments against a JSON Schema
- **LoadMemoryTool / PreloadMemoryTool** - On-demand and turn-start memory search (feature: `memory-tools`)
- **GraphMemoryToolset** - Knowledge-graph memory curation tools (feature: `graph-memory-tools`)
- **CodeTool / PythonCodeTool / JavaScriptCodeTool / MontyPythonCodeTool** - Code execution over `adk-code` (feature: `code`)
- **SlackToolset** - Send messages, read channels, add reactions, list threads (feature: `slack`)
- **BigQueryToolset / SpannerToolset** - SQL execution, schema inspection, and table listing (features: `bigquery`, `spanner`); SQL execution is read-only by default
- **ExampleStoreClient / ExampleStoreProvider** - Vertex AI Example Store few-shot retrieval (feature: `example-store`)
- **AgentRegistryClient / AgentSearchTool** - Google Agent Registry client and agent discovery tool (feature: `vertex-agent-registry`)

## Installation

```toml
[dependencies]
adk-tool = "3.0"

# Or, for local MCP servers via stdio:
# adk-tool = { version = "3.0", features = ["mcp"] }

# Or, for remote MCP servers via HTTP (implies `mcp`):
# adk-tool = { version = "3.0", features = ["http-transport"] }
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "3.0", features = ["tools"] }
```

## Quick Start

### `#[tool]`

The macro uses the doc comment as the description, derives the parameters schema from the
argument type, and generates a unit struct named after the function in PascalCase. The argument
type needs `serde::Deserialize` and `schemars::JsonSchema`, so add `serde`, `serde_json` and
`schemars` to your dependencies.

```rust
use adk_tool::{AdkError, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to get weather for
    city: String,
}

/// Get current weather for a city.
#[tool(read_only, concurrency_safe)]
async fn get_weather(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city, "temperature": 22, "condition": "sunny" }))
}

// Generated: `struct GetWeather`, with the function's visibility, which implements `Tool`.
fn weather_tool() -> GetWeather {
    GetWeather
}
```

`#[tool(...)]` accepts `read_only`, `idempotent`, `concurrency_safe`, `long_running`, and
`lenient_args`.

### FunctionTool

```rust
use adk_core::{Result, ToolContext};
use adk_tool::FunctionTool;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Serialize, JsonSchema)]
struct WeatherParams {
    /// The city to get weather for
    city: String,
}

async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    let city = args["city"].as_str().unwrap_or("Unknown");
    Ok(json!({ "city": city, "temperature": 22, "condition": "sunny" }))
}

fn weather_tool() -> FunctionTool {
    FunctionTool::new("get_weather", "Get current weather for a city", get_weather)
        // The schema tells the model what to send and is enforced on every call.
        .with_parameters_schema::<WeatherParams>()
}
```

### Use the tool in an agent

This program runs the tool through a Gemini agent. It needs `adk-agent`, `adk-model`,
`adk-runner`, `adk-session`, `futures`, and `tokio` alongside the dependencies above, and a
`GOOGLE_API_KEY`.

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Result, ToolContext};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Serialize, JsonSchema)]
struct WeatherParams {
    /// The city to get weather for
    city: String,
}

async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    let city = args["city"].as_str().unwrap_or("Unknown");
    Ok(json!({ "city": city, "temperature": 22, "condition": "sunny" }))
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let model = GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?;
    let tool = FunctionTool::new("get_weather", "Get current weather for a city", get_weather)
        .with_parameters_schema::<WeatherParams>();

    let agent = LlmAgentBuilder::new("weather_agent")
        .instruction("Answer weather questions with the get_weather tool.")
        .model(Arc::new(model))
        .tool(Arc::new(tool))
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "weather".into(),
            user_id: "user".into(),
            session_id: Some("session-1".into()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("weather")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()?;

    let question = Content::new("user").with_text("What is the weather in Nairobi?");
    let mut events = runner.run_str("user", "session-1", question).await?;
    while let Some(event) = events.next().await {
        let event = event?;
        if event.is_final_response() {
            for part in event.content().map(|c| c.parts.as_slice()).unwrap_or_default() {
                if let Some(text) = part.text() {
                    println!("{text}");
                }
            }
        }
    }
    Ok(())
}
```

### Argument Validation

A `#[tool]` tool checks each call's arguments against its schema before the function runs:
a key the schema does not name, a value outside a `range`, `length` or `pattern` bound, or a
missing required field fails the call with a `tool.invalid_arguments` error
(`ErrorCategory::InvalidInput`) that lists each violation by JSON pointer. An `LlmAgent` shows
that message to the model so it can correct the call; the function does not run. `null` stays
accepted for an optional field. `#[tool(lenient_args)]` opts a tool out, or
`#[schemars(extend("additionalProperties" = true))]` keeps one struct's keys open.

A `FunctionTool` or `StatefulTool` given a schema with `with_parameters_schema::<T>()` applies the
same check, and `with_lenient_args(true)` opts out. All of them read an integral float such as
`3.0` at an integer field as `3`, also in lenient mode. Hand-written tools use the same helpers:

```rust
use adk_tool::{ToolArgumentValidator, close_object_schemas, coerce_integral_floats};
use serde_json::json;

fn main() {
    let mut schema = json!({
        "type": "object",
        "properties": { "count": { "type": "integer", "minimum": 1, "maximum": 10 } }
    });
    close_object_schemas(&mut schema); // reject keys the schema does not name
    let validator = ToolArgumentValidator::new(&schema).expect("a valid JSON Schema");
    assert!(validator.validate("book", &json!({ "count": 50 })).is_err());
    assert!(validator.validate("book", &json!({ "count": 5, "extra": true })).is_err());

    let mut args = json!({ "count": 3.0 });
    coerce_integral_floats(&schema, &mut args);
    assert_eq!(args, json!({ "count": 3 }));
}
```

### Tool Metadata and Effects

`ToolExecutionStrategy::Auto` runs a call concurrently only when its tool is both read-only and
concurrency-safe. `Tool::effect()` decides retries: an agent with a `RetryBudget`
(`LlmAgentBuilder::default_retry_budget` or `tool_retry_budget`) retries a failed call only when
the tool's effect is `ReadOnly` or `Idempotent` and the error is retryable or a timeout; without
a budget every call runs once. A tool that declares nothing is `NonIdempotent` and runs once, and
a configured `ActionLedger` records its calls.

```rust
use adk_tool::{FunctionTool, ToolEffect};
use serde_json::json;

fn tools() -> (FunctionTool, FunctionTool) {
    let lookup = FunctionTool::new("lookup", "Look up data", |_ctx, args| async move {
        Ok(json!({ "query": args["query"] }))
    })
    .with_read_only(true) // effect: ReadOnly
    .with_concurrency_safe(true);

    let set_status = FunctionTool::new("set_status", "Set a ticket's status", |_ctx, args| async move {
        Ok(json!({ "status": args["status"] }))
    })
    .with_effect(ToolEffect::Idempotent); // retried under a RetryBudget

    (lookup, set_status)
}
```

### StatefulTool

Wrap shared state with a tool handler — the `Arc<S>` is cloned per invocation:

```rust
use adk_tool::StatefulTool;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

struct Counter {
    count: RwLock<u64>,
}

fn counter_tool() -> StatefulTool<Counter> {
    let state = Arc::new(Counter { count: RwLock::new(0) });
    StatefulTool::new("increment", "Increment counter", state, |s, _ctx, _args| async move {
        let mut count = s.count.write().await;
        *count += 1;
        Ok(json!({ "count": *count }))
    })
}
```

### SimpleToolContext

Call tools outside the agent loop (testing, MCP servers, sub-agent delegation):

```rust
use adk_tool::{SimpleToolContext, Tool};
use serde_json::{Value, json};
use std::sync::Arc;

async fn call(tool: &dyn Tool) -> adk_tool::Result<Value> {
    let ctx = SimpleToolContext::new("my-test-harness").with_session_id("session-1");
    tool.execute(Arc::new(ctx), json!({ "key": "value" })).await
}
```

Defaults: `user_id()` → `"anonymous"`, `session_id()` → `""` (set it with `with_session_id`),
`agent_name()` and `app_name()` → the caller name, and unique UUIDs for the invocation and
function call IDs. `with_acting_context` sets the identity tools see through
`acting_context()`.

### AgentTool

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::Llm;
use adk_tool::{AgentTool, AgentToolFailureMode};
use std::sync::Arc;
use std::time::Duration;

fn coordinator(model: Arc<dyn Llm>) -> adk_core::Result<adk_agent::LlmAgent> {
    let math = LlmAgentBuilder::new("math_expert")
        .description("Solves mathematical problems")
        .instruction("Solve problems step by step.")
        .model(model.clone())
        .build()?;

    let math_tool = AgentTool::new(Arc::new(math))
        .timeout(Duration::from_secs(120))
        .failure_mode(AgentToolFailureMode::Propagate);

    LlmAgentBuilder::new("coordinator")
        .instruction("Delegate math questions to math_expert.")
        .model(model)
        .tool(Arc::new(math_tool))
        .build()
}
```

- **Timeout** — the calling agent applies `AgentTool::timeout` instead of its own tool timeout;
  without one the delegation runs until it finishes or the run is cancelled. A delegation that
  runs past it fails with an `ErrorCategory::Timeout` error in every failure mode, so
  `on_tool_error`, the circuit breaker, and the action ledger see it.
- **Failure mode** — `ReturnErrorObject` (default) returns a failed delegation as an
  `{"error": ..., "agent": ...}` result; `Propagate` returns the delegate's error with its
  category and code.
- **Held calls** — every call the delegate holds for confirmation holds the caller's
  `AgentTool` call, with one `ToolConfirmationRequest` per delegate call, each under the
  caller's call ID; once decided, the delegate resumes each decided call and it runs once, and
  a call still undecided keeps the delegation held.
- **Result** — the delegate's final text is returned as `"response"`, and provider-side tool
  results such as Gemini's `codeExecutionResult` as `"server_tool_responses"`.

## MCP

### MCP Server Manager (Multi-Server Lifecycle)

Manage a changing registry of local MCP server processes with connection monitoring, bounded restart, configuration persistence, and tool aggregation:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::Llm;
use adk_tool::mcp::manager::{McpServerConfig, McpServerManager};
use std::sync::Arc;
use std::time::Duration;

async fn run(model: Arc<dyn Llm>) -> adk_core::Result<()> {
    // Load from Kiro mcp.json format
    let manager = Arc::new(
        McpServerManager::from_json(
            r#"{
                "mcpServers": {
                    "workspace": {
                        "command": "/opt/company/bin/workspace-mcp",
                        "args": ["--stdio", "--root", "/srv/workspace"],
                        "disabled": false
                    }
                }
            }"#,
        )?
        .with_startup_timeout(Duration::from_secs(30))
        .with_health_check_interval(Duration::from_secs(30))
        .with_grace_period(Duration::from_secs(5)),
    );

    // Start all non-disabled servers concurrently, then watch for crashes
    let results = manager.start_all().await;
    manager.start_monitoring();

    // Use as a Toolset — tools from all running servers are aggregated
    let agent = LlmAgentBuilder::new("agent").model(model).toolset(manager.clone()).build()?;

    // Dynamic management at runtime
    let github = McpServerConfig {
        command: "/opt/company/bin/github-mcp".into(),
        args: vec!["--stdio".into()],
        ..Default::default()
    };
    manager.add_server("github".into(), github.clone()).await?;
    manager.start_server("github").await?;
    manager.update_server("github", github).await?;
    manager.disable_server("github").await?;
    manager.enable_server("github").await?;
    manager.save_json_file("mcp.json").await?;
    manager.remove_server("github").await?;

    // Graceful shutdown: Runner::close leaves a shared manager's servers running
    manager.shutdown().await
}
```

- **Status** — while a server's child completes the MCP handshake it is `ServerStatus::Starting`.
  The handshake runs without holding the registry lock, so other servers keep serving tools,
  status, and lifecycle calls. A handshake that exceeds `with_startup_timeout` (default 30
  seconds) leaves the server `FailedToStart`. Stopping, updating, disabling, or removing a
  `Starting` server cancels its handshake.
- **Monitoring** — `start_monitoring()` checks running servers every
  `with_health_check_interval` (default 30 seconds), marks a server whose connection closed
  `Crashed`, and restarts it with exponential backoff when it has a `restartPolicy`.
- **Name collisions** — a tool name published by more than one server is exposed as
  `{server_id}__{tool_name}`. When a prefixed name still collides, the first server in ID order
  keeps it and the later tool is skipped with a warning; a prefixed name over 64 bytes is skipped.
- **Discovery** — `Toolset::tools` lists every running server concurrently, so one slow server
  does not hold up the others.
- **Close and shutdown** — a manager is typically shared by several agents or runners, so
  `Toolset::close` (and therefore `Runner::close`) leaves its servers running. `stop_server` and
  `shutdown` return once each server's process has exited or the grace period has elapsed.

Use absolute, versioned executable paths in deployment configuration. The
manager preserves `autoApprove` for configuration compatibility but does not
turn that field into authorization or human approval policy.

### MCP Tools (Local Server via stdio)

Connect to local MCP servers running as child processes:

```rust
use adk_core::Toolset;
use adk_tool::{
    McpToolset,
    mcp::rmcp::{ServiceExt, transport::TokioChildProcess},
};
use std::sync::Arc;
use tokio::process::Command;

async fn connect() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new("/opt/company/bin/workspace-mcp");
    cmd.arg("--stdio").arg("--root").arg("/srv/workspace");

    let client = ().serve(TokioChildProcess::new(cmd)?).await?;

    let toolset = Arc::new(
        McpToolset::new(client)
            .with_name("filesystem-tools")
            .with_filter(|name| matches!(name, "read_file" | "write_file")),
    );

    // ... use the toolset with an agent ...

    // Ends the MCP session and waits for the server process to exit
    toolset.close().await;
    Ok(())
}
```

A successful result with no content reaches the model as `{"output": ""}`. A tool that the
filter excludes is not exposed, and `call_tool_value` refuses it without contacting the server.

`McpToolset` implements `Toolset::close`, which `Runner::close` calls for every toolset of the
runner's agents. It cancels the remote tasks the toolset started and its rmcp session, then
waits for the transport to close: a stdio server's process has exited (rmcp kills one still
running 3 seconds after its input closed), and a Streamable HTTP session is ended with
`DELETE`. The wait is bounded by `with_close_timeout` (5 seconds by default), and closing again
is harmless. A closed toolset, its clones and its tools fail later requests with the
`tool.mcp.closed` error and do not reconnect through a connection factory. `FilteredToolset`,
`MergedToolset` and `PrefixedToolset` close their inner toolsets.
[`examples/mcp_close`](https://github.com/zavora-ai/adk-rust/tree/main/examples/mcp_close)
covers each case.

### MCP Schema and Description Limits

Discovery skips a tool whose input or output schema exceeds `McpSchemaLimits`
(256 KiB and 10 000 JSON values by default), logs a warning naming the toolset
and the tool, and registers the remaining tools. A server-supplied description
longer than `max_description_bytes` (8 KiB by default) is cut at a character
boundary with a warning, and the tool is kept. Raise the limits for a trusted
server:

```rust
use adk_tool::mcp::{McpSchemaLimits, McpToolset};

fn admit_larger_schemas(toolset: McpToolset) -> McpToolset {
    toolset.with_schema_limits(
        McpSchemaLimits::default()
            .with_max_bytes(1024 * 1024)
            .with_max_nodes(40_000)
            .with_max_description_bytes(32 * 1024),
    )
}
```

`McpServerManager::with_schema_limits` applies the same limits to every managed
server.

### MCP Discovery Cache and Concurrency

`McpToolset` reuses a `tools/list` result for 60 seconds, dropping it early on a
reconnect, on `notifications/tools/list_changed` (connections served by
`AdkClientHandler`), or on `invalidate_tool_list_cache()`, which also clears
rmcp's response cache and keeps a fetch already in flight from caching its
result. Set the TTL with `with_tool_list_cache_ttl`; `Duration::ZERO` disables
the cache.

Every catalog listing (`tools/list`, `resources/list`, `resources/templates/list`,
`prompts/list`) is bounded by `McpDiscoveryLimits`: at most 64 pages, no repeated
cursor, and 30 seconds including reconnects by default. A listing that breaks a
bound fails with `AdkError::Tool`. Set them with `with_discovery_limits` on
`McpToolset`, `McpServerManager`, `ConnectionRefresher`, or `SimpleClient`.

Discovery admits a tool name once per server and skips names that no model can
call: empty, containing control characters, or longer than 64 bytes. Each
skipped tool logs a `warn!` with the escaped name.

Calls and discovery on one toolset run concurrently over its shared connection.
A `tools/call` dropped before it completes, such as by an agent tool timeout,
sends `notifications/cancelled` to the server, or `tasks/cancel` when the server
turned the call into a task.

### MCP Tools (Remote Server via HTTP)

Connect to remote MCP servers using HTTP transport (requires `http-transport` feature):

```rust
use adk_tool::{McpHttpClientBuilder, McpToolset};
use std::time::Duration;

// Connect to a service owned by your organization or integration provider
async fn connect() -> adk_tool::Result<McpToolset> {
    McpHttpClientBuilder::new("https://mcp.example.com/mcp")
        .timeout(Duration::from_secs(30))
        .connect()
        .await
}
```

### MCP Authentication

Connect to authenticated MCP servers:

```rust
use adk_tool::{McpAuth, McpHttpClientBuilder, OAuth2Config};
use std::time::Duration;

async fn connect_all() -> Result<(), Box<dyn std::error::Error>> {
    // Static bearer token supplied by your deployment identity system
    let bearer = McpHttpClientBuilder::new("https://mcp.example.com/mcp")
        .with_auth(McpAuth::bearer(std::env::var("MCP_TOKEN")?))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    // API key in custom header
    let api_key = McpHttpClientBuilder::new("https://mcp.example.com/v1")
        .with_auth(McpAuth::api_key("X-API-Key", std::env::var("MCP_API_KEY")?))
        .connect()
        .await?;

    // OAuth2 client credentials flow
    let oauth_config = OAuth2Config::new("your-client-id", "https://auth.example.com/oauth/token")
        .with_secret(std::env::var("MCP_CLIENT_SECRET")?)
        .with_scopes(vec!["mcp:read".into(), "mcp:write".into()]);

    let oauth = McpHttpClientBuilder::new("https://mcp.example.com/v1")
        .with_auth(McpAuth::oauth2(oauth_config))
        .connect()
        .await?;
    Ok(())
}
```

With OAuth2, each request carries the current access token. The token is
refreshed before the `expires_in` its token response declared, and a request the
server rejects with HTTP 401 is sent once more with a newly fetched token. The
token URL must use `https`; plain `http` is accepted only for a loopback host.

`McpHttpClientBuilder::with_credential_broker(broker, acting, scopes)` takes the bearer from an
`adk_core::CredentialBroker` instead: the broker mints a token bound to the endpoint's audience
for the given acting agent, and the token is attached only to requests inside that audience.

### MCP Task Support (Long-Running Operations)

Enable the negotiated MCP 2026-07-28 SEP-2663 Tasks extension for long-running
tool operations:

```rust
use adk_tool::{McpTaskConfig, McpToolset};
use std::time::Duration;

fn with_tasks(toolset: McpToolset) -> McpToolset {
    toolset.with_task_support(
        McpTaskConfig::enabled()
            .poll_interval(Duration::from_secs(2))
            .timeout(Duration::from_secs(300))
            .max_attempts(100),
    )
}
```

Task mode needs two settings: the client declares the extension during the
handshake (`AdkClientHandler::with_tasks()`), and the toolset enables polling
(`with_task_support`). The server, which must advertise the extension, decides
per call whether to return a task; without the client declaration it answers
inline.
ADK-Rust polls `tasks/get`, fulfils paused `input_required` requests through the
configured elicitation handler and `tasks/update`, and requests `tasks/cancel`
when the local timeout or poll bound is reached.

Managed servers select lifecycle and task policy in `mcp.json`:

```json
{
  "mcpServers": {
    "controlled-server": {
      "command": "controlled-server",
      "lifecycle": "discover",
      "taskConfig": {
        "enableTasks": true,
        "pollIntervalMs": 500,
        "timeoutMs": 300000,
        "maxInputRounds": 10
      }
    },
    "third-party-server": {
      "command": "third-party-server",
      "lifecycle": "auto"
    }
  }
}
```

`discover` requires the stateless 2026 lifecycle. `auto` probes discovery and
falls back only on `METHOD_NOT_FOUND`. The default is `initialize`.
Stateless MRTR elicitation is driven automatically and
bounded by `maxInputRounds`; opaque `requestState` is echoed without inspection.

### MCP Auto-Reconnect (Connection Resilience)

A `ConnectionFactory` creates the same concrete `rmcp::RunningService` again after a
retryable failure. Give one to `McpToolset::with_connection_factory`, or wrap a single
connection in a `ConnectionRefresher`. Configure bounded attempts and delay with
`RefreshConfig`. For a changing set of local stdio processes, prefer
`McpServerManager`, whose registry, monitoring, restart, and persistence model
is easier to operate.

```rust
use adk_tool::mcp::{
    ConnectionFactory, ConnectionRefresher, McpToolset,
    rmcp::{RoleClient, ServiceExt, service::RunningService, transport::TokioChildProcess},
};
use std::sync::Arc;
use tokio::process::Command;

struct WorkspaceServer;

#[adk_tool::async_trait]
impl ConnectionFactory<()> for WorkspaceServer {
    async fn create_connection(&self) -> Result<RunningService<RoleClient, ()>, String> {
        let mut cmd = Command::new("/opt/company/bin/workspace-mcp");
        cmd.arg("--stdio");
        let transport = TokioChildProcess::new(cmd).map_err(|e| e.to_string())?;
        ().serve(transport).await.map_err(|e| e.to_string())
    }
}

async fn connect() -> Result<McpToolset, String> {
    let factory = Arc::new(WorkspaceServer);

    // A single connection with reconnect; tool calls replay only after opting in.
    let refresher = ConnectionRefresher::new(factory.create_connection().await?, factory.clone())
        .with_tool_call_retries();
    let tools = refresher.list_tools().await?.value;

    // A toolset that replaces a dead connection before its next request.
    let client = factory.create_connection().await?;
    Ok(McpToolset::new(client).with_connection_factory(factory))
}
```

The refresher reconnects on these error variants, never on the text of a
server's response:
- `Transport closed`, reported when a stdio server process exits
- Broken pipe, connection reset or aborted, and unexpected EOF I/O errors
- An interrupted Streamable HTTP stream or an expired session (HTTP 404)
- An HTTP 401 rejection, so a factory can reconnect with fresh credentials

An HTTP 5xx or a JSON-RPC error is an answer from a live session and is
returned unchanged. `should_refresh_connection` exposes the same decision for an
`rmcp::ServiceError`.

Requests through `ConnectionRefresher`, `SimpleClient`, and `McpToolset` share
one connection and run concurrently. A connection whose transport has already
closed is replaced before the next request is sent, so that request is never a
replay. `McpToolset::is_closed` returns `true` once the transport has closed.

Discovery calls reconnect and retry automatically. Discovered tool wrappers
also replay when the server publishes `readOnlyHint: true` or
`idempotentHint: true`, which map to `ToolEffect::ReadOnly` and
`ToolEffect::Idempotent`. `Tool::annotations()` returns the server's
annotations, including `title`, `destructiveHint`, and `openWorldHint`. Missing hints keep replay disabled because a lost
response can leave a mutating tool's external result uncertain. The direct
`call_tool_value` and `ConnectionRefresher::call_tool` paths do not have
discovered per-tool metadata; opt them in with `with_tool_call_retries()` only for
read-only tools or operations protected by a stable provider idempotency guarantee.

MCP annotations are server-published hints. Trust them only for servers inside
the application's security boundary.

### MCP examples and guides

[`examples/mcp_manager`](https://github.com/zavora-ai/adk-rust/tree/main/examples/mcp_manager)
runs a real Rust stdio server locally and verifies
discovery, tool execution, dynamic registry changes, persistence, and shutdown
without a package download or network dependency.
[`examples/mcp_elicitation`](https://github.com/zavora-ai/adk-rust/tree/main/examples/mcp_elicitation)
demonstrates a server asking its client application for additional information.

The [MCP guide](https://github.com/zavora-ai/adk-rust/tree/main/docs/official_docs/mcp)
covers client construction, server authoring,
dynamic management, security, testing, resources, prompts, completion,
reconnect-safe subscriptions with `ResourceNotificationHandler`, elicitation,
and tasks.

## Google Search

```rust
use adk_tool::GoogleSearchTool;

// Add to a Gemini agent with `.tool(Arc::new(search))` to enable grounded web search.
fn search() -> GoogleSearchTool {
    GoogleSearchTool::new()
}
```

## BigQuery and Spanner (`bigquery`, `spanner` features)

`bigquery_execute_sql` is **read-only by default**: it accepts a single `SELECT` or
`WITH … SELECT` statement, a BigQuery dry run must classify it as `SELECT` before it
runs, and the tool reports `is_read_only() == true`. `spanner_execute_sql` runs in a
read-only single-use transaction.

```rust
use adk_tool::bigquery::BigQueryToolset;

fn toolsets() -> (BigQueryToolset, BigQueryToolset) {
    let read_only = BigQueryToolset::with_project("my-gcp-project");
    // Allow DML, DDL, and scripts; the tool then reports itself as mutating.
    let read_write = BigQueryToolset::with_project("my-gcp-project").with_read_only(false);
    (read_only, read_write)
}
```

Service-account keys resolved through `from_secret` are parsed in memory and never
written to disk. Spanner tools close their client on every exit, including failed queries.

## Code Execution Tools (`code` feature)

Language-preset tool wrappers over the `adk-code` execution substrate:

| Tool | Backend | Required scopes |
|------|---------|-----------------|
| `CodeTool` | Rust, through the `adk_code::RustExecutor` passed to `CodeTool::new` | `code:execute`, `code:execute:rust` |
| `JavaScriptCodeTool` | Embedded JavaScript (`code-embedded-js`) | `code:execute` |
| `PythonCodeTool` | CPython in a `python:3.12-slim` container through the `docker` CLI | `code:execute`, `code:execute:container` |
| `MontyPythonCodeTool` | In-process Python via Monty (`code-embedded-python`) | `code:execute` |

`MontyPythonCodeTool` runs model-written Python in the Monty interpreter — no container, no subprocess. It supports one-shot mode (fresh interpreter per call) and REPL mode (state persists across calls, scoped per ADK session), with host-granted filesystem/environment/clock access and registered host functions. Its LLM-facing description is composed from the executor's own capability report, so it always matches the built environment. For the full Python ecosystem (pip packages, C extensions, the complete standard library), use the container-backed `PythonCodeTool` instead.

```rust
use adk_code::PathAccess;
use adk_tool::MontyPythonCodeTool;

fn python_tool() -> Result<MontyPythonCodeTool, Box<dyn std::error::Error>> {
    let tool = MontyPythonCodeTool::builder()
        .allow_path("/data", "/srv/agent/data", PathAccess::ReadOnly)
        .environ_var("PROJECT", "acme")
        .system_clock()
        .build_repl()?;
    Ok(tool)
}
```

## Features

| Feature | Description |
|---------|-------------|
| `mcp` | Local MCP clients via stdio, `McpToolset`, and `McpServerManager` |
| `http-transport` | Remote MCP servers via streamable HTTP, with bearer, API-key, OAuth2, and brokered auth (implies `mcp`) |
| `mcp-sampling` | `LlmSamplingHandler` for server-initiated `sampling/createMessage`; deprecated upstream (implies `mcp`) |
| `memory-tools` | `LoadMemoryTool` and `PreloadMemoryTool` over `adk-memory` |
| `graph-memory-tools` | `GraphMemoryToolset`, `RememberTool`, and `RelateTool` (implies `memory-tools`) |
| `code` | Code execution tools over the `adk-code` substrate |
| `code-embedded-js` | `JavaScriptCodeTool` live path (boa_engine) |
| `code-embedded-python` | `MontyPythonCodeTool` live path (Monty interpreter) |
| `code-docker` | `adk-code`'s Docker SDK executor for persistent containers |
| `slack` | `SlackToolset`: send messages, read channels, reactions, threads |
| `bigquery` | `BigQueryToolset`: SQL execution (read-only by default), schemas, datasets, tables |
| `spanner` | `SpannerToolset`: read-only SQL execution, schemas, tables |
| `example-store` | Vertex AI Example Store client (v1beta1 data plane) and `ExampleStoreProvider` few-shot retrieval |
| `vertex-agent-registry` | Google Agent Registry client and `AgentSearchTool` |

No feature is enabled by default.

## Toolset Composition

Compose, filter, and namespace toolsets for complex agent configurations:

```rust
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Tool, Toolset};
use adk_tool::{BasicToolset, FilteredToolset, MergedToolset, PrefixedToolset, string_predicate};
use std::sync::Arc;

fn agent(
    model: Arc<dyn Llm>,
    weather_tools: Vec<Arc<dyn Tool>>, // get_weather, get_forecast
    util_tools: Vec<Arc<dyn Tool>>,    // search, calculate
) -> adk_core::Result<LlmAgent> {
    // Group tools into named toolsets
    let weather = Arc::new(BasicToolset::new("weather", weather_tools));
    let utils = Arc::new(BasicToolset::new("utils", util_tools));

    // Filter: expose only specific tools from a toolset
    let filtered = FilteredToolset::new(weather.clone(), string_predicate(vec!["get_weather".into()]));

    // Or use a custom predicate
    let custom = FilteredToolset::with_name(
        weather.clone(),
        Box::new(|tool| tool.name().starts_with("get_")),
        "get_only",
    );

    // Merge: combine multiple toolsets (first-wins deduplication)
    let merged = MergedToolset::new("all_tools", vec![weather.clone(), utils.clone()]);

    // Prefix: namespace tool names to avoid collisions
    let prefixed = PrefixedToolset::new(weather.clone(), "wx"); // wx_get_weather, wx_get_forecast

    // Chain them: prefix → filter → merge
    let composed = MergedToolset::new(
        "composed",
        vec![
            Arc::new(PrefixedToolset::new(weather, "wx")) as Arc<dyn Toolset>,
            Arc::new(FilteredToolset::new(utils, string_predicate(vec!["search".into()]))),
        ],
    );

    // Register with an agent
    LlmAgentBuilder::new("agent").model(model).toolset(Arc::new(composed)).build()
}
```

All composition utilities implement `Toolset` and work with any `Toolset` implementation
including `McpToolset` and `BrowserToolset`. `PrefixedToolset` forwards each wrapped tool's
`is_read_only`, `is_concurrency_safe`, effect, and annotations.

## rmcp compatibility

ADK-Rust 3 uses `rmcp` 3, the official Rust SDK (`adk-tool` requires `rmcp = "3.1"`; the
workspace lockfile resolves 3.5.1). `McpToolset::new(client)`
remains the primary adapter. Advanced server authoring, transports, protocol
extensions, and SDK types are available through `adk_tool::mcp::rmcp`, keeping
them on the same version used internally.

### Protocol revisions

The default connection sends `initialize` and advertises `2025-11-25`, the latest
revision with an `initialize` handshake (rmcp's `ProtocolVersion::LATEST_WITH_INITIALIZE`).
Servers of both generations answer it, so one client reaches both.

`2026-07-28` also adds a stateless `server/discover` handshake. It is opt-in,
because a server that predates it is free to refuse an unknown method with
something other than `METHOD_NOT_FOUND`, and the SDK treats only that one code
as proof of a legacy peer. Select it per connection:

| Mode | Sends first | Against an older server |
|------|-------------|-------------------------|
| `Initialize` (default) | `initialize` | Works |
| `Auto` | `server/discover` | Falls back only on `METHOD_NOT_FOUND` |
| `Discover` | `server/discover` | Fails; no fallback |

```rust
use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, ClientLifecycleMode, ClientServiceExt,
    McpToolset,
    rmcp::{model::ProtocolVersion, transport::TokioChildProcess},
};
use std::sync::Arc;
use tokio::process::Command;

async fn connect() -> Result<McpToolset<AdkClientHandler>, Box<dyn std::error::Error>> {
    let transport = TokioChildProcess::new(Command::new("/opt/company/bin/workspace-mcp"))?;
    let client = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler))
        .serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await?;
    Ok(McpToolset::new(client))
}
```

### Tasks

SEP-2663 replaced the experimental task design. A tool no longer declares
whether it supports task execution; the server decides per call, and the client
reads the response to find out. `Tool::is_long_running` therefore reports per
connection rather than per tool: it is true when tasks are enabled and the server
negotiated them.

Sampling, roots, and logging are deprecated upstream by SEP-2577. The
`mcp-sampling` feature exists for compatible deployments and should not be the
default design for a new system.

`LlmSamplingHandler` meters each sampling request against the budget and spend ledger
of the run whose tool call is in flight on the connection. Requests outside any run
count against the scope attached with `McpToolset::with_sampling_scope`, or are
unmetered without one. A request the budget or spend ledger refuses returns that error
unchanged.

When migrating code that imports `rmcp` types directly, align it to `rmcp 3.1`
or import the SDK through `adk_tool::mcp::rmcp`.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Tool` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agents that use tools
- [adk-code](https://crates.io/crates/adk-code) - Code execution substrate behind the `code` tools

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
