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
| Edit (`write_file`, `edit_file`) | Deny | Deny | Approval | Allow |
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
symlink resolves to. The [`adk-harness` README](https://github.com/zavora-ai/adk-rust/blob/main/adk-harness/README.md#mode-table)
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

A call nobody decides within `confirmation_timeout` (300 seconds by default) is denied.

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
| Origin | The tool's publisher and declaration digest (`OriginPin::Exact`): a redeclared tool no longer matches, and the harness records `harness.rule.stale` |
| Predicates | Argument predicates; for `bash`, `ShellPrefix`, which matches only a command of plain words with those leading tokens — never one with `;`, `&&`, pipes, redirections or substitutions |
| Decision | `StandingDecision::Allow`, or `Deny { reason }`; a Deny rule wins over an Allow rule |

Rules come from a person only: from a prompt's suggestions (`ApproveAndRemember`), from
`HarnessDecision::ApproveAndRemember`, or from the store directly, as `adk-rust rules add`
does. No model-callable tool creates one. A rule never applies to a non-waivable call:
`exit_plan_mode`, edits of protected paths, unsandboxed retries, `memory_write` and
`memory_delete` are decided every time. A suggestion for `bash` never starts with a command
that runs arbitrary code (`sh`, `bash`, `env`, `sudo` and similar), and the harness re-reads
the store on every call, so a revoked rule stops applying at once.

## Related

- [Configuration](configuration.md): every builder option with its default.
- [Shell and sandbox](shell-and-sandbox.md): the shell rows in detail.
- [Tool authorization](../security/tool-authorization.md): the governed path every ADK-Rust
  agent shares.
