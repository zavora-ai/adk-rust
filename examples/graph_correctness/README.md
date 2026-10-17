# Graph Correctness

Joins after conditional branches and loops, agent errors inside a graph, and work a graph
re-enters — subgraphs, children invoked with `run_node`, and graph tools. Every scenario runs a
real graph as a `GraphAgent` under a `Runner`, checkpointed to SQLite, and asserts on what the
turn answered, what each model received, how often each node ran, and what the checkpoint holds.

Agent nodes are `LlmAgent`s on the real `OpenAIClient`, pointed at a scripted OpenAI-compatible
Chat Completions server on `127.0.0.1`; two scenarios play a provider that answers `429`. The
`live_conditional_merge` scenario runs one `LlmAgent` node on Gemini when `GOOGLE_API_KEY` is set.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `review_loop` | Scripted | Three reviewer agents re-entered by a revise loop, as in `examples/coding_graph`, run in both rounds (6 reviewer calls), `synthesize` runs once per round, and the turn answers `approved in round 2` |
| `conditional_merge` | None | A merge after a conditional branch runs on each of two turns, once per turn, and the final SQLite checkpoint leaves no join waiting |
| `builder_join` | None | A join built with `GraphAgentBuilder` on branches of unequal length runs once and sees both branches |
| `agent_error_stops_the_graph` | Scripted, `429` | A rate-limited writer agent fails its node: the 2-attempt retry policy sends 2 requests, the turn ends with the error, and `publish` never runs |
| `agent_error_is_retried` | Scripted, `429` once | The writer's retry policy recovers from one `429`, and the second answer is published once |
| `graph_tool_called_twice` | Scripted | A model calls a checkpointed graph tool twice in one turn and gets `first=42 second=10`; the graph runs once per call |
| `subgraph_in_a_loop` | None | A checkpointed subgraph runs on every pass of a 3-pass loop and again on a second turn (`2,4,6`, then `20,22,24`; 6 runs) |
| `child_in_a_loop` | None | A node in a 3-pass loop invoking a child with `run_node` gets `1,2,3`, one child run per pass |
| `parent_retry_serves_finished_children` | None | A checkout retried after its notification fails once charges once, and a child with its own retry policy recovers without failing the checkout |
| `timeout_policy_with_subgraph_and_children` | None | With a default timeout policy, a subgraph and a node invoking a child still run |
| `fan_in_state_stays_bounded` | None | An automatic join in a 12-pass loop sees the same two arrivals on every pass, and the final SQLite checkpoint state is under 4 KB |
| `live_conditional_merge` | Gemini | A live classifier on a conditional branch files a double-charge ticket as `billing`, through the merge after the branch |

## Running

```bash
cp examples/graph_correctness/.env.example examples/graph_correctness/.env   # optional, for the live scenario
cargo run --manifest-path examples/graph_correctness/Cargo.toml
cargo run --manifest-path examples/graph_correctness/Cargo.toml -- --scenario review_loop,builder_join
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on
any `FAIL`. Set `ADK_ENV_FILE` to read keys from another dotenv file and `GEMINI_MODEL` to change
the live model (default `gemini-3.5-flash-lite`).
