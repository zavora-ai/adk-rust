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

Models: `gpt-5.6-terra` (OpenAI), `claude-sonnet-4-6` (Anthropic), `gemini-3.5-flash-lite`
(Gemini).
