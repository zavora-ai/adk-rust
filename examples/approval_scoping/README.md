# Approval Scoping

A tool approval authorizes one agent's call to one tool. This example drives the real stack —
`LlmAgentBuilder`, `SequentialAgent`, `Runner`, `SqliteSessionService`, `SqliteApprovalStore`,
`OpenAIClient`, and `McpToolset` over two in-process MCP servers — and asserts which tool
executions each approval authorizes.

A scripted OpenAI-compatible server on 127.0.0.1 plays the model where two agents must issue the
same call with the same arguments, or where a provider reuses call IDs. The live scenario runs the
same two-agent flow against OpenAI when `OPENAI_API_KEY` is set.

## Run

```bash
cargo run --manifest-path examples/approval_scoping/Cargo.toml
cargo run --manifest-path examples/approval_scoping/Cargo.toml -- --scenario mcp_resume_stays_on_its_server
```

The example reads `ADK_ENV_FILE`, then `./.env`, then the repository's `.env`; values in the first
file found replace shell exports.

## Scenarios

| Scenario | Setup | Asserts |
|----------|-------|---------|
| `store_approval_stays_with_its_agent` | `Sequential[agent_b, agent_a]`, each with its own `transfer` requiring confirmation; `agent_a` holds `transfer {"amount": 40}`; the approver decides it in the SQLite approval store; `agent_b` issues the same call in run 2 | `agent_b`'s tool does not run; its call waits for its own approval |
| `approved_call_runs_once` | The same flow, three runs | `agent_a`'s held call runs once and its result answers the hold |
| `run_wide_approval_stays_with_its_agent` | `agent_a`'s surfaced fingerprint passed as `RunConfig::tool_approvals` to runs 2 and 3 | `agent_b`'s call is not authorized; `agent_a`'s call runs once across both runs |
| `call_id_decision_stays_with_its_agent` | The provider reuses call ID `call_0`; runs 2 and 3 carry a call-ID decision bound to `agent_a`'s fingerprint | `agent_b`'s `call_0` is not authorized; `agent_a`'s call runs once |
| `preapproved_fingerprint_portable_across_runs` | One agent; its own fingerprint passed as a run-wide approval | The held call resumes in run 2 and a new call with a new ID runs in run 3 |
| `mcp_resume_stays_on_its_server` | `wire` held on MCP server `bank_a`; approved; the tool name then resolves to `bank_b`, which publishes an identical declaration | `bank_b` receives no call; the held call is answered as not run, naming both origins |
| `mcp_redefined_tool_needs_new_approval` | `wire` held on `bank_a`; approved; `bank_a` then redefines `wire` with a wider description and a new parameter | The redefined tool receives no call; the held call is answered as not run |
| `mcp_resume_on_same_server_runs_once` | `wire` held on `bank_a`; approved; nothing changes | `bank_a` receives the call once |
| `live_store_approval_stays_with_its_agent` | The two-agent flow against `gpt-5.6-terra` (`OPENAI_MODEL` overrides) | As `store_approval_stays_with_its_agent` and `approved_call_runs_once`; skipped when the model does not make the calls |

## Results

| Scenario | `main` (f94e2870) | This branch |
|----------|-------------------|-------------|
| `store_approval_stays_with_its_agent` | FAIL — `agent_b`'s transfer ran once with `agent_a`'s approval | PASS |
| `approved_call_runs_once` | FAIL — `agent_a`'s held call ran 0 times | PASS |
| `run_wide_approval_stays_with_its_agent` | FAIL — `agent_b` ran once; `agent_a` ran twice by run 3 | PASS |
| `call_id_decision_stays_with_its_agent` | FAIL — `agent_b` ran once; `agent_a` ran twice by run 3 | PASS |
| `preapproved_fingerprint_portable_across_runs` | PASS | PASS |
| `mcp_resume_stays_on_its_server` | FAIL — `bank_b` executed the call approved on `bank_a` | PASS |
| `mcp_redefined_tool_needs_new_approval` | FAIL — the redefined tool executed under the old approval | PASS |
| `mcp_resume_on_same_server_runs_once` | PASS | PASS |
| `live_store_approval_stays_with_its_agent` | FAIL — `agent_b` ran once; `agent_a` ran 0 times | PASS |
