# Launcher

The `Launcher` runs an ADK agent with one line of code. `adk_rust::Launcher` names one of two
launchers, depending on the `cli` feature:

| Feature set | `adk_rust::Launcher` is | Modes |
|-------------|-------------------------|-------|
| default (`minimal`) and other tiers without `cli` | `adk_runner::Launcher` | Console only |
| `cli` (or `cli-openai`, `cli-anthropic`, `cli-deepseek`, `cli-groq`, `cli-ollama`, `cli-all-providers`) | `adk_cli::Launcher` | Console, and an HTTP server with the web UI (`serve`) |

No tier includes `cli`; add it to the tier you use:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["cli"] }
```

The `cli-*` variants also enable a model provider in `adk-cli`.

## Overview

With a single line of code, you can:

- Run your agent in an interactive console for testing and development
- Serve your agent over HTTP with a web UI (`cli` feature)
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
2. Creates a session for the user `user`
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

1. Initializes telemetry: logs to stdout, and runtime spans for the web UI's telemetry view
2. Uses the configured session service, or an in-memory one
3. Starts an HTTP server with the REST API, on `127.0.0.1` by default
4. Serves the web UI
5. Stops on Ctrl+C or SIGTERM

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

### Web UI

The server includes a built-in web UI at `http://localhost:8080/ui/`. The UI provides:

- Interactive chat interface
- Session, state, artifact, and event-timeline inspection
- Real-time streaming responses and tool activity
- Team delegation and handoff topology
- Agent and UI-protocol capability discovery

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
cargo run -- serve     # server and web UI on http://127.0.0.1:8080
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

## Best Practices

1. **Environment Variables**: Load sensitive configuration (API keys) from environment variables
2. **Error Handling**: Use proper error handling with `Result` types
3. **Shutdown**: The `cli` launcher's server stops gracefully on Ctrl+C or SIGTERM; Ctrl+C in its console interrupts the current response
4. **Port Selection**: Choose ports that don't conflict with other services (default 8080)
5. **Session Management**: In production, pass a persistent session service such as `SqliteSessionService` or `PostgresSessionService` to `with_session_service`
6. **Pre-Deployment Check**: Run `cargo adk build` before deploying to catch issues early

## Related

- [Server API](server.md) - Detailed REST API documentation
- [Sessions](../sessions/sessions.md) - Session management
- [Artifacts](../artifacts/artifacts.md) - Artifact storage
- [Observability](../observability/telemetry.md) - Telemetry and logging


---

**Previous**: [← Telemetry](../observability/telemetry.md) | **Next**: [Server →](server.md)
