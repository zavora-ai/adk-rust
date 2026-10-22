# Harness Team

The `HarnessAgent` (`adk-harness`, feature `team`) delegates subtasks to members: local
agents it builds, ACP-compatible coding agents, remote A2A agents, or any `Agent` you supply.
The coordinator — the harness `LlmAgent` — and its members compile to one `TeamSpec` with a
`Delegate` edge per member, so every delegation gets a timeout, a circuit breaker, the team
budget and the same policy as the coordinator's own tool calls.

## Members

| Kind | Agent | Its tool calls are decided by |
|---|---|---|
| `MemberKind::Local { instruction, model }` | an `LlmAgent` the harness builds | the harness policy, under the session's mode |
| `MemberKind::Acp(AcpAgentConfig)` (`team-acp`) | an ACP-compatible coding agent | each permission request it makes, through the governed path |
| `MemberKind::Remote(RemoteA2aConfig)` (`team-a2a`) | `RemoteA2aAgent` | the remote agent |
| `MemberKind::Agent(Arc<dyn Agent>)` | the agent you supply | the harness policy; its tools are `External` |

`MemberSpec` fields:

| Field | Default | Meaning |
|---|---|---|
| `name` | — | The member's name; the coordinator calls it as a tool of that name |
| `description` | — | When to use it, shown to the coordinator's model |
| `kind` | — | What the member is |
| `tools` | `MemberTools::ReadOnly` | `ReadOnly` (`read_file`, `glob`, `grep`, the todo tools), `Full` (every harness tool but `exit_plan_mode`), or `Custom(tools)` |
| `skills` | `false` | Whether the member gets the skill tools |
| `max_iterations` | `50` | Most model turns of one delegation |
| `timeout` | 15 minutes | The delegation's timeout; never unlimited |
| `output_schema` | `None` | A JSON Schema the member's answer is validated against |

`MemberSpec::explore()` (read-only) and `MemberSpec::general()` (full tools) are the default
members; `without_default_members()` leaves them out, and a member with the same name
replaces one. `MemberSpec::reviewer_acp(cmd)` is a reviewer that runs `cmd` as an ACP agent.

```rust,no_run
use std::sync::Arc;

use adk_harness::{HarnessAgent, MemberSpec};
use adk_model::gemini::GeminiModel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let harness = HarnessAgent::builder("harness")
        .model(model.clone())
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .member_model(model)
        .member(MemberSpec::reviewer_acp("my-coding-agent --acp"))
        .build()?;
    assert_eq!(harness.info().members, vec!["explore", "general", "reviewer"]);
    Ok(())
}
```

## What every delegation edge carries

| `RelationshipPolicy` field | Value |
|---|---|
| `context` | `Isolated`: the member starts with the request only, no history, state, artifacts or memory |
| `history` | `None` |
| `state_keys`, `state_write_keys`, `artifact_prefixes` | empty: nothing a member writes reaches the parent |
| `timeout_ms` | the member's `timeout` |
| `approval` | `Never`: the member's own tool calls are governed, not the delegation |
| `failure` | a read-only local member retries once after a second; any other member is attempted once, since a retry would repeat its effects |
| `circuit_breaker` | three consecutive failures open the edge for a minute |

The team policy is `team_policy(..)` or the default, with an isolated context and a
delegation depth of 1 always: a member has no delegate tools, no `exit_plan_mode` and no
memory writes. Its budget (`TeamBudget`), termination policy and concurrency (4 by default)
apply as given.

A member's state writes, such as its own todos, stay in its own session: it reads them
back, and they never merge into the parent's state.

## Members inherit the mode

Members share the parent's app, user and session, so the policy reads the same mode for
them. In Plan, a `general` member's `write_file` and `bash` are refused; in Ask they are
held for a person, and the delegation is held with them until the decision. A member that
holds several calls in one turn surfaces each as its own request under the delegation's call
ID, so the person sees one card per call; the delegation stays held until every call has a
decision, and each approved call runs once.

## ACP members

An ACP member runs an ACP-compatible coding agent in the harness workspace. Every
permission the agent asks for is decided on the governed path, under the member's run
config:

| ACP tool kind | Decided as |
|---|---|
| `read`, `search` | `Read`: allowed in every mode |
| `edit`, `delete`, `move` with a path | `Edit` on that path: refused in ReadOnly and Plan, an approval in Ask, allowed in Auto, a one-time approval for a protected path |
| anything else | an `External` call with origin `acp:<member>` |

An ACP session cannot wait for an approval inbox, so a call that needs a person is put to
the harness's live confirmation handler (the prompter), and refused when there is none.

> **Note:** an ACP agent that requires the ACP `authenticate` step before `session/new`
> cannot be a member yet: `adk-acp` does not send it.

## Discovering members

`member_registry(Arc<dyn TeamAgentRegistry>)` adds every registry candidate that no other
member names, resolved and authorized by the registry when the harness is built
(`TeamSpec::compile_with_registry`). A discovered member is wrapped like any other: audited,
isolated, with its own edge.

## Review rounds

`HarnessTeam::review_round(name, reviewers, max_rounds)` builds a round-robin
`BlackboardSpec` over members the harness built, each reviewer reading what the others said.
A plan's `Review` gate uses the member named `reviewer`, a review round when several members
are named `reviewer*`, or `explore`.

A speaker sees what the other speakers said, called and got back as other-agent context —
user-role text that names the speaker, such as `[review_correctness] said:` — never as turns
it appears to have taken itself. Its own earlier turns stay its own.

### A review board as a runner root

`HarnessTeam::review_round_with(name, reviewers, policy)` takes the whole
`BlackboardPolicy`: rounds, history window, `TeamBudget` and termination.
`HarnessAgent::governed_root(round)` runs the round as the root agent of a `Runner` under
the harness layer:

| What | How |
|---|---|
| Policy | `configure_run` applies the round's own configuration, then `HarnessRuntime::layer()`: every member call goes through `authorize_tool_call` under the harness policy and mode |
| Refusal | without that layer in the run config the root refuses with `harness.ungoverned`, and with feature `auth` without a blocking audit trail |
| Audit | the members record `harness.member.spawned` and `.finished`, their calls governed `ToolCall` records, all in the runner's audit trail |
| Spend | with feature `observe` the run is metered into `HarnessRuntime::spend` and recorded as `harness.spend` |
| Transfer | never: the runner always starts at the round |

```rust,no_run
use std::sync::Arc;
use std::time::Duration;

use adk_agent::{BlackboardHistoryPolicy, BlackboardPolicy, TeamBudget};
use adk_core::{Content, SessionId, UserId};
use adk_harness::{HarnessAgent, HarnessMode, MemberKind, MemberSpec, MemberTools};
use adk_model::gemini::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

fn seat(focus: &str) -> MemberSpec {
    MemberSpec {
        name: format!("review_{focus}"),
        description: format!("Reviews the change for {focus}."),
        kind: MemberKind::Local {
            instruction: format!("Review the workspace for {focus}. End with a VERDICT line."),
            model: None,
        },
        tools: MemberTools::ReadOnly,
        skills: false,
        max_iterations: 8,
        timeout: Duration::from_secs(600),
        output_schema: None,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let board = HarnessAgent::builder("review_board")
        .model(model)
        .workspace(adk_devtools::Workspace::new("."))
        .mode(HarnessMode::ReadOnly)
        .without_shell()
        .without_default_members()
        .member(seat("correctness"))
        .member(seat("style"))
        .build()?;
    let round = board.runtime().team().expect("two seats").review_round_with(
        "review_board",
        &["review_correctness", "review_style"],
        BlackboardPolicy {
            max_rounds: 2,
            history: BlackboardHistoryPolicy::Full,
            budget: TeamBudget { max_model_requests: Some(16), ..TeamBudget::default() },
            ..BlackboardPolicy::default()
        },
    )?;
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "review".into(),
            user_id: "alice".into(),
            session_id: Some("board".into()),
            state: Default::default(),
        })
        .await?;
    let runner = board
        .configure_runner(
            Runner::builder()
                .app_name("review")
                .agent(board.governed_root(round))
                .session_service(sessions),
        )
        .build()?;
    let mut events = runner
        .run(
            UserId::new("alice")?,
            SessionId::new("board")?,
            Content::new("user").with_text("Review the change against the task."),
        )
        .await?;
    while let Some(event) = events.next().await {
        println!("{} spoke", event?.author);
    }
    Ok(())
}
```

`adk-rust ultracode` runs its Blackboard review this way ([CLI](cli.md#ultracode)).

## Audit and spans

| Record | Fields |
|---|---|
| `harness.member.spawned` | `parentAgentId`, `memberAgentId`, `member`, `kind` (`local`, `acp`, `a2a`, `agent`), `depth`, `timeoutMs`, `budget` |
| `harness.member.finished` | `memberAgentId`, `member`, `kind`, `outcome` (`completed`, `held`, `failed`, `abandoned`), `durationMs` |

Each delegation runs in a `harness.member` span with `harness.member` and `invocation.id`.
