# Agent Lifecycle

How an agent's turn starts, hands off, and ends, and what the runner's plugins see. Every
scenario drives the real stack — `LlmAgentBuilder` agents, `ParallelAgent`, `LoopAgent`, a
`PluginManager`, the `RetryReflectPlugin`, and `Runner` over the in-memory session service —
and asserts on what a provider receives, what the session keeps, and which callbacks ran,
never on model prose.

A scripted OpenAI-compatible upstream on `127.0.0.1` plays the model through the real
`OpenAIClient`, records every request, and produces the faults a live API does not: an HTTP
400 and a stream that ends on an error frame. The `live_*` scenarios run the same checks
against Gemini (`gemini-3.5-flash-lite`) when `GOOGLE_API_KEY` is set; the model-fallback
scenario sends its primary call to a model ID the API does not serve.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `ar5_tool_transfer_restrictions` | Scripted | A worker with `disallow_transfer_to_parent` and `disallow_transfer_to_peers` whose tool hands control to its parent, or to a peer, keeps control: the target gets no model call |
| `live_ar5_tool_transfer_restrictions` | Gemini | The same tree on Gemini: the coordinator gets one model call, not a second turn after the worker's `hand_back` tool |
| `ar8_literal_braces` | Scripted | An instruction with `${HOME}`, `{{customer_name}}` and `\{not_a_var}` reaches the model as written, with `{topic}` filled in and `{nickname?}` empty |
| `live_ar8_literal_braces` | Gemini | The same instruction runs and Gemini answers from the filled-in topic |
| `ar9_after_agent_exit_paths` | Scripted | `after_agent` runs after a transfer (for both agents), a turn-ending tool result, a cancellation, and an answer, and not for a held call |
| `live_ar9_after_agent_exit_paths` | Gemini | `after_agent` runs after a Gemini transfer and a turn-ending tool result |
| `ar10_after_run_on_dropped_stream` | Scripted | A consumer that drops the stream while a tool runs: `after_run` runs once, the session answers the call outcome-unknown, and the run is no longer active |
| `live_ar10_after_run_on_dropped_stream` | Gemini | The same with Gemini calling the slow tool |
| `ar13_on_model_error_fallback` | Scripted | An HTTP 400 and a stream error frame each reach the plugin's `on_model_error` once, and its response answers the turn and is persisted |
| `live_ar13_on_model_error_fallback` | Gemini | A call to a model ID Google does not serve fails with 404; the plugin answers with `gemini-3.5-flash-lite` |
| `ar14_retry_reflect_counters` | Scripted | After one user uses up a `max_retries(1)` budget, another user's first failure is still reflected; after a success, the next failure is reflected again |
| `live_ar14_retry_reflect_counters` | Gemini | Two users of one plugin each get a reflection for their first failure |
| `ar17_short_circuit_output_key` | Scripted | A `before_agent` callback's answer is stored under `output_key`, with no model call |
| `live_ar17_short_circuit_output_key` | Gemini | Turn 1 stores the callback's answer, turn 2 Gemini's |
| `ar18_output_key_last_turn` | Scripted | `output_key` holds the final answer, not the text beside the tool call |
| `live_ar18_output_key_last_turn` | Gemini | `output_key` holds Gemini's final turn |
| `ar19_parallel_escalate` | Scripted | In `LoopAgent[ParallelAgent[escalator, slowpoke]]`, the escalation stops slowpoke's 4 s scan and the loop after one round |
| `live_ar19_parallel_escalate` | Gemini | The same on Gemini with a 10 s scan |
| `ar20_on_event_coverage` | Scripted | `on_event` sees a `before_run` early exit, the answer a cancelled run gives its call, and a budget stop event |
| `live_agent_tool_nesting` | Gemini | Three nested `AgentTool`s on a runtime worker thread (2 MiB stack): run 1 surfaces the innermost agent's held `delete_file`, run 2 approves it and it runs once |
| `ar21_runner_close` | Scripted | `Runner::close` closes the plugin and the toolsets of the root and its sub-agent once each |
| `live_ar21_runner_close` | Gemini | A Gemini turn uses the toolset's tool, then `Runner::close` closes the toolset and the plugin |

## Running

```bash
cargo run --manifest-path examples/agent_lifecycle/Cargo.toml
cargo run --manifest-path examples/agent_lifecycle/Cargo.toml -- --no-live
cargo run --manifest-path examples/agent_lifecycle/Cargo.toml -- --scenario ar5_tool_transfer_restrictions,ar9_after_agent_exit_paths
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence, followed by a summary table;
the process exits non-zero on any `FAIL`. A live scenario that hits a Gemini rate limit runs
again once after 60 seconds. Keys are loaded from the dotenv file named by `ADK_ENV_FILE`,
which overrides the process environment, else from a `.env` in the working directory.
`--no-live` skips the live scenarios.

The `main-baseline` feature builds against a release without `Runner::close` and
`Toolset::close`, so the AR21 scenarios report `FAIL` instead of compiling.
