# AgentTool Isolation

What an agent behind an `AgentTool` inherits from its caller, what it hands back, and how its
timeouts and held calls reach the caller. Every scenario drives the real stack —
`LlmAgentBuilder`, `AgentTool`, `Runner`, `InMemorySessionService`, and `OpenAIClient` — and
asserts on what reached each model, what the session recorded, and how often each tool ran,
never on model prose.

Deterministic scenarios point the real client at scripted OpenAI-compatible Chat Completions
servers on `127.0.0.1`, one per agent. The `live_*` scenarios run against OpenAI when
`OPENAI_API_KEY` is set.

The tree in most scenarios is a `coordinator` that transfers to a `specialist`, which delegates
to a `helper` through an `AgentTool`.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `child_topology` | Scripted | With the run carrying a context-cache handle and an invocation toolset, the helper's model request has no `transfer_to_agent` tool, no cache handle and no invocation tool, while the specialist's request keeps its own |
| `skip_summarization_payload` | Scripted | A helper whose tool skips summarization hands the specialist that tool's result (`{"answer":42}`) |
| `nested_skip_summarization` | Scripted | A helper ending on a nested `AgentTool` with `skip_summarization(true)` hands the specialist the nested agent's answer |
| `helper_timeout_is_failure` | Scripted | An `AgentTool` with a 50 ms timeout over a slow helper is a failed call: `on_tool_error` runs once with `50ms` in its message, the action ledger records no success, and the specialist is told the call failed |
| `propagated_timeout_category` | Scripted | The same with `propagate_failures(true)`, and the error's category is `Timeout` |
| `nested_confirmation` | Scripted | A helper tool that requires confirmation surfaces a `ToolConfirmationRequest` for that tool at the caller, with the pending approval in the caller's session scope; after an `ApprovalStore` approval and an empty turn, the tool runs exactly once and the helper resumes from its held call without asking its model again |
| `nested_confirmation_call_id` | Scripted | The same, approved with a call-ID decision for the surfaced request |
| `forwarded_events_slow_consumer` | Scripted | 400 events a tool forwards from a worker task all reach the session although the client stalls for 400 ms |
| `live_child_topology` | OpenAI | The helper's request carries no `transfer_to_agent` tool |
| `live_nested_confirmation` | OpenAI | The helper's `delete_file` call is surfaced at the caller, does not run before approval, and runs exactly once after it |

## Running

```bash
cp examples/agent_tool_isolation/.env.example examples/agent_tool_isolation/.env   # optional, for live scenarios
cargo run --manifest-path examples/agent_tool_isolation/Cargo.toml
cargo run --manifest-path examples/agent_tool_isolation/Cargo.toml -- --scenario nested_confirmation,child_topology
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on
any `FAIL`. Set `ADK_ENV_FILE` to read keys from another dotenv file and `OPENAI_MODEL` to
override the live model (default `gpt-5.6-terra`).
