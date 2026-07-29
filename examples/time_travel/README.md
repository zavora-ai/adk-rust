# Time Travel Example

Demonstrates ADK-Rust's time-travel debugging capabilities for graph workflows, showing how to navigate, replay, and fork execution history using checkpoints.

## What This Shows

- **`TimeTravelHandle`** — the primary interface for navigating execution history, obtained from a graph after executing with checkpointing enabled
- **`steps()`** — lists all checkpoints with step numbers, timestamps, and state summaries, providing a complete view of the execution timeline
- **`resume_from(step)`** — resumes execution from an earlier checkpoint with a different random seed, producing divergent results to explore alternative outcomes
- **`fork_at(step, thread_id)`** — creates a new execution thread branching from a historical checkpoint, enabling parallel exploration of alternative research directions
- **`replay(from_step, to_step)`** — re-executes between two steps and returns state transitions at each point, useful for understanding how state evolved
- **`StepInfo`** — metadata struct for each checkpoint containing step number, timestamp, description, and state summary

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

Set up your environment:

```bash
cp examples/time_travel/.env.example examples/time_travel/.env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

```bash
cargo run --manifest-path examples/time_travel/Cargo.toml
```

To enable debug logging:

```bash
RUST_LOG=debug cargo run --manifest-path examples/time_travel/Cargo.toml
```

## Expected Output

```
╔════════════════════════════════════════════╗
║  Time Travel — ADK-Rust v1.0            ║
╚════════════════════════════════════════════╝

  ✓ GOOGLE_API_KEY loaded (39 chars)
  → Using model: gemini-2.5-flash

--- Step 1: Execute Multi-Step Graph with Checkpointing ---

  → Configuring graph with time-travel checkpointing enabled...
  → Thread ID: "research_thread"
  → Executing 6-step research workflow...

  ✓ Graph executed: 6 steps completed with checkpoints
  ✓ Final state: confidence=91%, findings=6, sources=5

--- Step 2: List All Steps with steps() ---

  → Calling handle.steps() to list all checkpoints...

  Step |           Timestamp | Description                                        | State
  --------------------------------------------------------------------------------------------------------------
     0 | 14:32:01.123        | Initialize research topic and parameters            | phase=initialization, findings=0, confidence=5%, sources=0
     1 | 14:32:01.124        | Gather preliminary academic and blog sources        | phase=source_gathering, findings=0, confidence=15%, sources=2
     2 | 14:32:01.125        | Analyze primary findings from academic papers       | phase=primary_analysis, findings=2, confidence=40%, sources=3
     3 | 14:32:01.126        | Cross-reference with real-world concurrent systems  | phase=cross_reference, findings=3, confidence=65%, sources=5
     4 | 14:32:01.127        | Synthesize findings into preliminary conclusions    | phase=synthesis, findings=4, confidence=82%, sources=5
     5 | 14:32:01.128        | Produce final research report                      | phase=final_report, findings=6, confidence=91%, sources=5

  ✓ Listed 6 checkpoints with timestamps

--- Step 3: Resume from Earlier Step with resume_from() ---

  → Resuming from step 2: "Analyze primary findings from academic papers"
  → Re-executing with different random seed for divergent results...

  Original execution vs. Divergent execution (from step 2):
  ------------------------------------------------------------------------------------------
  Step 2: Original confidence=40% | Divergent confidence=35%
         → DIVERGENT: Ownership model has higher learning curve but lower bug density
  Step 3: Original confidence=65% | Divergent confidence=58%
         → DIVERGENT: Arc<Mutex<T>> pattern shows ownership enables safe shared state
  Step 4: Original confidence=82% | Divergent confidence=75%
         → DIVERGENT: Performance overhead of ownership checks is negligible at runtime
  Step 5: Original confidence=91% | Divergent confidence=88%
         → DIVERGENT: Ownership model is most impactful in systems with shared mutable state

  ✓ Resumed from step 2 with divergent results across 4 steps
  ✓ Original final confidence: 91% vs Divergent: 88%

--- Step 4: Fork at Step into New Thread with fork_at() ---

  → Forking at step 1 into new thread: "alternative_research"
  → Base state at fork point: phase=source_gathering, findings=0, confidence=15%, sources=2

  Forked thread "alternative_research" execution:
  --------------------------------------------------------------------------------
  Step 1: [forked_exploration] Fork point: branching into thread 'alternative_research'
       → FORK[alternative_research]: Exploring memory safety without garbage collection
  Step 2: [forked_analysis] Forked thread 'alternative_research': deeper analysis
       → FORK[alternative_research]: Affine types provide similar guarantees with different ergonomics
  Step 3: [forked_conclusion] Forked thread 'alternative_research': conclusion
       → FORK[alternative_research]: Rust's approach is pragmatic — ownership + borrowing balances safety and usability

  ✓ Forked into thread "alternative_research" with 3 new checkpoints
  ✓ Forked thread confidence: 37% (exploring alternative direction)

--- Step 5: Replay Between Steps with replay() ---

  → Replaying steps 1 through 4...
  → Printing state transitions at each step:

  Step 1 → Step 2:
    Before: phase=source_gathering, findings=0, confidence=15%, sources=2
    After:  phase=primary_analysis, findings=2, confidence=40%, sources=3
    Changes:
      • phase: "source_gathering" → "primary_analysis"
      • findings: +2 new
      • confidence: 15% → 40%
      • sources: +1 new

  Step 2 → Step 3:
    Before: phase=primary_analysis, findings=2, confidence=40%, sources=3
    After:  phase=cross_reference, findings=3, confidence=65%, sources=5
    Changes:
      • phase: "primary_analysis" → "cross_reference"
      • findings: +1 new
      • confidence: 40% → 65%
      • sources: +2 new

  Step 3 → Step 4:
    Before: phase=cross_reference, findings=3, confidence=65%, sources=5
    After:  phase=synthesis, findings=4, confidence=82%, sources=5
    Changes:
      • phase: "cross_reference" → "synthesis"
      • findings: +1 new
      • confidence: 65% → 82%

  ✓ Replayed 3 state transitions (steps 1 → 4)

--- Summary ---

  Executed 6-step research graph with checkpointing enabled.
  Listed all steps with timestamps using steps().
  Resumed from step 2 showing divergent results (different random seed).
  Forked at step 1 into "alternative_research" thread.
  Replayed steps 1→4 showing state transitions at each step.

  Key APIs demonstrated:
    • TimeTravelHandle::steps() — list all checkpoints
    • TimeTravelHandle::resume_from(step) — resume with divergent execution
    • TimeTravelHandle::fork_at(step, thread_id) — branch into new thread
    • TimeTravelHandle::replay(from, to) — re-execute and observe transitions

  Time travel enables exploring alternative execution paths without
  re-running the entire workflow from scratch.

✅ Example completed successfully.
```

## Key APIs Demonstrated

### TimeTravelHandle

The primary interface for navigating execution history. Obtained from a graph after executing with checkpointing enabled:

```rust
use adk_graph::time_travel::{TimeTravelHandle, StepInfo};

// After executing a graph with checkpointing
let handle = graph.time_travel("research_thread");
```

### steps() — List All Checkpoints

Returns a vector of `StepInfo` structs describing each checkpoint in the execution history:

```rust
let steps: Vec<StepInfo> = handle.steps().await?;

for step in &steps {
    println!(
        "Step {}: [{}] {} — {}",
        step.step,
        step.timestamp,
        step.description,
        step.state_summary
    );
}
```

### resume_from() — Resume with Divergent Execution

Restores state from a historical checkpoint and re-executes subsequent steps, potentially producing different results:

```rust
// Resume from step 2 — re-executes steps 2..end with a new random seed
let divergent_result = handle.resume_from(2, Some(exec_config)).await?;

// Compare original vs divergent outcomes
println!("Original confidence: {:.0}%", original.confidence * 100.0);
println!("Divergent confidence: {:.0}%", divergent.confidence * 100.0);
```

### fork_at() — Branch into a New Thread

Creates a new execution thread starting from the state at a historical checkpoint, enabling exploration of alternative paths without modifying the original:

```rust
// Fork at step 1 into a new thread called "alternative_research"
handle.fork_at(1, "alternative_research").await?;

// The forked thread has its own independent execution history
let forked_handle = graph.time_travel("alternative_research");
let forked_steps = forked_handle.steps().await?;
```

### replay() — Re-execute and Observe Transitions

Replays execution between two steps, returning the state at each transition point for inspection:

```rust
// Replay steps 0 through 3, observing state changes
let states = handle.replay(0, Some(3)).await?;

for (i, state) in states.iter().enumerate() {
    println!("Step {i}: {:?}", state);
}
```

### StepInfo — Checkpoint Metadata

Each checkpoint carries metadata about the execution state at that point:

```rust
pub struct StepInfo {
    /// Step number (0-indexed).
    pub step: usize,
    /// Timestamp when the checkpoint was created.
    pub timestamp: DateTime<Utc>,
    /// Human-readable description of what happened at this step.
    pub description: String,
    /// Summary of the state at this checkpoint.
    pub state_summary: String,
}
```
