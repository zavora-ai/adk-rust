# Tool Concurrency Example

Demonstrates ADK-Rust's per-tool concurrency limits with backpressure policies, showing how to throttle expensive tools independently while allowing cheap tools to run at higher concurrency.

## What This Shows

- **Per-tool concurrency limits** — configuring independent `max_concurrency` values for different tools via `ToolConcurrencyConfig`
- **`BackpressurePolicy::Queue`** — queued execution when more calls are dispatched than the concurrency limit allows; calls wait for a slot and eventually complete
- **`BackpressurePolicy::Fail`** — immediate rejection of excess calls when no concurrency slot is available, providing fast feedback under overload
- **Timing effects** — how concurrency limits cause batching (e.g., 6 calls with limit=2 execute in ~3 batches of 2s each) versus unlimited concurrency
- **Realistic agent scenario** — an expensive "web_scraper" tool (2s artificial delay, limit=2) alongside a cheap "calculator" tool (instant, limit=8)

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

Set up your environment:

```bash
cp examples/tool_concurrency/.env.example examples/tool_concurrency/.env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

```bash
cargo run --manifest-path examples/tool_concurrency/Cargo.toml
```

To enable debug logging:

```bash
RUST_LOG=debug cargo run --manifest-path examples/tool_concurrency/Cargo.toml
```

## Expected Output

```
╔════════════════════════════════════════════╗
║  Tool Concurrency — ADK-Rust v1.0       ║
╚════════════════════════════════════════════╝

  ✓ GOOGLE_API_KEY loaded (39 chars)

--- Step 1: Configure Tool Concurrency Limits ---

  ✓ RunConfig created with tool_concurrency_overrides:
      web_scraper: max_concurrency = 2 (expensive, rate-limited)
      calculator:  max_concurrency = 8 (cheap, instant)
      backpressure_policy: Queue (wait for slot)

--- Step 2: Demonstrate BackpressurePolicy::Queue ---

  → Dispatching 6 web_scraper calls with concurrency limit = 2
  → Expected: 3 batches × 2s each ≈ 6s total (vs 2s if unlimited)

  ✓ Call #1 completed in 2.00s: Scraped content from: https://example.com/page1
  ✓ Call #2 completed in 2.00s: Scraped content from: https://example.com/page2
  ✓ Call #3 completed in 4.00s: Scraped content from: https://example.com/page3
  ✓ Call #4 completed in 4.00s: Scraped content from: https://example.com/page4
  ✓ Call #5 completed in 6.00s: Scraped content from: https://example.com/page5
  ✓ Call #6 completed in 6.00s: Scraped content from: https://example.com/page6

  ✓ Total time for 6 queued web_scraper calls: 6.01s
  → With unlimited concurrency, all 6 would complete in ~2s
  → With limit=2, they execute in ~3 batches of 2: ~6s

  → Dispatching 8 calculator calls with concurrency limit = 8
  ✓ Calc #1: 2 + 2 = 4
  ✓ Calc #2: 10 * 5 = 50
  ✓ Calc #3: 100 / 4 = 25
  ...
  ✓ Total time for 8 calculator calls: 0.0001s (all run concurrently, limit=8)

--- Step 3: Demonstrate BackpressurePolicy::Fail ---

  → Dispatching 6 web_scraper calls with concurrency limit = 2
  → Expected: 2 calls succeed immediately, 4 are rejected

  ✓ Call #1 succeeded in 2.00s: Scraped content from: https://example.com/page1
  ✓ Call #2 succeeded in 2.00s: Scraped content from: https://example.com/page2
  ⚠ Call #3 rejected in 0.0001s: Concurrency limit exceeded for tool 'web_scraper'
  ⚠ Call #4 rejected in 0.0001s: Concurrency limit exceeded for tool 'web_scraper'
  ⚠ Call #5 rejected in 0.0001s: Concurrency limit exceeded for tool 'web_scraper'
  ⚠ Call #6 rejected in 0.0001s: Concurrency limit exceeded for tool 'web_scraper'

  ✓ Fail policy results: 2 succeeded, 4 rejected
  ✓ Total time: 2.00s (rejected calls return instantly)

--- Summary ---

  Queue policy: 6 web_scraper calls with limit=2 took 6.01s (~3 batches × 2s)
  Queue policy: 8 calculator calls with limit=8 took 0.0001s (all concurrent)
  Fail policy: 2 calls succeeded, 4 rejected immediately
  Per-tool limits let you throttle expensive tools without blocking cheap ones.
  Queue policy ensures all calls eventually complete (higher latency).
  Fail policy provides fast feedback when system is overloaded.

✅ Example completed successfully.
```

## Key APIs Demonstrated

### ToolConcurrencyConfig

Configure per-tool concurrency limits with a map of tool names to their maximum concurrent execution slots:

```rust
use std::collections::HashMap;

struct ToolConcurrencyConfig {
    /// Per-tool concurrency limits (tool_name → max_concurrent).
    per_tool: HashMap<String, usize>,
    /// What to do when a tool's limit is reached.
    backpressure: BackpressurePolicy,
}

let config = ToolConcurrencyConfig {
    per_tool: HashMap::from([
        ("web_scraper".to_string(), 2),   // Expensive: limit to 2 concurrent
        ("calculator".to_string(), 8),    // Cheap: allow up to 8 concurrent
    ]),
    backpressure: BackpressurePolicy::Queue,
};
```

### BackpressurePolicy

Controls behavior when a tool's concurrency limit is reached:

```rust
enum BackpressurePolicy {
    /// Queue the call and wait until a slot becomes available.
    /// All calls eventually complete, but with higher latency.
    Queue,

    /// Immediately reject the call with an error.
    /// Provides fast feedback when the system is overloaded.
    Fail,
}
```

### RunConfig with Tool Concurrency

Integrate concurrency settings into the agent's run configuration:

```rust
let run_config = RunConfig {
    tool_concurrency: ToolConcurrencyConfig {
        per_tool: HashMap::from([
            ("web_scraper".to_string(), 2),
            ("calculator".to_string(), 8),
        ]),
        backpressure: BackpressurePolicy::Queue,
    },
    model: "gemini-2.5-flash".to_string(),
};
```

### Switching Policies at Runtime

Demonstrate both policies by creating separate configurations:

```rust
// Queue policy — all calls eventually succeed
let queue_config = ToolConcurrencyConfig {
    per_tool: HashMap::from([("web_scraper".to_string(), 2)]),
    backpressure: BackpressurePolicy::Queue,
};

// Fail policy — excess calls are rejected immediately
let fail_config = ToolConcurrencyConfig {
    per_tool: HashMap::from([("web_scraper".to_string(), 2)]),
    backpressure: BackpressurePolicy::Fail,
};
```
