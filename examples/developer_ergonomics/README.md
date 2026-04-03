# Developer Ergonomics Example

Validates all seven developer ergonomics improvements in ADK-Rust 0.5.x.

## Features Demonstrated

- `ToolExecutionStrategy` — Sequential, Parallel, Auto dispatch modes
- Tool metadata — `is_read_only()` / `is_concurrency_safe()` on the Tool trait
- `RunnerConfigBuilder` — typestate builder with compile-time required field enforcement
- `SimpleToolContext` — lightweight ToolContext for non-agent callers
- `Runner::run_str()` — string convenience method (no manual UserId/SessionId construction)
- `StatefulTool<S>` — shared-state tool wrapper with `Arc<S>`
- `#[tool(read_only, concurrency_safe)]` — macro attributes for tool metadata

## Binaries

### `validate` — offline validation (no API key needed)

```bash
cargo run --manifest-path examples/developer_ergonomics/Cargo.toml --bin validate
```

Runs 26 assertions covering every ergonomics feature without calling any LLM.

### `llm_demo` — live LLM agent demo

```bash
export GOOGLE_API_KEY=your-key   # or OPENAI_API_KEY or ANTHROPIC_API_KEY
cargo run --manifest-path examples/developer_ergonomics/Cargo.toml --bin llm_demo
```

Builds a travel assistant agent with four tools (two read-only FunctionTools, one StatefulTool counter, one `#[tool]` macro tool) and runs it against a live LLM with `ToolExecutionStrategy::Auto`.
