# Telemetry Demo

Demonstrates all ADK telemetry and tracing capabilities using real Gemini API calls.

## Requirements

Set `GOOGLE_API_KEY` or `GEMINI_API_KEY` in your environment or `.env` file.

## What's covered

1. Console logging initialization with `init_with_adk_exporter`
2. Structured logging at all levels (trace, debug, info, warn, error)
3. Real non-streaming LLM call with automatic token usage recording via `with_usage_tracking`
4. Real streaming LLM call with incremental usage on final chunk
5. Manual `llm_generate_span` + `record_llm_usage` for custom providers
6. Pre-configured spans: `agent_run_span`, `tool_execute_span`, `callback_span`
7. Context attribute propagation via `add_context_attributes`
8. Nested span hierarchy (agent → model → tool) with real LLM call
9. ADK span exporter for programmatic span access
10. OpenTelemetry metrics (counters, histograms)

## Run

```bash
export GOOGLE_API_KEY=your-key
cargo run -p telemetry-demo
```

For verbose output:

```bash
RUST_LOG=debug cargo run -p telemetry-demo
```

## OTLP export

To export to Jaeger or another OTLP collector, swap `init_with_adk_exporter` for `init_with_otlp`:

```rust
adk_telemetry::init_with_otlp("telemetry-demo", "http://localhost:4317")?;
```
