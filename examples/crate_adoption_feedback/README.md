# Crate Adoption Feedback — Feature Showcase

Demonstrates all five adoption fixes from GitHub issue #262 using live LLM agents:

1. **SQLx lifetime fix** — `MemoryService` works inside `#[async_trait]` tools
2. **Tool context in callbacks** — `tool_name()` / `tool_input()` in before/after-tool hooks
3. **Composable telemetry** — `build_otlp_layer` for custom subscriber stacks
4. **Developer-friendly content filter** — "hack"/"exploit" no longer blocked by default
5. **PluginBuilder** — fluent API for constructing plugins with lifecycle callbacks

## Setup

```bash
cp .env.example .env
# Add your API key (GOOGLE_API_KEY, OPENAI_API_KEY, or ANTHROPIC_API_KEY)
```

## Run

```bash
cargo run --manifest-path examples/crate_adoption_feedback/Cargo.toml
```

Features 3, 4, and 5 run without an API key. The full LLM agent demo (features 1+2+4) requires a configured provider.
