# 0.8.0 Performance Validation Example

> **Note:** This page records the validation example written for the 0.8.0 performance
> release. The example still builds against the current crates; the settings it names are
> described here as they behave today.

The live validation example at `adk-rust/examples/performance_0_8_llm_agents.rs` runs 12 real LLM agents using provider credentials from the environment or the repository `.env`. Each agent maps one 0.8.0 optimization to an adoption scenario and validates the response internally without printing model output:

1. `scaffold_advisor` validates current `cargo-adk` templates for a small support bot.
2. `install_doctor` explains the rustls-only install path for OpenSSL-friction cases.
3. `starter_agent` validates the minimal tier for a simple reminder agent.
4. `cli_provider_advisor` shows provider-specific CLI installs.
5. `telemetry_triage` separates local tracing from OTLP export.
6. `tooling_advisor` keeps MCP out of local function-tool agents.
7. `gemini_debug_advisor` keeps Gemini backtraces debug-only.
8. `session_budgeter` covers empty state-delta session writes.
9. `history_window_support` uses `RunConfig::history_max_events`.
10. `privacy_observer` sets `RunConfig::trace_payload_max_bytes`, the cap on recorded payload fields when `record_payloads` is on; with it off, the default, payloads are not recorded.
11. `operations_dispatcher` sets `RunConfig::tool_concurrency` (`ToolConcurrencyConfig::max_concurrency`).
12. `cache_operator` validates cache lifecycle work that does not hold the cache mutex across network calls.

Run it locally with:

```bash
cargo run -p adk-rust --example performance_0_8_llm_agents --features openrouter
```

The example uses the first provider whose key is set, in this order:

| Key | Feature | Model variable | Default model |
|-----|---------|----------------|---------------|
| `OPENROUTER_API_KEY` | `openrouter` | `OPENROUTER_MODEL` | `openai/gpt-4.1-mini` |
| `ANTHROPIC_API_KEY` | `anthropic` | — | `adk_model::catalog::ANTHROPIC_DEFAULT` |
| `OPENAI_API_KEY` | `openai` | `OPENAI_MODEL` | `gpt-5-mini` |
| `GOOGLE_API_KEY` or `GEMINI_API_KEY` | `gemini` | `GEMINI_MODEL` | `gemini-2.5-flash` |

The Gemini default predates the current model line; set `GEMINI_MODEL=gemini-3.7-flash` when
running it with a Gemini key.
