# Parallel Shared State Example

Demonstrates `ParallelAgent` with `SharedState` coordination — three sub-agents work on the same workbook concurrently.

## Pattern

```
DataAgent ──→ creates workbook ──→ publishes "workbook_id" to SharedState
                                          │
                                    ┌─────┴─────┐
                                    ▼             ▼
                              FormatAgent    ChartAgent
                              (wait_for_key) (wait_for_key)
                                    │             │
                                    ▼             ▼
                              applies formatting  adds charts
```

- **DataAgent** creates the workbook and publishes the handle via `set_shared("workbook_id", ...)`
- **FormatAgent** and **ChartAgent** call `wait_for_key("workbook_id", timeout)` to block until the handle is available, then proceed in parallel

## Run

```bash
cargo run --manifest-path examples/parallel_shared_state/Cargo.toml
```

## Key APIs

```rust
// Enable shared state on ParallelAgent
let agent = ParallelAgent::new("team", sub_agents)
    .with_shared_state();

// In a tool or agent, access shared state via context
let shared = ctx.shared_state().expect("shared state enabled");

// Publish a value
shared.set_shared("workbook_id", json!("wb-123")).await?;

// Wait for a value (with timeout)
let handle = shared.wait_for_key("workbook_id", Duration::from_secs(30)).await?;

// Read without waiting
let value = shared.get_shared("workbook_id").await;
```

## Related

- [Tool Authorization](../../docs/official_docs/security/tool-authorization.md)
- [ParallelAgent](../../docs/official_docs/agents/workflow-agents.md)
