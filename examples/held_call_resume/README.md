# Held Call Resume

What happens to a tool call held for confirmation while it waits, and whether it runs exactly
once when its decision arrives. Every scenario drives the real stack — `LlmAgentBuilder` agents
in `SequentialAgent`, `LoopAgent`, `ParallelAgent` and `LlmConditionalAgent`, an `AgentTool`,
`Runner` with session compaction (`EventsCompactionConfig`), the durable worker, and both a
SQLite and an in-memory session service — and counts every model call per agent and every tool
execution, never asserting on model prose.

A scripted upstream on `127.0.0.1` plays the model on two routes: the real `OpenAIClient` talks
to its OpenAI-compatible Chat Completions route (calls with IDs), and the real `OllamaModel` to
its `/api/chat` route, which sends tool calls without IDs as Ollama does. The
`live_workflow_resume` scenario runs against OpenAI (`gpt-5.6-terra`) when `OPENAI_API_KEY` is
set.

## Scenarios

Each scripted scenario runs once per session backend, as `[memory]` and `[sqlite]`.

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `compaction_overlap_0` | OpenAI-compatible | With compaction after every run and overlap 0, the call is still held after the run that held it, and the approval runs it once |
| `compaction_overlap_1` | OpenAI-compatible | With overlap 1, a second message while the call waits compacts the first run, yet the call stays held and a third run's approval runs it once |
| `worker_parks_after_compaction` | OpenAI-compatible | A durable worker run holding the call parks as `Waiting` although compaction ran after it; the approval and a wake complete it with the call run once |
| `sequential_stops_at_hold` | OpenAI-compatible | `SequentialAgent[intake, ops, report]` stops at ops's hold; the resume starts at ops, intake never runs again, report runs once, and every request answers each call once |
| `loop_keeps_its_iteration` | OpenAI-compatible | `LoopAgent[counter, looper]` with 3 iterations stops at looper's hold in round 2, resumes there, and runs round 3 only |
| `parallel_resumes_held_branch` | OpenAI-compatible | `SequentialAgent[ParallelAgent[fast, ops], report]`: fast is not run again, ops resumes, report runs once |
| `router_keeps_its_route` | OpenAI-compatible | `LlmConditionalAgent` routes "send 10" to ops, which holds; the empty resume returns to ops without classifying again |
| `idless_calls_keep_arguments` | Ollama | Two `transfer` calls without IDs ({10}, {20}) are held with their own arguments; approving 10 runs exactly that one and leaves 20 held |
| `idless_agent_tool_resumes` | Ollama + OpenAI-compatible | An Ollama agent's `AgentTool` call without an ID, held for its delegate's call, resumes the delegation, which runs the delegate's call once |
| `budget_counts_held_call_once` | OpenAI-compatible | One `BudgetTracker` across the hold and the resume, at most one tool call: the held call counts 0 while held and 1 once resumed |
| `live_workflow_resume` | OpenAI | `SequentialAgent[ops, report]` with compaction after every run: report does not run while the transfer is held, the approval runs it once, and report runs once |

## Running

```bash
cp examples/held_call_resume/.env.example examples/held_call_resume/.env   # optional, for the live scenario
cargo run --manifest-path examples/held_call_resume/Cargo.toml
cargo run --manifest-path examples/held_call_resume/Cargo.toml -- --scenario sequential_stops_at_hold,idless_calls_keep_arguments
```

Each case prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on any
`FAIL`. Keys are loaded from the dotenv file named by `ADK_ENV_FILE`, else from this example's
`.env`, else from the repository's `.env`, over the process environment, whose exported keys may
be stale. `OPENAI_MODEL` overrides the live model.
