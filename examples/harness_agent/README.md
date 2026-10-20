# harness_agent

Scenarios for the `adk-harness` `HarnessAgent` (spec 3.1 §11). Every pass condition is
asserted on files, stores and audit records, never on model prose.

## Scripted scenarios

No keys needed. Each scenario runs a real `LlmAgent` behind a real `Runner` with a scripted
model, once against `HarnessAgent` and once against the 3.0
`adk_agent::coding::CodingAgent`, and prints a main-versus-branch table.

```bash
cargo run                      # every scripted scenario
cargo run -- mode_matrix       # one scenario
```

| Scenario | What passes |
|---|---|
| `mode_matrix` | Read, write, host `bash` and `exit_plan_mode` are executed, denied or held exactly as the spec 3.1 §7.4 table says in each mode |
| `ask_allow_once` | A host command is prompted once and runs once, with `harness.bash.begin`/`end` records |
| `always_allow_creates_standing_rule` | "Always allow `cargo test`" covers `cargo test --quiet --lib` in another session, not `cargo publish`, and not another user |
| `rule_stale_after_redeclaration` | A rule for a tool stops applying when the tool's declaration changes; `harness.rule.stale` is recorded |
| `edit_args_rechecked_by_policy` | Arguments an approver edits through the inbox are checked by the policy again; an escape is refused |
| `todos_isolated_per_session` | Each session has its own todo list |
| `agents_md_and_skills_loaded` | `AGENTS.md` is listed until the workspace is trusted, then inlined as a delimited block; skills are offered through `list_skills`; every run records `harness.instructions.loaded` |
| `unsandboxed_retry_is_held` | Host `bash` is held in ask mode; an unsandboxed retry with no failed sandboxed call is refused |
| `team_budget_stops_run` (H3) | Members lower to one `TeamSpec` with isolated, timed edges and delegation depth 1; a team budget of one delegation stops the second; the delegation is audited with its parent and member |
| `acp_member_governed` (H3) | A scripted ACP coding agent as the `reviewer` member: in Plan its read is allowed and its edit refused; in Ask the edit is put to the person and written once; the delegation is audited as `acp` |
| `plan_graph_approve_gate` (H4) | An approved three-step plan runs steps 1 and 2; the Approve gate on step 3 is a held call bound to the plan hash and step; after `Harness::decide`, step 3 runs and the plan is done |
| `plan_graph_rewind` (H4) | Step 3 fails; `rewind` to step 2 forks the thread; step 3 runs on the fork and steps 1 and 2 never run again |
| `lost_result_answered_from_ledger` (H1) | The process stops after a command ran and before its result was saved; after the restart the command does not run again and the model reads the ledger's answer |
| `goal_resumes_after_crash` (H1) | A durable goal parks on a held write; the worker is killed; a new process approves the call and the run completes at the same iteration with the write done once |

## Live scenarios

```bash
cp .env.example .env           # add the keys you have
cargo run -- --live            # every provider with a key
cargo run -- --live --provider anthropic
```

`HARNESS_ENV_FILE=/path/to/.env` loads keys from another file.

| Scenario | What passes |
|---|---|
| `live_plan_mode_writes_nothing` | In plan mode the workspace tree is unchanged after a fix-the-bug task, and the todo list is non-empty |
| `live_standing_rule` | After "always allow `cargo test`" on the first task, the second task in a new session runs `cargo test --quiet` with zero prompts |
| `live_team_review` | A local `explore` member and an ACP-compatible coding agent as `reviewer` (the command in `HARNESS_ACP_CMD`): both delegations run and complete, the bug is fixed, and each delegation's end is audited |
| `live_plan_graph` | A three-step plan with an Approve gate on step 3: steps 1 and 2 run, the gate waits, `Harness::decide` approves it, step 3 runs, the plan is done |
| `live_durable_goal` | `make the failing test pass` until `cargo test --quiet` as a durable run on SQLite: the run parks on a held edit, the worker is killed, a new process approves the calls, the run completes, the goal is done and the tests pass |

`HARNESS_ACP_CMD` is the command of an installed ACP-compatible coding agent, for example
`my-agent --acp`; `live_team_review` reports an error without it. An agent that requires the
ACP `authenticate` step cannot be a member yet.

Models: `gpt-5.6-terra` (OpenAI), `claude-sonnet-4-6` (Anthropic), `gemini-3.5-flash-lite`
(Gemini).
