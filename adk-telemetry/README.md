# adk-telemetry

OpenTelemetry integration for Rust Agent Development Kit (ADK-Rust) agent observability.

[![Crates.io](https://img.shields.io/crates/v/adk-telemetry.svg)](https://crates.io/crates/adk-telemetry)
[![Documentation](https://docs.rs/adk-telemetry/badge.svg)](https://docs.rs/adk-telemetry)
[![License](https://img.shields.io/crates/l/adk-telemetry.svg)](../LICENSE)

## Overview

`adk-telemetry` provides observability infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)), built on `tracing`, OpenTelemetry 0.32, and tracing-opentelemetry 0.33:

- **Logging** - Structured logging with tracing-subscriber
- **Tracing** - OTLP trace export over gRPC (tonic 0.14) with the `otlp` feature
- **Metrics** - OTLP metrics export with the `otlp` feature
- **In-process spans** - `AdkSpanExporter` keeps recent spans for the server's debug endpoints
- **SQLite and Google Cloud export** - behind the `sqlite` and `gcp` features
- **GenAI Semantic Conventions** - OTel GenAI semconv v1.41.0 attributes:
  - `GenAiSpanBuilder` — fluent API for model call spans with `gen_ai.*` attributes
  - `GenAiResponseRecorder` — records response model, finish reasons, token usage
  - `GenAiProvider` / `GenAiOperation` enums for all supported providers
  - `map_finish_reason()` — provider-specific finish reason mapping
  - `ContentEventEmitter` — opt-in prompt/completion capture
  - Feature: `genai-semconv` (enabled by default)

## Installation

```toml
[dependencies]
adk-telemetry = "3.0.0"
```

| Feature | Default | Enables |
|---------|---------|---------|
| `genai-semconv` | yes | The `semconv`, `config`, and `events` modules |
| `otlp` | no | `init_with_otlp`, `build_otlp_layer`, and the OpenTelemetry re-exports (`global`, `Meter`) |
| `metrics` | no | Alias that implies `otlp` |
| `json` | no | JSON log formatting |
| `sqlite` | no | `init_with_sqlite`, `SqliteSpanExporter`, `SqliteTraceReader` |
| `gcp` | no | `init_with_gcp` and `init_json_logging` (implies `otlp` and `json`) |

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["telemetry"] }
```

## Quick Start

```rust
use adk_telemetry::init_telemetry;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize with service name
    init_telemetry("my-agent")?;

    // Your agent code here...
    Ok(())
}
```

## Configuration

Set the `RUST_LOG` environment variable:

```bash
# Debug logging for ADK
RUST_LOG=adk=debug cargo run

# Trace level for specific modules
RUST_LOG=adk_agent=trace,adk_model=debug cargo run
```

`RUST_LOG` controls console verbosity. Runtime span collection configured by
`init_with_adk_exporter` is independent, so production settings such as
`RUST_LOG=warn` do not disable the server's session telemetry.

## OpenTelemetry Export

Configure OTLP export for distributed tracing (`otlp` feature; `adk-rust` forwards it as `telemetry-otlp`):

```rust
use adk_telemetry::init_with_otlp;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_with_otlp("my-agent", "http://localhost:4317")?;
    
    // Your agent code here...
    Ok(())
}
```

## SQLite Export (zero infrastructure)

Persist spans directly to a local SQLite file — no collector or backend to
deploy. Enable the `sqlite` feature (`adk-rust` forwards it as
`telemetry-sqlite`):

```toml
adk-telemetry = { version = "3.0.0", features = ["sqlite"] }
```

```rust
use adk_telemetry::init_with_sqlite;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let exporter = init_with_sqlite("my-agent", "traces.db")?;

    // Your agent code here...

    exporter.flush()?; // ensure everything is committed before exit
    Ok(())
}
```

Spans are written by a background thread (batched transactions, WAL mode), so
the traced code path never blocks on I/O. By default agent-loop and portable
team spans are stored (`agent.execute`, `call_llm`, `send_data`,
`execute_tool*`, and `team.*`);
`SqliteSpanExporter::new(path)?.record_all_spans(true)` keeps everything the
subscriber's telemetry-layer filter lets through.

Read traces back with `SqliteTraceReader` (or any SQLite client — the schema
is one `spans` table with an `attributes` JSON column):

```rust
use adk_telemetry::sqlite::SqliteTraceReader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let reader = SqliteTraceReader::open("traces.db")?;
    for session in reader.sessions()? {
        println!("{}: {} spans", session.session_id, session.span_count);
        for span in reader.session_trace(&session.session_id)? {
            println!("  {} ({} ms)", span.span_name, span.duration_nanos() / 1_000_000);
        }
    }
    Ok(())
}
```

## Telemetry to Google Cloud (`gcp` feature)

Export traces straight to Google Cloud Observability and emit Cloud
Logging-parseable JSON logs. `adk-rust` forwards it as `gcp-telemetry`,
and the `gemini-agent-platform` meta-feature includes it:

```toml
adk-telemetry = { version = "3.0.0", features = ["gcp"] }
```

```rust
use adk_telemetry::init_with_gcp;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Requires GOOGLE_CLOUD_PROJECT and Application Default Credentials.
    init_with_gcp("my-agent").await?;

    // Your agent code here...

    adk_telemetry::shutdown_telemetry();
    Ok(())
}
```

Spans go to `https://telemetry.googleapis.com` with per-request
`Authorization: Bearer` headers minted from ADC (refreshed in the
background), plus `x-goog-user-project`. Resource attributes
(`service.name`, `gcp.project_id`, `cloud.platform = gcp.agent_engine`) are
detected from `K_SERVICE`, `GOOGLE_CLOUD_PROJECT`, and
`GOOGLE_CLOUD_AGENT_ENGINE_ID`. `init_json_logging()` installs the Cloud
Logging JSON format standalone. See
[docs/official_docs/observability/gcp.md](../docs/official_docs/observability/gcp.md)
for the collector-sidecar fallback.

## Available Functions

| Function | Description |
|----------|-------------|
| `init_telemetry(service_name)` | Basic console logging |
| `init_with_otlp(service_name, endpoint)` | OTLP trace and metrics export to collectors (`otlp` feature) |
| `build_otlp_layer(service_name, endpoint)` | The OTLP tracing layer, for a subscriber you assemble (`otlp` feature) |
| `init_with_adk_exporter(service_name)` | ADK-style span exporter |
| `init_with_sqlite(service_name, db_path)` | Direct SQLite span export (`sqlite` feature) |
| `init_with_gcp(service_name)` | OTLP trace export to Google Cloud with ADC auth (`gcp` feature) |
| `init_json_logging()` | Cloud Logging structured JSON on stdout (`gcp` feature) |
| `shutdown_telemetry()` | Flush and shut down every tracer and meter provider this crate built, including the one held by the tracing layer |

`AdkSpanExporter` retains at most `DEFAULT_MAX_SPANS` (10,000) spans, evicting the least recently stored; `with_max_spans` and `with_ttl` change the bound. Child spans inherit `adk.app_name` and `adk.user_id` from `agent.execute`, so the server's debug routes can check the owner of every span. Model and tool payloads are omitted from spans and logs unless `RunConfig::record_payloads` is set.

## Span Helpers

Pre-configured spans for instrumenting ADK operations:

| Function | Description |
|----------|-------------|
| `agent_run_span(name, invocation_id)` | Agent execution span |
| `model_call_span(model_name)` | Model API call span |
| `llm_generate_span(provider, model, stream)` | LLM generation span with `gen_ai.usage.*` fields |
| `tool_execute_span(tool_name)` | Tool execution span |
| `callback_span(callback_type)` | Callback execution span |
| `team_run_span`, `team_member_span`, `team_relationship_span` | Portable team spans (`team.*`) |
| `record_llm_usage(&usage)` | Record token counts on the current span |

### Token Usage Tracking

`llm_generate_span` pre-declares OpenTelemetry GenAI semantic convention fields. After receiving a response, call `record_llm_usage` to populate them:

```rust
use adk_telemetry::{LlmUsage, llm_generate_span, record_llm_usage};

fn main() {
    let span = llm_generate_span("openai", "gpt-5.6-terra", true);
    let _enter = span.enter();

    // After receiving the LLM response:
    record_llm_usage(&LlmUsage {
        input_tokens: 100,
        output_tokens: 50,
        total_tokens: 150,
        cache_read_tokens: Some(80),
        ..Default::default()
    });
}
```

Recorded fields: `gen_ai.usage.input_tokens`, `output_tokens`, `total_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `thinking_tokens`, `audio_input_tokens`, `audio_output_tokens`.

## Re-exports

Convenience re-exports from `tracing`:

```rust
#[allow(unused_imports)]
use adk_telemetry::{Span, debug, error, info, instrument, trace, warn};

fn main() {}
```

### OpenTelemetry Dependency Versions

| Crate | Version |
|-------|---------|
| `opentelemetry` | 0.32 |
| `opentelemetry_sdk` | 0.32 |
| `opentelemetry-otlp` | 0.32 |
| `tracing-opentelemetry` | 0.33 |
| `tonic` | 0.14 |

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| `AdkSpanExporter` keeps at most `DEFAULT_MAX_SPANS` (10,000) spans | `with_max_spans` and `with_ttl` change the bound |
| `shutdown_telemetry` flushes and shuts down every provider this crate built | — |
| Child spans carry `adk.app_name` and `adk.user_id` from `agent.execute` | — |
| Model and tool payloads stay out of spans and logs unless `RunConfig::record_payloads` is set | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core traits and types

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
