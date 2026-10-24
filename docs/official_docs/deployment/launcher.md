# Launcher

The `Launcher` runs an ADK agent with one line of code. `adk_rust::Launcher` names one of two
launchers, depending on the `cli` feature:

| Feature set | `adk_rust::Launcher` is | Modes |
|-------------|-------------------------|-------|
| default (`minimal`) and other tiers without `cli` | `adk_runner::Launcher` | Console only |
| `cli` (or `cli-openai`, `cli-anthropic`, `cli-deepseek`, `cli-groq`, `cli-ollama`, `cli-all-providers`) | `adk_cli::Launcher` | Console, and an HTTP server with the REST API (`serve`) |

No tier includes `cli`; add it to the tier you use:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["cli"] }
```

The `cli-*` variants also enable a model provider in `adk-cli`.

## Overview

With a single line of code, you can:

- Run your agent in an interactive console for testing and development
- Serve your agent over HTTP (`cli` feature); the [web home](../surfaces/web-home.md) is
  served by `adk-rust serve` and `adk-rust up`, or mounted on a server you build
- Customize the application name and the session, memory, and (`cli` feature) artifact services

## Basic Usage

### Console Mode (Default)

Create the Launcher with your agent and call `run()`:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| AdkError::config("GOOGLE_API_KEY is not set"))?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

    let agent = LlmAgentBuilder::new("my_agent")
        .description("A helpful assistant")
        .instruction("You are a helpful assistant.")
        .model(model)
        .build()?;

    Launcher::new(Arc::new(agent)).run().await
}
```

Run your agent:

```bash
cargo run
```

### Server Mode

With the `cli` feature, the same program serves the agent over HTTP:

```bash
cargo run -- serve
```

The server starts and displays:

```
ADK Server starting on http://127.0.0.1:8080
Open http://localhost:8080 in your browser
Press Ctrl+C to stop
```

To start from a generated server project instead, see the `api` template in
[Server API](server.md#scaffold).

## Configuration Options

| Method | Console launcher | `cli` launcher | Default |
|--------|------------------|----------------|---------|
| `app_name(name)` | yes | console mode; serve mode uses the agent's name | the agent's name |
| `with_session_service(service)` | yes | yes | `InMemorySessionService` |
| `with_memory_service(service)` | yes | yes | none |
| `with_artifact_service(service)` | — | yes | none |
| `with_compaction(config)`, `with_context_cache(config, model)` | — | serve mode | none |
| `with_bind_address(address)` | — | serve mode | `127.0.0.1` |
| `with_security_config(config)`, `with_request_context_extractor(extractor)` | — | serve mode | loopback hosts, no cross-origin access, no authentication |
| `with_a2a_base_url(url)` | — | serve mode | no A2A routes |
| `with_telemetry(config)` | — | serve mode | in-memory ADK span exporter |
| `with_streaming_mode(mode)`, `with_thinking_mode(mode)` | — | console mode | SSE streaming, thinking shown |
| `with_run_config(config)` | — | console mode, merged with the console settings | `RunConfig::default()` |
| `with_tool_policy(policy)` | — | console mode; `build_app` fails while one is set | none |
| `with_confirmation_handler(handler)` | — | console mode | `ConsoleConfirmationHandler` at the terminal |
| `with_user_id(id)` | — | console mode | `user` |
| `with_session_id(id)` | — | console mode: continue the session, or create it under this id | a new session |
| `with_console_command(command)` | — | console mode | none |
| `with_approval_service(service)` | — | serve mode: `/api/approvals` and its expiry ticker | no approval inbox |
| `with_background_runs(state)` | — | serve mode: `/api/runs` | no background runs |

The console settings combine in any order: `with_streaming_mode`, `with_tool_policy` and
`with_confirmation_handler` take precedence over the same fields of `with_run_config`. The
server takes no `RunConfig`, so they do not reach serve mode, and `build_app` returns a
configuration error while a tool policy is set rather than serve without it.

### Custom Application Name

By default, the console uses the agent's name as the application name. Serve mode always
serves the agent under its own name:

```rust
Launcher::new(Arc::new(agent))
    .app_name("my_custom_app")
    .run()
    .await
```

### Custom Artifact Service

With the `cli` and `artifacts` features, provide your own artifact service:

```rust
use adk_rust::artifact::InMemoryArtifactService;

let artifact_service = Arc::new(InMemoryArtifactService::new());

Launcher::new(Arc::new(agent))
    .with_artifact_service(artifact_service)
    .run()
    .await
```

## Console Mode Details

In console mode, the Launcher:

1. Uses the configured session service, or an in-memory one
2. Continues the `with_session_id` session for the `with_user_id` user (default `user`),
   creating it when it does not exist, or creates a new session
3. Starts an interactive read loop
4. Streams agent responses in real time
5. Shows text, thinking, and tool calls as they arrive

`exit`, `quit`, `/exit`, and `/quit` end the session.

### Console Interaction

The console launcher prints:

```
  ADK-Rust — my_agent
  Type a message to chat. Type 'exit' to quit.

You > What is the capital of France?
The capital of France is Paris.

You > exit

Goodbye.
```

The `cli` launcher adds a banner, line editing with history, `/help`, and Ctrl+C to interrupt a
response without leaving the session.

### Approvals in the Console

The `cli` launcher answers tool calls held for confirmation at the terminal. Without
`with_confirmation_handler`, console runs use a `ConsoleConfirmationHandler`, which shows the
call and asks:

```
[tool-call] delete_file {"path":"notes.txt"}

[approval needed] delete_file {"path":"notes.txt"}
  [y] allow once  [n] deny (reason)  [e] edit args  [?] details > y

[approved] delete_file

[tool-response] delete_file {"deleted":"notes.txt"}
```

| Answer | Effect |
|--------|--------|
| `y` | Runs the call once, with the edited arguments when `e` changed them |
| `n` | Denies the call; a reason typed at the follow-up prompt is reported to the model |
| `e` | Replaces the arguments with JSON from `$VISUAL`/`$EDITOR`, or typed on one line, then asks again; the policy checks the edited call again |
| `?` | Shows the call id, the acting agent and subject, the tool's origin, and the full arguments |
| Ctrl+C | Denies the call and cancels the run |

Prompts for calls in one batch are asked one after another. When stdin is not a terminal,
every held call is denied with a message, so an unattended run never waits. Console runs wait
up to `CONSOLE_CONFIRMATION_TIMEOUT` (one hour) for an answer unless the run configuration
sets another `tool_confirmation_timeout`. The handler answers the governed path through
`ToolConfirmationHandler::decide_approval`, so edited arguments and the denial reason travel
in the `ToolApproval`; a direct `decide` call offers only `[y] allow once  [n] deny  [?] details`.

A held call that reaches the console without a decision, for example from a delegate, is
printed as `[approval needed] <tool> <args> — <reason>`, and a decision applied to a call as
`[approved] <tool>` or `[denied] <tool>`.

The REPL and every prompt read through one `ConsoleIo` reader thread, so Ctrl+C at a prompt
leaves no stdin read behind to swallow the next line. A custom handler that prompts at the
terminal should read through `ConsoleIo::terminal()` as well.

```rust
use adk_rust::prelude::*;
use adk_rust::{DeclarativePolicy, Launcher, RunBudget};
use std::sync::Arc;

async fn launch(agent: Arc<dyn Agent>) -> Result<()> {
    // Every tool that is not read-only waits for an answer at the terminal.
    let policy = DeclarativePolicy::builder()
        .allow_read_only()
        .require_approval("*", "this tool changes something")
        .build();

    Launcher::new(agent)
        .with_tool_policy(Arc::new(policy))
        .with_run_config(RunConfig::builder().budget(RunBudget::new().max_model_calls(50)).build())
        .run()
        .await
}
```

### Slash Commands

`with_console_command` registers a `ConsoleCommand`. The REPL runs `/name args` through the
command with that name, prints what it returns, and lists it in `/help`. The built-in `/help`,
`/clear`, `/quit` and `/exit` take precedence, and a slash line no command matches goes to
the agent:

```rust
use adk_cli::{ConsoleCommand, ConsoleCommandContext, Launcher};
use async_trait::async_trait;
use std::sync::Arc;

struct Session;

#[async_trait]
impl ConsoleCommand for Session {
    fn name(&self) -> &str {
        "session"
    }

    fn description(&self) -> &str {
        "Show the session id"
    }

    async fn run(&self, _args: &str, ctx: &ConsoleCommandContext) -> adk_core::Result<String> {
        Ok(format!("session {} for {}", ctx.session_id, ctx.user_id))
    }
}

async fn launch(agent: Arc<dyn adk_core::Agent>) -> adk_core::Result<()> {
    Launcher::new(agent).with_console_command(Arc::new(Session)).run_console_directly().await
}
```

### Multi-Agent Console

The `cli` launcher shows tool calls and results as `[tool-call]` and `[tool-response]` lines, a
transfer as `[transfer -> agent_name]`, and labels the output with `[agent_name]` when another
agent starts responding:

```
You > ORDER-12345

[tool-call] transfer_to_agent {"agent_name":"order_lookup"}
[transfer -> order_lookup]
[tool-response] transfer_to_agent {"transferred_to":"order_lookup"}

[order_lookup] Your order ORDER-12345 shipped yesterday.
```

## Server Mode Details

In server mode, the `cli` launcher:

1. Initializes telemetry: logs to stdout, and runtime spans for the web home's developer panel
2. Uses the configured session service, or an in-memory one
3. Starts an HTTP server with the REST API, on `127.0.0.1` by default
4. Mounts the approval inbox (`with_approval_service`) and background runs
   (`with_background_runs`) when configured
5. Stops on Ctrl+C or SIGTERM

The app is built with `ServerBuilder`. The approval inbox answers `401` unless
`with_request_context_extractor` authenticates the caller; see [Server API](server.md).
`run_serve_directly` serves the router with `into_make_service_with_connect_info`, so an
extractor can read the peer address from `ConnectInfo<SocketAddr>`.

### Production Escape Hatch

For applications that need custom routes, middleware, metrics, or ownership of the serve loop,
`build_app()` returns the Axum router without serving it:

```rust
let app = Launcher::new(Arc::new(agent))
    .with_a2a_base_url("https://agent.example.com")
    .build_app()?;

let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
axum::serve(listener, app).await?;
```

The app answers only loopback names, IP addresses, and the A2A base URL's host, and allows no
cross-origin browser access; see [Host, Origin, and Bind Defaults](server.md#host-origin-and-bind-defaults).
Routes merged into the returned router afterwards do not pass through those checks or the
authentication layer. Register custom routes with `ServerBuilder::add_api_routes` or
`add_root_routes` instead; see [Server API](server.md#serverconfig-and-create_app).

`build_app_with_a2a(base_url)` sets the A2A base URL and builds the app in one call.

### Available Endpoints

The server exposes, among others:

- `GET /api/health` - Health check
- `POST /api/run_sse` - Run the agent with Server-Sent Events streaming
- `POST /api/run` - Run the agent and return every event as one JSON array
- `POST /api/sessions` - Create a session
- `GET /api/sessions/{app_name}/{user_id}/{session_id}` - Get session details
- `DELETE /api/sessions/{app_name}/{user_id}/{session_id}` - Delete a session
- `GET /api/apps/{app_name}/users/{user_id}/sessions` - List a user's sessions

See the [Server API](server.md) documentation for detailed endpoint specifications.

### Web home

Serve mode serves the REST API and no page: nothing answers at `/` or `/ui/`. The
[web home](../surfaces/web-home.md) is the harness's — `adk-rust serve` and `adk-rust up` serve
it — because its **Stop all** freezes every run the server makes and its cards come from an
approval inbox the harness files held calls in. A server you build with `ServerBuilder` mounts
it as [Mount it on your own server](../surfaces/web-home.md#mount-it-on-your-own-server) shows.

## CLI Arguments

The `cli` launcher supports the following commands:

| Command | Description | Example |
|---------|-------------|---------|
| (none) | Interactive console (default) | `cargo run` |
| `chat` | Interactive console (explicit) | `cargo run -- chat` |
| `serve` | HTTP server mode | `cargo run -- serve` |
| `serve --port PORT` | HTTP server on custom port (default `8080`) | `cargo run -- serve --port 3000` |
| `serve --host ADDRESS` | Listen on another address; `127.0.0.1` by default | `cargo run -- serve --host 0.0.0.0` |

`serve` listens on `127.0.0.1` unless `--host` or `Launcher::with_bind_address` says otherwise.
Before listening on every interface, configure authentication and list the server's public host
names in `SecurityConfig::allowed_hosts`. `run_console_directly()` and
`run_serve_directly(port)` start a mode without parsing arguments.

## Complete Example

An agent with a tool that runs in both modes:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["tools", "cli"] }
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize, Serialize, JsonSchema)]
struct WeatherArgs {
    /// City to look up
    location: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| AdkError::config("GOOGLE_API_KEY is not set"))?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get the current weather for a location",
        |_ctx, args| async move {
            let location = args["location"].as_str().unwrap_or("unknown");
            Ok(json!({
                "location": location,
                "temperature": 72,
                "condition": "sunny"
            }))
        },
    )
    .with_parameters_schema::<WeatherArgs>();

    let agent = LlmAgentBuilder::new("weather_agent")
        .description("An agent that provides weather information")
        .instruction("You are a weather assistant. Use the get_weather tool to provide weather information.")
        .model(model)
        .tool(Arc::new(weather_tool))
        .build()?;

    Launcher::new(Arc::new(agent))
        .app_name("weather_app")
        .run()
        .await
}
```

```bash
cargo run              # console
cargo run -- serve     # server on http://127.0.0.1:8080
```

## Pre-Deployment Verification

`cargo adk build` compiles the project without deploying it:

```bash
# Release build (default)
cargo adk build

# Debug build, for faster iteration
cargo adk build --debug
```

This catches compilation errors, missing dependencies, and configuration issues before a
deployment. Use it in CI as a gate before `cargo adk deploy`.

See [cargo adk build](../development/cargo-adk-build.md) for full command documentation.

## Harness Launcher

`adk_cli::harness::HarnessLauncher` (feature `harness`, on by default in `adk-cli`) is the
launcher for a `HarnessAgent` (spec 3.1). It is what the `adk-rust` binary runs:

| Mode | `Launcher` | `HarnessLauncher` |
|------|------------|-------------------|
| Console | REPL over any agent | Harness REPL: banner with workspace, mode and shell; approvals through the harness's prompter (`ConsolePrompter`, with `[a] always allow`); a denial ends the turn; `/mode`, `/sandbox`, `/todos`, `/rules`, `/approvals` and more |
| One turn | — | `run_once(text)` returns the calls held for a decision |
| Server | REST API | The same server with the harness mounted behind an extractor: loopback peers are the operator by default (`LoopbackOperatorExtractor`), other addresses need `with_auth_token` or another extractor; with feature `surface` (on in the `adk-rust` binary), the web home at `/ui/` over the harness's modes, standing rules and inbox, its turns run as the operator, and one kill switch (`with_governance`) for the server's runs, the background runs and the home |

```rust
use std::sync::Arc;

use adk_cli::console::ConsoleIo;
use adk_cli::harness::{ConsolePrompter, HarnessLauncher};
use adk_harness::HarnessAgent;

async fn run(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
    let harness = HarnessAgent::builder("my-harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .prompter(Arc::new(ConsolePrompter::new(ConsoleIo::terminal()?)))
        .build()?;
    HarnessLauncher::new(harness).run().await
}
```

See [The harness in your terminal](../harness/cli.md) and
[Serving the harness](../harness/serving.md).

## Best Practices

1. **Environment Variables**: Load sensitive configuration (API keys) from environment variables
2. **Error Handling**: Use proper error handling with `Result` types
3. **Shutdown**: The `cli` launcher's server stops gracefully on Ctrl+C or SIGTERM; Ctrl+C in its console interrupts the current response
4. **Port Selection**: Choose ports that don't conflict with other services (default 8080)
5. **Session Management**: In production, pass a persistent session service such as `SqliteSessionService` or `PostgresSessionService` to `with_session_service`
6. **Pre-Deployment Check**: Run `cargo adk build` before deploying to catch issues early

## Related

- [Server API](server.md) - Detailed REST API documentation
- [The harness in your terminal](../harness/cli.md) - `adk-rust`, `HarnessLauncher` and approvals
- [Sessions](../sessions/sessions.md) - Session management
- [Artifacts](../artifacts/artifacts.md) - Artifact storage
- [Observability](../observability/telemetry.md) - Telemetry and logging


---

**Previous**: [← Telemetry](../observability/telemetry.md) | **Next**: [Server →](server.md)
