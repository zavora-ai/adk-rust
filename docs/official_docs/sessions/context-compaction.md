# Context Compaction

As an ADK agent runs, it accumulates context — user messages, tool responses, generated content. As this context grows, LLM processing times increase because more data is sent with each request. Context compaction addresses this by summarizing older events using a sliding window approach.

## How It Works

Context compaction uses a sliding window to periodically summarize older conversation events within a session. When the number of completed invocations reaches the configured interval, the summarizer compresses older events into a single summary event.

```
Invocations 1-3: [event1, event2, event3] → Summarized into "Summary A"
Invocations 4-6: [Summary A, event3, event4, event5, event6] → Summarized into "Summary B"
```

The `overlap_size` parameter controls how many events from the previous window carry over into the next summary, preserving continuity.

## Configuration

Add compaction to your runner configuration:

```rust
use adk_agent::LlmEventSummarizer;
use adk_runner::{Runner, RunnerConfig, EventsCompactionConfig};
use std::sync::Arc;

// Use any LLM for summarization (a fast, cheap model works well)
let summarizer_llm = Arc::new(my_model);
let summarizer = Arc::new(LlmEventSummarizer::new(summarizer_llm));

let runner = Runner::new(RunnerConfig {
    app_name: "my_app".to_string(),
    agent: root_agent,
    session_service: sessions,
    artifact_service: None,
    memory_service: None,
    plugin_manager: None,
    run_config: None,
    compaction_config: Some(EventsCompactionConfig {
        compaction_interval: 3,  // Compact every 3 invocations
        overlap_size: 1,         // Keep 1 prior invocation for context
        summarizer,
    }),
})?;
```

## Configuration Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `compaction_interval` | `u32` | Number of completed invocations that triggers compaction |
| `overlap_size` | `u32` | Events from the previous window included in the next summary |
| `summarizer` | `Arc<dyn BaseEventsSummarizer>` | The summarization strategy |

## Custom Summarizer

You can customize the summarization prompt:

```rust
let summarizer = LlmEventSummarizer::new(llm)
    .with_prompt_template(
        "Summarize this conversation focusing on action items \
         and decisions:\n\n{conversation_history}"
    );
```

Or implement `BaseEventsSummarizer` for full control:

```rust
use adk_core::{BaseEventsSummarizer, Event, Result};
use async_trait::async_trait;

struct MySummarizer;

#[async_trait]
impl BaseEventsSummarizer for MySummarizer {
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
        // Custom summarization logic
        // Return None to skip compaction for this window
        todo!()
    }
}
```

## How Compaction Affects History

When `conversation_history()` is called on a session with compaction events:

1. The most recent compaction event is found
2. Its summary replaces all events up to the compaction boundary
3. Only events after the boundary are included individually

This is transparent to agents — they receive a coherent conversation history that includes the summary followed by recent events.

## Example Timeline

With `compaction_interval: 3` and `overlap_size: 1`:

| Invocation | Events | Action |
|------------|--------|--------|
| 1 | user→agent | — |
| 2 | user→agent | — |
| 3 | user→agent | Compact events 1-3 into Summary A |
| 4 | user→agent | — |
| 5 | user→agent | — |
| 6 | user→agent | Compact events 3-6 (overlap=1) into Summary B |

After invocation 6, the agent sees: `[Summary B, event 6 overlap events, event 7+]`

## Notes

- Compaction failure is non-fatal — the runner logs a warning and continues
- Compaction runs after the invocation stream completes, not during
- The compaction event is persisted to the session service for durability
- Use a fast, inexpensive model for summarization (e.g., `gemini-2.0-flash`)
