# Context Compaction Example

Demonstrates ADK-Rust's automatic context window management, showing how to handle long conversations without exceeding model token limits using both truncation and summarisation compaction strategies.

## What This Shows

- **`CompactionConfig`** — configuring a token budget and attaching a compaction strategy that triggers automatically when the context exceeds the budget
- **`TruncationCompaction`** — dropping older events while preserving the most recent N, providing instant compaction with zero LLM cost
- **`SummarisationCompaction`** — replacing older events with an LLM-generated summary that retains semantic meaning across compaction boundaries
- **Token budget enforcement** — setting a low `context_budget` (2000 tokens) and observing how both strategies reduce context size to fit within the limit
- **Coherence after compaction** — verifying that follow-up questions referencing earlier conversation topics can still be answered after compaction, comparing how truncation loses context while summarisation preserves it

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

Set up your environment:

```bash
cp examples/context_compaction/.env.example examples/context_compaction/.env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

```bash
cargo run --manifest-path examples/context_compaction/Cargo.toml
```

To enable debug logging:

```bash
RUST_LOG=debug cargo run --manifest-path examples/context_compaction/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════════╗
║  Context Compaction — ADK-Rust v1.0        ║
╚══════════════════════════════════════════════╝

  ✓ GOOGLE_API_KEY loaded (39 chars)

--- Step 1: Build Multi-Turn Conversation ---

  ✓ Built conversation with 10 events (5 turns)
  ✓ Total estimated tokens: 3847
      Turn 1: [user] 21 tokens — "What is Rust's ownership model and how doe..."
      Turn 1: [assistant] 195 tokens — "Rust's ownership model ensures memory safe..."
      Turn 2: [user] 22 tokens — "How does async/await work in Rust compared..."
      Turn 2: [assistant] 258 tokens — "Rust's async/await differs fundamentally f..."
      ...

--- Step 2: TruncationCompaction — Low Budget Triggers Compaction ---

  ✓ Context budget: 2000 tokens
  ✓ Strategy: TruncationCompaction (preserve_recent: 4)
  → Current context: 3847 tokens (exceeds budget by 1847)
  → Compaction needed: YES (3847 > 2000)

  ✓ Token estimates BEFORE compaction:
      Total tokens: 3847
      Event count: 10

  ✓ Token estimates AFTER TruncationCompaction:
      Total tokens: 1632
      Event count: 4
      Reduction: 57.6%

  → Remaining events after truncation:
      Turn 4: [user] 18 tokens — "What are the best practices for error hand..."
      Turn 4: [assistant] 312 tokens — "For production async Rust error handling, ..."
      Turn 5: [user] 20 tokens — "How do I implement graceful shutdown in a t..."
      Turn 5: [assistant] 282 tokens — "Implementing graceful shutdown in a tokio a..."

--- Step 3: SummarisationCompaction — LLM-Generated Summary ---

  ✓ Context budget: 2000 tokens
  ✓ Strategy: SummarisationCompaction (turns_to_summarise: 6)
  → Applying summarisation to full conversation...

  ✓ Token estimates BEFORE compaction:
      Total tokens: 3847
      Event count: 10

  ✓ Token estimates AFTER SummarisationCompaction:
      Total tokens: 1456
      Event count: 5
      Reduction: 62.2%

  → Events after summarisation:
      [system] 187 tokens — "[LLM-Generated Summary] Previous conversation covered: User asked about..."
      [user] 18 tokens — "What are the best practices for error hand..."
      [assistant] 312 tokens — "For production async Rust error handling, ..."
      ...

--- Step 4: Coherence After Compaction — Follow-up Referencing Earlier Context ---

  → Simulating follow-up question: "Earlier you mentioned work-stealing in tokio..."

  ✓ Truncation strategy coherence check:
      ⚠ Context about tokio's scheduler was truncated (older turn)
      ⚠ Follow-up would lack context — model may hallucinate

  ✓ Summarisation strategy coherence check:
      ✓ Summary retained key information about tokio scheduling
      ✓ Follow-up can be answered with summary context
      ✓ LLM summary preserves semantic meaning across compaction

--- Step 5: Strategy Comparison ---

  ✓ Truncation vs Summarisation:

      ┌─────────────────────┬──────────────┬──────────────┐
      │ Metric              │ Truncation   │ Summarisation│
      ├─────────────────────┼──────────────┼──────────────┤
      │ Tokens after        │         1632 │         1456 │
      │ Events after        │            4 │            5 │
      │ Reduction %         │        57.6% │        62.2% │
      │ Preserves semantics │   No (drops) │ Yes (summary)│
      │ LLM call required   │           No │          Yes │
      │ Latency cost        │         None │        ~1-2s │
      └─────────────────────┴──────────────┴──────────────┘

--- Summary ---

  Initial context: 3847 tokens (10 events)
  Context budget: 2000 tokens
  After TruncationCompaction: 1632 tokens (4 events, 57.6% reduction)
  After SummarisationCompaction: 1456 tokens (5 events, 62.2% reduction)
  Truncation is fast but loses older context entirely.
  Summarisation preserves semantic meaning at the cost of one LLM call.

✅ Example completed successfully.
```

## Key APIs Demonstrated

### CompactionConfig

Configure the compaction behavior with a strategy and token budget. Compaction triggers automatically when the context exceeds the budget:

```rust
use adk_runner::compaction::{CompactionConfig, TruncationCompaction, SummarisationCompaction};

// Create a compaction config with a strategy and budget
let config = CompactionConfig::new(
    Box::new(TruncationCompaction { preserve_recent: 4 }),
    2000, // token budget — compaction triggers above this threshold
);

// Apply compaction only when needed
let compacted_events = config.compact_if_needed(&events);
```

### TruncationCompaction

Drops older events while preserving the most recent N. Zero-cost (no LLM call), but loses historical context entirely:

```rust
use adk_runner::compaction::TruncationCompaction;

let strategy = TruncationCompaction {
    /// Number of recent events to always preserve.
    preserve_recent: 4,
};

// Compaction keeps only the 4 most recent events,
// then continues dropping from the front if still over budget.
let compacted = strategy.compact(&events, budget);
```

### SummarisationCompaction

Replaces older events with an LLM-generated summary that preserves semantic meaning. Costs one LLM call but retains context coherence:

```rust
use adk_runner::compaction::SummarisationCompaction;

let strategy = SummarisationCompaction {
    /// Number of older turns to condense into a single summary message.
    turns_to_summarise: 6,
};

// Older events are replaced by a system message containing
// an LLM-generated summary of the conversation so far.
let compacted = strategy.compact(&events, budget);
```

### Full Configuration with RunConfig

Integrate compaction into the agent runner's configuration for automatic context management during multi-turn conversations:

```rust
use adk_runner::compaction::{CompactionConfig, SummarisationCompaction};

// Summarisation strategy — preserves semantics across compaction
let compaction = CompactionConfig {
    strategy: Box::new(SummarisationCompaction {
        turns_to_summarise: 6,
    }),
    context_budget: 2000,
};

// The runner automatically applies compaction when the
// conversation context exceeds the configured budget.
```

### Choosing a Strategy

| Strategy | Cost | Latency | Semantic Preservation |
|----------|------|---------|----------------------|
| `TruncationCompaction` | Free | None | No — older context is lost |
| `SummarisationCompaction` | 1 LLM call | ~1-2s | Yes — summary retains meaning |

Use truncation for cost-sensitive workloads where only recent context matters. Use summarisation when follow-up questions may reference earlier parts of the conversation.
