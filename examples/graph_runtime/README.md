# Graph Runtime

How a graph dispatches, streams, retries, pauses, and stops, on the stack a user runs. Every
graph scenario runs a `GraphAgent` under a `Runner` with `LlmAgent` nodes; the two workflow
scenarios run an `#[entrypoint]` workflow whose tasks call an `LlmAgent` through a `Runner`.
Checkpoints go to the in-memory, SQLite, or Redis checkpointer.

Scripted scenarios point the real `OpenAIClient` at a scripted OpenAI-compatible Chat Completions
server on `127.0.0.1`. Live scenarios run the same graphs on Gemini when `GOOGLE_API_KEY` is set,
or call the real OpenAI endpoint with a key it rejects, which needs no key.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `session_state_templates` | Scripted | An agent node's instruction `{customer_name}`, `{plan_tier}`, `{topic}` resolves from session state and graph state, and the agent sees the session's real id |
| `live_session_state_templates` | Gemini | The same graph greets the customer by name |
| `node_events_stream` | Scripted | The researcher's and writer's events reach the `Runner` stream on their node branches, the researcher's at least 1 s before the turn ends, and the session holds both |
| `live_node_events_stream` | Gemini | Both agents' events stream, in order, and the session holds both |
| `failed_step_stops_dispatch` | Scripted, `400` and `503` | `b_validate` fails: queued `d_ship` does not run, `c_enrich`'s 1 s backoff is cut short, the turn ends in under 1.5 s, and the next turn resumes with `a_audit`'s kept write |
| `live_failed_step_stops_dispatch` | OpenAI `401`, scripted `503` | The same with a live `401`: `d_ship` does not run and the turn ends in under 3.5 s |
| `suspension_skips_retry` | Scripted | A `#[task(retry)]` that pauses suspends at once as `interrupt-1`, is not logged as failed, and resumes at once |
| `live_suspension_skips_retry` | Gemini | The same with a live drafting task |
| `entry_join_runs_once` | Scripted | `compose`, reached from `START` and from `lookup`, calls its model once, with the order status |
| `live_entry_join_runs_once` | Gemini | The same with a live answer |
| `retry_delay_within_max` | Scripted, `503` | With full jitter, none of 12 retry delays exceeds `max_delay` (200 ms, plus 25 ms) |
| `live_retry_delay_within_max` | OpenAI `401` | None of 7 delays exceeds `max_delay` (300 ms, plus 25 ms) |
| `parallel_pauses_surface` | Scripted | Two agent nodes asking to confirm a tool call in one step both reach the caller; answering both finishes the graph with each tool run once |
| `live_parallel_pauses_surface` | Gemini | The same, answered by fingerprint |
| `resume_answer_is_scoped` | Scripted | An approval resumes its pause, and the next refund on the next turn pauses again |
| `live_resume_answer_is_scoped` | Gemini | The same with a live planner |
| `agent_error_category` | Scripted, `429` | The turn fails with `RateLimited`, HTTP 429, retryable |
| `live_agent_error_category` | OpenAI `401` | The turn fails with `Unauthorized`, HTTP 401 |
| `cancellation_stops_workflow` | Scripted | Cancelling the `ExecutionConfig` token stops the workflow: no task starts after it |
| `live_cancellation_stops_workflow` | Gemini | The same while a live task runs |
| `child_single_flight` | Scripted | Two concurrent `run_node_with` calls under one run id call the charge agent's model once |
| `live_child_single_flight` | Gemini | The same with a live charge agent |

## Running

```bash
cp examples/graph_runtime/.env.example examples/graph_runtime/.env   # optional, for the live scenarios
cargo run --manifest-path examples/graph_runtime/Cargo.toml
cargo run --manifest-path examples/graph_runtime/Cargo.toml -- --checkpointer memory
REDIS_URL=redis://127.0.0.1:6379 cargo run --manifest-path examples/graph_runtime/Cargo.toml -- --checkpointer redis
cargo run --manifest-path examples/graph_runtime/Cargo.toml -- --scenario node_events_stream,failed_step_stops_dispatch
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on
any `FAIL`. `--checkpointer` takes `memory`, `sqlite` (the default), or `redis`. Set
`ADK_ENV_FILE` to read keys from another dotenv file and `GEMINI_MODEL` to change the live model
(default `gemini-3.5-flash-lite`).
