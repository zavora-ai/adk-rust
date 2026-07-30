# Deferred Nodes Example

## What This Shows

This example demonstrates ADK-Rust's scatter-gather fan-in barriers — parallel upstream paths feeding into a deferred node that waits for all (or some) results before executing its aggregation logic.

**APIs and behaviors demonstrated:**

- `DeferredNodeConfig` — configuring a deferred (fan-in) node with merge strategy and timeout
- `MergeStrategy::Collect` — gathering all upstream outputs into a `Vec<Value>`
- `MergeStrategy::MergeMap` — merging upstream state maps into a single flattened map
- `FanInTracker` — tracking completion status of upstream paths and applying merge logic
- `fan_in_timeout` — enforcing a deadline on upstream path completion, excluding slow paths

The example uses a realistic parallel research scenario where three branches each investigate a different aspect of "Artificial Intelligence" (history, technology, economics) and a deferred aggregator node combines the results.

## Prerequisites

- **Rust** 1.95+ (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

Copy `.env.example` to `.env` and fill in your API key:

```bash
cp .env.example .env
# Edit .env and set GOOGLE_API_KEY=your_key_here
```

## Run

```bash
cargo run --manifest-path examples/deferred_nodes/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════╗
║  Deferred Nodes — ADK-Rust v1.0        ║
╚══════════════════════════════════════════╝

--- Step 1: Configure Parallel Research Branches ---

  → Topic: "Artificial Intelligence"
  → Branch 1: History        (simulated delay: 500ms)
  → Branch 2: Technology     (simulated delay: 800ms)
  → Branch 3: Economics      (simulated delay: 1000ms)
  ✓ API key loaded (39 chars)
  ✓ Three parallel upstream paths configured

--- Step 2: MergeStrategy::Collect ---

  → Gathering all upstream outputs into a Vec<Value>
  ✓ All 3 paths completed in 1.002s
  ✓ Completed paths: ["history", "technology", "economics"]
  ✓ Merge strategy: Collect
  → Result type: Array with 3 elements
    [0] branch="history", completed_in=500ms
    [1] branch="technology", completed_in=800ms
    [2] branch="economics", completed_in=1000ms

--- Step 3: MergeStrategy::MergeMap ---

  → Merging upstream state maps into a single map
  ✓ All 3 paths completed in 1.001s
  ✓ Merge strategy: MergeMap
  → Result type: Object with 9 keys
    "branch": "economics"...
    "elapsed_ms": 1000...
    "findings": "AI is projected to add $15.7 trillion to the global ec...
    "topic": "Economic Impact of AI"...

--- Step 4: Fan-In Timeout ---

  → Configuring fan_in_timeout = 1200ms
  → Branch 1 (history):    delay = 500ms  (will complete)
  → Branch 2 (technology): delay = 800ms  (will complete)
  → Branch 3 (economics):  delay = 3000ms (will TIMEOUT)
  ⚠ Fan-in timeout reached (1.2s), path 'economics' did not complete in time
  ✓ Deferred node completed in 1.201s
  ✓ Paths that completed before timeout: ["history", "technology"]
  → Partial result: Array with 2 elements (out of 3 expected)
    [0] branch="history", completed_in=500ms
    [1] branch="technology", completed_in=800ms
  ⚠ Paths excluded due to timeout: ["economics"]

--- Summary ---

  Parallel branches configured:  3 (history, technology, economics)
  MergeStrategy::Collect:        ✓ gathered 3 items into Vec
  MergeStrategy::MergeMap:       ✓ merged into map with 9 keys
  Fan-in timeout (1200ms):       ✓ 2/3 paths completed before deadline
  Total Collect elapsed:         1.002s
  Total MergeMap elapsed:        1.001s
  Total Timeout elapsed:         1.201s

✅ Deferred Nodes example completed successfully.
```

## Key APIs Demonstrated

### DeferredNodeConfig

Configure a deferred node with a merge strategy and optional fan-in timeout:

```rust
use adk_graph::deferred::{DeferredNodeConfig, MergeStrategy};
use std::time::Duration;

// Collect all upstream outputs into a vector (no timeout)
let collect_config = DeferredNodeConfig {
    merge_strategy: MergeStrategy::Collect,
    fan_in_timeout: None,
};

// Merge upstream maps with a 30-second deadline
let merge_config = DeferredNodeConfig {
    merge_strategy: MergeStrategy::MergeMap,
    fan_in_timeout: Some(Duration::from_secs(30)),
};
```

### MergeStrategy

Choose how upstream outputs are combined at the fan-in point:

```rust
use adk_graph::deferred::MergeStrategy;

// Collect: gathers all outputs into Vec<Value>
// Order matches the order paths complete
let strategy = MergeStrategy::Collect;

// MergeMap: merges upstream JSON objects into a single map
// Later keys overwrite earlier ones on conflict
let strategy = MergeStrategy::MergeMap;
```

### FanInTracker

Track upstream path completion and apply the merge strategy:

```rust
use adk_graph::deferred::{FanInTracker, MergeStrategy};

// Create a tracker expecting results from three paths
let mut tracker = FanInTracker::new(vec![
    "history".to_string(),
    "technology".to_string(),
    "economics".to_string(),
]);

// Record results as upstream paths complete
tracker.record("history", serde_json::json!({"findings": "..."}));
tracker.record("technology", serde_json::json!({"findings": "..."}));
tracker.record("economics", serde_json::json!({"findings": "..."}));

// Check completion status
assert!(tracker.is_complete());

// Apply merge strategy to produce final output
let merged = tracker.merge(&MergeStrategy::Collect);
// Result: Array of three JSON objects
```

### fan_in_timeout

Enforce a deadline on upstream path completion — paths that don't finish in time are excluded from the merged result:

```rust
use adk_graph::deferred::DeferredNodeConfig;
use std::time::Duration;

let config = DeferredNodeConfig {
    merge_strategy: MergeStrategy::Collect,
    fan_in_timeout: Some(Duration::from_millis(1200)),
};

// If a branch takes longer than 1200ms, it is excluded
// from the merged result and a warning is emitted.
// The deferred node proceeds with partial results.
```
