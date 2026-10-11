# Autonomy Validation

Live validation of the autonomy-readiness Phase 0, Phase 1, and Phase 2 fixes against OpenAI, Anthropic,
and Gemini. Each
scenario drives real model calls through the public ADK APIs and asserts on observable facts —
session history, tool execution counters, persisted state, checkpoints, and usage — never on model
prose. Prompts are directive and tools are deterministic; when a model ignores a directive prompt,
or a provider answers HTTP 429, the scenario is retried once (after 60 s for a rate limit) and the
retry is reported next to the result.

## Scenarios

| Scenario | Fix | Asserts |
|----------|-----|---------|
| `transfer_roundtrip` | #747, #749, #759 | Coordinator → specialist → coordinator over two user turns; every function call in session history has exactly one response; a scripted `[transfer("ghost_agent"), transfer("billing_agent")]` turn answers each call once and a real second turn carrying that history is accepted |
| `runner_plugin_denies_tool` | #761 | A runner `PluginManager` `before_tool` hook that denies `delete_records` with `Ok(Some(Content))`, the documented form: the tool runs 0 times, the turn completes, and the denial reaches the model as the call's function response |
| `runner_plugin_denies_tool_err` | #761 | The same hook denying with `Err(..)` |
| `path_guardrail_fail_closed` | #762 | `PathAllowList` on `path`: a tool that takes its path under another argument name is denied and runs 0 times, a path outside the root is denied, a permitted path runs once |
| `failing_toolset_skipped` | #760 | A toolset whose `tools()` fails is skipped and the working tool runs; with `strict_toolsets(true)` the turn fails before any model call |
| `shared_state_fresh` | #763 | SQLite sessions: a tool on session A reads `app:kill_switch` written through session B; 12 concurrent `app:`/`user:` deltas across three sessions all persist |
| `graph_resume_once` | #757 | `#[entrypoint]` workflow on `SqliteCheckpointer`: an LLM task, then a charge that crashes twice; after two resumes the LLM task and the charge each ran once |
| `eval_judge_fail_closed` | #758 | `Evaluator` with an `LlmJudge` on the provider's own model and the default judge config: a correct answer passes, a wrong answer fails on the judge's score, a semantic criterion without a judge fails, and a judge request the provider rejects fails the scenario |
| `anthropic_web_tools` | #754, #759 | `web_search_20260209` with `allowed_callers(["direct"])` and with dynamic filtering: a second turn replaying the server tool history is accepted |
| `anthropic_cache_cost` | #735, #759 | 1-hour prompt cache: call 1 writes only 1h entries, call 2 reads them, `estimate_cost` bills the write at 2x base input |
| `mcp_concurrency` | #760 | Two tool calls from one model response, dispatched in parallel to an in-process MCP server, are in flight together on one `McpToolset` connection |

### Phase 1

| Scenario | Area | Asserts |
|----------|------|---------|
| `payment_timeout_once` | Effect contract, action ledger | A `NonIdempotent` `charge_card` records its charge and outlives the 2 s tool timeout with 3 retries configured: one call, one charge, and the model receives an `outcome_unknown` response. After a simulated crash the SQLite `ActionLedger` is reopened, the record is still begun, and a replay of the same call in the same invocation is answered `outcome_unknown` without charging again |
| `codeact_payment_once` | Action ledger, CodeAct | A `CodeActAgent` on the Monty runtime writes Python that calls a `NonIdempotent` `charge` tool, which outlives the 2 s tool timeout with 3 retries configured: one charge, and the program sees an `outcome_unknown` error. After a simulated crash the SQLite `ActionLedger` is reopened, the write-ahead checkpoint is restored to session state, and a new runner replays the step from the Python snapshot: the ledger answers `outcome_unknown` and the card is not charged again |
| `team_budget_cap` | Run budgets, cost | A coordinator delegating to a researcher through an `AgentTool` under a 400 micro-USD `RunBudget`: the run ends with `ResourceExhausted` (`budget.cost`), every model call reported `UsageMetadata::cost`, no call started after the cumulative cost reached the cap, spend is at most the cap plus one call, and the tracker's counters match the reported costs |
| `default_deny_policy` | Governed path | A `DeclarativePolicy` that allows only `lookup_order` and `refund` up to 50: `delete_account` and a refund of 500 run 0 times and are answered `denied by policy`, a refund of 20 runs once |
| `approval_across_runs` | Governed path | Run 1 holds `send_invoice` for approval (0 executions, one pending entry in the `ApprovalStore`); an approver decides by fingerprint; run 2 re-issues the call under a new call ID, it runs once, and the decision is consumed |
| `approval_service` | Approval service | Run 1 holds `send_invoice` and submits one request (quorum 2, group `finance`) to a `StoreApprovalService` over SQLite; the requester's own approval is refused; one approval keeps the call held through run 2; the second resolves it; run 3 resumes and executes it once, run 4 does not; notifications Submitted, Decided, Decided, Resolved |
| `kill_switch` | Governed path | A tool freezes the runner's `GovernanceControl` mid-run: no later tool runs, the run ends with `governance.frozen`, and the next run is refused at its start |
| `spend_ledger_daily_cap` | Spend ledger | One daily org cap covers model spend and payments: a probe call's reservation is at least its cost (an upper bound), a one-cent checkout through the `adk-payments` tools, then runs (output capped at 2,048 tokens) until one is refused before its model call, then a checkout larger than the remaining room refused by the `spend_limit` policy before it reaches the merchant; total spend never exceeds the cap and is split by vendor (provider and merchant) |
| `ap2_payment_governed` | AP2 governance | Two 12 USD AP2 payment mandates submitted by the model through `payments_ap2_payment_submit` under a 20 USD daily cap at the merchant: the first completes and commits 12 USD under `app / agent / merchant`, the second is refused by the `spend_limit` policy before the payment backend, and the payment audit trail records allowed then denied |
| `openai_tools_routing` | Providers | `gpt-5.6-luna` with function tools on the Chat Completions client (`OpenAIClient`) completes a tool call (routed to the Responses API); `openai-chat` only |
| `anthropic_thinking_replay` | Providers | `claude-haiku-4-5` with budget thinking, which thinks on every turn: two tool-use turns whose history replays signed thinking blocks are accepted; Anthropic only |
| `builtin_tool_policy` | Built-in tools | A default-deny policy keeps the provider's web search tool (`OpenAIWebSearchTool`, Anthropic `WebSearchTool`, Gemini `GoogleSearchTool`) out of every request: no built-in use is reported and the model events carry a `policyDenied` withheld record. With `allow_read_only()` the tool is attached, the reported uses land in `UsageMetadata::builtin_tool_uses`, and `cost` equals the token cost plus the published fee; `openai-chat` is skipped |
| `durable_crash_resume` | Durable worker | A `DurableWorker` is killed while the non-idempotent `charge_card` call is in flight; a second worker recovers the run after its lease expires, answers the unanswered call `outcome_unknown` from the SQLite action ledger, and completes the run on the same session: the card is charged once and the call has one response |
| `audit_trail` | Signed audit log | A runner with `audit_log` over a SQLite `AuditLogStore` and a named default-deny policy: one turn makes an allowed `lookup_account`, a denied `delete_account`, and a `buy_credits` purchase through the `adk-payments` checkout tools. The log holds exactly tool_call allowed, denied, allowed, then a decision and an outcome for each payment step, each naming `urn:adk:agent:<app>/<agent>` acting for the user, and no record contains an argument. The exported log passes `adk-rust audit verify` (with `ADK_RUST_BIN` set; the library `verify` otherwise), and fails once the denial is edited |

### Phase 2

| Scenario | Area | Passes when |
|----------|------|-------------|
| `agent_identity` | Agent identity | Two deployments (`replica-a`, `replica-b`) of a billing agent configured with `Runner::builder().agent_principal(...)` call `lookup_invoice` under a policy that allows only `spiffe://autonomy.test/agent/billing` in tenant `acme`: every policy decision carries that agent id, the call executes, and all model spend is recorded under the agent id rather than the derived `urn:adk:agent:autonomy-identity/billing` |

## Providers

| `--provider` | Client | Default model | Override |
|--------------|--------|---------------|----------|
| `openai` | `OpenAIResponsesClient` (Responses API) | `gpt-5.6-luna` | `OPENAI_MODEL` |
| `openai-chat` | `OpenAIClient` (Chat Completions) | `gpt-5.4-mini` | `OPENAI_CHAT_MODEL` |
| `anthropic` | `AnthropicClient` | `claude-haiku-5-5` | `ANTHROPIC_MODEL` |
| `gemini` | `GeminiModel` | `gemini-3.5-flash-lite` | `GEMINI_MODEL` |

`anthropic_web_tools` and `builtin_tool_policy` use `claude-sonnet-5-5` (`ANTHROPIC_WEB_MODEL`),
since `web_search_20260209` is not available on Haiku. `builtin_tool_policy` uses
`gemini-3.7-flash` (`GEMINI_SEARCH_MODEL`) for Google Search grounding. `openai_tools_routing` uses `OPENAI_ROUTED_MODEL` and
`anthropic_thinking_replay` uses `ANTHROPIC_THINKING_MODEL`. `both` selects `openai` and
`anthropic`; `all` selects `openai`, `openai-chat`, `anthropic`, and `gemini`.

> **Note:** current OpenAI models such as `gpt-5.6-luna` reject function tools on Chat
> Completions, so `openai-chat` defaults to `gpt-5.4-mini`.

## Run

```bash
cp examples/autonomy_validation/.env.example examples/autonomy_validation/.env
# set OPENAI_API_KEY, ANTHROPIC_API_KEY, and GEMINI_API_KEY

cargo run --manifest-path examples/autonomy_validation/Cargo.toml -- --provider all --scenario all
cargo run --manifest-path examples/autonomy_validation/Cargo.toml -- --provider anthropic --scenario shared_state_fresh,graph_resume_once
```

`ADK_ENV_FILE` points the example at a dotenv file elsewhere; its values win over variables already
set in the environment. The process prints one line per
scenario and provider, then a summary table, and exits non-zero when any scenario fails. Set
`RUST_LOG=warn` to see retry and toolset warnings.
