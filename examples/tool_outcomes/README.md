# Tool Outcomes

What an `LlmAgent` tells the model when a tool call does not return its result normally. Every
scenario drives the real stack — `LlmAgentBuilder`, `Runner`, `InMemorySessionService`, and
`OpenAIClient` — and asserts on what reached the model and the session, and on how often each
tool ran, never on model prose.

Faults a live provider cannot be made to produce come from a scripted OpenAI-compatible Chat
Completions server on `127.0.0.1` that the real client is pointed at through its base URL. The
script plays a model that retries a call it believes failed, and one that sends malformed
tool-call JSON. The `live_*` scenarios run against OpenAI when `OPENAI_API_KEY` is set.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `after_callback_error` | Scripted | A `NonIdempotent` `charge_card` whose `after_tool_callback` fails runs once; the model is told the call ran and not to call it again; the callback's error message reaches neither the model nor the session; the action ledger records the call as succeeded |
| `after_callback_full_error` | Scripted | The same with a failing `after_tool_callback_full` |
| `runner_plugin_after_tool_error` | Scripted | The same with a failing runner `PluginManager` `after_tool` hook |
| `enhanced_plugin_after_tool_error` | Scripted | The same with a failing enhanced plugin `after_tool_call` |
| `tool_error_redacted` | Scripted | A tool error quoting `postgres://admin:hunter2@db.internal…` reaches neither any model request nor the session; the model is told the call failed and its category |
| `expose_error_messages` | Scripted | With `expose_tool_error_messages(true)` the model receives the error message |
| `on_tool_error_text` | Scripted | Text returned by an `on_tool_error` callback reaches the model |
| `sub_second_timeout` | Scripted | A 50 ms tool timeout is reported as `50ms` |
| `malformed_args_stream` | Scripted, streaming | A truncated tool-call JSON is answered with an error without running the tool, the run continues, and the model's corrected call runs once |
| `malformed_args_unary` | Scripted, non-streaming | The same through the unary path |
| `live_after_callback_error` | OpenAI | `charge_card` with a failing audit after-callback runs once across the run, and its function response says not to call it again |
| `live_tool_error_redacted` | OpenAI | A tool error with a credential: the credential is in no model request, not in the session, and not in the reply, though the model is told to quote the error |

## Running

```bash
cp examples/tool_outcomes/.env.example examples/tool_outcomes/.env   # optional, for live scenarios
cargo run --manifest-path examples/tool_outcomes/Cargo.toml
cargo run --manifest-path examples/tool_outcomes/Cargo.toml -- --scenario malformed_args_stream,tool_error_redacted
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on
any `FAIL`. Set `ADK_ENV_FILE` to read keys from another dotenv file and `OPENAI_MODEL` to
override the live model (default `gpt-5.6-terra`).
