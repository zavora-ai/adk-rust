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
held for a person, and the delegation is held with them until the decision.

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

## Audit and spans

| Record | Fields |
|---|---|
| `harness.member.spawned` | `parentAgentId`, `memberAgentId`, `member`, `kind` (`local`, `acp`, `a2a`, `agent`), `depth`, `timeoutMs`, `budget` |
| `harness.member.finished` | `memberAgentId`, `member`, `kind`, `outcome` (`completed`, `held`, `failed`, `abandoned`), `durationMs` |

Each delegation runs in a `harness.member` span with `harness.member` and `invocation.id`.
