# ACP OpenAI end-to-end example

This example drives `adk-acp` from both sides against a live OpenAI model and asserts every
outcome. One binary plays both roles: client mode spawns the same executable with
`--serve-acp`, which serves an OpenAI-backed ADK agent over stable ACP v1 stdio.

```text
acp-openai-e2e (client mode)                  acp-openai-e2e --serve-acp (child)
┌──────────────────────────────┐   stdio     ┌──────────────────────────────────┐
│ coordinator LlmAgent (OpenAI)│  ACP v1     │ AcpServer                         │
│  └ delegate_to_workspace ────┼────────────▶│  └ workspace_agent LlmAgent       │
│      (persistent AcpSession) │◀────────────┤     (OpenAI)                      │
│ recording PermissionPolicy ◀─┼─ request_   │     ├ read_workspace_file (r/o)   │
│ assertions                   │  permission │     └ record_finding (gated)      │
└──────────────────────────────┘             └──────────────────────────────────┘
      AcpAgentConfig.env(OPENAI_API_KEY, OPENAI_MODEL, ACP_E2E_WORKSPACE, ACP_E2E_ENV_PROBE)
```

## Run it

```bash
cd examples/acp_openai_e2e
cp .env.example .env
# Add OPENAI_API_KEY to .env
cargo run
```

`OPENAI_MODEL` selects the model for both agents and defaults to `gpt-6-luna`. A passing run
ends with `all checks passed`; any failed check exits non-zero with the reason.

## Phases

1. **One-shot.** `prompt_agent_with_policy` spawns a fresh agent process, which reads
   `secret.txt` — a random marker in a temporary workspace — and returns its contents.
2. **Agent-driven.** A coordinator `LlmAgent` reaches the workspace agent only through
   `delegate_to_workspace_agent`, backed by one persistent `AcpSession`. The workspace agent
   records the marker with `record_finding`, which requires confirmation, so the ADK
   confirmation interrupt becomes an ACP `session/request_permission` that the client policy
   approves.

## What is verified

| Check | Proves |
|-------|--------|
| The child writes `ACP_E2E_ENV_PROBE` to `.probe` and it matches | `AcpAgentConfig::env` reaches the spawned process |
| The phase 1 reply contains the marker | One-shot prompting, streaming, and tool calls over ACP |
| The coordinator called `delegate_to_workspace_agent` | The model-driven path is exercised, not bypassed |
| `AcpSession::prompt_count() >= 1` and `close()` succeeds | Persistent-session streaming and shutdown |
| At least one permission request was received | The confirmation-to-permission bridge fires |
| `findings.log` contains the marker, with one line per approval | Approved calls execute exactly once; nothing unapproved runs |
| The coordinator's reply contains the marker | Results flow back through both agents |

Assertions check what the tools recorded rather than model wording, except for the random
marker, which a model cannot produce without reading the file.

## Notes

- In server mode, stdout carries only ACP traffic; tracing goes to stderr.
- `read_workspace_file` accepts only a bare file name inside the temporary workspace.
- Each phase has a timeout, so a hung child fails the run instead of blocking it.
- CI compiles this example but does not run it, because it needs an API key.
