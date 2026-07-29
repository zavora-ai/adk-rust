# Delta Checkpoints Example

Demonstrates ADK-Rust's delta checkpointing system that stores incremental state diffs instead of full snapshots, with periodic full snapshots for efficient state reconstruction and significant storage savings.

## What This Shows

- **`DeltaCheckpointer`** — wrapping any inner checkpointer (e.g., `MemoryCheckpointer`) with a delta-aware layer that automatically decides between full snapshots and incremental diffs
- **`DeltaConfig`** — configuring `full_snapshot_interval` to control how often full snapshots are stored (e.g., every 3 steps) versus delta checkpoints
- **`StateDiff`** — computing the difference between consecutive states, capturing added/modified keys and removed keys, and applying diffs to reconstruct state
- **`CheckpointType`** — distinguishing between `FullSnapshot` (complete state) and `Delta` (incremental diff) entries at each step
- **State reconstruction** — walking back to the nearest full snapshot and applying deltas forward to reconstruct any historical state with verified round-trip integrity
- **Storage savings** — comparing delta checkpointing storage costs against full-snapshot-only strategies, demonstrating significant byte reduction for incrementally-changing state

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

Set up your environment:

```bash
cp examples/delta_checkpoints/.env.example examples/delta_checkpoints/.env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

```bash
cargo run --manifest-path examples/delta_checkpoints/Cargo.toml
```

To enable debug logging:

```bash
RUST_LOG=debug cargo run --manifest-path examples/delta_checkpoints/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════╗
║  Delta Checkpoints — ADK-Rust v1.0     ║
╚══════════════════════════════════════════╝

  ✓ GOOGLE_API_KEY loaded (39 chars)
  → Using model: gemini-2.5-flash

--- Step 1: Configure Delta Checkpointer ---

  → DeltaConfig { full_snapshot_interval: 3 }
  → Wrapping MemoryCheckpointer with delta-aware layer
  ✓ Delta checkpointer configured
  → Full snapshots at steps: 0, 3, 6, ...

--- Step 2: Execute Multi-Step Graph (8 steps) ---

  → Step 0: [📸 FULL] Initialize research topic and parameters (112 bytes)
  → Step 1: [📝 DELTA] Gather initial sources from web search (198 bytes)
       Added/Modified: ["source_count", "sources"]
  → Step 2: [📝 DELTA] Analyze source relevance and extract key findings (215 bytes)
       Added/Modified: ["analysis_status", "findings"]
       Removed: ["max_sources"]
  → Step 3: [📸 FULL] Generate summary from findings (587 bytes)
  → Step 4: [📝 DELTA] Add citations and references (267 bytes)
       Added/Modified: ["citation_count", "citations"]
  → Step 5: [📝 DELTA] Peer review and quality scoring (189 bytes)
       Added/Modified: ["analysis_status", "quality_score", "review_notes"]
  → Step 6: [📸 FULL] Finalize report with metadata (812 bytes)
  → Step 7: [📝 DELTA] Archive and compress results (108 bytes)
       Added/Modified: ["archive_id", "archived", "compression_ratio"]
  ✓ Graph execution complete: 8 steps checkpointed

--- Step 3: Delta vs Full Checkpoint Comparison ---

   Step |         Type |       Size | Details
  --------------------------------------------------
      0 | Full Snapshot |     112 B | Initialize research topic and parameters
      1 |        Delta |     198 B | Gather initial sources from web search
      2 |        Delta |     215 B | Analyze source relevance and extract key findings
      3 | Full Snapshot |     587 B | Generate summary from findings
      4 |        Delta |     267 B | Add citations and references
      5 |        Delta |     189 B | Peer review and quality scoring
      6 | Full Snapshot |     812 B | Finalize report with metadata
      7 |        Delta |     108 B | Archive and compress results

  ✓ Full snapshots stored: 3 (total 1511 bytes)
  ✓ Delta checkpoints stored: 5 (total 977 bytes)

--- Step 4: Reconstruct State from Delta Checkpoint ---

  ✓ Step 0: Reconstructed state matches original ✓ (4 keys)
  ✓ Step 1: Reconstructed state matches original ✓ (6 keys)
  ✓ Step 2: Reconstructed state matches original ✓ (7 keys)
  ✓ Step 3: Reconstructed state matches original ✓ (8 keys)
  ✓ Step 4: Reconstructed state matches original ✓ (10 keys)
  ✓ Step 5: Reconstructed state matches original ✓ (12 keys)
  ✓ Step 6: Reconstructed state matches original ✓ (13 keys)
  ✓ Step 7: Reconstructed state matches original ✓ (15 keys)

  ✓ All state reconstructions verified — round-trip integrity confirmed

  → Detailed reconstruction for step 5 (delta checkpoint):
  →   Checkpoint type: Delta
  →   Stored size: 189 bytes
  →   Reconstructed state has 12 keys
  →   Full state size would be: 723 bytes
  ✓   Storage savings: 73.9% (delta: 189 B vs full: 723 B)

--- Step 5: Storage Size Comparison ---

  Storage Strategy Comparison:
  ┌─────────────────────────────────────────────────┐
  │ Full-snapshot-only:   4892 bytes (8 snapshots)  │
  │ Delta checkpointing:  2488 bytes (3 full + 5 delta) │
  │ Storage savings:      49.1%                        │
  └─────────────────────────────────────────────────┘

  Per-step breakdown:
    📸 Step 0: full= 112B, actual= 112B, savings=  0.0%
    📝 Step 1: full= 310B, actual= 198B, savings= 36.1%
    📝 Step 2: full= 498B, actual= 215B, savings= 56.8%
    📸 Step 3: full= 587B, actual= 587B, savings=  0.0%
    📝 Step 4: full= 654B, actual= 267B, savings= 59.2%
    📝 Step 5: full= 723B, actual= 189B, savings= 73.9%
    📸 Step 6: full= 812B, actual= 812B, savings=  0.0%
    📝 Step 7: full= 896B, actual= 108B, savings= 87.9%

--- Summary ---

  Delta checkpointing with full_snapshot_interval: 3
  Total graph steps executed: 8
  Full snapshots: 3 | Delta checkpoints: 5
  Total storage: 2488 bytes (vs 4892 bytes full-only)
  Storage savings: 49.1%
  All 8 state reconstructions verified ✓

  Key concepts:
    • DeltaCheckpointer wraps any inner Checkpointer
    • Stores only state diffs between consecutive steps
    • Periodic full snapshots ensure bounded reconstruction cost
    • State can be reconstructed by applying diffs to last full snapshot
    • Round-trip reconstruction preserves exact state equality

✅ Example completed successfully.
```

## Key APIs Demonstrated

### DeltaCheckpointer

Wraps any inner checkpointer with delta-aware logic. Automatically decides whether to store a full snapshot or an incremental diff based on the configured interval:

```rust
use adk_graph::delta::{DeltaCheckpointer, DeltaConfig};
use adk_graph::checkpoint::MemoryCheckpointer;

let config = DeltaConfig { full_snapshot_interval: 3 };
let inner = MemoryCheckpointer::new();
let mut checkpointer = DeltaCheckpointer::new(inner, config);

// Checkpoint state at each step — automatically chooses full vs delta
checkpointer.checkpoint(step, &current_state);

// Reconstruct any historical state by walking back to nearest full snapshot
let reconstructed = checkpointer.reconstruct_state(target_step)?;
```

### DeltaConfig

Controls how often full snapshots are stored versus delta checkpoints:

```rust
use adk_graph::delta::DeltaConfig;

let config = DeltaConfig {
    /// Store a full snapshot every N steps.
    /// Steps 0, 3, 6, ... get full snapshots; all others get deltas.
    full_snapshot_interval: 3,
};
```

### StateDiff

Computes and applies the difference between two state maps:

```rust
use adk_graph::delta::StateDiff;
use std::collections::BTreeMap;

// Compute the diff between old and new state
let diff = StateDiff::compute(&old_state, &new_state);

// Inspect what changed
println!("Added/Modified: {:?}", diff.added_or_modified.keys());
println!("Removed: {:?}", diff.removed);
println!("Diff size: {} bytes", diff.size_bytes());

// Apply the diff to a base state to reconstruct the new state
let reconstructed = diff.apply(&old_state);
assert_eq!(reconstructed, new_state);
```

### CheckpointType

Distinguishes between full snapshots and delta checkpoints at each step:

```rust
use adk_graph::delta::CheckpointType;

match entry.checkpoint_type {
    CheckpointType::FullSnapshot => {
        // Complete state stored — used as base for future delta reconstruction
        let state = entry.full_state.unwrap();
    }
    CheckpointType::Delta => {
        // Only the diff is stored — must apply to a base state to reconstruct
        let diff = entry.diff.unwrap();
        let state = diff.apply(&base_state);
    }
}
```

### State Reconstruction

Reconstruct any historical state by walking back to the nearest full snapshot and applying deltas forward:

```rust
// Reconstruct state at step 5 (a delta checkpoint)
// Internally: finds full snapshot at step 3, applies deltas for steps 4 and 5
let state_at_step_5 = checkpointer.reconstruct_state(5)?;

// Verify round-trip integrity
assert_eq!(state_at_step_5, original_state_at_step_5);
```
