# Harness Hooks

A hook is a command the operator, the user or the workspace configures to run on a tool
event. The `HarnessAgent` (`adk-harness`, feature `hooks`) runs it through the same launcher
as `bash`, hands it the event as JSON on standard input, and reads its outcome from standard
output. A hook is an observer with a veto: it can add a Deny, fail a call or annotate a
result, and it can never allow a call, change the mode, create a standing rule or answer a
call (spec 3.1 §7.22, §9.2 I53). Whatever answers a call before `authorize_tool_call` skips
the policy, the guardrails and the confirmation, and whatever grants is a permission source
outside the harness stores, so the builder exposes neither.

## Events

| Event | Runs | Input on stdin | Allowed outcomes | Effect |
|---|---|---|---|---|
| `pre_tool` | inside the policy decision, step 4a, before standing rules | `tool`, final `args`, `class`, `mode`, `isolation`, `session`, `actingAgent`, `functionCallId` | `deny` | The call is denied; a hook can only tighten the table's decision |
| `post_tool` | after the tool executed | the above plus `result` (capped), `isError`, `exitCode`, `durationMs` | `fail`, `annotate` | `fail` replaces the result with `{"error": message}`; `annotate` adds the text under `hook_notes`, inside `<hook-note source="…">`, delimited as untrusted |
| `post_edit` | after `write_file` or `edit_file` changed a file | `path`, `beforeSha256`, `afterSha256`, `diff` (capped) | `fail`, `annotate` | A formatter or linter reports; the harness never reverts |
| `on_hold` | when a call is held for a decision | `heldCall` | `annotate` | A notifier; its note reaches the user, never the model |
| `on_stop` | when the harness turn ends | `summary`: tool counts, held calls, model calls, tokens | `annotate` | Shown to the user, not the model |
| `on_session_start` | once per session, when its first run starts | `workspace`, `mode`, `session` | `annotate` | Joins the `<environment>` block of every run of the session |

Every input carries `mode`, `isolation` (the shell's enforcer), `session` (app, user,
session and invocation ids) and `actingAgent`. The environment variable `ADK_HOOK_EVENT`
names the event.

## Outcomes

A hook that exits `0` is read as JSON. Anything that is not one of the three decisions,
including `{"decision": "allow"}`, plain text or no output, is `continue`.

| Standard output | Outcome |
|---|---|
| `{"decision": "deny", "reason": "…"}` | `Deny` — on `pre_tool`; `continue` on every other event |
| `{"decision": "fail", "message": "…"}` | `Fail` — on `post_tool` and `post_edit` |
| `{"decision": "annotate", "text": "…"}` | `Annotate` — on `post_tool`, `post_edit`, `on_hold`, `on_stop`, `on_session_start` |
| `{"decision": "allow"}`, `"approve"`, `"permit"` | `continue`; the attempt is recorded as `outcome: ignored_grant` |
| anything else | `continue` |

A hook that exits non-zero, crashes or times out is `continue`, recorded as
`outcome: error`. With `fail_closed = true`, a `pre_tool` hook's failure is `Deny`.

## Configuration

`[[hooks]]` tables in `.adk/harness.toml` (the workspace), the user's `harness.toml`, and
`HarnessAgentBuilder::hooks` (the operator) merge in the order operator, user, workspace.
Within one event, hooks run serially in that order.

```toml
[[hooks]]
event = "pre_tool"          # pre_tool | post_tool | post_edit | on_hold | on_stop | on_session_start
matcher = "write_*"         # glob on the tool name: bash, write_*, mcp:*; omit for every tool
command = "./scripts/deny-migrations.sh"   # run as `sh -c` through the launcher
timeout_secs = 5            # default 30; 5 for pre_tool; never more than the workspace's bash timeout
isolation = "inherit"       # inherit (as the shell) | host
fail_closed = false         # true: a crashed or timed-out pre_tool hook denies

[[hooks]]
event = "post_edit"
command = "cargo fmt"
```

| Field | Default | Meaning |
|---|---|---|
| `event` | required | When the hook runs |
| `matcher` | every tool | A glob on the tool name; `*` matches any run of characters, `?` one |
| `command` | required | The command, run as `sh -c` |
| `timeout_secs` | 30; 5 for `pre_tool` | Capped by the workspace's bash timeout |
| `isolation` | `inherit` | `inherit` runs as the shell does — inside the OS sandbox when `bash` is sandboxed; `host` runs on the host even then. A workspace hook needs the workspace's trust for `host`; in Auto only an operator hook may run on the host |
| `fail_closed` | `false` | A failed `pre_tool` hook denies instead of continuing |

A table with an unknown field, an unknown event or an empty command fails the build with
`harness.hooks.config`, naming the entry. `HookSet::max_per_event` (8) bounds the hooks of
one event; `HookSet::max_time_per_turn` (60 s) bounds the time all hooks of one turn may
take together — once it is spent, a `pre_tool` dispatch denies and a `post_tool` dispatch
fails with the reason, and the turn ends after that call.

From code:

```rust,no_run
use std::sync::Arc;
use std::time::Duration;

use adk_harness::HarnessAgent;
use adk_harness::hooks::{HookEvent, HookIsolation, HookSet, HookSpec};

# async fn build(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
let hooks = HookSet::new()
    .with(
        HookSpec::new(HookEvent::PreTool, "./scripts/check-call.sh")
            .matching("bash")
            .fail_closed(true),
    )
    .with(
        HookSpec::new(HookEvent::PostEdit, "cargo fmt")
            .timeout(Duration::from_secs(90))
            .isolation(HookIsolation::Inherit),
    );
let harness = HarnessAgent::builder("harness")
    .model(model)
    .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
    .hooks(hooks)
    .build()?;
# let _ = harness;
# Ok(())
# }
```

`HarnessAgentBuilder::without_hooks` runs none, the workspace's included (the `--no-hooks`
flag of spec 3.1 §8.1). `HarnessRuntime::hooks` returns the merged set.

## In the CLI

| Surface | Behaviour |
|---|---|
| `[[hooks]]` in `.adk/harness.toml` | Workspace hooks; they run once the workspace is trusted, and the trust prompt lists each with its command (spec 3.1 §9.2 I54) |
| `[[hooks]]` in `<config dir>/adk-rust/harness.toml` | User hooks, run before the workspace's; an invalid table stops the command with the entry number |
| `--no-hooks` | Runs none, the workspace's included; the banner shows `Hooks : off (--no-hooks)` |
| `/hooks` | The effective set: each hook's event, matcher and command, its source, isolation, timeout, `fail-closed`, and whether it runs; then the bounds of one turn |
| `[hook:source] text` | A note from an `on_hold` or `on_stop` hook, shown after the call or the turn; the model never sees it |

```text
You > /hooks
  pre_tool [bash]: ./scripts/check-call.sh — workspace · inherit · 5 s · fail-closed · runs
  post_edit: cargo fmt — user · inherit · 30 s · runs
  bounds: 8 per event, 60 s per turn
```

## Trust

A workspace hook runs only while the workspace is trusted under the content-keyed workspace
trust of spec 3.1 §7.7: `.adk/harness.toml` joins the digest a
trust decision covers (`ProjectInstructions::instructions_digest`,
`ProjectInstructions::config_file`), so a hook added or changed after the decision is not
trusted until the user decides again, and the trust prompt can list each hook with its
command (`HookSet::describe`). An untrusted workspace hook is skipped and never recorded as
run. `.adk` is a protected directory: the model writes `.adk/harness.toml` only behind a
non-waivable approval, in every mode that allows edits.

## Execution

| Rule | Mechanism |
|---|---|
| Same launcher as `bash` | `inherit` hooks run through the shell's `CommandLauncher` — `SandboxLauncher` when the shell is sandboxed, with the read-only workspace in Plan and ReadOnly — and `host` hooks through `HostLauncher` |
| Same environment | The shell's environment allowlist, plus `ADK_HOOK_EVENT` |
| Same bounds | The hook's timeout is capped by the workspace's bash timeout; stdout and stderr by its output cap; the process starts in a new session with no controlling terminal and its process group is killed when it exits or times out |
| Input and output | The event JSON on stdin, the outcome on stdout, stderr logged at debug level |
| Members | The harness's own agents and its team members run the same `pre_tool` and `post_tool` hooks under the same policy; `on_stop` fires for the harness turn only |

## What a hook cannot do

| Attempt | Result |
|---|---|
| Print `{"decision": "allow"}` | `continue`; `harness.hook.ran` records `outcome: ignored_grant` |
| Print a mode, a rule, or `tool_approvals` | Ignored: the runner reads only `decision` and its text; it holds no mode store, rule store or approval store |
| Write `harness.db`, the rule store or session state | Changes no decision: the policy reads the harness stores, never a hook's output (spec 3.1 §6 D-3, §9.2 I1) |
| Reach the model | Only through `annotate`, inside `<hook-note source="…" hook="sha256:…">` with the closing tag neutralized, cut to 4 KiB (I32) |
| Run between `authorize_tool_call` and `execute` | `pre_tool` runs inside the decision; `post_tool` and `post_edit` after `execute` returns; the governed path of spec 3.1 §9 is unchanged |

`PreToolVerdict` has no `Allow` variant, and `PreToolVerdict::apply` returns the policy's
decision or the stricter of it and a `Deny`; `adk-harness/tests/hooks_property_tests.rs`
feeds arbitrary bytes and decision-shaped JSON through the parser and asserts that no
output loosens any decision.

## Audit

Every hook run writes one `harness.hook.ran` record before its outcome takes effect; an
outcome whose record cannot be written does not apply.

| Field (`ext`) | Value |
|---|---|
| `event`, `matcher`, `tool`, `functionCallId` | The event, the hook's matcher, the tool and call it ran for |
| `commandDigest`, `source`, `isolation` | `sha256:` of the command; `workspace`, `user` or `operator`; the launcher's isolation label |
| `exitCode`, `durationMs`, `timedOut`, `error` | What the process did |
| `outcome` | `continue`, `deny`, `fail`, `annotate`, `error` or `ignored_grant` |

Notes from `on_hold` and `on_stop` hooks travel as events with no content and the rendered
note under `provider_metadata["harness.hook.notice"]` (`adk_harness::hooks::HOOK_NOTICE_KEY`),
so a surface can show them and the model never receives them.

## Scenario

`examples/harness_agent` runs `hooks_cannot_grant` (spec 3.1 §5 H13): a workspace hook
that prints `{"decision": "allow"}` for `bash` is ignored and audited, a hook that denies
writes under `migrations/` denies in Auto, the same hooks in an untrusted workspace do not
run, and a hook's stdout reaches the model only as a delimited note. The live
`live_screenshot_and_format_hook` scenario has the model read a screenshot through
`read_file` and write a Rust file that a `post_edit` hook formats with `rustfmt`, and
prints the `harness.hook.ran` record.
