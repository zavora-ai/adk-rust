# Embedded runtime UI showcase

These three OpenAI-backed examples run ordinary ADK-Rust agent roots through
the same embedded interface used by generated projects and deployments. They
demonstrate tool calling, deterministic graph orchestration, and portable team
handoff without a separate frontend service.

Each walkthrough includes a screenshot captured from the corresponding live
OpenAI run, along with the prompt and observable control flow used to reproduce
it.

| Example | Binary | Walkthrough |
|---|---|---|
| Agent with tools | `runtime-ui-tools` | [Tools README](tools/README.md) |
| Graph workflow | `runtime-ui-graph` | [Graph README](graph/README.md) |
| Portable team | `runtime-ui-team` | [Team README](team/README.md) |

## Setup

```bash
cp examples/runtime_ui_showcase/.env.example .env
# Edit .env and set OPENAI_API_KEY.
```

Run one binary, then open `http://127.0.0.1:8088/ui/`. Stop it before starting
another because all three use the same address by default. Override
`ADK_UI_ADDRESS` or `RUNTIME_UI_MODEL` when needed.

These examples compile in CI without making network calls. OpenAI is contacted
only after a binary is run and a user sends a message.
