# Serving the harness

`adk-rust serve` puts the harness behind the ADK HTTP server: the same agent, policy and
standing rules as the REPL, with held calls decided through the approval inbox instead of a
terminal prompt (spec 3.1 §8.2).

```bash
adk-rust serve                                  # 127.0.0.1:8080, plan mode
adk-rust serve --mode ask --dir ./project       # edits and commands wait for approval
adk-rust serve --host 0.0.0.0 --auth-token "$TOKEN"
adk-rust serve --agent chat                     # the plain chat agent, as before the harness
adk-rust serve --store postgres://harness@db/harness --port 8081   # one replica of several
```

| Default | Value |
|---|---|
| Agent and app name | `adk-rust` |
| Mode | `plan`; the model reads the workspace and proposes plans, nothing changes |
| Approval inbox | `/api/approvals`, expiry ticker every 30 s |
| Standing rules | `/api/rules`, for callers with the `operator` scope |
| Background runs | `/api/runs`, target `adk-rust` |
| Storage | `<config dir>/adk-rust/harness.db`, shared with the REPL, in SQLite's write-ahead-log mode (`harness.db-wal` and `harness.db-shm` sit beside it while it is open); `--store <url>` for [replicas](#running-replicas) |
| Plans and goal graphs | Checkpointed in the same store; `--checkpointer <url>` names another |
| Request timeout | 15 minutes, since a turn runs model calls and commands (`with_security_config` sets another) |

> **Important:** a served harness in plan mode still lets the model read the whole workspace.
> Serve only workspaces whose contents the model may see.

## Who may call it

The server's routes need an authenticated caller; the harness mount always has one.

| Listening on | Caller | Identity |
|---|---|---|
| `127.0.0.1` / `::1` (default) | Any loopback peer | `LoopbackOperatorExtractor`: the OS user, scope `operator` |
| Any other address | `Authorization: Bearer <token>` | `BearerTokenExtractor`: `--auth-token` or `ADK_SERVE_TOKEN`, compared in constant time |

Without a token, `serve` refuses a non-loopback `--host`. `--mode auto` beyond loopback also
needs `--sandbox`. The served harness applies standing rules and auto mode only to runs whose
caller was authenticated (spec 3.1 §9.2 I6); every caller the mount admits is. The caller's identity replaces the `userId` a request body names, and
held calls are routed to the `operator` group, so the operator approves the calls of their
own runs.

The [web home](../surfaces/web-home.md) at `http://127.0.0.1:8080/ui/` shows held calls as
decision cards and answers them with the version it saw (spec 3.3 §5.1, §7). It is the
operator's, as the mount's other routes are: on loopback it needs nothing more, and with
`--auth-token` the page asks for the token once and sends it as `Authorization: Bearer` on
every request. A message typed in the home runs as the caller the server admitted, so the
mode and standing rules apply to it as they do to `POST /api/run`. Its **Stop all** freezes
the server's runs, the background runs and the home's turns alike, on every replica sharing
the `--store`, and only a caller with the `operator` scope may stop or resume (spec 3.3 §11
S-I10).

## Running replicas

Several `adk-rust serve` processes behind one load balancer share their state through one
store (spec 3.1 §8.2, §13 Q2). `--store` names it, and every store of the served harness moves
there together:

```bash
cargo install adk-cli --features postgres          # or --features redis
adk-rust serve --store postgres://harness:…@db/harness --host 0.0.0.0 --port 8080 --auth-token "$TOKEN"
adk-rust serve --store postgres://harness:…@db/harness --host 0.0.0.0 --port 8081 --auth-token "$TOKEN"
```

| Store | `postgres://…` | `redis://…` |
|---|---|---|
| Sessions | `PostgresSessionService` | `RedisSessionService` |
| Held calls and decisions | `PostgresApprovalStore` | `RedisApprovalStore` |
| Durable runs | `PostgresRunStore` | `RedisRunStore` |
| Modes | `PostgresModeStore` (`adk_harness_modes`) | `RedisModeStore` |
| Standing rules | `PostgresStandingRuleStore` (`adk_harness_rules`, `adk_harness_rule_versions`) | `RedisStandingRuleStore` |
| Plans and goal graphs | `PostgresCheckpointer` | `RedisCheckpointer` |
| Action ledger | `PostgresActionLedger` | `RedisActionLedger` |
| Spend ledger | `PostgresSpendLedger` | `RedisSpendLedger` |

The PostgreSQL stores share one connection pool and migrate under advisory locks, so
replicas can start together. The banner names the store with its password redacted.
`adk-rust rules --store <url>` lists, adds and revokes the standing rules every replica
reads:

```bash
adk-rust rules add bash --prefix "cargo test" --store postgres://harness:…@db/harness
adk-rust rules list --store postgres://harness:…@db/harness
adk-rust rules rm 1 --store postgres://harness:…@db/harness
```

| Guarantee | Mechanism |
|---|---|
| A mode tightened through one replica applies on every replica at the next tool call (I14) | The policy reads the mode store on every evaluation |
| A rule granted through one replica's inbox is matched by every replica | Rules live in the shared store; a replica reads them at evaluation time |
| A revocation through one replica is refused by every replica from its next call (I4) | Each grant and revocation increments the app's rule version in the transaction or script that changes the rule; a replica serves rules from its cache only while the stored version matches, so an unchanged app costs one primary-key read or one `GET` per call |
| A decision taken on one replica wakes the parked run on any replica | The approval store and the durable run store are shared |
| An approved plan or a goal's graph continues on another replica | Checkpoints are in the shared store |

`adk-harness/tests/shared_store_postgres_tests.rs` and `shared_store_redis_tests.rs` run the
mode and rule conformance suites, two store instances over one backend, and two `Harness`
values over one backend: a rule granted through the first is matched by the second, a
revocation through the first is refused by the second on its next call, and a mode tightened
on the first applies on the second. CI's `postgres` job runs both against its PostgreSQL and
Redis services.

| Limit | Reason |
|---|---|
| `--store` refuses a store key (`--store-key`, `[store] key`) | The shared mode and rule stores are not sealed by the store envelope; encrypt the backend at rest instead (spec 3.1 I57) |
| The shell ledger, goal records and workspace trust stay per process | An unsandboxed retry is offered only on the replica that saw the failed command; goal records live in the memory of the replica that started the goal, so run goals against one replica |
| Long-term memory is off | As on every served harness (spec 3.1 §7.15.2) |

## Deciding a held call over HTTP

With `--mode ask`, a call the policy holds ends the run with a `tool_confirmation` event and
an approval request in the inbox. Deciding it and sending an empty message resumes the
session; the approved call runs once.

```bash
# 1. A run holds its command.
curl -s localhost:8080/api/run -H 'content-type: application/json' -d '{
  "appName": "adk-rust", "userId": "me", "sessionId": "s1",
  "newMessage": {"role": "user", "parts": [{"text": "Run: echo hi >> greeting.txt"}]}}'

# 2. The inbox lists it with its id and version.
curl -s localhost:8080/api/approvals

# 3. Approve it.
curl -s localhost:8080/api/approvals/apr_…/decide -H 'content-type: application/json' \
  -d '{"expectedVersion": 1, "action": "approve"}'

# 4. An empty message resumes the session; the command runs once.
curl -s localhost:8080/api/run -H 'content-type: application/json' -d '{
  "appName": "adk-rust", "userId": "me", "sessionId": "s1",
  "newMessage": {"role": "user", "parts": []}}'
```

`"action": "reject"` with a `"reason"` denies the call instead. The same steps from the
command line:

```bash
adk-rust approvals list --server http://127.0.0.1:8080
adk-rust approvals approve apr_… --server http://127.0.0.1:8080
adk-rust approvals deny apr_… --reason "not on main" --server https://agents.example.com --token "$TOKEN"
```

## Standing rules over HTTP

A served harness keeps its standing rules under `/api/rules` (spec 3.1 §7.5). The routes sit
behind the same authentication as `/api/approvals` and need the `operator` scope: no caller
is `401`, a caller without the scope `403`. A rule's subject is always the authenticated
caller — a body that names a `userId` is refused — so a rule covers only the caller's own
calls, and a caller lists and revokes only their own rules.

| Route | Effect |
|---|---|
| `GET /api/rules` | The caller's rules, oldest first |
| `GET /api/rules/suggestions/{approvalId}` | The rules a call held in one of the caller's sessions offers, as a prompt would |
| `POST /api/rules` | `{"approvalId": "apr_…", "suggestion": 1}` remembers an offered rule; `{"tool": "bash", "prefix": "cargo test"}` creates an explicit rule pinned to the served harness's declaration of the tool. Optional: `"decision": {"kind": "deny", "reason": "…"}`, `"expiresInHours": 8`; explicit rules also take `"pathWithin": "src"` and `"sessionId": "s1"`. Answers `201` with the rule |
| `DELETE /api/rules/{id}` | Revokes one of the caller's rules; it stops applying from the next call |
| `POST /api/rules/{id}/extend` | `{"expiresAt": "2026-12-24T23:59:00Z"}` moves the end of one of the caller's rules that has one to a later time ("Extend", spec 3.3 §8.1). A rule without an end date, or an end not after both its end and now, is `400`; another person's rule, or one revoked meanwhile, is `404` — an extension never brings a revoked rule back. Answers with the rule |

Every grant, extension and revocation is recorded as `harness.rule.created`,
`harness.rule.extended` (with the old end as `from` and the new one as `to`) or
`harness.rule.revoked` with `surface: rest` before the store write. Creating a rule from a
held call does not decide that call: decide it through `/api/approvals`.

```bash
# The held call's offers, then "always allow `cargo test …`" (offer 0) for every session.
curl -s localhost:8080/api/rules/suggestions/apr_…
curl -s localhost:8080/api/rules -H 'content-type: application/json' \
  -d '{"approvalId": "apr_…", "suggestion": 0}'

# An explicit rule, extended to the end of 24 December, and its revocation.
curl -s localhost:8080/api/rules -H 'content-type: application/json' \
  -d '{"tool": "bash", "prefix": "cargo test", "expiresInHours": 8}'
curl -s localhost:8080/api/rules/<rule id>/extend -H 'content-type: application/json' \
  -d '{"expiresAt": "2026-12-24T23:59:00Z"}'
curl -s -X DELETE localhost:8080/api/rules/<rule id>
```

A `bash` allow rule needs a `prefix` of plain command words that does not start with a
command runner (`sh`, `bash`, `env`, `sudo` and similar); `exit_plan_mode`, memory writes,
payments and desktop actions are never covered by a rule.

## Serving your own harness

`HarnessLauncher::build_app` mounts any `HarnessAgent` the same way; build the agent with
the approval service the server mounts, so held calls reach the inbox.

```rust
use std::sync::Arc;

use adk_cli::harness::{HarnessLauncher, OPERATOR_SCOPE};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, InMemoryApprovalStore, StoreApprovalService,
};
use adk_harness::{HarnessAgent, HarnessMode};

async fn serve(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(routing),
    );
    let harness = HarnessAgent::builder("my-harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Plan)
        .approval_service(approvals.clone())
        .build()?;
    HarnessLauncher::new(harness)
        .with_user_id("ana")
        .with_approval_service(approvals)
        .run_serve_directly(8080)
        .await
}
```

The server serves the harness under its agent name, which is the `appName` of requests.
Without an extractor, `build_app` admits loopback peers as the operator and refuses to build
for a non-loopback bind address; serve the router with
`into_make_service_with_connect_info::<SocketAddr>()` (as `run_serve_directly` does) so the
peer address is known.

## One harness, every deployment

The same `HarnessAgent` value runs behind each surface (spec 3.1 H9); `harness.agent()`
hands it to any of them, and the held call → approve → resume round trip works on each.

| Surface | Mount | A held call is decided through | Resumed by |
|---|---|---|---|
| `Launcher`, `HarnessLauncher` | `build_app`, `run_serve_directly` | `/api/approvals` | an empty-message `/api/run` |
| `ServerBuilder` | `ServerBuilder::with_approval_service` | `/api/approvals` | an empty-message `/api/run` |
| Agent Engine | `serve_agent_engine(harness.agent(), ..)` | the approval service's inbox, over its durable store | the next `stream_query` of the session |
| Managed runtime | `HarnessResolver` on `DefaultManagedAgentRuntime` | `user.tool_confirmation`, or the runtime's approval service | the answer itself: it wakes the parked run |

`examples/harness_agent` runs the scenario `one_harness_every_surface`: one harness, one
approvals inbox, four surfaces, and a log that gains exactly one line per surface.

## On Agent Engine

`serve_agent_engine` serves the harness in a Gemini Enterprise Agent Platform container
(spec 3.1 §8.5). The platform reaches the container only through the class-method dispatch
routes, so held calls go to the harness's approval service on a durable store, and the
approvals REST of a server over the same store, or an `ApprovalNotifier`, decides them.

```rust
use std::sync::Arc;

use adk_core::{ApprovalPolicy, ApprovalService, ApprovalStore, ApproverSelector, Llm, StoreApprovalService};
use adk_harness::{HarnessAgent, HarnessMode};
use adk_server::agent_engine::{AgentEngineOptions, serve_agent_engine};

async fn serve(model: Arc<dyn Llm>, approvals: Arc<dyn ApprovalStore>) -> adk_core::Result<()> {
    let routing = ApprovalPolicy::new([ApproverSelector::group("operator")]);
    let inbox: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(approvals).with_router(routing));
    let harness = HarnessAgent::builder("my-harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new("/workspace").allow_bash(true))
        .mode(HarnessMode::Ask)
        .approval_service(inbox)
        .build()?;
    serve_agent_engine(harness.agent(), AgentEngineOptions::new()).await
}
```

| Step | Over the dispatch protocol |
|---|---|
| 1. A turn holds a call | `stream_query` streams an event whose `actions.tool_confirmation` names the call; the request is in the inbox |
| 2. An operator decides it | `POST /api/approvals/{id}/decide` on a server over the same approval store, or the service's `decide` |
| 3. The session resumes | the next `stream_query` of the session, with `"message": ""`, runs the approved call once |

`adk-harness/tests/agent_engine_tests.rs` drives the app `build_agent_engine_app` returns
through its class-method dispatch on a loopback port, with no GCP call.

## On the managed runtime

For hosted tenants, a harness is a `HarnessDef`: a `ManagedAgentDef` that also declares the
mode, the shell policy, skills roots, members and packs (spec 3.1 §7.18, feature `managed`;
`adk-managed` is Experimental, so the pack is too). A `HarnessResolver` on the runtime builds
the harness from the definition, and the runtime runs it in its supervised session loop.

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["managed"] }
adk-managed = { version = "3.0.0", features = ["openai"] }
adk-session = "3.0.0"
```

```rust
use std::sync::Arc;

use adk_harness::HarnessMode;
use adk_harness::packs::managed::{HarnessDef, HarnessResolver, MemberDef, ShellPolicy};
use adk_managed::resolver::DefaultModelResolver;
use adk_managed::types::{ModelRef, RuntimeError};
use adk_managed::{DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedOwner};
use adk_session::InMemorySessionService;

async fn host() -> Result<(), RuntimeError> {
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(DefaultModelResolver::new()),
        Arc::new(InMemorySessionService::new()),
    )
    .with_agent_resolver(Arc::new(HarnessResolver::new("/srv/workspaces")));

    let def = HarnessDef::new("builder", ModelRef::Shorthand("gpt-5.6-terra".into()))
        .with_mode(HarnessMode::Ask)
        .with_shell(ShellPolicy::os_sandbox().with_fail_if_unavailable(true))
        .with_member(MemberDef::new("tester", "Runs the tests.", "Run the tests and report."));
    let agent = runtime.create(def.into()).await?;
    let session = runtime.start_session(&agent, &ManagedOwner::new("tenant-a", "ana")?, None).await?;
    let _ = session;
    Ok(())
}
```

The harness fields travel in the definition's `metadata` under `adk.harness`, so any store or
service that keeps a `ManagedAgentDef` keeps them. As JSON, a `HarnessDef` is the
definition's snake_case shape with the harness fields beside it:

```json
{
  "name": "builder",
  "model": "gpt-5.6-terra",
  "system": "Keep notes in notes.txt.",
  "mode": "ask",
  "shell": { "isolation": "os_sandbox", "fail_if_unavailable": true },
  "skills_roots": ["skills"],
  "members": [{ "name": "tester", "description": "Runs the tests.", "instruction": "Run the tests and report." }],
  "tools": [{ "type": "custom", "name": "lookup_order", "input_schema": { "type": "object" } }]
}
```

| Field | In a harness |
|---|---|
| `model` | resolved by the runtime's `ModelResolver`; `DefaultModelResolver` builds Gemini, OpenAI, Anthropic and Ollama |
| `mode` | `read_only`, `plan`, `ask` (default) or `auto`; an unknown mode refuses the definition |
| `shell.isolation` | `os_sandbox` (default), `host` or `none` |
| `skills_roots` | directories relative to the workspace; `..` and absolute paths are refused |
| `members` | local members on the harness model, read-only unless `"tools": "full"` |
| `packs` | `browser`, `computer_use`, `payments`, `voice` |
| `tools` | `custom` tools only: the client executes them |
| `permission_policy`, `mcp_servers`, `skills` | refused, with the field and the replacement named |

A definition comes from a tenant, so the resolver keeps the operator's limits:

| Limit | Default | Operator setting |
|---|---|---|
| Host shell | refused; an unavailable sandbox fails the build rather than fall back to the host | `HarnessResolver::allow_host_shell(true)` |
| Workspace | `<root>/<definition name>`, created on first use | `HarnessResolver::with_workspace` |
| Operator files | the operator's `AGENTS.md` and home skills directory are not loaded | — |
| Stores, policy, audit, approvals, MCP, ACP and A2A members | in memory; nothing beyond the definition | `HarnessResolver::configure`, applied after the definition |

`HarnessResolver::with_harness(&harness)` runs an application's own `HarnessAgent` for the
definition named like it, so the managed runtime serves the same value as the other surfaces.
`HarnessAgentBuilder::pack(Pack::Managed)` marks such a harness for the managed runtime:
`info().packs` and the prompt's environment name `managed`, as they do for every harness the
resolver builds; without feature `managed` the build fails naming the feature.

| Behaviour | Test in `adk-harness/tests/managed_tests.rs` |
|---|---|
| A held call parks the turn on its durable run with `requires_action`; `user.tool_confirmation` resumes it and the call runs once | `h1_a_held_write_runs_once_after_approval` |
| The definition's mode governs | `h2_the_definitions_mode_governs` |
| A custom tool is confirmed, then parks for `user.custom_tool_result` | `h3_a_custom_tool_is_confirmed_then_parks_for_its_client_result` |
| An approval taken by a restarted instance over the same stores runs the call once | `a_held_bash_approved_on_a_restarted_instance_runs_once` |
| A call that ran before its result was saved is answered from the action ledger, never run again | `a_bash_that_ran_before_its_result_was_saved_is_answered_from_the_ledger` |
| A call whose decision was lost before it began is held again and reported as `requires_action` | `a_held_call_whose_decision_was_lost_before_it_began_is_held_again` |
| An `adk-enterprise` client reconnects with `Last-Event-ID` and receives what it missed | `an_enterprise_client_replays_a_parked_harness_session_after_a_reconnect` |

## Which durability engine

| Deployment | Engine | Why |
|---|---|---|
| The CLI, a local or single-tenant server | `DurableWorker` (`adk-server`, feature `background`) | runs on the server's session backend and approvals inbox, under fenced leases |
| Hosted tenants | the managed runtime (`adk-managed`) | per-tenant sessions, definitions as data, parked turns and event replay for `adk-enterprise` clients |

## Related

- [The harness in your terminal](cli.md)
- [Launcher](../deployment/launcher.md)
- [Agent Engine](../deployment/agent-engine.md)
- [Managed agent runtime](../managed-agents/runtime.md)
- [Durability](durability.md)
- Spec: `docs/design/spec-3.1-harness-agent.md` §8.2, §8.5, §7.18, §13 Q2
