# CodeAct Agent example

Runs the ADK-Rust [`CodeActAgent`](../../adk-agent/src/codeact) — the agent that
**acts by writing and running code** instead of emitting one tool call at a time.

This example is fully self-contained: it ships a tiny `CodeRuntime`
(`LineScriptRuntime`) and a deterministic model (`DemoLlm`), so it runs with **no
API key and no native interpreter**.

## Run

```bash
cargo run --manifest-path examples/codeact_agent/Cargo.toml
```

Expected output:

```
=== ADK-Rust CodeAct Agent example ===

[calculator] {
  "answer": 42,
  "explanation": "40 + 2 = 42"
}

Done.
```

## What it shows

The CodeAct loop, end to end:

1. The model writes one script per turn (a fenced code block).
2. The script calls a tool (`add`) exposed as a callable function.
3. The tool result is fed back as an observation; the loop continues.
4. A second turn returns a tagged `final_result`, ending the run.

## The pieces

- **`src/runtime.rs`** — `LineScriptRuntime`, a minimal but real
  [`CodeRuntime`] implementation over a line-script language. It supports
  suspend/resume at a call boundary (the continuation is just the remaining
  lines), which is what powers HITL confirmation and long-running tool deferral.
- **`src/main.rs`** — `DemoLlm` (a deterministic model), an `add` tool, and the
  `CodeActAgent` + `Runner` wiring.

## Going to production

Swap two pieces; the rest is unchanged:

- Replace `DemoLlm` with an [`adk-model`](../../adk-model) provider (Gemini,
  OpenAI, Anthropic, ...).
- Replace `LineScriptRuntime` with a real interpreter — the intended adapter
  wraps [Monty](https://github.com/pydantic/monty), a Rust-native Python whose
  snapshot-at-call-boundary model makes suspend/resume a true continuation.

[`CodeRuntime`]: ../../adk-agent/src/codeact/runtime.rs
