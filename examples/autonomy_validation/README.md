# Autonomy Validation

Live validation of the autonomy-readiness Phase 0 fixes against OpenAI and Anthropic. Each
scenario drives real model calls through the public ADK APIs and asserts on observable facts —
session history, tool execution counters, persisted state, checkpoints, and usage — never on model
prose. Prompts are directive and tools are deterministic; when a model ignores a directive prompt,
the scenario is retried once and the retry is reported next to the result.

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
| `eval_judge_fail_closed` | #758 | `Evaluator` with an `LlmJudge`: a correct answer passes, a wrong answer fails, a semantic criterion without a judge fails |
| `anthropic_web_tools` | #754, #759 | `web_search_20260209` with `allowed_callers(["direct"])` and with dynamic filtering: a second turn replaying the server tool history is accepted |
| `anthropic_cache_cost` | #735, #759 | 1-hour prompt cache: call 1 writes only 1h entries, call 2 reads them, `estimate_cost` bills the write at 2x base input |
| `mcp_concurrency` | #760 | Two tool calls from one model response, dispatched in parallel to an in-process MCP server, are in flight together on one `McpToolset` connection |

## Providers

| `--provider` | Client | Default model | Override |
|--------------|--------|---------------|----------|
| `openai` | `OpenAIResponsesClient` (Responses API) | `gpt-5.6-luna` | `OPENAI_MODEL` |
| `openai-chat` | `OpenAIClient` (Chat Completions) | `gpt-5.4-mini` | `OPENAI_CHAT_MODEL` |
| `anthropic` | `AnthropicClient` | `claude-haiku-5-5` | `ANTHROPIC_MODEL` |

`anthropic_web_tools` uses `claude-sonnet-5-5` (`ANTHROPIC_WEB_MODEL`), since `web_search_20260209`
is not available on Haiku. `both` selects `openai` and `anthropic`; `all` adds `openai-chat`.

> **Note:** current OpenAI models such as `gpt-5.6-luna` reject function tools on Chat
> Completions, so `openai-chat` defaults to `gpt-5.4-mini`.

## Run

```bash
cp examples/autonomy_validation/.env.example examples/autonomy_validation/.env
# set OPENAI_API_KEY and ANTHROPIC_API_KEY

cargo run --manifest-path examples/autonomy_validation/Cargo.toml -- --provider both --scenario all
cargo run --manifest-path examples/autonomy_validation/Cargo.toml -- --provider anthropic --scenario shared_state_fresh,graph_resume_once
```

`ADK_ENV_FILE` points the example at a dotenv file elsewhere. The process prints one line per
scenario and provider, then a summary table, and exits non-zero when any scenario fails. Set
`RUST_LOG=warn` to see retry and toolset warnings.
