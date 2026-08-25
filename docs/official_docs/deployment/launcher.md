# Launcher

The `Launcher` provides a simple, one-line way to run ADK agents. In the default minimal tier it is a lightweight console launcher from `adk-runner`. Enable an opt-in CLI feature such as `cli-openai` when you need the full CLI argument parser and HTTP server mode.

## Overview

The Launcher is designed to make agent deployment as simple as possible. With a single line of code, you can:

- Run your agent in an interactive console for testing and development
- Deploy your agent as an HTTP server with a web UI when a `cli-*` feature or the cargo-adk `api` template is used
- Customize the application name and artifact storage

## Basic Usage

### Console Mode (Default)

The simplest way to use the Launcher is to create it with your agent and call `run()`:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);
    
    let agent = LlmAgentBuilder::new("my_agent")
        .description("A helpful assistant")
        .instruction("You are a helpful assistant.")
        .model(model)
        .build()?;
    
    // Run with the lightweight console launcher
    Launcher::new(Arc::new(agent)).run().await
}
```

Run your agent:

```bash
# Interactive console (default)
cargo run

# Full CLI mode is available when your app enables a `cli-*` feature
```

### Server Mode

To run your agent as an HTTP server with a web UI, use the `api` template or enable a `cli-*` feature:

```bash
cargo adk new my-api --template api
cd my-api
cargo run
```

The server will start and display:

```
🚀 ADK Server starting on http://localhost:8080
📱 Open http://localhost:8080 in your browser
Press Ctrl+C to stop
```

## Configuration Options

### Custom Application Name

By default, the Launcher uses the agent's name as the application name. You can customize this:

```rust
Launcher::new(Arc::new(agent))
    .app_name("my_custom_app")
    .run()
    .await
```

### Custom Artifact Service

Provide your own artifact service implementation:

```rust
use adk_artifact::InMemoryArtifactService;

let artifact_service = Arc::new(InMemoryArtifactService::new());

Launcher::new(Arc::new(agent))
    .with_artifact_service(artifact_service)
    .run()
    .await
```

## Console Mode Details

In console mode, the Launcher:

1. Creates an in-memory session service
2. Creates a session for the user
3. Starts an interactive REPL loop
4. Streams agent responses in real-time
5. Handles agent transfers in multi-agent systems

### Console Interaction

```
🤖 Agent ready! Type your questions (or 'exit' to quit).

You: What is the capital of France?
Assistant: The capital of France is Paris.

You: exit
👋 Goodbye!
```

### Multi-Agent Console

When using multi-agent systems, the console shows which agent is responding:

```
You: I need help with my order

[Agent: customer_service]
Assistant: I'll help you with your order. What's your order number?

You: ORDER-12345

🔄 [Transfer requested to: order_lookup]

[Agent: order_lookup]
Assistant: I found your order. It was shipped yesterday.
```

## Server Mode Details

In server mode, the Launcher:

1. Initializes telemetry for observability
2. Creates an in-memory session service
3. Starts an HTTP server with REST API endpoints
4. Serves a web UI for interacting with your agent

### Production Escape Hatch

For production applications that need custom routes, middleware, metrics, or
ownership of the serve loop, use `build_app()`:

```rust
let app = Launcher::new(Arc::new(agent))
    .with_a2a_base_url("https://agent.example.com")
    .build_app()?;

let app = app.merge(my_admin_routes());
let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
axum::serve(listener, app).await?;
```

Use `build_app_with_a2a(...)` if you want the A2A routes enabled explicitly.

### Available Endpoints

The server exposes the following REST API endpoints:

- `GET /health` - Health check endpoint
- `POST /run_sse` - Run agent with Server-Sent Events streaming
- `GET /sessions` - List sessions
- `POST /sessions` - Create a new session
- `GET /sessions/:app_name/:user_id/:session_id` - Get session details
- `DELETE /sessions/:app_name/:user_id/:session_id` - Delete a session

See the [Server API](server.md) documentation for detailed endpoint specifications.

### Web UI

The server includes a built-in web UI accessible at `http://localhost:8080/ui/`. The UI provides:

- Interactive chat interface
- Session, state, artifact, and event-timeline inspection
- Real-time streaming responses and tool activity
- Exact team delegation and handoff topology
- Agent and UI-protocol capability discovery

## CLI Arguments

The full CLI launcher supports the following commands when a `cli-*` feature is enabled:

| Command | Description | Example |
|---------|-------------|---------|
| (none) | Interactive console (default) | `cargo run` |
| `chat` | Interactive console (explicit) | `cargo run -- chat` |
| `serve` | HTTP server mode | `cargo run -- serve` |
| `serve --port PORT` | HTTP server on custom port | `cargo run -- serve --port 3000` |

## Complete Example

Here's a complete example showing both modes:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // Load API key
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");
    
    // Create model
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);
    
    // Create agent with tools
    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get the current weather for a location",
        |params, _ctx| async move {
            let location = params["location"].as_str().unwrap_or("unknown");
            Ok(json!({
                "location": location,
                "temperature": 72,
                "condition": "sunny"
            }))
        },
    );
    
    let agent = LlmAgentBuilder::new("weather_agent")
        .description("An agent that provides weather information")
        .instruction("You are a weather assistant. Use the get_weather tool to provide weather information.")
        .model(model)
        .tool(Arc::new(weather_tool))
        .build()?;
    
    // Run with Launcher. Enable a `cli-*` feature for full CLI/server mode.
    Launcher::new(Arc::new(agent))
        .app_name("weather_app")
        .run()
        .await
}
```

Run in console mode:

```bash
cargo run
```

Run in server mode from a generated API project:

```bash
cargo adk new weather-api --template api
cd weather-api
cargo run
```

## Pre-Deployment Verification

Before deploying your agent, use `cargo adk build` to verify that the project compiles correctly without actually deploying:

```bash
# Verify compilation (no deployment)
cargo adk build

# Build with release optimizations
cargo adk build --release
```

This catches compilation errors, missing dependencies, and configuration issues before you commit to a deployment. It is especially useful in CI pipelines as a gate before `cargo adk deploy`.

See [cargo adk build](../development/cargo-adk-build.md) for full command documentation.

## Best Practices

1. **Environment Variables**: Always load sensitive configuration (API keys) from environment variables
2. **Error Handling**: Use proper error handling with `Result` types
3. **Graceful Shutdown**: The Launcher handles Ctrl+C gracefully in both modes
4. **Port Selection**: Choose ports that don't conflict with other services (default 8080)
5. **Session Management**: In production, consider using `PostgresSessionService` or `SqliteSessionService` instead of in-memory sessions
6. **Pre-Deployment Check**: Run `cargo adk build` before deploying to catch issues early

## Related

- [Server API](server.md) - Detailed REST API documentation
- [Sessions](../sessions/sessions.md) - Session management
- [Artifacts](../artifacts/artifacts.md) - Artifact storage
- [Observability](../observability/telemetry.md) - Telemetry and logging


---

**Previous**: [← Telemetry](../observability/telemetry.md) | **Next**: [Server →](server.md)
