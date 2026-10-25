# adk-harness

The governed harness agent of ADK-Rust: file and shell tools, a planning loop, skills,
approvals and resumable sessions, with every tool call on one governed path
(spec 3.1, [`docs/design/spec-3.1-harness-agent.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.1-harness-agent.md)).

[![Crates.io](https://img.shields.io/crates/v/adk-harness.svg)](https://crates.io/crates/adk-harness)
[![Documentation](https://docs.rs/adk-harness/badge.svg)](https://docs.rs/adk-harness)
[![License](https://img.shields.io/crates/l/adk-harness.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

## What it is

The harness is the agent that runs when you type `adk-rust` in a project. It reads your code,
plans the work, edits files and runs commands — and asks you before anything that changes your
project. Every action it takes passes through one checkpoint that applies your mode, your
remembered decisions and your organisation's policy, whichever runtime hosts it: the terminal,
a server, a background goal or the kernel.

### A session with the harness

```bash
# 1. Open the harness in a project. In ask mode (the default) it reads freely and asks before
#    every edit and every command.
adk-rust

# 2. Ask for work. It keeps a todo list, reads files and proposes an edit or a command, which
#    waits for your answer:
#      [y] allow once  [a] always allow…  [n] deny (reason)  [e] edit args  [?] details

# 3. Answer `a` once for `cargo test` and it stops asking for it. Your remembered rules:
adk-rust rules list

# 4. To have it look and plan without touching anything, type `/mode plan` in the session (or
#    start with --mode plan). It acts only after you approve its plan.

# 5. Run its commands in the OS sandbox: the workspace only, no network.
adk-rust --sandbox

# 6. Leave it working until a check passes.
adk-rust goal "make all tests pass" --until "cargo test"

# 7. Pick up the latest session in this project where you left it.
adk-rust --continue

# 8. See what it learned about you and the project. A guess stays "assumed" until you confirm it.
adk-rust memory list
adk-rust memory accept <id>
```

### The parts, in plain terms

| Part | What it does |
|---|---|
| Modes | `read-only` looks only; `plan` looks and plans, and acts only on an approved plan; `ask` (default) reads freely and asks before edits and commands; `auto` edits on its own, while commands on your machine and changes to protected files still ask |
| Self-knowledge | The agent knows what it can do in this harness: which calls will ask in each mode, the commands you can type, its packs, and what it can and cannot undo; it names the right command when you mistype one, and `harness_help` explains the rest |
| Approvals | Allow once, always allow (a rule tied to you and to that exact tool), deny with a reason, or edit the arguments before it runs. A changed tool asks again |
| Protected files | `AGENTS.md`, `.git`, CI files, toolchain and lockfiles and the like always need a person, so a cloned repository cannot approve its own commands |
| Shell | Commands run on your machine behind approval, or in the OS sandbox (Seatbelt on macOS, bubblewrap on Linux, a restricted token on Windows) or a container per session |
| Project instructions | `AGENTS.md` is read after you trust the workspace, and read again only after you trust its new content |
| Skills | Skills in `.skills/` load when a task needs them |
| Plans and goals | A todo list per session, approved plans run step by step from checkpoints, and goals loop until your check command passes |
| Team | Sub-tasks go to helper agents — built-in explorers, other coding agents, remote agents — under the same mode, rules and budget |
| Memory | Working notes and long-term facts about you and your projects; you list, accept, correct, forget, export and import them |
| Durability | Sessions, waiting approvals and completed actions survive a crash; a completed edit or command never runs twice |
| Oversight | Every decision is written to an audit log, spend is metered per agent and person, and a looping session is stopped and made read-only |
| Packs | Web search and fetch, MCP servers, voice, payments and browser control, each behind the same checkpoint |
| Personal preset | `PersonalAgent` or `--preset personal`: a workspace of notes and files under the home instead of a repository, no host shell (`bash` only in the OS sandbox or a container, never retried on the host), long-term memory on (off under `serve`, as for every preset), and quiet time and a daily spending limit asked at the first run. `Preset::Personal` on any builder sets the prompt, mode and shell |
| Take over | A tool asks for a password or a one-time code; you type it on your phone or the web home, and it reaches that one call — the model sees a placeholder, and no store, log or audit record keeps it |

With the `kernel` feature, the same harness runs as long-lived processes under `adk-rust up`;
see [`adk-kernel`](https://github.com/zavora-ai/adk-rust/blob/main/adk-kernel/README.md).

## How it is built

`HarnessAgent` is configuration and policy over an `LlmAgent`. Its `configure_run` composes
`HarnessPolicy` with the run's own policy (`StrictestPolicy`, the stricter decision wins),
so the policy reaches every runtime that runs the harness — a `Runner`, a transfer, an
agent tool — and the harness refuses to run without it.

| Capability | Mechanism |
|---|---|
| Self-knowledge | `HarnessManifest`, generated at build from the class registry and the mode table under the build's switches, the surface's commands (`HarnessAgentBuilder::surface`), packs, memory and what each safety mechanism covers; rendered as the `<harness>` block after `<environment>` (at most 6 KiB, byte-stable); the standing-rules digest in each request's reminder; `harness_help` for the detail |
| Modes | `ReadOnly`, `Plan`, `Ask` (default), `Auto` in a harness-owned `ModeStore` (in-memory, file, SQLite). Tightening applies at the next call; loosening only through `Harness::set_mode`, which cancels held calls first, or an approved `exit_plan_mode` |
| Tool classes | A call's class comes from its origin and name in the harness registry, never from an MCP `readOnlyHint` alone |
| Standing approvals | "Always allow `cargo test`" as a `StandingRule` bound to the subject and to the tool's declaration digest; `ShellPrefix` matches plain words only; a redeclared tool records `harness.rule.stale` and asks again |
| Prompts | `PromptingConfirmationHandler` serializes prompts, caps them per turn of each session, escapes control characters, refuses a truncated call approved unexpanded, and returns edited arguments for the policy to check again. It re-checks a held call against the run's whole policy, so an operator policy's approval always reaches the person and a standing rule answers only the harness's own |
| Durability | An action ledger on every run (in memory by default, `HarnessAgentBuilder::action_ledger` for a durable one, or the run's own): `bash`, `write_file` and `edit_file` are recorded before they execute and a re-issued or resumed call is answered from its record, never run twice. `Harness::send` and `Harness::resume` first settle a held call whose process stopped before saving its result: answered from the ledger, or held again |
| Shell | `bash` on the host behind approval, or opt-in under the OS sandbox (Seatbelt, bubblewrap, or on Windows the restricted token, which leaves the network open, runs MSYS2 from a private runtime per command while holding the user's namespaces of the installations on its `PATH`, and denies `bash` in Plan) through `SandboxLauncher`: workspace read-write with protected paths read-only (read-only in Plan), scratch `HOME`, no network, the enforcer probed at build and per call (`Degraded` or `harness.sandbox_unavailable`, never a host fallback); a failure hint and a once-only, non-waivable unsandboxed retry; `auto_allow_sandboxed` gated on the S3 conformance probe and the protected paths, with no host file tool running beside an unapproved command; a 250 ms kill-switch poll; `harness.bash.*` audit records. With `docker`, a container per session through `ContainerBashTool`: the workspace bind-mounted at `/workspace` with protected paths read-only, kept across runs, stopped on idle TTL or `Harness::close` |
| Todos | `write_todos` / `update_todo` in session state, one list per session |
| Instructions | `AGENTS.md` from the git root down to the workspace, verbatim, hashed, delimited as untrusted, after a workspace trust decision bound to the files' content (an edited file asks again and records `harness.instructions.changed`); nested files listed, never inlined |
| Skills | Progressive disclosure (`list_skills`, `load_skill`, `load_skill_resource`) without convention documents |
| Observability | `harness.run` and `harness.decide` spans with `harness.mode`, `harness.tool_kind`, `harness.isolation`, `harness.rule_id`, `harness.member`; spend metered per acting agent and subject (`Harness::spend`, `harness.spend`); `LoopGuard` tightens a looping session to ReadOnly and ends the turn; an `adk-eval` suite with a `BaselineStore` baseline |
| Identity | `RbacMap` permissions per tool class through `ScopeGuard`/`ProtectedTool`; per-call, subject-scoped shell credentials through a `CredentialBroker` and a credential helper revoked after the call; `require_authenticated_identity` for served harnesses; every run audited in Block mode, to a signed `AuditLog` (`EphemeralAuditLog` by default) |
| Working notes | `FileMemory` topic files and a derived `MEMORY.md` per user, outside the workspace; `memory_read`, and `memory_write`/`memory_delete` behind a one-time approval; the index last in the instruction |
| Team | Members (`explore` and `general` by default, local `LlmAgent`s, ACP-compatible coding agents, remote A2A agents, any `Agent`, or discovered in a `TeamAgentRegistry`) lowered to one `TeamSpec`: isolated context, a timeout and circuit breaker on every edge, delegation depth 1, the team budget, the parent's mode and policy; ACP permission requests decided on the governed path; `harness.member.*` records |
| Plan graph | An approved plan with `steps` runs as a checkpointed graph: one node per step, `interrupt_before` on `approve` gates (a held `plan_step_gate` call bound to the plan hash and step), a reviewer after `review` gates, rewind through `TimeTravelHandle::fork_at` |
| Goals | `Harness::start_goal` iterates until the operator's `until` command passes (`LoopAgent` or a checkpointed graph); goal state in a `GoalStore`; durable goals as `harness.goal` runs for a `DurableWorker`; ambient goals on cron, file-watch and webhook triggers |
| Durability | A completed call never runs twice after a crash (the turn after one answers it from the action ledger), held approvals survive process loss, resumed runs follow the current mode |
| Long-term facts | `MemoryFact`s with provenance in a `FactStore` (in-memory, file, SQLite), found by a `FactIndex` (`LocalFactIndex` by embedding or shared words, `MemoryServiceIndex` over any `adk-memory` backend) and recalled into a `<memory-facts>` block on each user turn; written by `memory_remember` (assumed only), by distilling a session on a cheaper model, and by a person; `Known` only from the user's own quoted words or a person's confirmation; redacted before storage; `Harness::{facts, accept_fact, forget_fact, correct_fact}` |
| Fact consolidation | The spec 3.2 consolidation tick's work on facts: `Harness::consolidate_memory` distils the sessions the per-session distiller never reached (assumed only), supersedes `Assumed` near-duplicates and expires stale `Assumed` facts without a model call, merges similar `Assumed` facts with provenance, never touches a `Known` fact, stops at a loss cap, and keeps a record `Harness::undo_consolidation` reverts; `Harness::distilled_sessions` lists how far distillation read each session, so a host finds the sessions that ended undistilled, as `adk-rust` does at start |

## Mode table

| Class | ReadOnly | Plan | Ask | Auto |
|---|---|---|---|---|
| Read, Todo, Delegate | Allow | Allow | Allow | Allow |
| `exit_plan_mode` | Deny | Approval, non-waivable | Deny | Deny |
| Edit | Deny | Deny | Approval | Allow |
| Edit of a protected path | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| Sandboxed shell | Deny | Allow, read-only workspace (active enforcer only) | Approval (Allow with `auto_allow_sandboxed`) | Allow when protected paths hold; Approval otherwise |
| Host shell (also a degraded sandbox) | Deny | Deny | Approval | Approval (Allow with `allow_host_shell_in_auto`) |
| Unsandboxed retry | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| Memory write (`memory_write`, `memory_delete`) | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| Fact proposal (`memory_remember`, assumed facts only) | Deny | Deny | Allow | Allow |
| Web, allowlisted host | Allow | Allow | Allow | Allow |
| Web, other host | Deny | Deny | RA | RA |
| External tool, read-only, trusted origin | Allow | Allow | Allow | Allow |
| Any other external tool | Deny | Deny | Approval | Approval |

A standing rule turns an approval into Allow; it never applies to a non-waivable row, and a
Deny rule always wins. A path is protected as written or at the target it resolves to, so a
symlink inside the workspace to `AGENTS.md` or into `.git` is an edit of a protected path.
Protected at any depth: `.git`, `.adk`, `.skills`, `.claude`, `.cargo/config*`,
`AGENTS.md`, `AGENT.md`, `CLAUDE.md`, `MEMORY.md`, `.envrc`, `lefthook.yml`. Protected from
the workspace root (spec 3.4 §7.5): `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`,
`CODEOWNERS`, `publish.sh`, `.github/**`, `scripts/**`, `xtask/**`, `adk-eval/**`, the
governance sources of `adk-core/src`, `adk-harness/src/{policy.rs,approvals/**,selfimprove/**}`,
`adk-harness/tests/suite/**` and `.adk/selfimprove/**`.

## Example

```rust,ignore
use std::sync::Arc;

use adk_core::{SessionId, UserId};
use adk_harness::{HarnessAgent, HarnessMode};
use adk_model::gemini::GeminiModel;
use adk_session::InMemorySessionService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Plan)
        .build()?
        .into_harness("my-app", Arc::new(InMemorySessionService::new()))?;
    let user = UserId::new("alice")?;
    let session = harness.open_session(&user, None).await?;
    let _events = harness.send(&user, &session, "Plan the fix for the failing test").await?;
    Ok(())
}
```

The [`harness_agent`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_agent)
example runs the scripted scenarios against the harness and the 3.0 `CodingAgent`, and the
live scenarios on OpenAI, Anthropic and Gemini.
[`harness_openai`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_openai) and
[`harness_anthropic`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_anthropic)
walk through the spec 3.1 §5 capabilities on one provider each, scripted or live.

## Hooks (feature `hooks`)

A hook is a command that runs on a tool event — `pre_tool`, `post_tool`, `post_edit`,
`on_hold`, `on_stop`, `on_session_start` — through the same launcher as `bash`, with the
event as JSON on stdin. It can deny a call, fail or annotate a result, or notify the user; it
can never allow a call, change the mode, create a rule or answer a call, and
`{"decision":"allow"}` is ignored and audited. Workspace hooks (`.adk/harness.toml`) run
only under the workspace's content-keyed trust. Every run is a `harness.hook.ran` record.
`adk-rust` builds the crate with this feature: `--no-hooks` runs none, `/hooks` lists the
effective set, and `on_hold` and `on_stop` notes are shown at the console. See
`docs/official_docs/harness/hooks.md`.

## Kernel (feature `kernel`)

`Harness::kernel` returns an `adk_kernel::KernelBuilder` with the harness's runner registered
as the app's target (`adk-rust` in the CLI) and as `harness.goal`, and with the harness's
sessions, approval service, kill switch, audit trail, spend ledger and, when it has one, run
store (spec 3.2 §10 HK1). The caller adds the process store, the `KernelConfig` and the
control plane, then builds and starts the kernel.

| Harness part | What the kernel gets |
|---|---|
| Approvals | A decision through the harness's approval service wakes the process asleep on `Wake::Approval` for that call |
| Process view | A `ProgressSource`: a goal's iteration of max, duration and last check, the loop guard's verdict, and the session's model calls, tool calls and cost for spend attribution |
| Goals | The goal thread keeps the last `2 × GOAL_STALL_WINDOW` checkpoints. Under a kernel, a goal whose check repeats its last `GOAL_STALL_WINDOW` (3) results, or whose iteration made no tool call (`Graph` engine), records `harness.goal.stalled` and parks `Waiting` before its next model call instead of spending its iteration cap |
| Drivers | `HarnessAgentBuilder::kernel_drivers(drivers, partition, ids)` attaches a partition's kernel drivers: their tools classed by the approved manifest (`driver_tool_kind`: `read` a trusted read-only `External`, every other class a trusted `External` with effects, whatever the server's `readOnlyHint`), under the kernel's `DriverPolicy` composed with the harness's. `Harness::driver_rule_pins` moves a person's standing rules onto the declarations they re-approve (spec 3.2 §7.7) |
| Partitions | `HarnessAgentBuilder::partition(partition, partitions)` builds one partition's harness: its mode, its class and tenant on every agent's principal, its `memory_scope` as the project of every long-term fact and of its working notes, its drivers (`partition_drivers`: only those its own list names and whose manifests serve it), and its `PartitionPolicy`; `partition_coordinator` marks the one harness that may cross into another partition, under a non-waivable approval, and `partition_driver_crossings` offers it other partitions' drivers as crossings `<partition>__<tool>` (spec 3.2 §7.8, K-I15). `host_partition` adds a partition's harness to a kernel as `<app>@<partition>` and `harness.goal@<partition>`, with its progress source for the partition's processes, so its ledger rows carry its partition; its `observation_audience` answers its own partition's observations only and remembers them in its memory scope. `HarnessResolver` builds one harness per partition of a managed definition (`PartitionAgentResolver`), mounting the tenant's drivers per partition and the coordinator's crossings into the others |

```rust,ignore
use std::sync::Arc;

use adk_kernel::{InMemoryProcessStore, KernelConfig};

async fn host(harness: &adk_harness::Harness) -> Result<(), Box<dyn std::error::Error>> {
    let kernel = harness
        .kernel()
        .config(KernelConfig::default())
        .process_store(Arc::new(InMemoryProcessStore::new()))
        .build()?;
    kernel.start().await?;
    kernel.shutdown_on_signal().await?;
    Ok(())
}
```

## Website (feature `website`)

The runtime half of an AWP 2.0 website (spec 3.5 §7.1, §7.10, §7.11, §7.18). A product builds
its team from it; the product's agents, site kinds and onboarding questions are its own.

| Part | What it does |
|---|---|
| `Preset::Website` | The website prompt, plan mode until the charter compiles to more, no shell, website tools classed by their catalogue |
| `website_back_office(&BackOfficeKit)` | Content drafts, edits, schedules, corrections, withdrawals and reverts; translations; media; preview, publish and rollback over `adk-site`; host reads; the launch gate; `awp verify`; charter drafts; charter-commit and promotion requests as decision cards. The launch and every DNS change are held for the owner in every mode (A-I21) |
| `website_public(PublicKit)` | `search_site` over the release's Markdown twins, `emit_signal` with screened bounded fields, `adk-drivers`' `reply_in_thread`, the declared capabilities, and nothing else (A-I2, A-I3) |
| `LadderCompiler`, `promote_job`, `apply_demotion` | Each job's level compiled to a mode, a policy read at every call, within-budget rules and owner-created standing rules; only an owner's decision raises a level (A-I13) |
| `AuthoringRouter` | Paired devices author through the Steward; everyone else reaches the public partition (A-I24) |
| `SiteSigning`, `ReleaseLogVerifier` | Web Bot Auth on crawls, `awp verify` and opted-in `web_fetch`; the release-log half of `awp verify` before a site publishes its contract |
| `OnboardingStage`, `InterviewForm` | Plan-graph steps for discovery, interview, charter, build, gate, connections and launch; facts `Known` only from the owner's words (A-I18) |

See `docs/official_docs/harness/website.md`.

## Features

| Feature | Default | Pulls in |
|---|---|---|
| `runner` | on | `adk-runner`, `adk-session` (`Harness`) |
| `skills` | on | `adk-skill/progressive-disclosure` |
| `team` | on | `adk-tool`, `adk-agent/team-tools` |
| `graph` | on | `adk-graph`: the plan graph and the `Graph` goal engine, checkpointed in the store `plan_checkpoint_store(url)` names (`checkpointer_from_url`), in memory without one |
| `sandbox` | on | `adk-sandbox/sandbox-native` |
| `observe` | on | `adk-telemetry`, `adk-eval` |
| `auth` | on | `adk-auth` |
| `memory` | on | working notes and long-term facts (`adk-memory`, `adk-guardrail`) |
| `memory-embeddings` | off | `RagEmbedder`: `adk-rag`'s OpenAI and Gemini embedders for long-term facts |
| `web` | off | `web_fetch`; web search through the model provider's built-in search by default, or the governed `web_search` over a `SearchProvider` (`SearxngSearch`, `DeepSeekSearch`); screening and `ProviderWebTools` (`reqwest`, `adk-guardrail`); on in `adk-rust` |
| `mcp` | off | MCP elicitation, sampling and resource bridges (`adk-tool/mcp`, `adk-tool/mcp-sampling`); on in `adk-rust` |
| `web-fixture` | off | a loopback HTTP server stands in for the web in tests; never on in `adk-rust` |
| `hooks` | off | `toml`; `[[hooks]]` from `.adk/harness.toml`, the user config and the builder, run through the shell's launcher (spec 3.1 §7.22) |
| `memory-embeddings-local` | off | `RagEmbedder::onnx`: `adk-rag`'s in-process ONNX embedder (`BAAI/bge-small-en-v1.5`), no network and no key; `ort` downloads the onnxruntime binary at build time |
| `encrypted` | off | The store envelope of spec 3.1 §7.12: `StoreKeyring`, `StoreKeySource` (secret provider, OS keychain, environment), sessions, approvals and the SQLite stores sealed at rest, key rotation |
| `sqlite` | off | SQLite mode, rule, shell-ledger and goal stores, and `sqlite://` plan checkpoints |
| `postgres`, `redis` | off | `PostgresModeStore`, `PostgresStandingRuleStore`, `RedisModeStore`, `RedisStandingRuleStore`: the mode and rule stores every replica of a served harness shares (spec 3.1 §13 Q2), `postgres://` and `redis://` plan checkpoints, and `adk-session`'s stores on the same backend |
| `mongodb`, `neo4j`, `firestore` | off | `mongodb://`, `neo4j://` and `firestore://` plan checkpoints (spec 3.1 §13 Q5) |
| `ambient` | off | `adk-agent/ambient`: goals started by triggers |
| `kernel` | off | `adk-kernel`: `Harness::kernel`, the harness as a kernel's resident agent, and goal stall parking (spec 3.2 §10 HK1, HK3) |
| `docker` | off | `adk-sandbox/workspace-docker`: the container shell (`ShellIsolation::Container`, `ContainerBashTool`) |
| `skills-registry`, `team-acp`, `team-a2a`, `memory-graph` | off | registry skills, ACP and A2A members, graph memory |
| `acp-server` | off | `StandingPermissions` for `HarnessAgent`: an editor's "Always allow" over ACP becomes a standing rule (spec 3.3 §10 A1) |
| `payments` | off | the payments pack (spec 3.1 §7.16): `adk-payments` tools as class `Payment` under a `PaymentMandate`, `harness.payment` with the journal receipt |
| `managed` | off | the managed form (spec 3.1 §7.18): `HarnessDef`, `HarnessResolver` on the managed runtime, `Pack::Managed`; with `kernel` also the kernel's managed form, one kernel per tenant (spec 3.2 §7.13) |
| `browser` | off | the browser pack (spec 3.1 §7.24): `adk-browser`'s tools as `Read`, `Web` and `External` classes under the web rules, `BrowserPack`, `harness.browser.*` |
| `computer-use` | off | the computer-use pack (spec 3.1 §7.24): `computer_use_observe`, `computer_use_preview` and the never-waivable `computer_use_execute` through `adk-computer-use`'s reference graph, `ComputerUsePack` |
| `voice` | off | the voice surface (spec 3.1 §7.17): `VoiceSurface` runs a harness marked `Pack::Voice` over `adk-realtime`'s `IntegratedRealtimeRunner`, with spoken or inbox approvals and `harness.voice.*` audit records; with `kernel` it also answers kernel processes parked on a held call (spec 3.2 §7.11) |
| `website` | off | the website runtime (spec 3.5 §7.1): `Preset::Website`, the back-office and public toolsets over `adk-site`, the site's two partitions and their signals, the autonomy ladder compiled to policy, the authoring router, Web Bot Auth signing of crawls, `awp verify` and opted-in `web_fetch`, and the onboarding plan's steps |

## Status

Beta, as [`STABILITY.md`](https://github.com/zavora-ai/adk-rust/blob/main/STABILITY.md) records.
The managed form (feature `managed`) is Experimental, as `adk-managed` is.

## Guides

| Topic | Guide |
|---|---|
| Modes, approvals and standing rules | [modes and approvals](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/modes-and-approvals.md) |
| The `adk-rust` commands and the REPL | [CLI](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/cli.md) |
| Host shell, OS sandbox and container shell; under the container shell, hooks with `inherit` isolation, goal checks and shell credentials are refused rather than run on the host | [shell and sandbox](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/shell-and-sandbox.md) |
| `harness.toml` and the user config | [configuration](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/configuration.md) |
| Plans, the plan graph and goals | [plans and goals](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/plan-and-goals.md) |
| Team members and delegation | [team](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/team.md) |
| Crash safety and resumption | [durability](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/durability.md) |
| Working notes, long-term facts and consolidation | [memory](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/memory.md) |
| Serving over HTTP and on the managed runtime | [serving](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/serving.md) |
| Identity, permissions and credentials | [identity](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/identity.md) |
| Spans, spend, loop guard and evaluation | [observability](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/observability.md) |
| The website runtime | [website](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/website.md) |
| Hooks | [hooks](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/hooks.md) |
| Web tools and MCP servers | [web and MCP](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/web-and-mcp.md) |
| Browser and computer-use packs | [packs](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/packs.md) |
| Payments pack | [payments](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/payments.md) |
| Voice surface | [voice](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/voice.md) |

[`examples/harness_agent`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_agent)
runs the harness scenarios and
[`examples/harness_redteam`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_redteam)
the shell red-team scenarios.
