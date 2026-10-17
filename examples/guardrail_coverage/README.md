# Guardrail Coverage

What output guardrails and tool-result screens let reach the client, the session, and the
model. Every scenario drives the real stack and asserts on what each of them received, never on
model prose:

- `LlmAgentBuilder` with `adk-guardrail` output guardrails (`PiiRedactor`, and a `ContentFilter`
  that blocks "wire the funds"), `Runner`, `InMemorySessionService`, and `OpenAIClient` pointed
  at a scripted OpenAI-compatible Chat Completions server on `127.0.0.1` that writes text and
  calls a tool in the same turn.
- `RealtimeAgent` under `Runner`, and `RealtimeRunner` with a native handler, with
  `OpenAIRealtimeModel` pointed at a scripted WebSocket server on `127.0.0.1` that speaks the
  OpenAI Realtime event protocol and records each `function_call_output` it is sent. One tool
  returns an email address and an SSN; another fails with a database credential in its message.

The `live_*` scenarios run against OpenAI when `OPENAI_API_KEY` is set.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `preamble_pii_redacted` | Scripted Chat Completions | Text written beside a tool call reaches neither the client nor the session with the email address, the redacted text is saved and replayed, and the call still runs |
| `preamble_blocked` | Scripted Chat Completions | Blocked text beside a call ends the run with `guardrail.output_blocked`; the call does not run and nothing of the turn is sent or saved |
| `block_is_structured` | Scripted Chat Completions | Output and input blocks carry category `InvalidInput` (HTTP 400), codes `guardrail.output_blocked` and `guardrail.input_blocked`, and the guardrail name and severity in `details.metadata` |
| `low_severity_recorded` | Scripted Chat Completions | A `Low` failure passes the answer and is recorded on the delivered and saved event under `adk.guardrail.warnings` |
| `llm_tool_result_screened` | Scripted Chat Completions | `ResultTextGuardrail(PiiRedactor)` in `tool_guardrails` redacts a tool result before the next model request and the session |
| `realtime_agent_result_screened` | Scripted Realtime | `RealtimeAgent::tool_result_screen` redacts the result the model is sent and the tool event the session saves |
| `realtime_agent_screen_fails_closed` | Scripted Realtime | A screen that cannot decide withholds the result (`result_withheld`) from the model and the session |
| `realtime_agent_error_redacted` | Scripted Realtime | A tool error reaches the model and the session as its category and code, without the credential; `expose_tool_error_messages(true)` opts in to the message |
| `realtime_runner_result_screened` | Scripted Realtime | `RealtimeRunner::tool_result_screen` redacts a native handler's result before `function_call_output` |
| `realtime_runner_screen_fails_closed` | Scripted Realtime | A failing screen withholds a native handler's result |
| `realtime_runner_error_redacted` | Scripted Realtime | A native handler's error reaches the model without the credential |
| `live_openai_preamble` | OpenAI `gpt-5.6-terra` | Told to repeat the user's email beside its tool call, the model's text reaches neither the client nor the session with the address |
| `live_openai_realtime` | OpenAI `gpt-realtime-2.1` | Asked to read back a record with PII, the model only ever had the redacted record, and the session holds no SSN |

## Running

```bash
cp examples/guardrail_coverage/.env.example examples/guardrail_coverage/.env   # optional, for the live scenarios
cargo run --manifest-path examples/guardrail_coverage/Cargo.toml
cargo run --manifest-path examples/guardrail_coverage/Cargo.toml -- --scenario preamble_blocked,realtime_agent_error_redacted
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on
any `FAIL`. Keys are read from the dotenv file named by `ADK_ENV_FILE`, else from `.env` in
this example's directory, else from the repository's `.env`, ahead of the process environment.
