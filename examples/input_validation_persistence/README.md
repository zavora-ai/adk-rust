# Input Validation Persistence

What a session keeps when input guardrails redact or block a user message. Every scenario
drives the real stack — `LlmAgentBuilder` with `adk-guardrail` input guardrails (`PiiRedactor`
and a `ContentFilter` that blocks the word "forbidden"), `Runner`, the durable worker, and both a
SQLite and an in-memory session service — and asserts on what the session holds and what each
model request carried, never on model prose.

A scripted OpenAI-compatible Chat Completions server on `127.0.0.1`, which the real
`OpenAIClient` is pointed at through its base URL, records every request verbatim. The
`live_redaction` scenario runs against OpenAI (`gpt-5.6-terra`) when `OPENAI_API_KEY` is set,
or Gemini (`gemini-3.5-flash-lite`) when only `GEMINI_API_KEY` is.

## Scenarios

Each scripted scenario runs once per session backend, as `[memory]` and `[sqlite]`.

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `redaction_persists` | Scripted | Run 1 sends an email address; run 2's model request carries only `[EMAIL REDACTED]`, and the session never holds the address |
| `blocked_never_saved` | Scripted | Run 1 holds a `transfer` for approval; run 2's blocked message is refused and not saved; run 3, an empty resume with the approval, runs the transfer once, and no model request carries the blocked message |
| `worker_recovery_rechecks` | Scripted | A durable worker recovers a run whose user turn was saved unscreened, as an earlier release saved it: a blocked turn fails the run with no model call, and a redacted turn reaches the model only redacted |
| `nested_agent_guardrails` | Scripted | In `SequentialAgent[intake, billing]`, only `billing` has guardrails, yet `intake` never sees the address, and a message `billing` blocks is refused before `intake` calls its model |
| `resume_with_new_text` | Scripted | A held call whose tool sets `skip_summarization` is resumed in a run that also brings new text: the run answers the text or fails with an error instead of leaving it unanswered |
| `live_redaction` | OpenAI or Gemini | A live model given a redacted address in run 1 cannot repeat it in run 2: no request carried it, the reply does not contain it, and the SQLite session does not hold it |

## Running

```bash
cp examples/input_validation_persistence/.env.example examples/input_validation_persistence/.env   # optional, for the live scenario
cargo run --manifest-path examples/input_validation_persistence/Cargo.toml
cargo run --manifest-path examples/input_validation_persistence/Cargo.toml -- --scenario blocked_never_saved,worker_recovery_rechecks
```

Each case prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on any
`FAIL`. Keys are read from the dotenv file named by `ADK_ENV_FILE`, else from `.env` in the
working directory or in this example's directory, ahead of the process environment.
`OPENAI_MODEL` and `GEMINI_MODEL` override the live models.
