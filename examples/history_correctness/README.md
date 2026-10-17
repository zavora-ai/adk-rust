# History Correctness

What each agent's model receives, and what the session keeps. Every scenario drives the real
stack — `LlmAgentBuilder` agents, `SequentialAgent`, `LoopAgent`, an `AgentTool`, and `Runner`
with intra-invocation compaction, session compaction (`EventsCompactionConfig` with
`LlmEventSummarizer`) and context compaction (`TruncationCompaction`,
`SummarisationCompaction`) — over a SQLite session service, and asserts on the requests a
provider would receive, never on model prose.

A scripted OpenAI-compatible upstream on `127.0.0.1` plays the model through the real
`OpenAIClient` and records every request with the agent that sent it. `ScriptedLlm` plays a
provider whose stream ends on a partial chunk. The `live_*` scenarios run against Gemini
(`gemini-3.5-flash-lite`), Anthropic (`claude-sonnet-4-6`) and OpenAI (`gpt-5.6-terra`) when
their keys are set.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `ar1_sequential_foreign_turns` | Scripted | In `SequentialAgent[a, b]`, b's request carries a's call, result and answer as user-role context, no assistant or tool turn of a's, and ends on a user turn |
| `ar1_loop_foreign_turns` | Scripted | In `LoopAgent[a, b]` over two rounds, each agent keeps only its own answers as model turns, reads the other's as context, and every request ends on a user turn |
| `live_ar1_gemini_then_claude` | Gemini + Anthropic | `SequentialAgent[gemini-3.5-flash-lite, claude-sonnet-4-6]` completes and the Claude agent answers |
| `ar2_history_window_orphan` | Scripted | With `history_max_events = 2` starting on a tool result, the request carries no result without its call |
| `live_ar2_anthropic`, `live_ar2_openai`, `live_ar2_gemini` | Anthropic, OpenAI, Gemini | The same window is accepted by each provider and it answers |
| `ss1_intra_compaction_cut` | Scripted | Intra-invocation compaction with an overlap that would cut between a call and its result leaves no orphan result |
| `ss1_truncation_compaction_cut` | Scripted | `TruncationCompaction` keeps the call of every result it keeps |
| `ss1_summarisation_compaction_cut` | Scripted | `SummarisationCompaction` never summarises a call whose result it keeps |
| `ss16_tool_payload_compaction` | Scripted | An 8,000-character tool result triggers context compaction, the summary prompt carries the result's facts, and the next request no longer carries the payload |
| `ss19_summary_must_shrink` | Scripted | Session compaction keeps the history when the summary is longer than it, or was cut short (`finish_reason: length`) |
| `ar7_temp_state_in_invocation` | Scripted | A `temp:` key set by one tool is read by the next tool and by a transferred agent's `{temp:token}` instruction, and is not persisted |
| `ar11_partial_tail_persisted` | `ScriptedLlm` | A stream that ends on partial chunks is persisted whole and replayed on the next turn |
| `ar12_empty_stop_turns` | Scripted | Empty, whitespace-only and thinking-only turns fail with `model.empty_response` and write no `output_key` |
| `ar6_after_model_replacement` | Scripted | An `after_model_callback` that rebuilds each streamed delta persists one event per response on SQLite, with usage, and the next request replays it once |
| `ar15_fenced_json_schema` | Scripted | A fenced JSON answer passes `output_schema` in one model call and `output_key` holds the JSON without its fence |
| `ar16_forwarded_agent_tool_events` | Scripted | Events `AgentTool::forward_events(true)` forwards stay out of the caller's next request; the sub-agent's answer reaches it as the tool result |

## Running

```bash
cargo run --manifest-path examples/history_correctness/Cargo.toml
cargo run --manifest-path examples/history_correctness/Cargo.toml -- --no-live
cargo run --manifest-path examples/history_correctness/Cargo.toml -- --scenario ar1_sequential_foreign_turns,ar12_empty_stop_turns
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence, followed by a summary table;
the process exits non-zero on any `FAIL`. Keys are loaded from the dotenv file named by
`ADK_ENV_FILE`, which overrides the process environment, else from a `.env` in the working
directory. `--no-live` skips the live scenarios.
