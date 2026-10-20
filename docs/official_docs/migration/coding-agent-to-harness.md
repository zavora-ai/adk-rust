# Migrating from `CodingAgent` to the harness

`adk_agent::coding::CodingAgent` (3.0, feature `coding`) wired the dev tools, `write_todos`
and a coding prompt onto an `LlmAgent`, and ran `bash` on the host with no approval. In 3.1
the harness supersedes it (spec 3.1 §4, §7.19): `adk_harness::coding::CodingAgent` keeps the
same builder and builds a `HarnessAgent` in ask mode, so every edit and every command passes
the governed path. `adk_agent::coding` stays as it is in 3.1, is deprecated from 3.2.0, and is
removed in 4.0.0 ([STABILITY.md](https://github.com/zavora-ai/adk-rust/blob/main/STABILITY.md)).

## Quick reference

| If your code… | Do this |
|---|---|
| depends on `adk-agent` with feature `coding` | depend on `adk-harness`, or `adk-rust` with feature `harness` ([dependencies](#dependencies)) |
| builds `adk_agent::coding::CodingAgent` | import `adk_harness::coding::CodingAgent`; the builder methods are the same ([the preset](#the-preset)) |
| runs it unattended (CI, scripts, a server) | answer approvals with a prompter or a decision, or build the harness in `auto` mode ([unattended runs](#unattended-runs)) |
| calls `coding.todos()` | read the session's list with `Harness::todos` or `HarnessAgent::todos` ([todos](#todos)) |
| nests the agent under another agent | make the harness the runner's root, or hand it to a team as a member ([nesting](#nesting)) |
| uses `adk-rust code`, `goal` or `ultracode` | nothing: the CLI already runs on the harness; see [The harness in your terminal](../harness/cli.md) |

## Dependencies

```toml
[dependencies]
# Before (3.0)
# adk-agent = { version = "3.0.0", features = ["coding"] }

# After
adk-harness = "3.0.0"
adk-devtools = "3.0.0"
adk-model = "3.0.0"
adk-runner = "3.0.0"
adk-session = "3.0.0"
```

`adk-rust = { version = "3.0.0", features = ["harness"] }` brings the same crates through
the umbrella, as `adk_rust::harness`.

## The preset

```rust
use std::sync::Arc;

use adk_core::{Content, Llm, SessionId, UserId};
use adk_devtools::Workspace;
use adk_harness::coding::CodingAgent;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};

async fn run(model: Arc<dyn Llm>) -> anyhow::Result<()> {
    // Same builder as adk_agent::coding::CodingAgent.
    let coding = CodingAgent::builder()
        .model(model)
        .workspace(Workspace::new("./my-repo").allow_bash(true))
        .instruction("Follow the project's existing style; prefer small diffs.")
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "my-app".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await?;
    // `configure_runner` gives the runner the harness's run config, approval store, audit log
    // and compaction.
    let runner = coding
        .harness()
        .configure_runner(
            Runner::builder().app_name("my-app").agent(coding.agent()).session_service(sessions),
        )
        .build()?;
    let _events = runner
        .run(UserId::new("user")?, SessionId::new("s1")?, Content::new("user").with_text("Fix the failing test"))
        .await?;
    Ok(())
}
```

The preset lowers to `HarnessAgentBuilder::new(name).base_prompt(CODING_SYSTEM_PROMPT)
.mode(HarnessMode::Ask).shell(ShellIsolation::Host).without_default_members()`. For anything
the preset does not expose — a mode, a sandboxed shell, members, skills, stores — build a
`HarnessAgent` directly; see [Configuration](../harness/configuration.md).

## What changes

| 3.0 `CodingAgent` | Harness |
|---|---|
| `bash`, `write_file` and `edit_file` run without asking | In ask mode reads run; edits and commands wait for approval ([Modes and approvals](../harness/modes-and-approvals.md)) |
| The prompt calls the workspace sandboxed | The prompt states where the shell runs; `bash` runs on the host unless you choose the OS sandbox ([Shell and sandbox](../harness/shell-and-sandbox.md)) |
| `write_todos` only | `write_todos` and `update_todo`, one list per session |
| No instructions loading | `AGENTS.md` loads after a workspace trust decision |
| A re-issued call after a crash runs again | `bash`, `write_file` and `edit_file` are recorded before they run; a re-issued call is answered from its record ([Durability](../harness/durability.md)) |
| Runs under any parent agent | Runs only where the harness layer reaches the run's config ([nesting](#nesting)) |

## Unattended runs

In ask mode a call that needs approval and finds no prompter is held: the turn ends with a
`ToolConfirmationRequest` event, and the call runs once after a decision. Pick one:

| Option | How |
|---|---|
| Decide held calls | `Harness::pending_approvals` and `Harness::decide`, then `Harness::resume` ([held calls](../harness/modes-and-approvals.md#held-calls)) |
| Answer at once in code | `HarnessAgentBuilder::prompter` with your `ApprovalPrompter` ([prompting](../harness/modes-and-approvals.md#prompting)) |
| Run like 3.0, governed | `HarnessAgentBuilder::mode(HarnessMode::Auto)` with `allow_host_shell_in_auto()`: edits and host commands run without asking; protected-path edits still ask |

```rust
use std::sync::Arc;

use adk_core::Llm;
use adk_devtools::Workspace;
use adk_harness::coding::CODING_SYSTEM_PROMPT;
use adk_harness::{HarnessAgent, HarnessMode};

fn unattended(model: Arc<dyn Llm>) -> adk_core::Result<HarnessAgent> {
    HarnessAgent::builder("coding-agent")
        .model(model)
        .workspace(Workspace::new("./my-repo").allow_bash(true))
        .base_prompt(CODING_SYSTEM_PROMPT)
        .without_default_members()
        .mode(HarnessMode::Auto)
        .allow_host_shell_in_auto()
        .build()
}
```

## Todos

`coding.todos()` is deprecated: it returns the list any session of the agent wrote last. The
list is per session, in session state under `harness:todos` (`adk_harness::TODOS_STATE_KEY`):

| Before | After |
|---|---|
| `coding.todos()` | `harness.todos(&user, &session).await?.items` on a `Harness` |
| `adk_agent::coding::TODOS_STATE_KEY` | `adk_harness::TODOS_STATE_KEY` (`"harness:todos"`) |

## Nesting

The runner calls `configure_run` on its root agent, which installs the harness layer (its
policy, approvals and ledger) in the run's config. A `HarnessAgent` refuses to run where the
layer is missing, so a harness nested under a `SequentialAgent` or another `LlmAgent`'s
`sub_agent` fails with `harness.ungoverned` instead of running unguarded. Make the harness
the runner's root, give the harness members of its own ([Team](../harness/team.md)), or
wrap it in an `AgentTool`, which calls `configure_run` for its delegate.
