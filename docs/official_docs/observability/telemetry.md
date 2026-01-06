# Telemetry

ADK-Rust provides production-grade observability through the `adk-telemetry` crate, which integrates structured logging and distributed tracing using the `tracing` ecosystem and OpenTelemetry.

## Overview

The telemetry system enables:

- **Structured Logging**: Rich, queryable logs with contextual information
- **Distributed Tracing**: Track requests across agent hierarchies and service boundaries
- **OpenTelemetry Integration**: Export traces to observability backends (Jaeger, Datadog, Honeycomb, etc.)
- **Automatic Context Propagation**: Session, user, and invocation IDs flow through all operations
- **Pre-configured Spans**: Helper functions for common ADK operations

## Quick Start

### Basic Console Logging

For development and simple deployments, initialize console logging:

```rust
use adk_telemetry::init_telemetry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize telemetry with your service name
    init_telemetry("my-agent-service")?;
    
    // Your agent code here
    
    Ok(())
}
```

This configures structured logging to stdout with sensible defaults.

### OpenTelemetry Export

For production deployments with distributed tracing:

```rust
use adk_telemetry::init_with_otlp;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize with OTLP exporter
    init_with_otlp("my-agent-service", "http://localhost:4317")?;
    
    // Your agent code here
    
    // Flush traces before exit
    adk_telemetry::shutdown_telemetry();
    Ok(())
}
```

This exports traces and metrics to an OpenTelemetry collector endpoint.

## Log Levels

Control logging verbosity using the `RUST_LOG` environment variable:

| Level | Description | Use Case |
|-------|-------------|----------|
| `error` | Only errors | Production (minimal) |
| `warn` | Warnings and errors | Production (default) |
| `info` | Informational messages | Development, staging |
| `debug` | Detailed debugging info | Local development |
| `trace` | Very verbose tracing | Deep debugging |

### Setting Log Levels

```bash
# Set global log level
export RUST_LOG=info

# Set per-module log levels
export RUST_LOG=adk_agent=debug,adk_model=info

# Combine global and module-specific levels
export RUST_LOG=warn,adk_agent=debug
```

The telemetry system defaults to `info` level if `RUST_LOG` is not set.

## Logging Macros

Use the standard `tracing` macros for logging:

```rust
use adk_telemetry::{trace, debug, info, warn, error};

// Informational logging
info!("Agent started successfully");

// Structured logging with fields
info!(
    agent.name = "my_agent",
    session.id = "sess-123",
    "Processing user request"
);

// Debug logging
debug!(user_input = ?input, "Received input");

// Warning and error logging
warn!("Rate limit approaching");
error!(error = ?err, "Failed to call model");
```

### Structured Fields

Add contextual fields to log messages for better filtering and analysis:

```rust
use adk_telemetry::info;

info!(
    agent.name = "customer_support",
    user.id = "user-456",
    session.id = "sess-789",
    invocation.id = "inv-abc",
    "Agent execution started"
);
```

These fields become queryable in your observability backend.

## Instrumentation

### Automatic Instrumentation

Use the `#[instrument]` attribute to automatically create spans for functions:

```rust
use adk_telemetry::{instrument, info};

#[instrument]
async fn process_request(user_id: &str, message: &str) {
    info!("Processing request");
    // Function logic here
}

// Creates a span named "process_request" with user_id and message as fields
```

### Skip Sensitive Parameters

Exclude sensitive data from traces:

```rust
use adk_telemetry::instrument;

#[instrument(skip(api_key))]
async fn call_external_api(api_key: &str, query: &str) {
    // api_key won't appear in traces
}
```

### Custom Span Names

```rust
use adk_telemetry::instrument;

#[instrument(name = "external_api_call")]
async fn fetch_data(url: &str) {
    // Span will be named "external_api_call" instead of "fetch_data"
}
```

## Pre-configured Spans

ADK-Telemetry provides helper functions for common operations:

### Agent Execution Span

```rust
use adk_telemetry::agent_run_span;

let span = agent_run_span("my_agent", "inv-123");
let _enter = span.enter();

// Agent execution code here
// All logs within this scope inherit the span context
```

### Model Call Span

```rust
use adk_telemetry::model_call_span;

let span = model_call_span("gemini-2.0-flash");
let _enter = span.enter();

// Model API call here
```

### Tool Execution Span

```rust
use adk_telemetry::tool_execute_span;

let span = tool_execute_span("weather_tool");
let _enter = span.enter();

// Tool execution code here
```

### Callback Span

```rust
use adk_telemetry::callback_span;

let span = callback_span("before_model");
let _enter = span.enter();

// Callback logic here
```

### Adding Context Attributes

Add user and session context to the current span:

```rust
use adk_telemetry::add_context_attributes;

add_context_attributes("user-456", "sess-789");
```

## Manual Span Creation

For custom instrumentation, create spans manually:

```rust
use adk_telemetry::{info, Span};

let span = tracing::info_span!(
    "custom_operation",
    operation.type = "data_processing",
    operation.id = "op-123"
);

let _enter = span.enter();
info!("Performing custom operation");
// Operation code here
```

### Span Attributes

Add attributes dynamically:

```rust
use adk_telemetry::Span;

let span = Span::current();
span.record("result.count", 42);
span.record("result.status", "success");
```

## OpenTelemetry Configuration

### OTLP Endpoint

The OTLP exporter sends traces to a collector endpoint:

```rust
use adk_telemetry::init_with_otlp;

// Local Jaeger (default OTLP port)
init_with_otlp("my-service", "http://localhost:4317")?;

// Cloud provider endpoint
init_with_otlp("my-service", "https://otlp.example.com:4317")?;
```

### Running a Local Collector

For development, run Jaeger with OTLP support:

```bash
docker run -d --name jaeger \
  -p 4317:4317 \
  -p 16686:16686 \
  jaegertracing/all-in-one:latest

# View traces at http://localhost:16686
```

### Trace Visualization

Once configured, traces appear in your observability backend showing:

- Agent execution hierarchy
- Model call latencies
- Tool execution timing
- Error propagation
- Context flow (user ID, session ID, etc.)

## Integration with ADK

ADK-Rust components automatically emit telemetry when the telemetry system is initialized:

```rust
use adk_rust::prelude::*;
use adk_telemetry::init_telemetry;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Initialize telemetry first
    init_telemetry("my-agent-app")?;
    
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    
    let agent = LlmAgentBuilder::new("support_agent")
        .model(model)
        .instruction("You are a helpful support agent.")
        .build()?;
    
    // Use Launcher for simple execution
    Launcher::new(Arc::new(agent)).run().await?;
    
    Ok(())
}
```

The agent, model, and tool operations will automatically emit structured logs and traces.

## Custom Telemetry in Tools

Add telemetry to custom tools:

```rust
use adk_rust::prelude::*;
use adk_telemetry::{info, instrument, tool_execute_span};
use serde_json::{json, Value};

#[instrument(skip(ctx))]
async fn weather_tool_impl(
    ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value> {
    let span = tool_execute_span("weather_tool");
    let _enter = span.enter();
    
    let location = args["location"].as_str().unwrap_or("unknown");
    info!(location = location, "Fetching weather data");
    
    // Tool logic here
    let result = json!({
        "temperature": 72,
        "condition": "sunny"
    });
    
    info!(location = location, "Weather data retrieved");
    Ok(result)
}

let weather_tool = FunctionTool::new(
    "get_weather",
    "Get current weather for a location",
    json!({
        "type": "object",
        "properties": {
            "location": {"type": "string"}
        },
        "required": ["location"]
    }),
    weather_tool_impl,
);
```

## Custom Telemetry in Callbacks

Add observability to callbacks:

```rust
use adk_rust::prelude::*;
use adk_telemetry::{info, callback_span};
use std::sync::Arc;

let agent = LlmAgentBuilder::new("observed_agent")
    .model(model)
    .before_callback(Box::new(|ctx| {
        Box::pin(async move {
            let span = callback_span("before_agent");
            let _enter = span.enter();
            
            info!(
                agent.name = ctx.agent_name(),
                user.id = ctx.user_id(),
                session.id = ctx.session_id(),
                "Agent execution starting"
            );
            
            Ok(None)
        })
    }))
    .after_callback(Box::new(|ctx| {
        Box::pin(async move {
            let span = callback_span("after_agent");
            let _enter = span.enter();
            
            info!(
                agent.name = ctx.agent_name(),
                "Agent execution completed"
            );
            
            Ok(None)
        })
    }))
    .build()?;
```

## Performance Considerations

### Sampling

For high-throughput systems, consider trace sampling:

```rust
// Note: Sampling configuration depends on your OpenTelemetry setup
// Configure sampling in your OTLP collector or backend
```

### Async Spans

Always use `#[instrument]` on async functions to ensure proper span context:

```rust
use adk_telemetry::instrument;

// ✅ Correct - span context preserved across await points
#[instrument]
async fn async_operation() {
    tokio::time::sleep(Duration::from_secs(1)).await;
}

// ❌ Incorrect - manual span may lose context
async fn manual_span_operation() {
    let span = tracing::info_span!("operation");
    let _enter = span.enter();
    tokio::time::sleep(Duration::from_secs(1)).await;
    // Context may be lost after await
}
```

### Log Level in Production

Use `info` or `warn` level in production to reduce overhead:

```bash
export RUST_LOG=warn,my_app=info
```

## Troubleshooting

### No Logs Appearing

1. Check `RUST_LOG` environment variable is set
2. Ensure `init_telemetry()` is called before any logging
3. Verify telemetry is initialized only once (uses `Once` internally)

### Traces Not Exported

1. Verify OTLP endpoint is reachable
2. Check collector is running and accepting connections
3. Call `shutdown_telemetry()` before application exit to flush pending spans
4. Check for network/firewall issues

### Missing Context in Spans

1. Use `#[instrument]` on async functions
2. Ensure spans are entered with `let _enter = span.enter()`
3. Keep the `_enter` guard in scope for the duration of the operation

## Best Practices

1. **Initialize Early**: Call `init_telemetry()` at the start of `main()`
2. **Use Structured Fields**: Add context with key-value pairs, not string interpolation
3. **Instrument Async Functions**: Always use `#[instrument]` on async functions
4. **Flush on Exit**: Call `shutdown_telemetry()` before application termination
5. **Appropriate Log Levels**: Use `info` for important events, `debug` for details
6. **Avoid Sensitive Data**: Skip sensitive parameters with `#[instrument(skip(...))]`
7. **Consistent Naming**: Use consistent field names (e.g., `user.id`, `session.id`)

## Related

- [Callbacks](../callbacks/callbacks.md) - Add telemetry to callbacks
- [Tools](../tools/function-tools.md) - Instrument custom tools
- [Deployment](../deployment/server.md) - Production telemetry setup


---

**Previous**: [← Events](../events/events.md) | **Next**: [Launcher →](../deployment/launcher.md)
