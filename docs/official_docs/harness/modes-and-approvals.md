# Modes and approvals

Every tool call of a `HarnessAgent` passes the governed path (`adk_core::authorize_tool_call`)
before it runs. There `HarnessPolicy` decides it from three things: the session's mode, the
call's tool class, and your standing rules. The decision is Allow, Deny, or Approval — the
call waits for a person. This page covers the modes, how an approval is answered, and how
"always allow" is remembered (spec 3.1 §7.4, §7.5).

## Modes

| Mode | Intent |
|---|---|
| `read_only` | Investigate: reads and todos only |
| `plan` | Investigate and propose: reads, todos, and a plan presented with `exit_plan_mode` |
| `ask` | Work with you: reads run, edits and commands wait for approval. The default |
| `auto` | Work alone in the workspace: reads and edits run, host commands still wait |

The mode decides each tool class:

| Class | `read_only` | `plan` | `ask` | `auto` |
|---|---|---|---|---|
| Read, todos, delegation to a member | Allow | Allow | Allow | Allow |
| `exit_plan_mode` | Deny | Approval, non-waivable | Deny | Deny |
| Edit (`write_file`, `edit_file`, `apply_patch`) | Deny | Deny | Approval | Allow |
| Edit of a protected path | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| `bash` on the host | Deny | Deny | Approval | Approval (Allow with `allow_host_shell_in_auto`) |
| `bash` in the OS sandbox | Deny | Allow, read-only workspace | Approval (Allow with `auto_allow_sandboxed`) | Allow when protected paths hold |
| Unsandboxed retry of a sandboxed command | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| External tool, read-only, from a trusted origin | Allow | Allow | Allow | Allow |
| Any other external tool | Deny | Deny | Approval | Approval |

A call's class comes from its origin and name in the harness's tool-class registry, never
from an MCP server's `readOnlyHint` alone; `trust_mcp_read_only_hints(true)` trusts the
hint for external tools. Protected paths are the files that steer the agent or the build:
`.git`, `.adk`, `.skills`, `.claude`, `AGENTS.md`, `CLAUDE.md`, `MEMORY.md`, `.envrc`,
`lefthook.yml` and `.cargo/config*` at any depth, matched as written and at the target a
symlink resolves to. An `apply_patch` call is judged on every path its patch names — a
move's source and destination included — so one protected path anywhere makes the whole call
non-waivable, and one path outside the workspace refuses it. The [`adk-harness` README](https://github.com/zavora-ai/adk-rust/blob/main/adk-harness/README.md#mode-table)
lists the root-level ones.

### Choosing and changing the mode

| How | Effect |
|---|---|
| `HarnessAgentBuilder::mode` | The mode of sessions with no mode record (default `ask`) |
| `HarnessAgentBuilder::config` | `.adk/harness.toml` may choose a stricter mode, never a looser one |
| `Harness::set_mode` | Sets one session's mode, as a person outside a run |
| An approved `exit_plan_mode` | Leaves plan mode: the only way the model changes the mode |

A session's mode lives in a harness-owned `ModeStore` (`InMemoryModeStore` by default,
`FileModeStore`, or `SqliteModeStore` with feature `sqlite`), never in session state, so
no tool, client `stateDelta` or delegate can change it. Tightening applies at the next call.
Loosening applies from the next turn and first cancels every call held in the session, so
nothing you have not seen under the looser mode runs. A mode the store cannot parse reads as
`read_only`.

## Approvals

A call whose decision is Approval is answered in one of three ways:

| Setup | What happens |
|---|---|
| A prompter (`HarnessAgentBuilder::prompter`) | The person answers at once; the call runs or is refused in the same turn |
| No prompter | The call is held: the turn ends, `Harness::pending_approvals` lists it, `Harness::decide` answers it, and the next turn runs it once |
| An approval service (`HarnessAgentBuilder::approval_service`) | Held calls reach its inbox, as on a served harness; see [Serving the harness](serving.md) |

### Prompting

`ApprovalPrompter` is the one method an interactive surface implements; the CLI's
`ConsolePrompter` is one (see [The harness in your terminal](cli.md#approvals-at-the-terminal)).
The harness wraps it in `PromptingConfirmationHandler`, which shows the call's final
arguments with control characters escaped, serializes prompts, caps them per turn, and
checks edited arguments against the policy again.

| `PromptAnswer` | Effect |
|---|---|
| `ApproveOnce` | Runs the call once |
| `ApproveWithArgs(args)` | Runs the call once with `args`, after the policy checks them |
| `ApproveAndRemember(suggestion)` | Runs the call and stores the suggested standing rule |
| `Deny { message }` | Refuses the call; `message` is reported to the model |
| `DenyAndRemember(suggestion)` | Refuses the call and stores a Deny rule |
| `DenyAllRemaining` | Refuses this call and every other call of the turn |

A call nobody decides within `confirmation_timeout` (300 seconds by default) is not denied: it
waits in the approval store, the run ends with it held, and a decision made later — through
`Harness::decide`, the approval inbox or any other surface — runs or refuses it on the next
`resume`.

```rust
use adk_rust::async_trait;
use adk_rust::harness::{ApprovalPrompt, ApprovalPrompter, PromptAnswer};

/// Approves `cargo test` once and refuses everything else.
#[derive(Debug)]
struct CargoTestOnly;

#[async_trait]
impl ApprovalPrompter for CargoTestOnly {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_rust::Result<PromptAnswer> {
        let command = prompt.request.args.get("command").and_then(|value| value.as_str());
        Ok(if prompt.request.tool_name == "bash" && command == Some("cargo test") {
            PromptAnswer::ApproveOnce
        } else {
            PromptAnswer::Deny { message: Some("only `cargo test` is approved here".into()) }
        })
    }
}
```

Pass it with `.prompter(Arc::new(CargoTestOnly))` on the builder.

### Change sets

Two or more edits of one model turn that need approval are reviewed as one change set and
decided once (spec 3.1 §7.26). The agent hands the handler the turn's batch before any call
runs (`ToolConfirmationHandler::batch_started`); at the batch's first held edit the handler
re-checks every `write_file`, `edit_file` and `apply_patch` call of it against the policy and
composes them in call order over an in-memory copy of the workspace, read under the file
tools' own rules, so successive edits of one file compose.

| Part of the card | Content |
|---|---|
| Files | A unified diff per file with line numbers, and its sha256 before and after |
| Digest | `sha256:` over the sorted (path, before, after) triples |
| Calls that would fail | Shown failing — no match, an escape, a hunk that does not apply — and answered with the reason, without running |
| Edits a standing rule allows | Not held, so not in the card: counted as "N more edits run under standing rules" |
| Protected paths (I29) | Not merged in: asked about on their own after the card, as is an edit of a file a call outside the card also changes |

`ApprovalPrompter::prompt_change_set` shows the card and returns one `ChangeSetAnswer`:

| `ChangeSetAnswer` | Effect |
|---|---|
| `ApproveAll` | Every call that does not fail runs |
| `ApproveFiles(paths)` | The calls that change only those files run; the others are denied |
| `DenyAll { message }` | No call runs; `message` is reported to the model |
| `Edit(calls)` | Replaces calls' arguments by call ID and shows the change set again; the policy checks the edited calls again |

Each approved call is answered with its own arguments plus `expect_sha256`: the hash its
file had at that point of the composition — the batch's base for the first call on a file,
the intermediate hash for the next — so the call writes only against the content the person
saw, and a file changed in between is refused as `devtools.stale` (I66). The calls still run
one at a time: a stale call refuses only itself, and `apply_patch` is the all-or-nothing unit.
The handler answers each call of the card from the one decision, whichever order the calls
are dispatched in; a call whose arguments differ from the reviewed ones is asked about on its
own. `harness.changeset.decided` records the digest, the files, the call IDs and the answer.
A prompter that does not override `prompt_change_set` asks about each call on its own.
`ChangeSet::compose` is public, so a surface that groups held edits itself — spec 3.3's
decision card — computes the same change set and the same `expect_sha256` per call.

### Held calls

Without a prompter nothing waits in the run: the call is recorded in the approval store,
the turn ends, and a decision can come later from any process that shares the stores. The
decision binds to the call's fingerprint — the acting agent, the tool's origin and
declaration, the name and the arguments — so it authorizes that call and nothing else.

```rust
use std::sync::Arc;

use adk_rust::futures::StreamExt;
use adk_rust::harness::{HarnessAgent, HarnessDecision, HarnessMode, InstructionsConfig};
use adk_rust::model::{ScriptedLlm, ScriptedResponse};
use adk_rust::serde_json::json;
use adk_rust::session::InMemorySessionService;
use adk_rust::{EventStream, UserId};

async fn drain(mut events: EventStream) -> adk_rust::Result<()> {
    while let Some(event) = events.next().await {
        event?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("held-call-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    // A scripted model: it asks to write a file, then answers once the write ran.
    let model = ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("write_file", json!({ "path": "notes.txt", "content": "hello" })),
            ScriptedResponse::text("The note is written."),
        ],
    );
    let harness = HarnessAgent::builder("harness")
        .model(Arc::new(model))
        .workspace(adk_devtools::Workspace::new(&dir))
        .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
        .mode(HarnessMode::Ask)
        .build()?
        .into_harness("my-app", Arc::new(InMemorySessionService::new()))?;
    let user = UserId::new("alice")?;
    let session = harness.open_session(&user, None).await?;

    // Ask mode and no prompter: the write is held, not run.
    drain(harness.send(&user, &session, "write a note").await?).await?;
    let pending = harness.pending_approvals(&user, &session).await?;
    assert_eq!(pending.len(), 1);
    assert!(!dir.join("notes.txt").exists());

    // Decide it; the next turn runs the call once, before the model speaks.
    harness.decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve).await?;
    drain(harness.resume(&user, &session).await?).await?;
    assert_eq!(std::fs::read_to_string(dir.join("notes.txt"))?, "hello");
    Ok(())
}
```

| `HarnessDecision` | Effect |
|---|---|
| `Approve` | Runs the call once |
| `ApproveWithArgs(args)` | Runs the call once with `args`, after the policy checks them |
| `ApproveAndRemember(suggestion)` | Runs the call and stores the rule |
| `Deny { message }` | Refuses the call |
| `DenyAndRemember(suggestion)` | Refuses the call and stores a Deny rule |

`Harness::decide` refuses a fingerprint that is not pending: a call already decided,
cancelled by a mode change, or never held. A decision also wakes a durable run parked on the
call, such as a goal (see [Plans and goals](plan-and-goals.md)).

## Standing rules

"Always allow" is a `StandingRule` in a harness-owned `StandingRuleStore`
(`InMemoryStandingRuleStore` by default, `FileStandingRuleStore`, or
`SqliteStandingRuleStore` with feature `sqlite`). A rule turns Approval into Allow — or, as a
Deny rule, into Deny — for the calls it covers.

| Part | Meaning |
|---|---|
| Owner | The subject and app it belongs to; another user never matches it |
| Scope | Every session of the user (`RuleScope::User`) or one session |
| Origin | The tool's publisher and declaration digest (`OriginPin::Exact`), or for a folder rule each of its tools' (`OriginPin::ExactEach`): a redeclared tool no longer matches, and the harness records `harness.rule.stale` |
| Predicates | Argument predicates; for `bash`, `ShellPrefix`, which matches only a command of plain words with those leading tokens — never one with `;`, `&&`, pipes, redirections or substitutions; for a folder rule, `WritesWithin` |
| Decision | `StandingDecision::Allow`, or `Deny { reason }`; a Deny rule wins over an Allow rule |

Rules come from a person only: from a prompt's suggestions (`ApproveAndRemember`), from
`HarnessDecision::ApproveAndRemember`, from a served harness's authenticated `/api/rules`
routes (see [Serving](serving.md#standing-rules-over-http)), or from `adk-rust rules add`.
No model-callable tool creates one. A rule never applies to a non-waivable call:
`exit_plan_mode`, edits of protected paths, unsandboxed retries, `memory_write` and
`memory_delete` are decided every time. A suggestion for `bash` never starts with a command
that runs arbitrary code (`sh`, `bash`, `env`, `sudo` and similar), and the harness re-reads
the store on every call, so a revoked rule stops applying at once.

A `PathWithin` rule covers an `apply_patch` call only when every path of the patch lies
within its directory; the suggestion offered for a patch names the directory all its paths
share (`apply_patch: within src/`).

### Always allow for this folder

"Always allow for this folder" on a surface or over ACP makes one rule for every file write in
the folder the held call writes in — the workspace root for a file there — for the session
(spec 3.3 §8.2). `HarnessAgent::folder_suggestion(request)` builds it:

| Part | Value |
|---|---|
| Tool | `*`, narrowed by the origin pin |
| Origin | `OriginPin::ExactEach`: `write_file`, `edit_file` and `apply_patch`, each at the declaration the harness gives it |
| Predicate | `WritesWithin { dir }`: the call only writes files, and every path it writes lies within `dir` as `PathWithin` resolves it |
| Label | `file writes: within notes/`, or `file writes: anywhere in the workspace` |
| Record | `harness.rule.created` says what the rule covers in `covers` (`file writes in notes/`), which "What your agents did" shows as "You allowed file writes in notes/ without asking."; the covered tools stay in `tools` and their digests in `declarationDigests` |

The set is the harness's Edit-class tools by their declared effects: `write_file` creates or
replaces a file and `edit_file` changes one, so every call of theirs is a write;
`apply_patch` also deletes and moves files, so it is covered only for a patch that adds files
or updates them in place. A write outside the folder, a path with `..`, a symlink out of the
folder, a protected path, a delete, a move, `bash` and every other tool still ask. A call no
folder rule covers, such as a patch that deletes a file, is offered **Allow once** and
**Deny** only.

A suggestion's label states what the rule matches (`edit_file: within src/`); every surface
shows its scope from `RuleSuggestion::scope`, once. A call a rule allows where the mode would
ask is announced, not run silently: the policy and the handler's re-check call
`ApprovalPrompter::rule_applied` with a `RuleHit`, and the console prints
`[allowed by rule 3] bash: cargo test …` before the call runs, numbered as `/rules` lists
the rule — also for a call a rule granted earlier in the same batch covers.

Code that authenticates the person itself creates and revokes rules through the harness,
which records `harness.rule.created` and `harness.rule.revoked` with the surface before the
store write — `rest` for the REST routes, `cli` for `adk-rust rules add` and `rules rm`:

| Method | Effect |
|---|---|
| `HarnessAgent::folder_suggestion(request)` | The folder rule for a held file write, or `None` for any other call |
| `HarnessAgent::rule_for_held_call(request, suggestion, decision, owner)` | The rule a held call's suggestion becomes, refused for a call no rule may cover or a suggestion that does not pin the call's declaration |
| `HarnessAgent::grant_rule(rule, surface, from)` | Stores the rule after its `harness.rule.created` record |
| `HarnessAgent::revoke_rule(app, user, id, surface)` | Revokes one of `user`'s rules after its `harness.rule.revoked` record; `None` for a rule `user` does not own |

## Agents a person adds

On the always-on kernel a person adds agents of their own beside the primary agent (spec 3.3
§5.7, §10 H3). The harness builds each one, suggests them, and keeps their threads and rules
apart:

| Item | What it does |
|---|---|
| `propose_agent` | Class `Todo`, on a harness given `HarnessAgentBuilder::agents_handle`: the primary agent only. It suggests an agent by name, what it does and its instructions, optionally its mode, what it can use and its model, in its own area. The suggestion's instructions are screened and redacted, and anything over the suggesting agent's own limits — a looser mode, a shell on this computer, payments, computer use, a pack, site, folder or driver it lacks, another area — is refused in plain words. It answers "Suggested; waiting for the person": only a person adds the agent |
| `suggester_ceiling`, `Harness::agent_ceiling` | The suggesting agent's limits now: the session's mode, its file tools, its shell (sandboxed or on this computer), its web tools, its packs and the drivers mounted for its area |
| `assess_agent` | The risk table of §5.7, money for the payments and computer-use packs, and the abilities as plain sentences |
| `HarnessAgentBuilder::added_agent(&record)` | The harness of an added agent: its id as its name, its description and instructions, the stricter of its mode and its area's, no members, goals or `propose_agent`; the workspace narrowed to its folder and a deny layer for the files, shell and web its definition does not give |
| `HarnessAgentBuilder::hosted_beside(&primary)` | Shares the primary agent's approval service and agenda handle, so its decisions wake the kernel and its follow-ups go on the kernel's agenda |
| `HarnessAgentBuilder::session_stamp(target)`, `AGENT_STATE_KEY` | Stamps a session `harness:agent` on the harness's first run of it; every other agent refuses it with `harness.agent_session` |
| `PinnedOnlyRules`, `RuleSuggestion::into_pinned_rule`, `copy_rule_to` | An added agent applies only rules whose agent is exactly its own id — never `None` or a glob — and a rule granted through it is pinned to it; a person copies one of their rules to it |

An added agent's instructions shape what it says and tries, never what it may do: its mode,
tools and rules are code. See [Your Agents](../surfaces/agents.md).

## Related

- [Configuration](configuration.md): every builder option with its default.
- [Shell and sandbox](shell-and-sandbox.md): the shell rows in detail.
- [Tool authorization](../security/tool-authorization.md): the governed path every ADK-Rust
  agent shares.
