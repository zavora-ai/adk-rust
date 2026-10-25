# Agents you create

A person adds agents of their own beside the primary agent, by asking or by hand (spec 3.3 §5.7).
The kernel keeps each one in its agent store, hosts it while it runs, and changes it only through
its audited agent table, `Kernel::agents()`. An agent may suggest an agent; only a person adds one.

| Way in | What happens |
|---|---|
| By hand | A surface previews the definition with `Agents::preview`, shows "Here is what Finance can do" with its risk, and applies it with `Agents::apply` and the preview's digest |
| By asking | The primary agent's `propose_agent` tool calls `Agents::propose`; the kernel checks the suggestion against the suggesting agent's limits, keeps it `Proposed`, and puts one `kernel:agent.create` decision in the inbox. The next scheduler tick adds it once a person approves the same definition |

The web home reaches the table through the surface engine in `adk-rust up`'s process; the terminal
reaches it through the `/agent` command. The control plane serves read routes only.

## The definition

| Field | Rules |
|---|---|
| `displayName` | 1 to 40 characters; unique among the kernel's active, paused and suggested agents, ignoring case; never "You" or the primary agent's name |
| `color` | One of eight avatar colours: `orange`, `green`, `blue`, `violet`, `slate`, `amber`, `teal`, `pink`; kept across a rename |
| `description` | One line of at most 200 characters |
| `instructions` | At most 16,000 characters. Instructions shape what the agent says and tries, never what it may do |
| `area` | Its partition; fixed once added |
| `mode` | `read_only`, `plan`, `ask` or `auto`: its area's mode or stricter |
| `model` | `{ provider, model }`, any provider whose key the host has; never a key. `None` runs the primary agent's model |
| `uses` | `files` (`none`, `read`, `readWrite`) in the area's workspace or one `folder` inside it (relative, no `..`); `shell` (`none`, `sandboxed`, `host`); `web` (`none`, listed `hosts`, `any`); `packs`; `drivers` mounted for its area |
| `dailyLimitMicroUsd` | Optional; at most its area's `dailySpendCap`. Once spent, the agent's unattended work waits until the next UTC day; a turn the person starts still runs |

`AgentDefinition::digest` is the SHA-256 of the definition's canonical JSON: it covers every field
above and nothing else, and a suggestion's decision binds to it.

## Ids

The kernel gives an agent its id once, from its first name: `finance`, or `finance-2` when
`finance` was ever used. A name with no ASCII letter or digit gets `agent`, `agent-2`, … The id
matches `[a-z0-9][a-z0-9-]{0,39}` and is never `adk-rust`, `self-improver`, an id with `@` or `.`,
a target or workflow the host registered, `new` (the web home's New agent screen), or one
`AgentFactory::reserved_ids` names. The id is the
agent's kernel target, its root agent name and its identity, `urn:adk:agent:{app}/{id}`. A removed
agent stays as a `Removed` tombstone, so its id is never given again.

## Who may change an agent

Every change takes an `AgentCaller`: who, from which surface and how, through which device and
transport, with what presence, and the calling process where the operating system reports it.

| Caller | Table |
|---|---|
| Not a `Human` subject | Refused |
| A paired device | Refused; devices answer suggestions in the inbox only |
| High risk or money, without presence | Refused; presence is `PairedLocal` (the kernel's own terminal) or `Confirmed` ("Confirm it's you") |
| `resume` outside the web home, without presence | Refused; an agent paused by hand is resumed in the web home or on the kernel's own terminal (S-I10) |
| A change applied with a digest that is not its preview's | Refused with `kernel.agent.unconfirmed` |
| A change made against an older version | Refused with `kernel.agent.out_of_date`, naming when it changed |

## Risk

The factory's `assess` and the kernel's own floor are combined; the higher risk wins.

| Risk | The agent would have any of |
|---|---|
| Low | `read_only` or `plan`; no shell; no web, or listed sites only; drivers that only read |
| Medium | `ask`; changing files; the sandboxed shell; any site; drivers that write locally or schedule with others |
| High | `auto`; a shell on this computer; drivers that send, delete or are undescribed; a suggestion made during work the person did not start at a screen |
| High, spends money | The `payments` or `computer-use` pack; drivers that spend or publish |

A change that only makes an agent do less is low risk and applies at its next step. A change that
lets it do more is assessed as the agent would be, and ends the decisions it had waiting once the
change is written; a change that loses to another ends none.

Instructions set up by hand pass `AgentFactory::screen` before they are kept: `adk-rust up`'s
factory redacts secrets and labels injection patterns as fetched text is (spec 3.1 I47), and
redacts secrets in the line. Every change records the surface it was made on (`changedOn`), which
a change refused as out of date names: "Finance was changed in the terminal at 10:01".

An agent's area is checked as `Agents::area` gives it: the mode its primary agent started in, or
the partition record's, tightened by a mode the person set while the kernel runs, and the kernel's
quotas lowered by the area's own and by a daily limit the person set.

## Suggestions

`Agents::propose(definition, &AgentProposer)` refuses a suggestion over the suggesting agent's
limits in its area (`AgentFactory::ceiling_in`, and `AgentProposer::ceiling`, the suggester's own
limits in that turn, which only tighten them) or its area's, and one that carries a shell on this
computer, payments or computer use (S-I12). The same suggestion waiting already is returned merged;
at most three wait per suggesting agent. The inbox request has the id `agent-<id>-<12 hex>`, the
hex from the definition's digest and the instant it was suggested, so a suggestion made again after
a refusal has a decision of its own; the tool name `kernel:agent.create`, the summary "Add an agent
called Finance", the actions Approve and Reject, the suggesting agent as requester and no expiry. A
suggestion whose decision cannot be written is taken back and recorded refused; one left without
its decision ends refused on a tick five minutes later.

`Agents::answered(request_id, &AgentCaller)` applies a person's answer at once, as the surface gave
it, so `kernel.agent.created` names the surface, device and presence; otherwise the next tick
applies it as `inbox`.

| Inbox answer | Next tick |
|---|---|
| Approved by a person, same digest, still within the suggester's and the area's limits | Added (`kernel.agent.created`, `approvedBy`) and hosted |
| Approved by a person, but over a limit now | `Refused`; the suggesting thread gets "Finance wasn't added: …" |
| Approved by anyone who is not a person | Ignored and recorded once |
| Rejected or cancelled | The suggestion is deleted; its id was never used |

"Change it first" is `AgentAction::Create` with `replaces` naming the suggestion: the person's
definition is added by hand and the suggestion ends as "You changed it and added it". The chat
gateway's card for a suggestion offers "Don't add" only, at any risk.

## Hosting

`KernelBuilder::agent_factory` registers how agents are built; `KernelBuilder::agent_store` where
they are kept (in memory by default). At start and on every scheduler tick, the kernel reconciles
its hosted agents with the store:

1. An active or paused agent not hosted at its definition is built by the factory; a pause or a
   resume keeps the build. A build from a read older than what is hosted is never swapped in. The
   kernel refuses
   a runner whose kill switch is not its own (S-I15) or whose root agent is not the agent's id,
   attaches its progress source and the ledger, and swaps it into the live target table every
   worker reads (spec 3.3 §10 S4). A build error keeps the record and shows "Can't start" with the
   reason.
2. A run of an agent this replica expects but has not built yet is handed back for a later claim.
3. Progress, usage and the loop guard of an added agent's processes are read from its own source
   before its area's.
4. A removal interrupted after its first record is finished, then the tombstone written.
5. Two kernels on one store converge within one tick.
6. A build that a change replaced, or of an agent removed, is closed once the runs that started on
   it finish — `HostedAgent::close` (`HostedClose`), else its runner — and every build is closed
   when the kernel stops.

A follow-up without a proposing run goes to its area's own agent, never to an added one.

## Pausing and removing

| Action | Effect |
|---|---|
| `pause` | Its running work stops at its next step and its queued and sleeping work is held, marked `Suspension::Paused`; new work is refused with "Finance is paused". A freeze and its lifting leave it paused |
| `resume` | Wakes only what its pause stopped |
| `remove` | Previews what ends (`EndedWork`: work, routines, decisions, follow-ups), then records `kernel.agent.removing`, kills its processes, cancels its waiting decisions with "You removed Finance at 10:02", withdraws its follow-ups, calls `AgentFactory::retired` to revoke its rules, retires its target and writes the tombstone |

## Read routes

| Route (under `/api`) | Caller | Answer |
|---|---|---|
| `GET /kernel/agents` | operator, or a device with `read` | `{ "agents": [AgentView] }`: every agent with its state — suggested, active, paused, being removed and removed — but refused suggestions, each with `cantStart` when this kernel cannot build it |
| `GET /kernel/agents/{id}` | operator, or a device with `read` | One `AgentView`, tombstones included; `404` `kernel.agent.not_found` otherwise |

`ControlClient::agents` and `ControlClient::agent` call them. No route creates, changes or removes
an agent.

## Events and audit records

Each change publishes `KernelEvent::Agent { id, change, reason }` (`EventKind::Agent`), with
`change` one of `proposed`, `created`, `changed`, `paused`, `resumed`, `removing`, `removed`,
`refused`, `cantStart` or `hosted`. Each is recorded before it acts; under
`AuditFailureMode::Block` a change whose record cannot be written does not happen.

| Record | Fields |
|---|---|
| `kernel.agent.proposed` | `id`, `digest`, `risk`, `money`, `area`, `suggestingAgent`, `session`, `run`, `unattended` |
| `kernel.agent.created` | `id`, `version`, `digest`, `risk`, `money`, `origin`, `approvalId` or `replaces`, and the caller |
| `kernel.agent.changed` | `id`, `version`, `fields`, `from`, `to`, `risk`, `widens`, and the caller |
| `kernel.agent.paused`, `kernel.agent.resumed` | `id`, and the caller |
| `kernel.agent.removing`, `kernel.agent.removed` | `id`, `ended`, and the caller |
| `kernel.agent.refused` | `id` or `name`, `reason` |

The caller's fields are `by`, `surface`, `how`, `device`, `presence`, `transport` and `peerPid`.

## Stores

`AgentStore` runs on every kernel backend, with one conformance suite
(`agent_store_conformance_tests!`, feature `conformance`):

| Backend | Type | Feature |
|---|---|---|
| In-memory | `InMemoryAgentStore` | — |
| SQLite | `SqliteAgentStore` (`adk_kernel_agents` in `harness.db`) | `sqlite` |
| PostgreSQL | `PostgresAgentStore` | `postgres` |
| Redis | `RedisAgentStore` | `redis` |
| MongoDB | `MongoAgentStore` | `mongodb` |
| Firestore | `FirestoreAgentStore` | `firestore` |
| Neo4j | `Neo4jAgentStore` | `neo4j` |

Every write is a compare-and-set on the stored version; only a `Proposed` record is ever deleted.

## Example

```rust
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_kernel::agents::{
    AgentAction, AgentCaller, AgentDefinition, AgentFactory, AgentOutcome, AgentUses, FileAccess,
    Presence,
};
use adk_kernel::stores::SqliteAgentStore;
use adk_kernel::{Kernel, PartitionMode};

async fn add_finance(
    builder: adk_kernel::KernelBuilder,
    pool: sqlx::SqlitePool,
    factory: Arc<dyn AgentFactory>,
) -> adk_kernel::Result<()> {
    let store = SqliteAgentStore::new(pool);
    store.migrate().await?;
    let kernel: Kernel = builder.agent_store(Arc::new(store)).agent_factory(factory).build()?;
    let caller = AgentCaller::new(SubjectRef::human("alice"), "terminal", "keyboard")
        .with_presence(Presence::PairedLocal);
    let definition = AgentDefinition::new("Finance", "default")
        .with_description("Reads invoices and drafts payment reminders")
        .with_mode(PartitionMode::Ask)
        .with_uses(AgentUses::default().with_files(FileAccess::Read).with_folder("Invoices"));
    let add = AgentAction::Create { definition, replaces: None };
    let preview = kernel.agents().preview(&caller, &add).await?;
    println!("{}: {:?} risk, confirm with {}", preview.id, preview.risk, preview.code());
    if let AgentOutcome::Created { agent } = kernel.agents().apply(&caller, add, &preview.digest).await? {
        println!("added {} as {}", agent.display_name, agent.agent_id("adk-rust"));
    }
    Ok(())
}
```
