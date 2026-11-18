# Context Compaction

As an ADK agent runs, it accumulates context — user messages, tool responses, generated content. As this context grows, LLM processing times increase because more data is sent with each request, until the request no longer fits the model's context window. Context compaction addresses this by summarizing older history.

Two mechanisms compact, and they compose:

| Mechanism | When it runs | Default |
|-----------|--------------|---------|
| Auto-compaction (`IntraCompactionConfig`) | Before every model call inside a run, once the request nears the model's context window | On |
| Session compaction (`EventsCompactionConfig`) | After every `compaction_interval` invocations | Off; on in the harness |

## Auto-Compaction Within a Run

Before every model call, `LlmAgent` and `CodeActAgent` ask the run's compactor
(`RunConfig::history_compactor`, installed by the runner at the start of each run) to compact
their history. Once the request passes the threshold, the compactor summarizes all but the most
recent history entries — at most `overlap_event_count` of them, within half the threshold — into
one summary turn, and the agent continues from it. A long tool loop — a harness goal, a 1,000-step autonomous run — therefore never grows
its request past the model's window.

| Setting | Default | Effect |
|---------|---------|--------|
| `enabled` | `true` | `IntraCompactionConfig::disabled()` turns auto-compaction off |
| `token_threshold` | `None` | A fixed threshold in estimated tokens (`IntraCompactionConfig::at_tokens(n)`); takes precedence over the window |
| `context_window_fraction` | `0.8` | The share of the model's context window (`Llm::context_window`) a request may reach |
| — | 100,000 tokens | The threshold when the model reports no context window (`DEFAULT_TOKEN_THRESHOLD`) |
| `overlap_event_count` | 10 | The most recent history entries kept as they are; they also stay within half the threshold, so the compacted request has room to grow |
| `chars_per_token` | 4 | The character-per-token ratio of the estimate |
| `native` | `true` | A model whose provider compacts conversations itself writes the summary ([Provider-Native Compaction](#provider-native-compaction)); `with_native(false)` keeps the runner's summarizer |
| `background` | `false` | The summary is written off the critical path ([Background Compaction](#background-compaction)); `in_background(fraction)` turns it on |
| `background_fraction` | `0.7` | The share of the threshold at which a background compaction starts |

The estimate covers the agent's instructions and history, and the agent also passes the prompt
size the provider reported for its previous call, so a history the character estimate
undercounts still crosses the threshold. `adk-model` providers report their window from the
pricing catalog (`PricingCatalog::context_window`), which records each vendor's published figure;
a model with no recorded window compacts at the 100,000-token default.

### Changing It

| Where | Threshold | Summarizer or strategy | Off |
|-------|-----------|------------------------|-----|
| Runner | `.intra_compaction_config(config)` | `.intra_compaction_summarizer(s)`, or `.context_compaction(CompactionConfig)` with the `context-compaction` feature | `.without_intra_compaction()` |
| One run | `RunConfig::intra_compaction` | `RunConfig::history_compactor` (any `HistoryCompactor`) | `RunConfig::builder().intra_compaction(IntraCompactionConfig::disabled())` |
| Harness | `HarnessAgentBuilder::intra_compaction(config)` | — | `.without_intra_compaction()` |
| CLI | `[compaction]` in `harness.toml` | — | `auto = false` |

```rust
use adk_core::IntraCompactionConfig;
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use std::sync::Arc;

fn build(agent: Arc<dyn adk_core::Agent>) -> adk_core::Result<Runner> {
    Runner::builder()
        .app_name("my_app")
        .agent(agent)
        .session_service(Arc::new(InMemorySessionService::new()))
        // Compact past 60% of the model's window, keeping the 6 most recent entries.
        .intra_compaction_config(
            IntraCompactionConfig::at_window_fraction(0.6).with_overlap_event_count(6),
        )
        .build()
}
```

By default the summary is written by the model the agent is calling: by its provider when the
model compacts conversations itself (`Llm::native_compaction`), and otherwise through
`LlmEventSummarizer`. A summarizer set with `intra_compaction_summarizer` takes the place of both,
and a `CompactionStrategy` set with `context_compaction` replaces the summary altogether: before
any call whose request passes its `context_budget`, the history is shortened with the strategy,
applied up to `max_retries` times.

### Provider-Native Compaction

A model that reports `Llm::native_compaction` writes the summary on its provider's side. The
runner sends the entries it compacts with the agent's instructions, tools and generation config,
marked with `NativeCompactionRequest`, and the provider returns a signed compaction in place of a
reply. The compaction becomes the first history entry, ahead of the kept entries, and every later
request sends it back exactly as the provider returned it.

| Provider | Models | Wire format | Kept entries keep their thinking |
|----------|--------|-------------|----------------------------------|
| Anthropic (`AnthropicClient`, Claude API) | Claude Fable 5.2 / 5.1 / 5, Mythos 5.2 / 5.1 / 5 / Preview, Opus 5.5 / 5 / 4.8 / 4.7 / 4.6, Sonnet 5.5 / 5 / 4.6, Haiku 5.5 | On-demand compaction: the `compaction` parameter and the signed `compaction` block, beta `compact-2026-09-04` | Yes |

The compaction entry is a model turn with two parts:

| Part | Read by |
|------|---------|
| The summary as text, headed `COMPACTION_SUMMARY_HEADING` | Every provider, including a fallback model of another vendor |
| The provider's signed copy, flagged replay-only and `NATIVE_COMPACTION_KEY` (`is_native_compaction`) | Only the provider and model that wrote it (`retain_replayable`) |

`AnthropicClient` sends the signed block first in `messages`, with the beta header, on every
request that carries it. A block anywhere else in a request, or one whose summary text a callback
rewrote, is sent as the summary's text instead, so the request never fails on it.

The compaction call is a model call made on behalf of the run: it counts against the run's budget
and spend ledger through the run's `ModelCallScope`, a frozen kill switch refuses it, and a call
held for confirmation, with everything after it, stays out of it. Its usage is the summarization
call's, which the Claude API reports in `usage.iterations`. A provider compaction that fails —
the call is refused or declined, or returns no compaction — falls back to `LlmEventSummarizer`
for that compaction.

```rust
use adk_core::{IntraCompactionConfig, Llm};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use std::sync::Arc;

fn build(agent: Arc<dyn adk_core::Agent>, api_key: &str) -> adk_core::Result<Runner> {
    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-5-5"))?;
    assert!(model.native_compaction().is_some());
    Runner::builder()
        .app_name("my_app")
        .agent(agent)
        .session_service(Arc::new(InMemorySessionService::new()))
        // Claude writes the summary; `with_native(false)` would keep `LlmEventSummarizer`.
        .intra_compaction_config(IntraCompactionConfig::default())
        .build()
}
```

### Background Compaction

With `IntraCompactionConfig::in_background(fraction)`, the summary is written while the agent goes
on, so no model call waits for it:

| When a model call finds | The compactor |
|-------------------------|---------------|
| A finished background summary whose entries still open the history | Swaps it in: the summary, then every entry after the ones it covers, including those added while it was written |
| A request past the threshold, or rejected as too long, with a summary still being written | Waits for that summary and swaps it in, instead of starting another |
| A request past `background_fraction` of the threshold and no summary pending | Starts one in the background and lets the call go ahead |
| A history that no longer starts with the entries a finished summary covers | Discards the summary |

The runner keeps a pending summary across runs, per session, agent and branch, so a summary started
during one turn opens the next turn without that turn paying for it; a summary no run takes within
an hour is dropped. A swap is recorded in the session like any compaction.

No background compaction starts while a call is held for confirmation, as OpenAI's Agents SDK
defers auto-compaction while approvals are pending, or while a kill switch is frozen. The summary
call runs in the run's `ModelCallScope`, so it counts against that run's budget and spend ledger
and a freeze refuses it. A provider-native summary written in the background keeps the kept
entries' thinking under the same conditions as one written in place.

```rust
use adk_core::IntraCompactionConfig;
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use std::sync::Arc;

fn build(agent: Arc<dyn adk_core::Agent>) -> adk_core::Result<Runner> {
    Runner::builder()
        .app_name("my_app")
        .agent(agent)
        .session_service(Arc::new(InMemorySessionService::new()))
        // Start summarizing at 60% of the threshold, while the agent works.
        .intra_compaction_config(IntraCompactionConfig::default().in_background(0.6))
        .build()
}
```

Background compaction trades spend for latency: a summary starts before the threshold requires
one, and a session that never continues pays for a summary it does not use.

### A Request the Model Rejects as Too Long

When a model rejects a request as longer than its context window
(`adk_core::intra_compaction::is_context_limit_error`: Anthropic's "prompt is too long", OpenAI's
`context_length_exceeded`, Gemini's input token limit), the agent compacts whatever the estimate
says and sends the call once more. The rejection arrives inside the model's response stream, before
any output. A second rejection of the same call, or a history with nothing left to compact, ends
the run with the model's error.

### Recorded in the Session

Each compaction is yielded as an event carrying `EventActions::compaction`, which the runner
persists, so later runs — the next iteration of a `LoopAgent`, the next user turn — start from
the summary instead of the full history. The compaction ends at the session event before the
first entry it keeps. The agent compacts only its working history, and records nothing, when:

- the first kept entry matches no single session event a whole millisecond after the event
  before it, so a store that keeps milliseconds only could not tell them apart;
- the agent runs in a `ParallelAgent` branch, since a session's compaction applies to every
  branch;
- the agent is an agent tool's delegate, which reads a session of its own.

`CodeActAgent` compacts its transcript, which its checkpoints carry.

A compaction event is not a final response (`Event::is_final_response`). An agent that calls a
model itself compacts the same way: before each call it passes its history to
`ctx.run_config().history_compactor` and continues from the result.

### Thinking in Kept Entries

A provider binds a turn's thinking to the history it was produced with, and Anthropic's
history-editing check rejects thinking replayed after earlier turns were replaced. Entries a
compaction keeps are therefore sent without their thinking parts, both in the agent's working
history and when a later run reads the session; their text, tool calls (with any thought
signatures) and tool results stay. Session compaction's kept events are treated the same way.

A provider-native compaction whose provider accepts the kept thinking
(`NativeCompaction::keeps_thinking`) keeps it, when the first kept entry and the last summarized
one have different speakers and no call the person asked for is among them. The compaction event
records that decision, so a later run that reads the session replays the same entries.

### Limits

- A summary that fails, or that is not smaller than what it replaces, leaves the history as it
  is; the runner then waits until the history has grown by `overlap_event_count` entries before
  trying again.
- The summary request carries the summarized entries, so a history far past the window when the
  model rejects it can be too long to summarize in one call; the default threshold leaves 20%
  of the window for this.
- One tool round — a model turn's calls and their results — is never split, so a round that
  alone passes the threshold, such as many parallel calls with large results, is sent whole.
- A call held for confirmation, and everything after it, is never compacted (see
  [Calls Held for Confirmation](#calls-held-for-confirmation)).

## Session Compaction

### How It Works

Context compaction periodically summarizes older conversation events within a session. After an invocation completes, the runner counts the session's user messages; each time the count is a multiple of `compaction_interval`, the summarizer compresses every event before the most recent `overlap_size` invocations into one summary event.

```
compaction_interval 3, overlap_size 1
After invocation 3: [inv 1, inv 2]                  → Summary A; inv 3 kept as is
After invocation 6: [Summary A, inv 3, inv 4, inv 5] → Summary B; inv 6 kept as is
```

`overlap_size` is the number of most recent invocations left out of the summary, so the model sees them verbatim beside it; the next compaction folds them into its summary. With `overlap_size: 0` the whole history is summarized; with fewer invocations than `overlap_size`, compaction is skipped.

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

### Configuration

Add compaction to your runner configuration:

```rust
use adk_agent::LlmEventSummarizer;
use adk_runner::{EventsCompactionConfig, Runner};
use std::sync::Arc;

// Use any LLM for summarization (a fast, cheap model works well)
let summarizer = Arc::new(LlmEventSummarizer::new(summarizer_llm));

let runner = Runner::builder()
    .app_name("my_app")
    .agent(root_agent)
    .session_service(sessions)
    .compaction_config(EventsCompactionConfig {
        compaction_interval: 3, // Compact every 3 invocations
        overlap_size: 1,        // Keep the latest invocation out of the summary
        summarizer,
    })
    .build()?;
```

### Configuration Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `compaction_interval` | `u32` | Compaction runs when the session's user-message count is a multiple of this |
| `overlap_size` | `u32` | Most recent invocations left out of the summary; `0` summarizes everything |
| `summarizer` | `Arc<dyn BaseEventsSummarizer>` | The summarization strategy |

### Custom Summarizer

You can customize the summarization prompt:

```rust
use adk_agent::LlmEventSummarizer;

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

### How Compaction Affects History

When `conversation_history()` (`adk_core::Session`) is called on a session with compaction events:

1. The most recent compaction event is found
2. Its summary replaces all events up to the compaction boundary
3. Only events after the boundary are included individually

This is transparent to agents — they receive a coherent conversation history that includes the summary followed by recent events.

### Example Timeline

With `compaction_interval: 3` and `overlap_size: 1`:

| Invocation | Events | Action |
|------------|--------|--------|
| 1 | user→agent | — |
| 2 | user→agent | — |
| 3 | user→agent | Compact invocations 1-2 into Summary A |
| 4 | user→agent | — |
| 5 | user→agent | — |
| 6 | user→agent | Compact Summary A and invocations 3-5 into Summary B |

After invocation 6, the agent sees: `[Summary B, invocation 6, invocation 7+]`

## Function Calls and Their Responses

A provider rejects a function response whose call is not in the request. Every compaction cut
therefore lands where no response after the cut answers a call before it
(`adk_core::history::balanced_cut`): the cut moves back to before such a call, keeping the call,
its response and every later event.

| Compaction | Where it cuts |
|------------|---------------|
| Session compaction (`EventsCompactionConfig`) | At the overlap boundary, moved back before any call answered after it |
| Auto-compaction (`IntraCompactionConfig`) | `overlap_event_count` history entries from the end, moved back the same way (`balanced_content_cut`), and back again to an entry the session can place when it is recorded |
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
  than the events it would replace, for session compaction and auto-compaction alike.
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
| Auto-compaction (`IntraCompactionConfig`) | The summarized entries end before the held call (`held_call_content_limit`); the call and every later entry are kept |
| Context compaction (`CompactionConfig`, any `CompactionStrategy`) | The strategy receives only the events before the held call; the call and every later event are appended unchanged |

Once the call is answered — resumed with a decision, or issued again and run — the next
compaction covers it as usual. A call that is never decided keeps the events from it onward
uncompacted; deny it to release them.

## Notes

- Compaction failure is non-fatal — the runner logs a warning and continues
- Session compaction runs after the invocation's events have streamed, before the run's stream
  ends; auto-compaction runs before each model call
- A history processor (`LlmAgentBuilder::history_processor`, see
  [History Processors](../callbacks/callbacks.md#history-processors)) runs before
  auto-compaction on every call, for windows and rewrites of your own
- The summarizer's model call is metered against the run's budget and spend ledger (`ModelCallScope`)
- The compaction event is persisted to the session service for durability
- Use a fast, inexpensive model for summarization (e.g., `gemini-3.5-flash-lite`)
