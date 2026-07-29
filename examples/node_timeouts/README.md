# Node Timeouts Example

## What This Shows

This example demonstrates ADK-Rust's node-level timeout policies for building resilient graph workflows. It covers:

- **`TimeoutPolicy`** — configuring per-node wall-clock and idle timeout budgets
- **`OnTimeout::Fail`** — aborting a node that exceeds its wall-clock deadline
- **`OnTimeout::Retry { max_attempts }`** — automatically retrying timed-out nodes with progressive adaptation
- **`OnTimeout::Skip`** — gracefully skipping optional nodes that stall
- **`idle_timeout`** — detecting nodes that stop making progress (stall detection)
- **`report_progress()`** — resetting the idle timer to keep long-running nodes alive

The example builds a four-node graph where each node demonstrates a different timeout recovery strategy, showing how to combine wall-clock deadlines, idle detection, and automatic recovery for production-grade workflows.

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

Set the API key in your shell or create a `.env` file:

```bash
export GOOGLE_API_KEY="your-gemini-api-key"
```

See `.env.example` for all required variables.

## Run

```bash
cargo run --manifest-path examples/node_timeouts/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════╗
║  Node Timeouts — ADK-Rust v1.0         ║
╚══════════════════════════════════════════╝

  ✓ GOOGLE_API_KEY loaded (39 chars)
  → Using model: gemini-2.5-flash (timeout demo uses simulated nodes)

--- Step 1: Configure Timeout Policies ---

  ✓ Node 'fast_research': run_timeout=2s, on_timeout=Fail
  ✓ Node 'retry_analysis': run_timeout=3s, on_timeout=Retry(max_attempts=3)
  ✓ Node 'optional_enrichment': idle_timeout=2s, on_timeout=Skip

--- Step 2: Execute 'fast_research' — Wall-Clock Timeout → Fail ---

  → Node intentionally sleeps 4s with a 2s wall-clock limit...
  ⚠ TIMEOUT on 'fast_research' after 2.00s
  ⚠ Recovery: FAIL — node aborted, graph halted

--- Step 3: Execute 'retry_analysis' — Wall-Clock Timeout → Retry (max 3 attempts) ---

  → Node adapts work duration on each retry attempt...
  → Attempt 1: sleeps 5s (exceeds 3s limit → timeout)
  → Attempt 2: sleeps 4s (exceeds 3s limit → timeout)
  → Attempt 3: sleeps 2s (within 3s limit → success!)
  ✓ 'retry_analysis' succeeded on attempt 3 in 2.00s
  ✓ Result: Analysis complete on attempt 3: sentiment=positive, confidence=0.87

--- Step 4: Execute 'optional_enrichment' — Idle Timeout → Skip (node stalls) ---

  → Node reports progress once, then stops — idle timeout fires after 2s...
  ⚠ 'optional_enrichment' idle timeout after 2.50s
  → Recovery: SKIP — idle timeout after 2.50s (no progress)
  → Graph continues without enrichment data (graceful degradation)

--- Step 5: Demonstrate report_progress() — Node Avoids Idle Timeout ---

  → Node 'active_enrichment': idle_timeout=2s, but reports progress every 1s
  → Total work takes ~4s — would timeout without progress reports...
  ✓ 'active_enrichment' completed in 3.60s (survived 4 idle windows!)
  ✓ Result: Enrichment complete: processed 4 phases

--- Summary ---

  Timeout policies configured: 4 nodes with different recovery actions

  Node 'fast_research':        run_timeout=2s, OnTimeout::Fail
    → Timed out and aborted (intentionally exceeded limit)

  Node 'retry_analysis':       run_timeout=3s, OnTimeout::Retry(3)
    → Timed out twice, succeeded on attempt 3 (adapted work)

  Node 'optional_enrichment':  idle_timeout=2s, OnTimeout::Skip
    → Stalled (no progress), skipped gracefully

  Node 'active_enrichment':    idle_timeout=2s, report_progress()
    → Reported progress every ~1s, completed 4s of work without timeout

  Key takeaways:
    • run_timeout enforces a hard wall-clock deadline
    • idle_timeout detects stalled nodes that stop making progress
    • report_progress() resets the idle timer — keeps active nodes alive
    • OnTimeout::Retry enables automatic recovery for transient slowness
    • OnTimeout::Skip enables graceful degradation for optional work

✅ Example completed successfully.
```

## Key APIs Demonstrated

### TimeoutPolicy Configuration

Each graph node gets its own timeout policy defining deadlines and recovery behavior:

```rust
use std::time::Duration;

// Wall-clock timeout with hard failure
let fail_policy = TimeoutPolicy {
    run_timeout: Some(Duration::from_secs(2)),
    idle_timeout: None,
    on_timeout: OnTimeout::Fail,
};

// Wall-clock timeout with automatic retry
let retry_policy = TimeoutPolicy {
    run_timeout: Some(Duration::from_secs(3)),
    idle_timeout: None,
    on_timeout: OnTimeout::Retry { max_attempts: 3 },
};

// Idle timeout with graceful skip
let skip_policy = TimeoutPolicy {
    run_timeout: None,
    idle_timeout: Some(Duration::from_secs(2)),
    on_timeout: OnTimeout::Skip,
};
```

### OnTimeout Recovery Actions

The `OnTimeout` enum defines what happens when a node exceeds its time budget:

```rust
enum OnTimeout {
    /// Abort the node — the graph fails at this point.
    Fail,
    /// Retry the node up to `max_attempts` times before failing.
    Retry { max_attempts: u32 },
    /// Skip the node and continue with a default/empty output.
    Skip,
}
```

### ProgressHandle Usage

Nodes with `idle_timeout` receive a `ProgressHandle` to report liveness. Each call to `report_progress()` resets the idle timer:

```rust
async fn long_running_work(handle: ProgressHandle) -> String {
    for phase in 1..=4 {
        // Do work for ~1s per phase
        tokio::time::sleep(Duration::from_millis(900)).await;

        // Report progress to avoid idle timeout
        handle.report_progress().await;
    }
    "Completed all phases".to_string()
}
```

### execute_with_timeout

The timeout execution engine wraps node work with deadline enforcement:

```rust
// Wall-clock timeout execution
let outcome = execute_with_wall_clock_timeout(&node, || async {
    // Node work that may exceed the deadline
    expensive_computation().await
}).await;

// Idle timeout execution with progress reporting
let outcome = execute_with_idle_timeout(&node, |progress_handle| async move {
    loop {
        do_incremental_work().await;
        progress_handle.report_progress().await;
    }
}).await;

// Retry execution with attempt-aware work
let (outcome, attempts) = execute_with_retry(&node, 3, |attempt| async move {
    // Adapt behavior based on retry attempt number
    do_work_with_budget(attempt).await
}).await;
```
