# Context Compaction

As an ADK agent runs, it accumulates context — user messages, tool responses, generated content. As this context grows, LLM processing times increase because more data is sent with each request. Context compaction addresses this by summarizing older events using a sliding window approach.

## How It Works

Context compaction uses a sliding window to periodically summarize older conversation events within a session. When the number of completed invocations reaches the configured interval, the summarizer compresses older events into a single summary event.

```
Invocations 1-3: [event1, event2, event3] → Summarized into "Summary A"
Invocations 4-6: [Summary A, event3, event4, event5, event6] → Summarized into "Summary B"
```

The `overlap_size` parameter controls how many events from the previous window carry over into the next summary, preserving continuity.

### What the Summarizer Reads

`LlmEventSummarizer` summarizes the window as the model sees it:

| Input | How it reaches the summary prompt |
|-------|-----------------------------------|
| The most recent earlier summary in the window | First, as `summary of earlier conversation: …`; the events it covers are left out |
| Text | `<author>: <text>` |
| Function calls | `<author>: [function call] <name>(<arguments JSON>)` |
| Function responses | `<author>: [function response] <name>: <response JSON>`, plus any attached file or inline data references |
| Inline data, file data, embedded resources, server tool calls and responses | A one-line reference (MIME type, size or URI, resource text) |
| Thinking parts and streamed partial events | Left out |

`SummarisationCompaction` (context compaction) renders events the same way, and the token
estimate that decides when context compaction runs counts function call arguments and responses
as well as text, so a history of large tool results crosses `context_budget`.

Each summary therefore carries the one before it, so facts from older turns and tool results
survive repeated compaction. This matters with `RunConfig::history_max_events`: the runner then
loads only the most recent events, so an older turn reaches the model only through the
summaries.

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

## Function Calls and Their Responses

A provider rejects a function response whose call is not in the request. Every compaction cut
therefore lands where no response after the cut answers a call before it
(`adk_core::history::balanced_cut`): the cut moves back to before such a call, keeping the call,
its response and every later event.

| Compaction | Where it cuts |
|------------|---------------|
| Session compaction (`EventsCompactionConfig`) | At the overlap boundary, moved back before any call answered after it |
| Intra-invocation compaction (`IntraCompactionConfig`) | `overlap_event_count` events from the end, moved back the same way |
| `TruncationCompaction` | Keeps the first event and the last `preserve_recent`, plus the call of any kept response |
| `SummarisationCompaction` | Summarises the first `turns_to_summarise` events, or fewer so that no call is split from its response |

A `RunConfig::history_max_events` window can still start on a response whose call fell outside
it. History leaves such a response out, as it does a second response to a call already
answered, so the request stays valid.

## Summaries That Do Not Shrink the History

A summary replaces history only when it is complete and smaller:

- `LlmEventSummarizer` fails with `model.summary_incomplete` when the model stops for any reason
  but `Stop` (for example `MaxTokens`) or its stream fails, and returns no summary when the
  summary is empty or at least as long as the window it renders.
- The runner keeps the history when a summarizer's event is not smaller, by the token estimate,
  than the events it would replace, for session and intra-invocation compaction alike.
- `SummarisationCompaction` keeps the events when its summary is empty or not smaller, and fails
  with `model.summary_incomplete` on an incomplete summary or a failed stream.
- For a failed stream, `model.summary_incomplete` keeps the stream error's category, retry hint,
  and details (provider, upstream status), records its code in
  `details.metadata["cause_code"]`, and has it as its `source()`, so a rate-limited or timed-out
  summary call stays retryable.

## Calls Held for Confirmation

Compaction never covers a tool call that is still held for confirmation. A compaction hides
every event up to its end, and an agent resumes only the held calls it can still see, so a
compacted held call would leave its approval with nothing to resume, and a durable run holding
it would complete instead of parking.

Every compaction the runner applies stops before the earliest event that issued a call still
held, by any agent on any branch (`adk_core::held_call_compaction_limit`):

| Compaction | Effect of a held call |
|------------|-----------------------|
| Session compaction (`EventsCompactionConfig`) | The window ends before the held call, whatever `overlap_size` allows; with nothing before it, the run skips compaction |
| Intra-invocation compaction (`IntraCompactionConfig`) | The summarized events end before the held call; the call and every later event are kept |
| Context compaction (`CompactionConfig`, any `CompactionStrategy`) | The strategy receives only the events before the held call; the call and every later event are appended unchanged |

Once the call is answered — resumed with a decision, or issued again and run — the next
compaction covers it as usual. A call that is never decided keeps the events from it onward
uncompacted; deny it to release them.

## Notes

- Compaction failure is non-fatal — the runner logs a warning and continues
- Compaction runs after the invocation stream completes, not during
- The compaction event is persisted to the session service for durability
- Use a fast, inexpensive model for summarization (e.g., `gemini-3.5-flash-lite`)
