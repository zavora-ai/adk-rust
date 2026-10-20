# Spec 3.1: HarnessAgent — a governed, durable, team-forming harness for ADK-Rust

| | |
|---|---|
| **Status** | Draft / RFC |
| **Supersedes** | [`coding-agent.md`](coding-agent.md) (Draft, target 1.2.x) for everything except the `adk-devtools` toolset, which shipped |
| **Target** | `adk-rust` 3.1.0 (core, CLI, server, scaffold); 3.1.x for the gated packs in [§7.16–§7.18](#716-payments-pack-feature-payments) |
| **Grounding commit** | `origin/main` @ `a24ebd8d` — every `file:line` reference below is against this commit |
| **Affected crates** | NEW `adk-harness`; additive changes in `adk-core`, `adk-devtools`, `adk-sandbox`, `adk-cli`, `cargo-adk`, `adk-rust`; deprecation in `adk-agent` |
| **Stability** | `adk-harness` enters at **Beta** ([STABILITY.md](../../STABILITY.md)) |
| **Extended by** | [`spec-3.2-agent-kernel.md`](spec-3.2-agent-kernel.md): the always-on form of the harness; `kernel` becomes a default feature of `adk-harness` |
| **Surfaces** | [`spec-3.3-harness-ui.md`](spec-3.3-harness-ui.md): what each surface shows and how it answers (the console, web client, ACP, AG-UI and notifiers); it changes two statements here, recorded in its §10: a confirmation that times out parks the call when an approval store exists (§7.5), and the bundled web UI is authenticated and answers approvals (§8.2) |
| **Self-improvement** | [`spec-3.4-self-improving-harness.md`](spec-3.4-self-improving-harness.md): the opt-in maintenance loop; it widens the protected-path list of §9.2 I29 to the evaluator, CI, release and governance paths, keys workspace trust (§7.7) to file content, and corrects §7.13's claim that spend is keyed per subject |

---

## 1. Summary

A harness is the ready-made agent a developer runs first: file and shell tools, a planning loop, skills, approvals, resumable sessions. Every 2026 agent framework ships one. They are local coding assistants with a session file: approvals are per-command prompts, a crash re-runs tools, nothing survives the laptop, and nothing knows who the agent acts for.

ADK-Rust already has what those harnesses lack, spread across the workspace: one governed path for every tool call with default-deny policy, fingerprint-bound approvals and a replicated kill switch ([`adk-core/src/governance.rs`](../../adk-core/src/governance.rs)); durable runs that park on an approval and resume in another process under fenced leases ([`adk-core/src/run_store.rs`](../../adk-core/src/run_store.rs), [`adk-server/src/background/worker.rs`](../../adk-server/src/background/worker.rs)); an action ledger that answers a re-issued non-idempotent call from its record; a signed audit log; teams with budgets and discovery; graphs with checkpoints, interrupts and time travel; five realtime transports; payments under mandates; identity with delegation chains; and the same agent served over REST, A2A, ACP, Agent Engine and a managed runtime.

`adk-harness` is the integration point for all of it. At its centre is a thin `LlmAgent` loop (the design rule from `coding-agent.md` §6 D-3 still holds: configuration and policy over `LlmAgent`, not a new agent type). Around it, the platform's subsystems attach as governed layers. The result is a harness defined by ten capabilities ([§5](#5-headline-capabilities-and-demonstration-scenarios)) that no shipping harness offers together, each proven by a scenario in the example crate rather than claimed in prose.

Three product rules shape every decision below:

| Rule | Consequence |
|---|---|
| **One governed path.** Every mutation, in the harness or any member it delegates to, passes `authorize_tool_call` and the action ledger. | Modes and standing approvals are a `ToolPolicy`, never a callback; nothing in the harness answers a tool call before authorization. |
| **Durable by construction.** A run survives process loss and resumes at the exact tool call. | Mode, rules, todos, goals and approvals live in stores a resumed run reads; `bash` and `write_file` are `NonIdempotent`, so the ledger prevents a second execution. |
| **The harness knows who it acts for.** Approvals, credentials and audit are per subject. | Standing rules are keyed by subject and bound to the tool's declaration digest; credentials come from the broker, never the shell environment. |

## 2. Goals

- **G1.** `cargo install adk-cli && adk-rust` gives a working harness in the current directory within five minutes, on any model provider, with approvals.
- **G2.** The same `HarnessAgent` runs in the CLI, behind `adk-rust serve`, in an Agent Engine container, under the managed runtime and as a team member, with no rewrite.
- **G3.** Every one of the ten capabilities in §5 is demonstrated by a scenario that fails on a plain `LlmAgent` and passes on the harness, run live on at least one provider.
- **G4.** No unsandboxed, unapproved, unaudited path to a side effect exists, including inside delegates and goals ([§9](#9-security-invariants-and-audit-records)).
- **G5.** Standing approvals ("don't ask again") persist across sessions, are per user, and are invalidated when the tool's declaration changes.
- **G6.** A crash, restart or replica change never re-executes a completed tool call or loses a held approval.
- **G7.** Other harnesses are members, not competitors: Claude Code and Codex over ACP, remote agents over A2A.
- **G8.** Footprint stays modular: every pack is a Cargo feature; the default build is model + devtools + governance.

## 3. Non-goals

- **N1.** Not a new agent runtime, planner or orchestration model. `LlmAgent`, `TeamSpec`, `StateGraph`, `LoopAgent` and the Runner are reused as they are.
- **N2.** Not an IDE or chat UI. ACP serves IDEs; the web UI and chat channels are separate roadmap items.
- **N3.** Not a Windows sandbox. The AppContainer enforcer is unimplemented ([`adk-sandbox/src/sandbox/windows.rs:77-95`](../../adk-sandbox/src/sandbox/windows.rs)); on Windows the shell runs in the degraded host mode of [§7.9](#79-shell).
- **N4.** Not information-flow labels. Untrusted-content provenance is spec 3.2 material; this spec confines untrusted text to delimited blocks ([§7.7](#77-instructions-agentsmd)).
- **N5.** Not CodeAct. The CodeAct agent remains an optional execution backend ([`adk-agent/src/codeact/`](../../adk-agent/src/codeact)); the harness default is tool calling with a shell, as every shipping harness does.

## 4. Relationship to `coding-agent.md`

| `coding-agent.md` item | Shipped | This spec |
|---|---|---|
| `adk-devtools` file/search/shell toolset (§7.1) | Yes, with its own `Workspace` and a host `bash` (not through `adk-code`) | Reused; three additive changes ([§10](#10-changes-outside-the-new-crate) D1–D3) |
| `CodingAgent` builder (§7.2) | As `adk_agent::coding` — toolset, prompt, `write_todos` only | Becomes a preset over `HarnessAgent` ([§7.19](#719-codingagent-preset)); the old module is deprecated from 3.2.0 |
| `TodoTool` in session state (§7.2.1) | In process memory, shared by every session ([`adk-agent/src/coding/todo.rs:28-31`](../../adk-agent/src/coding/todo.rs)) | Session state ([§7.6](#76-todos)) |
| Permission modes as a callback hook (§7.2.3) | Not shipped | A `ToolPolicy` ([§7.4](#74-modes)); a callback would skip the governed path |
| `SpawnSubagentTool` (§7.3) | Not shipped | `TeamSpec` lowering with ACP and A2A members ([§7.10](#710-team-subagents-and-external-harnesses)) |
| Skills via `ContextCoordinator` (§7.5) | Not wired into any agent | `SkillToolset` progressive disclosure ([§7.8](#78-skills)) |
| `/goal` on `adk-managed` (§8) | A CLI loop with `.adk/goal.json` | A durable goal engine ([§7.11](#711-plan-mode-the-plan-graph-and-the-goal-engine)); the managed form in [§7.18](#718-managed-form-feature-managed) |
| "No unsandboxed path" (§9 D-4) | Not true: `bash` runs on the host | Stated as the Claude Code pattern: sandbox opt-in, host shell behind approval ([§7.9](#79-shell)) |

## 5. Headline capabilities and demonstration scenarios

Each capability names the machinery it runs on and the scenario that proves it. The scenarios are the contents of `examples/harness_agent` ([§11](#11-testing-and-live-verification)); the pass condition is asserted on files, processes, stores and audit records, never on model prose.

| # | Capability | Scenario | Pass condition | Machinery |
|---|---|---|---|---|
| H1 | **A run you can leave for days.** A held approval parks the run; a decision from another process, days later, resumes it at the exact tool call; no completed `bash` or write runs twice | `goal "migrate to edition 2024" --until "cargo test"`; kill the process mid-iteration; restart; approve from `adk-rust approvals` | Same session and iteration resume; the ledger shows each `bash` executed once; `cargo test` passes | `DurableRunStore`, fenced leases, `RunCheckpointRef::AwaitingApproval` ([`run_store.rs:137-148`](../../adk-core/src/run_store.rs)), `ActionLedger` on `NonIdempotent` tools, sessions on six backends |
| H2 | **Approvals bound to what the tool is.** "Always allow `cargo test`" survives sessions for this user, and dies when the tool's declaration changes | Grant a rule; redeclare the tool (MCP server update); run again | Second run prompts; a `harness.rule.stale` audit record exists; another user is prompted on the first run | `ToolOrigin.declaration_digest` ([`context.rs:1089-1119`](../../adk-core/src/context.rs)), `HarnessPolicy` ([§7.5](#75-standing-approvals)) |
| H3 | **It forms a team, including other people's harnesses.** Delegates to Codex or Claude Code over ACP, to a remote agent over A2A, to specialists discovered at runtime, under one budget and audit trail | "Fix the failing test; have the reviewer check the diff; summarize" with an ACP reviewer member | Each member runs in an isolated context; the team budget stops the run at its cap; one audit trail shows the delegation chain | `TeamSpec` ([`team.rs:59`](../../adk-agent/src/team.rs)), `TeamAgentRegistry` ([`team/discovery.rs:61-76`](../../adk-agent/src/team/discovery.rs)), `AcpAgentTool` ([`adk-acp/src/tool.rs:59`](../../adk-acp/src/tool.rs)), `RemoteA2aAgent` ([`remote_agent.rs:47`](../../adk-server/src/a2a/remote_agent.rs)) |
| H4 | **Plan mode produces an executable, resumable graph.** The approved plan compiles to checkpointed nodes with HITL gates; a failed step can be rewound or forked | Plan a three-step change; approve; step 3 fails; `rewind` to step 2 | Steps 1–2 are not re-executed after rewind; the plan graph is visible at `/debug/graph` | `StateGraph`, `GraphAgentBuilder::interrupt_before` ([`agent.rs:657`](../../adk-graph/src/agent.rs)), `Checkpointer` ([`checkpoint.rs:222`](../../adk-graph/src/checkpoint.rs)), `TimeTravelHandle::fork_at` ([`time_travel.rs:266`](../../adk-graph/src/time_travel.rs)) |
| H5 | **Talk to it.** Voice in and out, interruption, spoken approvals; same agent, same policy | `adk-rust --voice "what's failing in the build?"` | A spoken "yes" approves the held call; the transcript and the tool call are in the same audit trail | `IntegratedRealtimeRunner` ([`integration/mod.rs:153`](../../adk-realtime/src/integration/mod.rs)), `tool_governance(RunConfig)` ([`integration/builder.rs:283`](../../adk-realtime/src/integration/builder.rs)) — gated, [§7.17](#717-voice-surface-feature-voice-gated) |
| H6 | **It knows who it acts for.** Per-user rules, RBAC on tool classes, on-behalf-of tokens for `gh` and cloud APIs, SSO on the server, secrets from a vault | Two authenticated users on one served harness each run `gh pr create` | Each PR is authored by its user's token; rules do not cross users; the audit records carry each delegation chain | `ActingContext` ([`principal.rs:335`](../../adk-core/src/principal.rs)), `ProtectedTool`/`ScopeGuard`, `TokenExchangeBroker` ([`adk-auth/src/token_exchange.rs`](../../adk-auth/src/token_exchange.rs)), SSO extractors, secret providers, signed audit |
| H7 | **It watches itself.** Cost per user and agent, loop detection that parks a runaway run, eval baselines, live team topology | A subagent loops on the same tool call | The trace analyzer flags the loop, the budget trips, the run parks with a reason; cost per subject is reported | `SpendLedger` ([`spend.rs:376`](../../adk-core/src/spend.rs)), `RunBudget` ([`budget.rs:105`](../../adk-core/src/budget.rs)), `TraceAnalyzer` ([`trace_analyzer.rs:91`](../../adk-eval/src/trace_analyzer.rs)), `BaselineStore` ([`baseline.rs:62`](../../adk-eval/src/baseline.rs)), OTel GenAI attributes ([`semconv.rs`](../../adk-telemetry/src/semconv.rs)) |
| H8 | **It can spend money, safely.** A purchase under an AP2 mandate, within budget, with approval and a receipt | "Register the domain if it costs under $20" against a payments test harness | The payment is made once, after approval, within the mandate; the ledger shows the spend; a receipt is in the journal | `adk-payments` (`kernel`, `journal`, `guardrail`, `tools`; [`lib.rs:47-54`](../../adk-payments/src/lib.rs)) — gated, [§7.16](#716-payments-pack-feature-payments) |
| H9 | **One harness, every deployment.** CLI → served to a team with an approvals inbox → Agent Engine container → managed runtime | The same `HarnessAgent` value behind `Launcher`, `ServerBuilder`, `serve_agent_engine` and a `ManagedAgentDef` | The held-call → approve → resume round trip works on each surface | `Launcher`, `ServerBuilder::with_approval_service` ([`rest/mod.rs:718`](../../adk-server/src/rest/mod.rs)), `serve_agent_engine` ([`entrypoint.rs:268`](../../adk-server/src/agent_engine/entrypoint.rs)), `ManagedAgentDef` ([`agent_def.rs:36`](../../adk-managed/src/types/agent_def.rs)) |
| H10 | **Any model, any tools, any surface.** Local mistral.rs or any cloud model; MCP, OpenAPI, browser and governed computer use as packs; skills from a registry; graph memory | `--provider` swap; `--pack browser` | The same scenarios pass on three providers; browser tools appear under the same modes | Provider facade, `McpToolset`, `BrowserToolset` ([`toolset.rs:82`](../../adk-browser/src/toolset.rs)), `build_reference_graph_with_checkpointer` ([`adk-computer-use/src/graph.rs:203`](../../adk-computer-use/src/graph.rs)), `adk-skill` registry, `adk-memory` |

H1, H2, H5, H8 and H9 have no equivalent in any shipping harness. H3, H4, H6 and H7 exist in pieces elsewhere, never together under one governance path.

## 6. Architecture

```text
┌─────────────────────────────────────────────────────────────────────────────────┐
│ SURFACES   adk-rust REPL · adk-rust serve (REST/SSE, approvals inbox) · Agent     │ §8
│            Engine BYOC · managed runtime · voice (realtime) · ACP (IDE) · A2A      │
├─────────────────────────────────────────────────────────────────────────────────┤
│ HARNESS (adk-harness)                                                            │ §7
│   HarnessAgent = LlmAgent + HarnessToolset + instruction provider                 │
│   HarnessPolicy (modes · standing rules · tool classes)   ← one governed path     │
│   Team (TeamSpec: members, ACP/A2A members, discovery)  · Plan graph · Goal engine │
│   Shell (host | OS sandbox | container) · Skills · Todos · File memory             │
│   Packs: browser · computer use · payments · voice · managed form                  │
├─────────────────────────────────────────────────────────────────────────────────┤
│ GOVERNANCE (adk-core)  kill switch → policy → guardrails → confirmation → ledger    │
│   ApprovalStore / ApprovalService · ActingContext · SpendLedger · RunBudget · audit │
├─────────────────────────────────────────────────────────────────────────────────┤
│ DURABILITY  DurableRunStore · leases · parked approvals · sessions (6 backends)     │
├─────────────────────────────────────────────────────────────────────────────────┤
│ SUBSTRATE   adk-devtools · adk-sandbox · adk-code · adk-skill · adk-memory ·        │
│             adk-graph · adk-agent teams · adk-realtime · adk-payments · adk-auth    │
├─────────────────────────────────────────────────────────────────────────────────┤
│ MODELS      adk-model (Gemini, OpenAI, Anthropic, Ollama, mistral.rs, …)           │
└─────────────────────────────────────────────────────────────────────────────────┘
```

**Decisions**

| | Decision | Grounding |
|---|---|---|
| **D-1** | The harness is configuration and policy over `LlmAgent`; `HarnessAgent` wraps it and implements `Agent`. | `coding-agent.md` D-3; [`llm_agent.rs:932-1626`](../../adk-agent/src/llm_agent.rs) |
| **D-2** | Modes and standing rules are a `ToolPolicy`. A callback or plugin that answers a call skips policy, guardrails and confirmation. | The governed path runs only when nothing answered the call: [`llm_agent.rs:2673`](../../adk-agent/src/llm_agent.rs) |
| **D-3** | Mode, rules, goals and the shell ledger live in harness-owned stores; session state holds display mirrors only. | Any tool, client `stateDelta`, AG-UI state or delegate merge can write session keys: [`tool.rs:347-349`](../../adk-core/src/tool.rs), [`runtime.rs:532-540`](../../adk-server/src/rest/controllers/runtime.rs), [`agent_tool.rs:1040-1049`](../../adk-tool/src/agent_tool.rs) |
| **D-4** | Policy and approvals reach every runtime through `Agent::configure_run`. | Called per run, on transfer and for delegates: [`runner.rs:1080, 1772`](../../adk-runner/src/runner.rs), [`agent_tool.rs:867, 1331`](../../adk-tool/src/agent_tool.rs) |
| **D-5** | Subagents are a `TeamSpec`, not hand-built `AgentTool`s. | Budgets, termination, approval, circuit breaker and discovery already exist on teams: [`team.rs:176-217, 328-403`](../../adk-agent/src/team.rs) |
| **D-6** | The shell follows the Claude Code pattern: sandbox opt-in, host shell behind per-call approval, explicit `fail_if_unavailable`. | Decision taken in review after comparing Claude Code, Codex, Gemini CLI and the library-only harnesses |
| **D-7** | Plan mode compiles to a graph when the plan has steps; the goal loop is pluggable (`LoopAgent` or checkpointed `StateGraph`). | [`agent.rs:657-663`](../../adk-graph/src/agent.rs), [`loop_agent.rs:654`](../../adk-agent/src/workflow/loop_agent.rs) |
| **D-8** | Packs (browser, computer use, payments, voice, managed) are features; each adds tools or a surface under the same policy. | G8 |

## 7. Component specifications

### 7.1 Crate layout and features

```text
adk-harness/src/
  lib.rs             re-exports, feature gates
  builder.rs         HarnessAgentBuilder
  agent.rs           HarnessAgent (impl Agent; configure_run), HarnessRuntime, HarnessInfo
  harness.rs         Harness (owns a Runner)                                   [runner]
  mode.rs            HarnessMode, ModeRecord, ModeStore (+ in-memory, file, sqlite)
  policy.rs          HarnessPolicy (ToolPolicy), ToolKind registry, decision table
  approvals/         StandingRule, StandingRuleStore (+ impls), matcher, ApprovalPrompter,
                     PromptingConfirmationHandler
  toolset.rs         HarnessToolset (tools(), per-request reminders)
  tools/{todos,plan,shell}.rs
  shell/{mod,os_sandbox,container}.rs                                          [sandbox]/[docker]
  instructions.rs    ProjectInstructions → InstructionProvider (AGENTS.md, trust, hashes)
  skills.rs          SkillToolset wiring                                        [skills]
  team.rs            MemberSpec → TeamSpec; ACP and A2A members                 [team]
  plan_graph.rs      plan → StateGraph compiler; GoalEngine                     [graph]
  goal.rs            GoalSpec/GoalState, UntilCheckAgent
  observe.rs         span attributes, spend keys, loop guard                    [observe]
  identity.rs        RBAC mapping, credential broker wiring                     [auth]
  memory.rs          FileMemory + tools                                         [memory]
  packs/{browser,computer_use,payments,voice,managed}.rs                        [feature each]
  presets/coding.rs  CodingAgent preset
  prompt.rs          HARNESS_SYSTEM_PROMPT
```

| Feature | Default | Pulls in |
|---|---|---|
| `runner` | on | `adk-runner`, `adk-session` (the `Harness` wrapper) |
| `skills` | on | `adk-skill/progressive-disclosure` |
| `skills-registry` | off | `adk-skill/vertex-skill-registry` |
| `team` | on | `adk-tool` (`AgentTool`), `adk-agent/team-tools` |
| `team-acp` | off | `adk-acp` (Claude Code, Codex, Kiro as members) |
| `team-a2a` | off | `adk-server/a2a` client (`RemoteA2aAgent`) |
| `graph` | on | `adk-graph` (plan graph, goal engine, checkpointers via `graph-sqlite` when `sqlite`) |
| `sandbox` | on | `adk-sandbox/sandbox-native` (no extra dependencies on unix) |
| `docker` | off | `adk-sandbox/workspace-docker` |
| `observe` | on | `adk-telemetry`, `adk-eval` (trace analyzer, baselines) |
| `auth` | on | `adk-auth` (RBAC wrappers, credential broker, signed audit) |
| `memory` | on | file memory; `memory-graph` adds `adk-memory` graph tools |
| `sqlite` | off | SQLite backends for the mode, rule and shell-ledger stores |
| `browser`, `computer-use`, `payments`, `voice`, `managed` | off | the packs of §7.15–§7.18 |

The umbrella `adk-rust` gets `harness = ["dep:adk-harness"]`, included in `full` ([§8.4](#84-umbrella-adk-rust)).

### 7.2 `HarnessAgentBuilder`

Type sketch; signatures are normative, bodies are not.

```rust
pub struct HarnessAgentBuilder { /* private */ }

impl HarnessAgentBuilder {
    pub fn new(name: impl Into<String>) -> Self;                         // "harness"
    pub fn model(self, model: Arc<dyn Llm>) -> Self;                      // required
    pub fn workspace(self, ws: adk_devtools::Workspace) -> Self;          // required
    pub fn description(self, text: impl Into<String>) -> Self;
    pub fn base_prompt(self, text: impl Into<Cow<'static, str>>) -> Self; // HARNESS_SYSTEM_PROMPT
    pub fn instruction(self, text: impl Into<String>) -> Self;            // operator text
    pub fn project_instructions(self, cfg: InstructionsConfig) -> Self;   // §7.7
    pub fn mode(self, mode: HarnessMode) -> Self;                         // Ask
    pub fn mode_store(self, store: Arc<dyn ModeStore>) -> Self;           // in-memory
    pub fn standing_rules(self, store: Arc<dyn StandingRuleStore>) -> Self; // in-memory
    pub fn operator_policy(self, policy: Arc<dyn ToolPolicy>) -> Self;    // composed with stricter()
    pub fn prompter(self, p: Arc<dyn ApprovalPrompter>) -> Self;          // interactive surfaces
    pub fn confirmation_handler(self, h: Arc<dyn ToolConfirmationHandler>) -> Self; // raw override
    pub fn approval_store(self, s: Arc<dyn ApprovalStore>) -> Self;       // in-memory
    pub fn approval_service(self, s: Arc<dyn ApprovalService>) -> Self;
    pub fn confirmation_timeout(self, t: Duration) -> Self;               // 300 s
    pub fn shell(self, isolation: ShellIsolation) -> Self;                // Host (§7.9)
    pub fn fail_if_unavailable(self) -> Self;
    pub fn auto_allow_sandboxed(self) -> Self;
    pub fn allow_unsandboxed_retry(self, yes: bool) -> Self;
    pub fn allow_host_shell_in_auto(self) -> Self;
    pub fn without_shell(self) -> Self;
    pub fn trust_mcp_read_only_hints(self, yes: bool) -> Self;            // false
    pub fn without_todos(self) -> Self;
    pub fn skills(self, cfg: SkillsConfig) -> Self;                       // §7.8
    pub fn member(self, spec: MemberSpec) -> Self;                        // §7.10
    pub fn member_registry(self, r: Arc<dyn TeamAgentRegistry>) -> Self;  // runtime discovery
    pub fn team_policy(self, p: TeamPolicy) -> Self;
    pub fn without_default_members(self) -> Self;
    pub fn member_model(self, m: Arc<dyn Llm>) -> Self;                   // model tiering
    pub fn goal_engine(self, e: GoalEngine) -> Self;                      // LoopAgent | Graph (§7.11)
    pub fn plan_graph(self, yes: bool) -> Self;                           // true when `graph`
    pub fn memory(self, cfg: MemoryConfig) -> Self;                       // §7.15
    pub fn budget(self, b: RunBudget) -> Self;
    pub fn spend_ledger(self, l: Arc<dyn SpendLedger>) -> Self;           // §7.13
    pub fn loop_guard(self, g: LoopGuard) -> Self;                        // §7.13
    pub fn credentials(self, b: Arc<dyn CredentialBroker>) -> Self;       // §7.14
    pub fn rbac(self, map: RbacMap) -> Self;                              // §7.14
    pub fn pack(self, p: Pack) -> Self;                                   // §7.16–§7.18
    pub fn tool(self, t: Arc<dyn Tool>) -> Self;
    pub fn toolset(self, t: Arc<dyn Toolset>) -> Self;
    pub fn max_iterations(self, n: u32) -> Self;                          // 100
    pub fn tool_timeout(self, t: Duration) -> Self;                       // 300 s
    pub fn history_max_events(self, n: usize) -> Self;
    pub fn compaction(self, c: EventsCompactionConfig) -> Self;           // interval 10, overlap 4
    pub fn without_compaction(self) -> Self;
    pub fn customize_llm(self, f: impl FnOnce(LlmAgentBuilder) -> LlmAgentBuilder + Send + 'static) -> Self;
    pub fn build(self) -> Result<HarnessAgent>;
}
```

The builder exposes **no** `before_tool_callback`, `enhanced_plugin` or `require_tool_confirmation*`. The first two can answer a call before authorization ([`llm_agent.rs:2613-2651, 2673`](../../adk-agent/src/llm_agent.rs)); the third forces a confirmation even when the policy allows ([`governance.rs:1621`](../../adk-core/src/governance.rs)), which defeats standing rules. The one confirmation policy the harness sets is `ToolConfirmationPolicy::PerTool` for the non-waivable tools (`exit_plan_mode`, unsandboxed retry, `memory_write`, payments), as defence in depth. `customize_llm` is the escape hatch and its rustdoc states both caveats.

`build()` composes existing APIs in this order:

1. Resolve the shell runtime (probe the enforcer; error if `fail_if_unavailable`).
2. Load instructions ([§7.7](#77-instructions-agentsmd)) and the tool-class registry.
3. `LlmAgentBuilder::new(name).model(..).description(..).instruction_provider(..)` ([`llm_agent.rs:1002`](../../adk-agent/src/llm_agent.rs)) `.toolset(HarnessToolset)` (1270) `.max_iterations` (1204) `.tool_timeout` (1217) `.parallelize_agent_delegations(true)` (1451); add `SkillToolset` ([§7.8](#78-skills)), extra tools and toolsets; `customize_llm`; `build()` (1626).
4. Lower members to a `TeamSpec` and compile it with the coordinator ([§7.10](#710-team-subagents-and-external-harnesses)); the compiled root is the agent `HarnessAgent` wraps.
5. Construct `HarnessPolicy`, the `PromptingConfirmationHandler` (if a prompter is set), the spend and loop guards, and the `HarnessRuntime`.

### 7.3 `HarnessAgent`, `configure_run` and `Harness`

```rust
pub struct HarnessAgent { root: Arc<dyn Agent>, runtime: HarnessRuntime, info: HarnessInfo }

impl Agent for HarnessAgent {
    // name, description, sub_agents, run, topology, transfer_targets_for delegate to `root`
    fn configure_run(&self, agent_name: &str, config: &mut RunConfig) {
        self.root.configure_run(agent_name, config);   // the compiled team installs member tools
        self.runtime.layer().apply(config);             // policy, approvals, timeout, budget, hooks
    }
}

impl HarnessAgent {
    pub fn agent(&self) -> Arc<dyn Agent>;
    pub fn runtime(&self) -> &HarnessRuntime;
    pub fn info(&self) -> &HarnessInfo;        // effective isolation, degraded reason, members, packs
    pub fn run_config(&self) -> RunConfig;      // for callers wiring their own Runner
    pub fn configure_runner<A, G, S>(&self, b: RunnerConfigBuilder<A, G, S>) -> RunnerConfigBuilder<A, G, S>;
    #[cfg(feature = "runner")]
    pub fn into_harness(self, app_name: impl Into<String>, sessions: Arc<dyn SessionService>) -> Result<Harness>;
}
```

`configure_run` is the mechanism that carries policy and approvals to every runtime without changing them (D-4). The rules of the layer it applies:

| Rule | Why |
|---|---|
| Compose, never replace, `config.tool_policy`: wrap it with `StrictestPolicy` ([§10](#10-changes-outside-the-new-crate) C2) | The runner installs the operator's builder policy before calling the hook ([`runner.rs:913-921`](../../adk-runner/src/runner.rs)); replacing it would drop it silently |
| Idempotent: detect the layer by `policy_ref()` and do not nest twice | The hook runs again on transfer and for delegates |
| Fill `approval_service`/`approval_store` only when empty or identical; otherwise warn and keep the run's own | [`governance.rs:1934-1938`](../../adk-core/src/governance.rs) reads `config.approval_store` |
| Strip `tool_approvals` entries that name non-waivable tools | `tool_approvals` is valid for the whole run and not consumed ([`context.rs:1477-1486`](../../adk-core/src/context.rs)) |
| Never touch run-owned fields: `durable_run_id`, the handler, call-ID decisions | Set by the worker and the surface ([`worker.rs:815`](../../adk-server/src/background/worker.rs)) |

`HarnessAgent::run` checks that its `policy_ref` is present in the run's `RunConfig` and refuses to run otherwise: the hook is called on the root agent only ([`runner.rs:1080`](../../adk-runner/src/runner.rs)), so a harness nested under another agent would otherwise run ungoverned.

`Harness` owns a `Runner` for callers who want a complete object. `Runner::run` never creates a session ([`runner.rs:790`](../../adk-runner/src/runner.rs)), hence `open_session`.

```rust
pub struct Harness { /* runner, goal runner, agent, sessions, app_name */ }
impl Harness {
    pub async fn open_session(&self, user: &UserId, session: Option<&SessionId>) -> Result<SessionId>;
    pub async fn send(&self, user: &UserId, session: &SessionId, text: impl Into<String>) -> Result<EventStream>;
    pub async fn resume(&self, user: &UserId, session: &SessionId) -> Result<EventStream>;   // empty turn resumes held calls
    pub async fn mode(&self, user: &UserId, session: &SessionId) -> Result<HarnessMode>;
    pub async fn set_mode(&self, user: &UserId, session: &SessionId, mode: HarnessMode) -> Result<()>;
    pub async fn todos(&self, user: &UserId, session: &SessionId) -> Result<TodoList>;
    pub async fn pending_approvals(&self, user: &UserId, session: &SessionId) -> Result<Vec<PendingApproval>>;
    pub async fn decide(&self, user: &UserId, session: &SessionId, fingerprint: &str, d: HarnessDecision) -> Result<()>;
    pub async fn start_goal(&self, user: &UserId, session: &SessionId, spec: GoalSpec) -> Result<EventStream>;
    pub async fn goal(&self, user: &UserId, session: &SessionId) -> Result<Option<GoalState>>;
    pub fn goal_target(&self) -> (String, Arc<Runner>);      // DurableWorker::with_agent
    pub fn standing_rules(&self) -> &Arc<dyn StandingRuleStore>;
    pub fn interrupt(&self, session: &SessionId) -> bool;
    pub fn runner(&self) -> &Runner;
    pub async fn close(&self);
}
```

### 7.4 Modes

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessMode { ReadOnly, Plan, #[default] Ask, Auto }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeRecord {
    pub mode: HarnessMode,
    pub revision: u64,
    pub set_by: ModeSource,              // Default | User | PlanApproval | Operator | SystemTighten
    pub at: DateTime<Utc>,
    pub effective_from: Option<String>,  // invocation id, when loosening
}

#[async_trait]
pub trait ModeStore: Send + Sync {
    async fn get(&self, app: &str, user: &str, session: &str) -> Result<Option<ModeRecord>>;
    async fn set(&self, app: &str, user: &str, session: &str, record: ModeRecord) -> Result<()>;
}
pub const MODE_STATE_KEY: &str = "harness:mode";   // display mirror; never read by the policy
```

- `HarnessPolicy::evaluate` reads the store by the (`app_name`, `user_id`, `session_id`) on `ToolPolicyRequest` ([`policy.rs:93-114`](../../adk-core/src/policy.rs)). An unparseable record is `ReadOnly`. Members share the parent's (app, user, session) ([`agent_tool.rs:967-973`](../../adk-tool/src/agent_tool.rs)), so they inherit the mode.
- **Tightening** applies at the next evaluation. **Loosening** is possible only through `Harness::set_mode` (outside a run; it appends a "cancelled by mode change" response to every held call in the session before writing, since a non-pending response answers a hold, [`governance.rs:2047-2056`](../../adk-core/src/governance.rs)) or an approved `exit_plan_mode`. Plan denies rather than holds, so an approved plan exit cancels nothing. Any other loosening is refused and audited. The reason: on every new run `LlmAgent` re-dispatches held calls before the first model call ([`llm_agent.rs:3403-3430`](../../adk-agent/src/llm_agent.rs)); a loosened mode must never execute a call a human has not seen under it.
- `exit_plan_mode { plan: String, next_mode?: "ask" | "auto", steps?: [PlanStep] }` is always declared (stable tool surface, prompt cache) and denied outside Plan. In Plan it is `RequireApproval` and non-waivable; the request carries the plan; the approver may edit `next_mode` (edited arguments are re-checked, [`governance.rs:1756-1804`](../../adk-core/src/governance.rs)); on approval the tool writes `ModeStore::set` (set by `PlanApproval`), mirrors `harness:plan` into state and, when `steps` are present and `plan_graph` is on, compiles the plan graph ([§7.11](#711-plan-mode-the-plan-graph-and-the-goal-engine)). Auto is never a default target.

**Tool classes.** A call's class comes from (`origin.source`, name) in the harness registry, never from `ToolEffect` or MCP hints alone: an MCP server's `readOnlyHint` becomes `ToolEffect::ReadOnly` ([`adk-tool/src/mcp/toolset.rs:404-410`](../../adk-tool/src/mcp/toolset.rs)) and `allow_read_only()` would trust it. A call is a harness class only if the agent is one of the harness's own agents, the name is registered, and the origin is `STATIC_TOOL_ORIGIN` ([`tool.rs:178`](../../adk-core/src/tool.rs)).

```rust
pub enum ToolKind {
    Read,            // read_file, glob, grep, list_skills, load_skill, load_skill_resource, search_skills, memory_read
    Todo,            // write_todos, update_todo
    PlanExit,        // exit_plan_mode
    Edit,            // write_file, edit_file (protected-path variant, §9)
    Shell(Isolation),// bash: OsSandbox | Container | Host
    ShellEscalation, // bash with unsandboxed: true
    Delegate,        // team member tools
    MemoryWrite,     // memory_write, memory_delete
    Payment,         // payments pack tools (never waivable)
    External { effect: ToolEffect, trusted_origin: bool },   // MCP, browser, extras, skill-activated
}
```

**Decision function** (the harness `ToolPolicy`, composed `stricter(operator, harness)` by `configure_run`):

```text
decide(call):
  1  hard_deny(call)                        -> Deny    // provider shell builtins; path escape; unsandboxed exec
                                                        // without a valid retry_of; class not in registry for mode
  2  m    = ModeStore.get(app, user, session) or default; unparseable -> ReadOnly
  3  base = MODE_TABLE[m][class(origin.source, name, canonical args)]
  4  if base == Deny -> Deny
  5  r = StandingRules.match(subject, app, origin.source, origin.declaration_digest, name, args)
  6  if r == Deny -> Deny
  7  if non_waivable(call) -> RequireApproval          // rules and tool_approvals never apply
  8  if r == Allow(id) && base == RequireApproval -> Allow (rule_ref = id)
  9  -> base
```

| Class | ReadOnly | Plan | Ask (default) | Auto |
|---|---|---|---|---|
| Read, Todo, Delegate | Allow | Allow | Allow | Allow |
| PlanExit | Deny | RA (NW) | Deny | Deny |
| Edit, non-protected path | Deny | Deny | RA | Allow |
| Edit, protected path | Deny | Deny | RA (NW) | RA (NW) |
| Shell, OS sandbox | Deny | Allow with read-only workspace mount, no network (active enforcer only) | RA; Allow if `auto_allow_sandboxed` (needs S3) | Allow (needs S3) |
| Shell, container | Deny | Deny | RA; Allow if `auto_allow_sandboxed` | Allow |
| Shell, host (incl. degraded) | Deny | Deny | RA | RA; Allow only with `allow_host_shell_in_auto` |
| ShellEscalation | Deny | Deny | RA (NW) | RA (NW) |
| MemoryWrite | Deny | Deny | RA (NW) | RA (NW) |
| Payment | Deny | Deny | RA (NW) | RA (NW) |
| External, ReadOnly, trusted origin | Allow | Allow | Allow | Allow |
| External, other | Deny | Deny | RA | RA unless operator-allowlisted |

RA = `RequireApproval`; NW = non-waivable (no standing rule or `tool_approvals` entry applies). Calls made inside a member are governed by the same policy and mode because the policy travels with the delegated `RunConfig` ([`agent_tool.rs:194, 230`](../../adk-tool/src/agent_tool.rs)).

### 7.5 Standing approvals

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandingRule {
    pub id: String,
    pub scope: RuleScope,                // User { app_name, user_id, subject_kind } | Session { .., session_id }
    pub tool: String,                    // glob
    pub origin: OriginPin,               // Exact { source, declaration_digest } | AnyDeclaration { source } (operator-only)
    pub agent: Option<String>,           // AgentId glob
    pub predicates: Vec<RulePredicate>,  // Arg(ArgSpec) | ShellPrefix { pointer, tokens } | PathWithin { pointer, dir }
    pub decision: StandingDecision,      // Allow | Deny { reason }
    pub created_by: SubjectRef,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub label: String,
    pub source_fingerprint: Option<String>,
}

#[async_trait]
pub trait StandingRuleStore: fmt::Debug + Send + Sync {
    async fn rules_for(&self, app: &str, user: &str, session: &str) -> Result<Vec<StandingRule>>;
    async fn grant(&self, rule: StandingRule) -> Result<StandingRule>;
    async fn revoke(&self, app: &str, id: &str) -> Result<bool>;
    async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>>;
}
// impls: InMemory; File (~/.adk/approvals/<app>/<sha256(user)>.json, atomic rename, 0600); Sqlite [sqlite]
```

| Matching rule | Grounding |
|---|---|
| Subject: `rule.subject == request.user_id == acting.on_behalf_of`, same `subject_kind`; a human's rule never applies to `Service` runs | The fingerprint has no subject ([`context.rs:1289-1311`](../../adk-core/src/context.rs)), so the rule carries it |
| Origin: `source` and `declaration_digest` must equal the pin; a mismatch is no match plus a `harness.rule.stale` record | Digest = sha256(canonical `{declaration, annotations}`) ([`context.rs:1101-1119`](../../adk-core/src/context.rs)) |
| `ShellPrefix` matches only a command that tokenizes to plain words with no `; & \| \` $( ${ > < ( )` or newline and no leading assignment, whose first N tokens equal the rule's; suggestions never start with `sh`, `bash`, `env`, `xargs`, `eval`, `exec`, `sudo`, `nohup` | `ArgPredicate::StartsWith` is lexical and warns about `..` ([`policy.rs:284-292`](../../adk-core/src/policy.rs)) |
| `PathWithin` normalizes against the workspace root and rejects `..`; never matches a protected path | [§9](#9-security-invariants-and-audit-records) I11 |
| Deny beats Allow; `PlanExit`, `Payment` and `MemoryWrite` are never eligible; `ShellEscalation` only matches a rule pinning `/unsandboxed == true` | Non-waivable classes |
| A bash rule granted under a sandbox never matches host bash | The bash declaration's annotations include `{enforcer, profileDigest, network}`, so the digests differ |

Rules are evaluated in `HarnessPolicy`, not in the confirmation handler: the policy runs on every path before a call can be held ([`governance.rs:1578-1667`](../../adk-core/src/governance.rs)), while the handler is consulted only on live surfaces (1940-1942) and server or durable runs park the call as `Pending`. This needs the policy to see the call's origin, which is core change C1 ([§10](#10-changes-outside-the-new-crate)). If C1 is rejected, the fallback is a handler re-check against `request.origin` ([`context.rs:1176`](../../adk-core/src/context.rs)) plus an `ApprovalNotifier` that auto-decides matching held requests, at the cost of a park-and-wake per call.

A rule is created only by an authenticated human action: `PromptAnswer::ApproveAndRemember` on a prompt the human answered, `Harness::decide(.., HarnessDecision::ApproveAndRemember(..))`, `adk-rust rules add`, or the authenticated REST API. Suggestions offered at a prompt: for bash, `ShellPrefix` on the first one or two tokens (User scope), flagged when the command runs scripts (`cargo test`, `npm test`, `make`); for edits, `PathWithin` the root (Session scope) or the file's directory; for externals, any arguments for this tool pinned to its exact origin.

```rust
#[async_trait]
pub trait ApprovalPrompter: fmt::Debug + Send + Sync {
    async fn prompt(&self, p: ApprovalPrompt) -> Result<PromptAnswer>;
}
pub struct ApprovalPrompt {
    pub request: ToolConfirmationRequest, pub kind: ToolKind, pub mode: HarnessMode,
    pub isolation: IsolationStatus, pub suggestions: Vec<RuleSuggestion>, pub rendered: RenderedCall,
}
pub enum PromptAnswer {
    ApproveOnce, ApproveWithArgs(Value), ApproveAndRemember(RuleSuggestion),
    Deny { message: Option<String> }, DenyAndRemember(RuleSuggestion), DenyAllRemaining,
}
pub struct PromptingConfirmationHandler { /* prompter, standing, policy, serial mutex, per-turn cap (20) */ }
impl ToolConfirmationHandler for PromptingConfirmationHandler { /* decide; decide_approval (core change C3) */ }
```

Prompts are serialized because calls in a batch are decided concurrently ([`context.rs:1320-1326`](../../adk-core/src/context.rs)), capped per turn, and a rule granted for an earlier call in the batch is re-checked before prompting again. `RenderedCall` escapes control characters and marks truncation; a truncated payload cannot be approved unexpanded.

### 7.6 Todos

Todos are display state, so session state is the right place: `TODOS_STATE_KEY = "harness:todos"` (session tier). `TodoList { items: Vec<TodoItem { id, content, active_form?, status: Pending | InProgress | Completed | Cancelled }>, revision }`. Tools `write_todos` (replaces the list) and `update_todo { id, status }` have effect `Idempotent` (retry-safe, not ledgered; [`tool.rs:480-484`](../../adk-core/src/tool.rs)), at most 50 items of 500 characters. They read through `ctx.session().map(|s| s.state().get(..))` ([`tool.rs:366`](../../adk-core/src/tool.rs)) and write through `actions().state_delta` + `set_actions()` (347-349; the pattern in [`exit_loop.rs:27-33`](../../adk-tool/src/builtin/exit_loop.rs)). UIs read `event.actions.state_delta["harness:todos"]`; `Harness::todos()` reads the session. `HarnessToolset::process_llm_request` appends a short reminder of open items at the end of the request only when they changed, keeping the cached prefix stable ([`llm_agent.rs:3601-3618`](../../adk-agent/src/llm_agent.rs)). Member todos never merge back (`state_write_keys` empty, [§7.10](#710-team-subagents-and-external-harnesses)).

### 7.7 Instructions (`AGENTS.md`)

Today `AGENTS.md` reaches a model only as a lexically selected "skill": `adk-skill` discovers convention files anywhere in the tree ([`discovery.rs:5-6, 82`](../../adk-skill/src/discovery.rs)), parses them leniently ([`parser.rs:101-175`](../../adk-skill/src/parser.rs)) and prepends the top match, cut to 2,000 characters, to the user turn ([`llm_agent.rs:437-444`](../../adk-agent/src/llm_agent.rs)). The harness loads instruction files verbatim.

```rust
pub struct InstructionsConfig {
    pub project_files: Vec<String>,   // ["AGENTS.md"]; fallbacks ["AGENT.md", "CLAUDE.md"] only if none found
    pub user_file: Option<PathBuf>,   // ~/.adk/AGENTS.md
    pub walk_up_to_git_root: bool,    // true: load from the git root down to the workspace root, outermost first
    pub nested_index: bool,           // true: list (never inline) AGENTS.md below the root, depth ≤ 4
    pub per_file_max_bytes: usize,    // 32 KiB
    pub total_max_bytes: usize,       // 64 KiB
    pub require_trust: bool,          // true: first use of a workspace needs a trust decision
}
```

Assembly, as one `system` content through `instruction_provider` ([`callbacks.rs:105`](../../adk-core/src/callbacks.rs)), which is **not** template-expanded ([`llm_agent.rs:408-412`](../../adk-agent/src/llm_agent.rs); plain `instruction()` text is, 413-414, so an `AGENTS.md` containing `{user:x}` would otherwise pull state into the prompt):

1. Base prompt; operator instruction.
2. `<environment>`: root, platform, date, git branch from `.git/HEAD`, isolation and network, default mode, active packs.
3. User-global `AGENTS.md`.
4. Project `AGENTS.md` files, outermost first, each inside `<project-instructions path="…" sha256="…">` and delimited as untrusted.
5. Nested-file index.
6. `<memory>` index ([§7.15](#715-file-based-memory-feature-memory)).

Files are deduplicated by canonical path and hash (`CLAUDE.md` is often a symlink to `AGENTS.md`); a truncated file ends with `[truncated — read_file <path> for the rest]`. The prompt states that instructions cannot grant permissions; the policy enforces that in code. Dynamic parts (mode banner, todos, goal status) are not in the instruction, which is built once per run ([`llm_agent.rs:3361`](../../adk-agent/src/llm_agent.rs)); they come from `Toolset::process_llm_request`, which runs for every request (3615).

**Trust.** The first use of a workspace lists the instruction files found (path, size, hash) and asks to trust them; the decision is recorded per workspace-root digest (`harness.workspace.trust`); `--trust` skips the prompt in CI. Every load is audited with hashes (`harness.instructions.loaded`).

The harness never calls `with_skills*` or `Runner::with_skill_injector`, and convention documents are filtered out of the skill index ([§7.8](#78-skills)), so nothing is injected twice.

### 7.8 Skills

```rust
pub enum SkillsMode { Progressive /* default */, Inject /* lexical, opt-in */, Off }
pub struct SkillsConfig {
    pub mode: SkillsMode,
    pub extra_roots: Vec<PathBuf>,                 // default: <ws>/.adk/skills, ~/.adk/skills
    pub pool: Vec<Arc<dyn Tool>>,                  // tools a skill's `allowed-tools` may activate
    pub toolset: adk_skill::SkillToolsetConfig,    // progressive.rs:239
    #[cfg(feature = "skills-registry")] pub registry: Option<Arc<SkillRegistryClient>>,
}
```

The index comes from `load_skill_index_with_extras(root, extra_roots)` ([`index.rs:94`](../../adk-skill/src/index.rs)): `.skills/` and `.claude/skills/` are built in ([`discovery.rs:73`](../../adk-skill/src/discovery.rs)), project-local wins on a name clash, and documents tagged `convention` ([`parser.rs:127-151`](../../adk-skill/src/parser.rs)) are dropped before the index is rebuilt with `SkillIndex::new` ([`model.rs:239`](../../adk-skill/src/model.rs)). `SkillToolset::new(index, registry, cfg)` ([`progressive.rs:372`](../../adk-skill/src/progressive.rs)) exposes `list_skills`, `load_skill` and `load_skill_resource` (plus `search_skills` with the Vertex registry, 407); activation lives under `skill:active:<agent>` (417); tools a skill activates appear on the next resolution (632-662) and are governed as `External`. Progressive disclosure is the default because lexical injection rewrites the user turn, is lossy and breaks prompt caching.

### 7.9 Shell

The shell follows the pattern Claude Code uses: the OS sandbox is **off by default**, host commands run behind per-command confirmation, the sandbox is opt-in, and a strict deployment can fail at startup when it is unavailable. Codex's alternative (sandbox on, an explicit dangerous flag to bypass) and the library-only harnesses' alternative (no shell without a sandbox backend) were considered and rejected for the default experience; both remain reachable through the builder.

```rust
pub enum ShellIsolation {
    Host,                                                 // default: devtools bash, governed per call
    #[cfg(feature = "sandbox")] OsSandbox(OsSandboxConfig),
    #[cfg(feature = "docker")]  Container(ContainerConfig),
}
pub struct OsSandboxConfig {
    pub network: bool,                 // false — adk-sandbox cannot filter domains (sandbox/mod.rs:110-114)
    pub allow_process_spawn: bool,     // true — `sh -c` forks
    pub extra_read: Vec<PathBuf>, pub extra_write: Vec<PathBuf>,
    pub read_toolchains: bool,         // true — read-only bind of PATH dirs, ~/.cargo ~/.rustup ~/.nvm ~/.pyenv
                                       //        /opt/homebrew /usr/local; Linux also /usr /bin /lib /lib64 /etc
    pub enforcer: Option<Arc<dyn SandboxEnforcer>>,   // default get_enforcer() (sandbox/mod.rs:370)
}
pub struct ShellConfig {
    pub isolation: ShellIsolation, pub fail_if_unavailable: bool, pub auto_allow_sandboxed: bool,
    pub allow_unsandboxed_retry: bool /* true for Host/OS, false for Container */, pub allow_host_shell_in_auto: bool,
}
pub enum IsolationStatus { Host, OsSandbox { enforcer: String }, Container { image: String },
                           Degraded { requested: String, reason: String } }   // behaves as Host
```

`HarnessBashTool` (name `bash`) owns its declaration `{ command, timeout_secs?, unsandboxed?: bool, reason?: string, retry_of?: string }`; `unsandboxed`, `reason` and `retry_of` are declared only when a sandbox is active. Its `annotations()` include `{enforcer, profileDigest, network}`, so the declaration digest differs per sandbox configuration and no rule or held call carries across configurations. It wraps the devtools `BashTool` through a new `CommandLauncher` ([§10](#10-changes-outside-the-new-crate) D1): `HostLauncher` reproduces today's spawn ([`bash.rs:79-95`](../../adk-devtools/src/tools/bash.rs)); `SandboxLauncher` builds the command with `adk_sandbox::sandbox::sandboxed_command(enforcer, "sh", ["-c", script], policy, cwd = workspace root)` (S1), with a read-write policy (RW on the canonical root plus a per-session scratch `TMPDIR`, no network) or, in Plan and ReadOnly, a read-only policy (RO root, RW scratch, no network). Environment is `ws.bash_env()` ([`workspace.rs:150`](../../adk-devtools/src/workspace.rs)) plus `TMPDIR`; inside the sandbox `HOME` is the scratch directory and no agent sockets are passed. Timeout ceiling, output cap, streaming and group kill come from devtools; the tool additionally polls `ctx.run_config()` governance every 250 ms and kills the group on a freeze.

On a non-zero exit the result gains `sandbox: { enforcer, denialSuspected, hint }` (stderr heuristics: `Operation not permitted`, `Read-only file system`, `deny(1)`, network unreachable) and the call is recorded in the harness's per-session shell ledger.

**Unsandboxed retry.** A call with `unsandboxed: true` is `ShellEscalation`. The policy hard-denies it unless `retry_of` names a failed sandboxed call in the same session with an identical command and no earlier retry; it is `RequireApproval` and non-waivable in Ask and Auto, `Deny` in ReadOnly and Plan or when `allow_unsandboxed_retry(false)`; the model must give a `reason`, shown quoted and capped in the prompt; the approval is consumed by that one call. This needs no mid-execution confirmation and so no dependency on `confirm_tool_call` (unmerged #899).

**Availability.** At `build()` the enforcer is probed; if unavailable (the Windows stub, a missing `bwrap`) and `fail_if_unavailable` is set, the build fails with `harness.sandbox_unavailable`; otherwise the harness warns, reports `Degraded`, every command needs approval in Ask, and `auto_allow_sandboxed` has no effect. Both checks run again per call. `auto_allow_sandboxed` also requires protected paths to be read-only inside the sandbox, which needs S3 ([§10](#10-changes-outside-the-new-crate)); until then it refuses to turn on.

**Container** (feature `docker`): one container per (app, user, session) through `SandboxClient::provision`/`start` ([`workspace/client.rs:64-75`](../../adk-sandbox/src/workspace/client.rs)), kept across runs and stopped on idle TTL or `Harness::close`; `ContainerBashTool` uses `SandboxSession::exec_command` ([`session.rs:57-61`](../../adk-sandbox/src/workspace/session.rs)) with a timeout and a post-hoc output cap; the host workspace is bind-mounted at `/workspace` through S4 so host file tools and the shell share one view.

The Anthropic native `bash_*` builtins ([`adk-tool/src/builtin/anthropic.rs:198-218`](../../adk-tool/src/builtin/anthropic.rs)) run `sh -lc` with the full environment, no working directory and no timeout; the harness rejects them at build.

### 7.10 Team: subagents and external harnesses

Subagents are a `TeamSpec`, not hand-built `AgentTool`s (D-5). The harness coordinator and its members compile to a team with `Delegate` edges, which gives per-edge timeouts, approval, circuit breakers, a team budget, termination and runtime discovery for free, and lets the harness itself be a member of a larger team.

```rust
pub struct MemberSpec {
    pub name: String,
    pub description: String,            // when to use (model-facing)
    pub kind: MemberKind,
    pub tools: MemberTools,             // ReadOnly (default) | Full | Custom(Vec<Arc<dyn Tool>>)
    pub skills: bool,                   // false
    pub max_iterations: u32,            // 50
    pub timeout: Duration,              // 15 min; never None
    pub output_schema: Option<Value>,
}
pub enum MemberKind {
    Local { instruction: String, model: Option<Arc<dyn Llm>> },           // an LlmAgent built by the harness
    #[cfg(feature = "team-acp")] Acp(AcpAgentConfig),                     // Claude Code, Codex, Kiro, … over ACP
    #[cfg(feature = "team-a2a")] Remote(RemoteA2aConfig),                 // a remote agent over A2A
    Agent(Arc<dyn Agent>),                                                // any Agent the caller supplies
}
impl MemberSpec { pub fn explore() -> Self; pub fn general() -> Self; pub fn reviewer_acp(cmd: &str) -> Self; }
```

Lowering (one `TeamSpec`, [`team.rs:59`](../../adk-agent/src/team.rs)): coordinator = the harness `LlmAgent`; one `TeamMemberSpec` per member; one `TeamRelationship` per member with `RelationshipKind::Delegate` and a `RelationshipPolicy` ([`team.rs:176-217`](../../adk-agent/src/team.rs)) of `context: Isolated`, `history: None`, `state_keys: []`, `state_write_keys: []`, `artifact_prefixes: []`, `timeout_ms: spec.timeout`, `approval: Never` (the member's own calls are governed by the shared policy, not the delegation), `failure: Fallback`/`Retry` per spec, and a circuit breaker; `TeamPolicy { context: Isolated, delegation depth 1, max concurrent delegations 4, budget: TeamBudget, termination: TeamTerminationPolicy }` — the default context is `Shared` ([`team.rs:328-334`](../../adk-agent/src/team.rs)), so the harness sets `Isolated` explicitly. `TeamSpec::compile_with_registry` ([`team/discovery.rs:126`](../../adk-agent/src/team/discovery.rs)) takes the `member_registry`, so members can be discovered at runtime (a `TeamAgentRegistry` over a skills directory, an MCP server list, or an A2A agent card index) and authorized per call. Delegate edges need `adk-agent/team-tools`, which the `team` feature enables.

What the team gives that the old `SpawnSubagentTool` design did not:

| Concern | Mechanism |
|---|---|
| Unbounded or unlimited delegates | `timeout_ms` on every edge; `AgentTool` defaults to **no** limit otherwise ([`agent_tool.rs:837-840`](../../adk-tool/src/agent_tool.rs)) |
| Cost | `TeamBudget` ([`team.rs:352-384`](../../adk-agent/src/team.rs)) on top of the shared `RunBudget` |
| Stopping | `TeamTerminationPolicy` (389-403): escalation, final authors, text markers |
| Who can be delegated to | `TeamAgentRegistry::{candidates, resolve, authorize}` ([`discovery.rs:61-76`](../../adk-agent/src/team/discovery.rs)) |
| State leakage | `state_write_keys: []`; delegate state writes are rejected ([`agent_tool.rs:1149-1157`](../../adk-tool/src/agent_tool.rs)) |
| Group work | `BlackboardSpec` with round-robin or model-selected speakers ([`team/blackboard.rs:42-51`](../../adk-agent/src/team/blackboard.rs)) for review rounds |
| Observability | The team publishes its topology; the embedded UI renders it live |

**External harnesses as members.** An ACP member wraps `adk-acp`'s client (`AcpAgentTool`, [`adk-acp/src/tool.rs:59`](../../adk-acp/src/tool.rs); `AcpToolset`, [`adk-acp/src/toolset.rs:27`](../../adk-acp/src/toolset.rs)) so Claude Code, Codex or Kiro run as isolated members whose tool calls surface through the ACP permission bridge and are decided by the same `HarnessPolicy`; an A2A member wraps `RemoteA2aAgent` ([`remote_agent.rs:47-52`](../../adk-server/src/a2a/remote_agent.rs)) so a remote ADK agent, or any A2A agent, runs under the team budget. Both appear in the delegation chain and the audit trail like any member.

Read-before-edit tracking is scoped per (session, agent) through D2 so members sharing a `Workspace` do not share read state. Member todos stay in the member's isolated session. Members have no `exit_plan_mode`, `memory_write` or nested delegation.

### 7.11 Plan mode, the plan graph and the goal engine

Two orchestration forms exist for stepwise work. The harness uses both where each fits.

**Plan graph** (feature `graph`, default on). When `exit_plan_mode` is approved with `steps: [PlanStep { id, title, instruction, depends_on, gate: None | Approve | Review }]`, `plan_graph.rs` compiles the steps into a `StateGraph` ([`graph.rs:14`](../../adk-graph/src/graph.rs)): one node per step running the harness `LlmAgent` with the step instruction, edges from `depends_on`, `GraphAgentBuilder::interrupt_before` ([`agent.rs:657`](../../adk-graph/src/agent.rs)) on every step with `gate: Approve`, a reviewer member on `gate: Review`, and `with_recursion_limit` ([`graph.rs:454`](../../adk-graph/src/graph.rs)) from the plan size. The graph runs under a `Checkpointer` ([`checkpoint.rs:222`](../../adk-graph/src/checkpoint.rs); SQLite in the CLI, the session's backend on the server), so a step survives process loss and `TimeTravelHandle::fork_at(step, new_thread_id)` and `state_history` ([`time_travel.rs:266, 327, 390`](../../adk-graph/src/time_travel.rs)) give rewind and fork. The interrupt is digest-bound in the way `adk-computer-use` binds its approval interrupts ([`adk-computer-use/src/graph.rs:203`](../../adk-computer-use/src/graph.rs)): the approval names the plan hash and the step id, so an edited plan needs a new approval. The graph is exported as `WorkflowSchema` ([`workflow.rs:62`](../../adk-graph/src/workflow.rs)) and visible at `/debug/graph`. Steps without `steps` keep today's behaviour: the mode switches and the model proceeds.

**Goal engine** (loop-until). `GoalSpec { goal, until, max_iterations, check_timeout }`; `GoalState { goal, until, iteration, max_iterations, status: Running | Done | Exhausted | Cancelled, last_check, updated_at }` in the harness goal store, mirrored to `harness:goal` (session tier; replaces `.adk/goal.json`, [`adk-cli/src/main.rs:222-229`](../../adk-cli/src/main.rs)).

```rust
pub enum GoalEngine {
    Loop,     // LoopAgent::new(..., vec![harness_llm_agent, UntilCheckAgent]).with_max_iterations(n)   (loop_agent.rs:32, 65)
    Graph,    // StateGraph: iterate → check → (done | iterate), checkpointed, interrupt_before("iterate") optional
}
```

`UntilCheckAgent` (impl `Agent`) checks the kill switch (`ctx.run_config().check_governance()`), runs `until` through the same launcher (the command comes from the operator, not the model, so it is not a governed tool call; it is audited as `harness.bash.*` with `goalCheck: true`), updates the goal state, and sets `escalate` on exit 0 (Done) or at the cap (Exhausted); `LoopAgent` stops on escalate ([`loop_agent.rs:654`](../../adk-agent/src/workflow/loop_agent.rs)) and a hold mid-iteration resumes at the right sub-agent through `workflow_positions` ([`event.rs:113-120`](../../adk-core/src/event.rs)). The `Graph` engine adds per-iteration checkpoints, time travel and a `/debug/graph` view at the cost of a second checkpointer. `Loop` is the default in the CLI; `Graph` is the default when a checkpointer is configured (server, managed form).

**Durable goals.** `Harness::goal_target()` returns `(target, Arc<Runner>)` for `DurableWorker::with_agent` ([`worker.rs:309`](../../adk-server/src/background/worker.rs)); `start_goal` with a run store writes the goal state and enqueues `NewRun { target: "harness.goal", session_id, concurrency_key: session, idempotency_key: "goal:<session>:<digest>", .. }` ([`run_store.rs:228-280`](../../adk-core/src/run_store.rs)); held calls park as `AwaitingApproval` (137-148) and the approval wakes the run. Goals require Ask or Auto.

**Ambient goals.** A goal can be triggered rather than started: `adk-agent`'s `AmbientAgent` with `CronTrigger`, `FileWatchTrigger` and `WebhookTrigger` ([`ambient/agent.rs:79`](../../adk-agent/src/ambient/agent.rs), [`cron_trigger.rs:73`](../../adk-agent/src/ambient/cron_trigger.rs), [`file_watch_trigger.rs:22`](../../adk-agent/src/ambient/file_watch_trigger.rs), [`webhook_trigger.rs:143`](../../adk-agent/src/ambient/webhook_trigger.rs)) can wrap the goal runner, so "every night, make `cargo test` pass on `main`" is a cron goal and "when `Cargo.lock` changes, run the audit" is a file-watch goal, both under the same policy and budget. On the server, the webhook form is the signed ingress ([`adk-server/src/webhooks/ingress.rs`](../../adk-server/src/webhooks/ingress.rs)).

### 7.12 Durability

The harness states its durability as invariants, not as a feature:

| Invariant | Mechanism |
|---|---|
| A completed `bash`, `write_file`, `edit_file`, member delegation or payment never executes twice after a crash, retry or resume | These tools are `NonIdempotent`; the action ledger (`RunConfig::action_ledger`, [`context.rs:1468`](../../adk-core/src/context.rs)) records `begin` before execution and answers a re-issued call from its record (`adk-agent/src/tool_ledger.rs`); a call that timed out or panicked stays begun and is answered `outcome_unknown` |
| A held approval survives process loss and is answered from any process | `ApprovalStore` on a durable backend ([`adk-session/src/approval.rs:498-533`](../../adk-session/src/approval.rs)); durable runs park as `AwaitingApproval` and `StoreApprovalService::apply` wakes them ([`approval.rs:1613`](../../adk-core/src/approval.rs)) |
| A resumed run is governed by the current mode and rules, never by the ones in force when it parked | The policy reads the stores at evaluation time; loosening cancels held calls ([§7.4](#74-modes)) |
| Only one process works a run at a time | Fenced leases with heartbeats (`lease_ttl/3`) and a fence before every model and tool call ([`worker.rs`](../../adk-server/src/background/worker.rs)) |
| Sessions, approvals, runs, modes and rules share one durable backend in each deployment | SQLite in the CLI (one file); the session service's backend on the server; six backends have the durability gates ([`changelog.d/added.durability-gates-every-backend.md`](../../changelog.d/added.durability-gates-every-backend.md)) |

The CLI stores everything in `~/.config/adk-rust/harness.db` ([§8.1](#81-adk-cli-the-binary-is-the-harness)). On the server, a `code` turn can itself be a durable run through `with_background_runs`, not only a goal.

### 7.13 Observability and self-correction (feature `observe`)

| What | Mechanism |
|---|---|
| Spans | Every model and tool call already carries the GenAI attributes ([`adk-telemetry/src/semconv.rs:21-31`](../../adk-telemetry/src/semconv.rs)); the harness adds `harness.mode`, `harness.tool_kind`, `harness.isolation`, `harness.rule_id`, `harness.member`, `harness.goal.iteration` on its own spans |
| Cost | `SpendLedger` ([`spend.rs:376`](../../adk-core/src/spend.rs)) is wired by default with `SpendKey` per agent and subject (105); `RunBudget` ([`budget.rs:105`](../../adk-core/src/budget.rs)) and `TeamBudget` bound a run and a team; `/cost`-style reporting in the CLI and `harness.spend` in the web UI |
| Loop guard | `LoopGuard { window, max_identical_calls, max_redundant }` runs `adk-eval`'s `TraceAnalyzer` ([`trace_analyzer.rs:91, 131-137`](../../adk-eval/src/trace_analyzer.rs)) over the invocation's events after each tool round from `process_llm_request`; on a detected loop it tightens the mode to `ReadOnly` for the run (`SystemTighten`), ends the turn with a reason, and records `harness.loop.detected`. It never answers a call |
| Eval suite | `adk-eval` cases for the §5 scenarios (`fix_failing_test`, `plan_mode_writes_nothing`, `goal_until`) with a `BaselineStore` ([`baseline.rs:62`](../../adk-eval/src/baseline.rs)) regression baseline run in CI on the scripted model and nightly on live models |
| Topology | The compiled team and plan graph publish topology; the embedded web UI renders it live; Mermaid/DOT export is a 3.2 item |
| Audit | [§9.3](#93-audit-records) |

### 7.14 Identity and credentials (feature `auth`)

| Need | Mechanism |
|---|---|
| Who the run acts for | `ActingContext { agent, on_behalf_of, delegation_chain }` ([`principal.rs:335`](../../adk-core/src/principal.rs)), set by the runner from `RunConfig::acting`; every `ToolPolicyRequest`, `ToolConfirmationRequest`, `SpendKey` and audit record carries it; a run acting for another subject fails `identity.subject_mismatch` unless a `SubjectDelegationPolicy` allows it ([`acting.rs:284`](../../adk-core/src/acting.rs)) |
| Authenticated identity on the server | A `RequestContextExtractor` is required to mount the harness ([§8.2](#82-adk-server)); without one `user_id` is whatever the client sends ([`runtime.rs:1331, 1448`](../../adk-server/src/rest/controllers/runtime.rs)) |
| Roles on a shared server | `RbacMap { class → required permission }` wraps harness tool classes in `ProtectedTool`/`ScopeGuard` from `adk-auth` (needs the annotations-forwarding fix, [§10](#10-changes-outside-the-new-crate) F2); e.g. `Shell(Host)` requires `harness:shell`, `Payment` requires `harness:pay` |
| Credentials for `git`, `gh`, cloud CLIs | Never in the shell environment: `credentials(Arc<dyn CredentialBroker>)` mints short-lived, subject-scoped tokens through `adk-auth`'s broker and `TokenExchangeBroker` ([`token_exchange.rs`](../../adk-auth/src/token_exchange.rs)), injected per call into a helper the sandboxed shell can read (`GIT_ASKPASS`/credential helper) and revoked after the call; requires the broker cache to be keyed by subject (F6) |
| Provider keys and secrets | `adk-auth` secret providers (AWS Secrets Manager, Azure Key Vault, GCP Secret Manager) through the model resolver; the CLI keyring for local use |
| Tamper-evident record | `adk_auth::signed_audit::AuditLog` through `Runner::builder().audit_log` ([`adk-runner/src/builder.rs:481`](../../adk-runner/src/builder.rs)), not `FileAuditSink` ([§9.3](#93-audit-records)) |

### 7.15 File-based memory (feature `memory`)

Off by default. `MemoryConfig { layout, limits, write_policy }`; `MemoryLayout::PerUser { base: ~/.adk/memory }` is the default (`<base>/<app>/<sha256(user)>/<project-slug>`, outside the workspace so no shell command can write it); `Project` (`<ws>/.adk/memory`) is opt-in and a protected path. `FileMemory` is a devtools `Workspace` over the memory root with `index()` (a derived `MEMORY.md` from each topic file's `summary:` front matter plus a preserved header, so concurrent writers never conflict), `read`, `write(path, content, summary)` (atomic, reindex), `delete`, and `as_core_memory()` for `PreloadMemoryTool`-style callers ([`preload_memory.rs:116`](../../adk-tool/src/memory/preload_memory.rs); the core `Memory` trait takes no user, [`context.rs:710-778`](../../adk-core/src/context.rs), so this is single-user). Tools: `memory_read` (Read), `memory_write`/`memory_delete` (`MemoryWrite`, non-waivable). The index is injected last in the instruction inside the untrusted block. Feature `memory-graph` adds `adk-memory`'s `remember`/`relate` graph tools under the same class. Long-term consolidation with revisions and provenance is spec 3.2 material.

### 7.16 Payments pack (feature `payments`)

`adk-payments` already routes an escalated payment through the governed path and meters it in the shared `SpendLedger` ([`adk-payments/src/lib.rs:47-54`](../../adk-payments/src/lib.rs): `auth`, `domain`, `guardrail`, `journal`, `kernel`, `protocol`, `server`, `tools`; the approval fix is in #899). The pack registers the payments tools as class `Payment`: `Deny` in ReadOnly and Plan, `RequireApproval` and non-waivable in Ask and Auto, never covered by a standing rule, always under `RunBudget` and the payments guardrails (mandate limits), with the journal receipt linked from the audit record (`harness.payment`). The prompt names the mandate and the remaining budget. No new payment code is written; the pack is wiring and classes.

### 7.17 Voice surface (feature `voice`; gated)

The same `HarnessAgent` is the agent of an `IntegratedRealtimeRunner` ([`integration/mod.rs:153`](../../adk-realtime/src/integration/mod.rs)) with `tool_governance(harness.run_config())` ([`integration/builder.rs:283`](../../adk-realtime/src/integration/builder.rs)), so spoken requests run under the same policy and a held call is surfaced as a realtime event the user answers by voice or in the UI. `store_to_memory` defaults to `true` on the integration ([`mod.rs:89, 105`](../../adk-realtime/src/integration/mod.rs)); the pack sets it to follow the harness memory policy. **Gate**: the Hardening Backlog's realtime rows ([`ROADMAP.md:112-132`](../../ROADMAP.md)) — realtime runners ignore a delegate's hold, realtime usage is unmetered, and realtime text gets no output guardrails — must land first; the pack ships in 3.1.x when they do.

### 7.18 Managed form (feature `managed`)

For hosted, multi-tenant harnesses, a `HarnessDef` extension of `ManagedAgentDef` ([`adk-managed/src/types/agent_def.rs:36`](../../adk-managed/src/types/agent_def.rs)) declares model reference, mode, shell policy, skills roots, members and packs, so the managed runtime ([`runtime.rs:194`](../../adk-managed/src/runtime.rs)) builds the harness from a definition, runs it in its supervised session loop, parks custom tool calls and confirmations on durable runs ([`session_loop.rs:613, 661`](../../adk-managed/src/session_loop.rs)) and replays events to `adk-enterprise` clients. The rule for the two durability engines: `DurableWorker` for local and single-tenant servers; the managed runtime for hosted tenants. `adk-managed` is Experimental, so the pack follows that tier.

### 7.19 `CodingAgent` preset

`adk_harness::coding::{CodingAgent, CodingAgentBuilder, CODING_SYSTEM_PROMPT}` keep today's builder methods ([`adk-agent/src/coding/mod.rs:85-168`](../../adk-agent/src/coding/mod.rs)) and lower to `HarnessAgentBuilder::new(name).base_prompt(CODING_SYSTEM_PROMPT).mode(Ask).shell(Host).without_default_members()`. `todos()` is deprecated in favour of `HarnessAgent::todos(&session)`. The prompt no longer claims a "sandboxed workspace" unless the shell is sandboxed. `adk_agent::coding` cannot re-export from `adk-harness` (dependency cycle), so it stays frozen, gains `#[deprecated(since = "3.2.0", note = "Use adk_harness::coding::CodingAgent. Removed in 4.0.0.")]` in 3.2.0 and is removed in 4.0.0 per STABILITY.md.

## 8. Surfaces

### 8.1 adk-cli: the binary is the harness

| Invocation | Behaviour |
|---|---|
| `adk-rust` | Harness REPL in `--dir` (default `.`), mode Ask, approvals in the terminal; refuses `$HOME` or `/` unless `--dir` is explicit |
| `adk-rust chat` | The plain agent, unchanged |
| `adk-rust code "task"` | One harness turn; exit code 3 means calls are held (prints the session id) |
| `adk-rust goal "g" --until CMD` | Durable goal in SQLite; `--resume` restores the conversation too |
| `adk-rust ultracode "task"` | The review graph on the harness: implement/revise in the user's mode, reviewers in ReadOnly |
| `adk-rust serve` | The harness in Plan mode with `/api/approvals` and background runs; loopback-only unless `--auth-token`; `--agent chat` keeps the old behaviour |
| `adk-rust --voice` | The voice surface (feature `voice`, 3.1.x) |
| `adk-rust sessions …`, `rules …`, `approvals … [--server URL --token T]` | Management commands |

Shared flags (`HarnessArgs`, flattened into the top level and into `code`, `goal`, `ultracode`, `serve`): `--dir`, `--mode read-only|plan|ask|auto`, `--yes` (approves held calls that reach the handler; never overrides a policy denial; never approves an unsandboxed retry without `--allow-unsandboxed`), `--session <id>` / `--continue` / `--ephemeral`, `--sandbox`, `--allow-unsandboxed`, `--fail-if-unavailable` (requires `--sandbox`), `--trust`, `--pack browser|computer-use|payments`, `--member name=acp:codex`. Without a terminal and without `--yes`, held calls are denied with an actionable message so CI never hangs. Precedence: flags → `.adk/harness.toml` → user config → defaults. Hidden aliases for two minors: `code --read-only` → `--mode read-only`; `goal --state` writes only the `.adk/goal.json` mirror.

**Behaviour change.** `code`, `goal` and `ultracode` today run host `bash` with no confirmation ([`main.rs:366-412`](../../adk-cli/src/main.rs)); in Ask mode unattended scripts need `--mode auto` or `--yes`. Recorded as a `security.` changelog fragment.

Persistence: one SQLite file at `dirs::config_dir()/adk-rust/harness.db` for sessions (`SqliteSessionService`, [`adk-session/src/sqlite.rs:76, 164`](../../adk-session/src/sqlite.rs)), approvals (`SqliteApprovalStore`, [`approval.rs:498-517`](../../adk-session/src/approval.rs)), durable runs (`SqliteRunStore`, [`run_store.rs:760-791`](../../adk-session/src/run_store.rs)), graph checkpoints, and the harness mode, rule and shell-ledger stores; the stores use distinct migration tables. `app_name = "adk-rust"`, user id = the OS username (fallback `"user"`); a session records its canonical workspace path and refuses to resume from another `--dir`.

Model and setup: every command goes through `setup::resolve` ([`setup.rs:104`](../../adk-cli/src/setup.rs); today only `Chat` and `Serve`); no prompt without a terminal; `compiled_providers()` replaces `ALL_PROVIDERS` ([`cli.rs:44`](../../adk-cli/src/cli.rs)); default features become `["gemini", "openai", "anthropic", "audit", "persist", "harness"]`.

Console: one rustyline reader thread (`ConsoleIo`) serves both the REPL ([`launcher.rs:357-363, 417-478`](../../adk-cli/src/launcher.rs)) and the prompter, so Ctrl-C at a prompt cannot leave a blocking stdin read behind; `ConsolePrompter` implements `ApprovalPrompter` with `[y] allow once  [a] always allow…  [n] deny (reason)  [e] edit args  [?] details`; `[e]` opens `$VISUAL`/`$EDITOR` on the JSON arguments; Ctrl-C denies and cancels; the console raises `tool_confirmation_timeout` to one hour. `StreamPrinter::handle_event` ([`launcher.rs:773-781`](../../adk-cli/src/launcher.rs)) also renders `actions.tool_confirmation` and `tool_confirmation_decision`. Slash commands through a data-driven `ConsoleCommand`: `/mode`, `/sandbox on|off|status`, `/todos`, `/rules [rm N]`, `/approvals`, `/plan`, `/cost`, `/team`, `/session`, `/skills`, `/memory`. The banner shows workspace, mode, isolation, members, session id.

`Launcher` additions (adk-core types only): `with_run_config`, `with_tool_policy`, `with_confirmation_handler`, `with_user_id` (today hard-coded `"user"`, 336), `with_session_id` (resume-or-create; today always new, 346-353), `with_console_command`, `with_approval_service`, `with_background_runs`. Harness glue lives in `adk_cli::harness` behind a default-on `harness` feature.

### 8.2 adk-server

Policy and approvals need no server change (D-4). `build_app` ([`launcher.rs:579-588`](../../adk-cli/src/launcher.rs)) switches from `create_app` to `ServerBuilder` (behaviour-preserving: `create_app_with_a2a` is `ServerBuilder::new(..).build()`, [`rest/mod.rs:399-406`](../../adk-server/src/rest/mod.rs)) and adds `.with_approval_service(harness.approval_service())` (718), the approval ticker, and `.with_background_runs(..)` ([`background/mod.rs:544-561`](../../adk-server/src/background/mod.rs)). `/api/approvals` answers 401 without an extractor ([`approvals.rs:5-9`](../../adk-server/src/approvals.rs)), so the harness mount **requires** one: `LoopbackOperatorExtractor` admits loopback peers via `ConnectInfo<SocketAddr>` as `RequestContext { user_id: whoami, scopes: ["operator"] }`; `BearerTokenExtractor` (`--auth-token`/`ADK_SERVE_TOKEN`, constant-time compare) is required for any non-loopback `--host`; SSO/JWT extractors from `adk-auth` for teams. `--mode auto` on a non-loopback host also requires `--sandbox`. Held-call flow over HTTP: SSE event with `actions.tool_confirmation` → `GET /api/approvals` → `POST /api/approvals/{id}/decide` → an empty-message run resumes (decisions already wake parked durable runs). Prerequisite: owner checks on `/runs` get/list/cancel ([`background/mod.rs:749-856`](../../adk-server/src/background/mod.rs)). The bundled web UI sends no `Authorization` header and never calls `/api/approvals`: loopback only, approvals via `adk-rust approvals` or `curl`.

### 8.3 cargo-adk `harness` template

Added to `builtin_agent_templates` ([`registry.rs:177+`](../../cargo-adk/src/registry.rs)) after the `tools` (468-520) and `api` (573-622) patterns: `required_features: ["minimal", "harness", "cli"]`; `incompatible_addons: ["server"]` (the launcher serves); `docker` compatible when `--mode` and the `--yes` policy are explicit in `.adk/harness.toml`; `additional_deps: [adk-session { features: ["sqlite"] }]`; `"harness"` added to `SELF_SERVING_TEMPLATES` ([`codegen.rs:224`](../../cargo-adk/src/codegen.rs)). Generated `main.rs`:

```rust
use std::sync::Arc;
use adk_rust::prelude::*;
use adk_rust::harness::{HarnessAgent, HarnessConfig, HarnessLauncher};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let model = Arc::new(GeminiModel::from_env()?);
    let sessions = Arc::new(adk_session::SqliteSessionService::new("sqlite://.adk/state.db?mode=rwc").await?);
    sessions.migrate().await?;
    let harness = HarnessAgent::builder("{name}")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .config(HarnessConfig::discover(".")?)      // .adk/harness.toml
        .build()?;
    HarnessLauncher::new(harness).with_session_service(sessions).run().await?;   // `cargo run` = REPL; `cargo run -- serve` = HTTP
    Ok(())
}
```

Additional files: `AGENTS.md` skeleton, `.skills/example/SKILL.md` (valid for `adk-rust skills validate`), `.adk/harness.toml` (mode, sandbox, members, packs, deploy), `tests/harness_smoke.rs` (`ScriptedLlm` in Plan mode asserts a scripted `write_file` is refused), README section, `.gitignore` for `.adk/state.db*`. CI: `scripts/check-cargo-adk-templates.sh` gains `harness:gemini`, `harness:openai`, `harness:anthropic`, `harness:gemini:telemetry,mcp`, with `cargo test` for harness entries.

### 8.4 Umbrella `adk-rust`

`harness = ["dep:adk-harness", "agents", "tools", "skills", "sandbox", "runner", "sessions", "graph"]` ([`adk-rust/Cargo.toml:116-148`](../../adk-rust/Cargo.toml)), in `full`; it links a new crate, so it passes rule R1 of `scripts/check-feature-growth.py`. `pub mod harness { pub use adk_harness::*; #[cfg(feature = "cli")] pub use adk_cli::harness::{HarnessLauncher, ConsolePrompter}; }`; prelude exports `HarnessAgent`, `HarnessAgentBuilder`, `HarnessMode`. `docs/feature-surface.md` gains a row. Pack features forward: `harness-browser`, `harness-computer-use`, `harness-payments`, `harness-voice`, `harness-managed`.

### 8.5 Deployment

| Target | Mechanism |
|---|---|
| Container | The `docker` addon with `.adk/harness.toml` `[deploy] mode = "plan"` and explicit `yes_policy`; `--fail-if-unavailable` when a sandbox is configured |
| Agent Engine (BYOC) | `serve_agent_engine(harness.agent(), AgentEngineOptions)` ([`entrypoint.rs:268`](../../adk-server/src/agent_engine/entrypoint.rs)); approvals through the approvals REST and `ApprovalNotifier` (chat or paging); `cargo adk deploy agent-engine` via `GcpDeployClient` ([`adk-deploy/src/gcp.rs:370`](../../adk-deploy/src/gcp.rs)) |
| Kubernetes / any host | `adk-rust serve` with the bearer extractor and a shared session backend; durable runs through `with_background_runs` |
| Managed runtime | [§7.18](#718-managed-form-feature-managed) |

Secrets come from `adk-auth` secret providers; nothing in the image. A served harness in Plan mode exposes the workspace to the model for reading; the changelog and docs say so.

## 9. Security invariants and audit records

### 9.1 Threat summary

| Area | Threats the design closes (mechanism) |
|---|---|
| Permission state | The model, a client `stateDelta`, an AG-UI state echo or a delegate writes mode or rules → stores are harness-owned; session state is a mirror (D-3). Project files pre-approve tools → project config can only add Deny/RequireApproval. A rule outlives revocation → the policy reads the store on every call |
| Identity | A rule from user A applied to B → subject in the rule, `ctx.user_id() != session.user_id()` refused. Unauthenticated server → mount requires an extractor; rules and Auto inert otherwise. Run-wide `tool_approvals` shared → never written by surfaces, stripped for non-waivable tools |
| Modes | MCP `readOnlyHint` passes ReadOnly → classes from the registry. Loosening re-runs held calls → loosening cancels them first. `exit_plan_mode` batched with `write_file` → the switch applies from the next invocation. Unknown mode → ReadOnly |
| Shell | Background survivors → group kill after exit (D3), bwrap `--die-with-parent`. stdin steal / TIOCSTI → stdin null, `setsid`, bwrap `--new-session` always (S2). Host bash reaches `~/.ssh`, `git credential fill`, the network → RequireApproval, prompt states "full user permissions, network"; sandbox HOME is scratch. Sandboxed auto-allowed write to `.git/hooks`, `.envrc`, `lefthook.yml`, `.cargo/config.toml` → auto-allow off until S3. Symlink race between sandboxed bash and host `read_file` → D4. Retry approval replayed → `retry_of` ledger, consumed once. Rule granted sandboxed applied on host → digest includes the sandbox config. Freeze during a prompt or a run → polled, group killed, prompt resolves "not run" |
| Filesystem | Self-modifying instructions via writes to `AGENTS.md`, `.skills/**`, `.claude/**`, `.adk/**`, `.git/**`, `MEMORY.md` → protected paths, non-waivable approval, case-folded and Unicode-normalized matching. Hard-link reads → refused (D4). `grep` over `.env`, `*.pem`, `id_*` → excluded by default (D5), reads need approval, sequential redaction guardrail |
| Instructions, skills, memory | A cloned repo's nested `AGENTS.md`/`CLAUDE.md` → only explicitly located files load; nested files are listed, never inlined; trust prompt. `{user:…}` templates → `instruction_provider`. Skill `allowed-tools` grant tools → granted tools are `External`; the registry contains no harness tools. `MEMORY.md` poisoning (OWASP ASI06) → outside the workspace, per user, size-capped, write non-waivable, delimited untrusted |
| Team | No timeout → `timeout_ms` on every edge. Recursion → depth 1. State leakage → `state_write_keys: []`. Parent artifacts → `artifact_prefixes: []`. Runaway cost → `TeamBudget`, `RunBudget`, loop guard |
| Bypasses | A plugin answers `bash` → the harness installs no answering hooks; bash refuses without the harness `policy_ref` in the run config; short-circuits audited (F1). Harness not root → `run()` self-check. MCP tool named `bash` shadows a rule → rules match (source, digest) |
| Approval UX | Auto-continue on timeout → core denies on timeout ([`governance.rs:1943-1976`](../../adk-core/src/governance.rs)); the turn ends. ANSI/CR hiding `; curl …` → control characters escaped; truncation marker; no approval while truncated. Prompt flooding → serialized, per-turn cap, "deny all remaining". Stderr "approved by admin" in a retry prompt → quoted, escaped, capped, never in the title |

### 9.2 Invariants

**Permission state.** I1 Mode, standing rules, the shell ledger, goal state and workspace trust are read only from harness stores; no session-state key, client `stateDelta`, AG-UI state, tool `state_delta` or delegate merge can create, change or restore them. I2 A standing rule is created only by an authenticated human action, never by a model-callable tool. I3 Workspace files add only Deny or RequireApproval. I4 A revocation applies to every call authorized after it commits, including in-flight, resumed and durable runs.

**Identity.** I5 A rule matches only if `rule.subject == request.user_id == acting.on_behalf_of` and `rule.app == app_name`; a run with `ctx.user_id() != session.user_id()` is refused. I6 Without authenticated identity on the server, rules and Auto are inert. I7 Surfaces never write `RunConfig::tool_approvals`; `configure_run` strips entries naming non-waivable tools.

**Binding.** I8 A rule matches only a call whose `ToolOrigin.source` and `declaration_digest` equal the rule's; a mismatch records `harness.rule.stale`. I9 The bash declaration digest covers (enforcer, profile digest, network). I10 Bash rules use `ShellPrefix` only. I11 Path predicates evaluate the canonical resolved path and never match a protected path.

**Modes.** I12 Class comes from (origin.source, name) in the registry, never from `ToolEffect` or MCP hints alone. I13 In ReadOnly and Plan nothing executes that can change the filesystem, network-visible state, persistent state other than todos, or permissions, by any route. I14 Tightening applies at the next evaluation; loosening applies from the next invocation and cancels every held call in the session. I15 The model changes mode only through `exit_plan_mode`; Auto is never the default target. I16 An unparseable mode is ReadOnly; a rule with unknown fields is ignored.

**Governed path.** I17 Every harness tool execution is preceded by an `authorize_tool_call` result of `Execute` for the same call ID and final arguments; the harness installs no answering hooks; `bash` refuses unless the harness `policy_ref` is in the run config. I18 In Ask every host bash follows a RequireApproval decision unless a rule matches; a non-waivable call is authorized only by a one-shot decision. I19 Denial or timeout never executes; timeout is denial; after a human denial on an interactive surface the turn ends. I20 Once a freeze is observed no harness tool starts, running processes die within one second, open prompts resolve to "not run". I21 Prompts show final post-screen arguments with control characters escaped, the origin, the acting agent and the isolation; a truncated payload cannot be approved unexpanded.

**Shell.** I22 `bash` runs with stdin `/dev/null`, in a new session with no controlling terminal, with allowlisted environment only; in the sandbox `HOME` is per-session scratch and no agent sockets or credentials are passed. I23 No process started by a bash call outlives it (bwrap and containers guarantee it; host and Seatbelt enforce it by group kill, with `setsid` escape as a documented residual). I24 A requested but unavailable sandbox refuses, at build and per call; never a host fallback. I25 An unsandboxed retry requires `retry_of` naming a failed sandboxed call in the same session with the identical command and no earlier retry; it is non-waivable and consumed by that one call. I26 Auto-allowing sandboxed commands requires network denied, protected paths read-only inside the sandbox (S3), and no concurrent host file tool. I27 No provider-executed shell builtin is ever attached.

**Filesystem.** I28 File tools never touch anything outside the canonical root, including under symlink swaps and hard links (D4). I29 Writes to protected paths are non-waivable in Ask and Auto and denied in ReadOnly and Plan. I30 Secret-pattern files are skipped by grep and need approval to read; tool results pass a sequential redaction guardrail.

**Instructions, skills, memory.** I31 Only explicitly located instruction files load; each is hashed and audited; first use of a workspace needs a trust decision. I32 Instruction, memory and skill text passes through `instruction_provider` inside a delimited untrusted block and cannot affect `decide()`. I33 Memory is written only through the memory tool, lives outside the workspace per user by default, is size-capped, and needs non-waivable approval. I34 Skill `allowed-tools` only make tools visible; granted tools are `External`.

**Team, durability, payments.** I35 Every member has a timeout, delegation depth 1, empty `state_write_keys` and `artifact_prefixes`, its own read-tracking scope, the parent's mode, budget, policy and audit; its held calls show the member's agent ID. I36 Goal and durable state live outside the workspace; a resumed run re-evaluates under the current mode and rules. I37 A completed non-idempotent call is never executed twice after a crash, retry or resume. I38 A payment is never auto-allowed and never covered by a standing rule.

**Audit.** I39 The harness requires `RunConfig::audit` in Block mode; every §9.3 event is recorded write-ahead and its outcome recorded afterwards; records carry digests, with raw payloads in a payload store behind `payload_ref`.

### 9.3 Audit records

Actor from `AuditActor::new(acting, invocation_id)` ([`audit_log.rs:288`](../../adk-core/src/audit_log.rs)); `ext.sessionId` as `ToolGate::record` sets it ([`governance.rs:1421-1432`](../../adk-core/src/governance.rs)); kind `Other` for `harness.*` actions ([`audit_log.rs:138-158`](../../adk-core/src/audit_log.rs)); written through `ctx.run_config().audit` ([`tool.rs:371`](../../adk-core/src/tool.rs)).

| Event | Action | Fields (`ext`) | Emitted at |
|---|---|---|---|
| Mode switch | `harness.mode.changed` / `.refused` | from, to, initiator, surface, effectiveFrom, cancelledHeldCallIds | `ModeStore::set`, write-ahead |
| Rule created / revoked / expired / stale | `harness.rule.*` | ruleId, ruleDigest, subject, app, tool, source, declarationDigest, predicatesDigest, decision, expiresAt, createdFromCallId, by, reason, observedDigest | store writes; policy evaluation |
| Rule matched | governed `ToolCall` record | `policy_ref = tool_policy:adk-harness#rule:<id>`, toolSource, declarationDigest | `authorize_tool_call` allow path (needs C1 or C2) |
| bash execution | `harness.bash.begin` / `.end` | functionCallId, enforcer, profileDigest, network, envKeys, args_digest, payload_ref, exitCode, durationMs, timedOut, killedByFreeze, truncated, retryOf, goalCheck | before spawn (Block) and after reap |
| Unsandboxed retry | `harness.bash.retry_unsandboxed` | retryOf, failedEnforcer, failureDigest, decision, source | policy (requested) and tool (consumed) |
| Member delegation | `harness.member.spawned` / `.finished` | parentAgentId, memberAgentId, kind (local/acp/a2a), depth, timeoutMs, budget snapshot, outcome, durationMs | team delegate tool |
| Plan compiled / step gate | `harness.plan.compiled`, `harness.plan.step` | planHash, steps, stepId, gate, decision | `exit_plan_mode`; graph interrupt |
| Goal iteration | `harness.goal.iteration` | iteration, max, checkExitCode, status | `UntilCheckAgent` |
| Loop detected | `harness.loop.detected` | window, identicalCalls, redundantCalls, action | loop guard |
| Spend | `harness.spend` | spendKey, modelCalls, tokens, cost, budgetRemaining | end of run |
| Payment | `harness.payment` | mandateId, amount, currency, journalReceiptRef, decision | payments pack |
| Memory write | `harness.memory.written` | logicalPath, beforeSha256, afterSha256, bytes, functionCallId | memory tool, write-ahead |
| Instruction files loaded / changed; workspace trust | `harness.instructions.*`, `harness.workspace.trust` | files [{logicalPath, sha256, bytes, scope, trusted}], rootDigest, decision, by | instruction provider; trust prompt |
| Skill activated | `harness.skill.activated` | skillId, hash, sourcePath, allowedTools | `load_skill` wrapper |
| Sandbox unavailable; held calls cancelled | `harness.sandbox.unavailable`, `harness.held.cancelled` | requested, error; callIds, reason | build and per call; mode loosening, freeze |

Records go to `adk_auth::signed_audit::AuditLog` (hash chain, Merkle tree, signed checkpoints) through `Runner::builder().audit_log` ([`adk-runner/src/builder.rs:481`](../../adk-runner/src/builder.rs)), not `FileAuditSink`, whose verification cannot detect a truncated tail or an edited last line ([`adk-auth/src/audit.rs:665-699`](../../adk-auth/src/audit.rs)).

## 10. Changes outside the new crate

| # | Crate | Change | Why |
|---|---|---|---|
| C1 (required) | adk-core | Provided method `ToolPolicy::evaluate_call(&self, req: &ToolPolicyRequest, call: &PolicyCall) -> PolicyDecision { self.evaluate(req).await }` with `#[non_exhaustive] pub struct PolicyCall<'a> { pub origin: Option<&'a ToolOrigin>, pub function_call_id: Option<&'a str> }`; `evaluate_policy` ([`governance.rs:1879-1900`](../../adk-core/src/governance.rs)) and `authorize_edited_args` (1756) call it | The policy cannot see the origin or digest today ([`policy.rs:93-115`](../../adk-core/src/policy.rs)); non-breaking, unlike adding fields to the exhaustive `ToolPolicyRequest` |
| C2 (required) | adk-core | `StrictestPolicy { layers: Vec<Arc<dyn ToolPolicy>> }` over `PolicyDecision::stricter` ([`policy.rs:55-62`](../../adk-core/src/policy.rs)) | `configure_run` must compose with, not replace, an operator policy |
| C3 (required) | adk-core | Provided method `ToolConfirmationHandler::decide_approval(&self, &ToolConfirmationRequest) -> Result<ToolApproval>` (default delegates to `decide`), called by `resolve_confirmation` ([`governance.rs:1940-1967`](../../adk-core/src/governance.rs)) | Handlers can return edited arguments and a reason |
| D1 (required) | adk-devtools | `pub trait CommandLauncher { fn command(&self, ws: &Workspace, script: &str, ctx: &dyn ToolContext) -> Result<tokio::process::Command, DevToolError>; fn isolation(&self) -> &str; }` + `BashTool::with_launcher`; `HostLauncher` = [`bash.rs:79-95`](../../adk-devtools/src/tools/bash.rs) | Devtools' timeout, output cap, streaming and group-kill semantics under a sandbox without forking the tool |
| D2 (required) | adk-devtools | Read-before-edit tracking scoped per (session_id, agent_name) from the `ToolContext` in `mark_read`/`was_read` ([`workspace.rs:307-316`](../../adk-devtools/src/workspace.rs)), LRU-evicted | The tracker is process-global and shared by clones (23, 31) |
| D3 (required) | adk-devtools | bash: `.stdin(Stdio::null())`, `setsid`, kill the process group after a normal reap ([`bash.rs:79-100, 116, 160-181`](../../adk-devtools/src/tools/bash.rs)) | I22, I23 |
| D4 (should) | adk-devtools | No-follow open on every path component for read/edit/write (Linux `openat2 RESOLVE_BENEATH`, macOS `O_NOFOLLOW_ANY`); canonicalize then open the write parent by path ([`read.rs:57-59`](../../adk-devtools/src/tools/read.rs), [`edit.rs:60-65`](../../adk-devtools/src/tools/edit.rs), [`workspace.rs:187-299`](../../adk-devtools/src/workspace.rs)); refuse reads of `nlink > 1` | I28 under a sandbox |
| D5 (should) | adk-devtools | `exclude_globs` on grep and glob ([`grep.rs:17`](../../adk-devtools/src/tools/grep.rs)) | I30 |
| S1 (required) | adk-sandbox | `pub fn sandbox::sandboxed_command(enforcer: &dyn SandboxEnforcer, program: &OsStr, args: &[OsString], policy: &SandboxPolicy, cwd: &Path) -> Result<tokio::process::Command, SandboxError>` extracted from [`process.rs:609-652`](../../adk-sandbox/src/process.rs); `ProcessBackend` calls it | No duplicated PATH resolution or Xcode shim handling; the working directory is the workspace, not a scratch dir |
| S2 (required) | adk-sandbox | bwrap: always pass `--new-session` ([`linux.rs:136-140`](../../adk-sandbox/src/sandbox/linux.rs)) | I22 |
| S3 (gates `auto_allow_sandboxed`) | adk-sandbox | Nested read-only / deny-write inside a read-write path with identical semantics on Seatbelt ([`macos.rs:198-207`](../../adk-sandbox/src/sandbox/macos.rs) emits only allow rules) and bwrap ([`linux.rs:142-157`](../../adk-sandbox/src/sandbox/linux.rs), mount order), plus an enforcer conformance test | I26, I29 for bash |
| S4 (docker) | adk-sandbox | `DockerClient::with_bind_mount(host, container, read_only)` applied in `host_config` ([`docker.rs:232-249`](../../adk-sandbox/src/workspace/docker.rs)) | Container isolation sharing the workspace with host file tools |
| A1 | adk-agent | Session-scoped `TodoTool` fix for the frozen module; `#[deprecated]` on `coding::*` from 3.2.0 | §7.19 |
| F1 (should) | adk-agent | Audit record when a hook or plugin short-circuits a tool call ([`llm_agent.rs:2565-2651`](../../adk-agent/src/llm_agent.rs)) | I17 evidence |
| F2 (required for RBAC) | adk-auth | `ProtectedTool` and `ScopeGuard` forward `annotations()` ([`scope.rs:75`](../../adk-auth/src/scope.rs)) | §7.14; MCP hints on wrapped tools |
| F3 (should) | adk-core | `toolSource` and `declarationDigest` in `AuditedCall::draft` ([`governance.rs:1825-1834`](../../adk-core/src/governance.rs)); a freeze subscribe API on `GovernanceControl` (1943-1966) | Stronger audit; prompt wait abandons on freeze |
| F4 (should) | adk-tool | Surface every held call of a delegate, not only the first ([`agent_tool.rs:1034-1038`](../../adk-tool/src/agent_tool.rs); #852 remainder) | Team members with several holds |
| F5 (should) | adk-runner | `InvocationContext` accessor reporting whether identity was authenticated ([`adk-runner/src/context.rs:768-775`](../../adk-runner/src/context.rs)) | I6 |
| F6 (required for OBO) | adk-auth | `TokenExchangeBroker` cache keyed by subject and delegation chain, not by agent ([`token_exchange.rs`](../../adk-auth/src/token_exchange.rs)) | §7.14 |
| K1 (optional) | adk-skill | `load_skill_index_from_dirs(dirs)` that skips the convention-file tree walk ([`discovery.rs:96-106`](../../adk-skill/src/discovery.rs)) | Performance |
| U1 | adk-rust | `harness` feature, module, prelude, pack features | §8.4 |
| L1, G1 | adk-cli, cargo-adk | §8.1, §8.3 | Surfaces |

The 3.0 defects the harness must not inherit, and how the design avoids them:

| Defect | Where | Avoided by |
|---|---|---|
| A plugin or callback answer skips the governed path | [`llm_agent.rs:2673`](../../adk-agent/src/llm_agent.rs) | D-2; no answering hooks; `bash` refuses without the harness `policy_ref` |
| `TodoTool` shared across sessions | [`coding/todo.rs:28-31`](../../adk-agent/src/coding/todo.rs) | §7.6 |
| `AgentTool` default timeout = unlimited | [`agent_tool.rs:837-840`](../../adk-tool/src/agent_tool.rs) | `timeout_ms` on every edge |
| `Workspace` read-tracker shared by clones | [`workspace.rs:31`](../../adk-devtools/src/workspace.rs) | D2 |
| bash group guard disarmed on normal exit | [`bash.rs:116`](../../adk-devtools/src/tools/bash.rs) | D3 |
| One `PluginContext` per agent | [`enhanced_manager.rs:67-181`](../../adk-plugin/src/enhanced_manager.rs) | No per-session state in plugins |
| Fingerprint lacks the subject | [`context.rs:1289`](../../adk-core/src/context.rs) | Subject in the rule; never `tool_approvals` |
| Parallel transforming guardrails keep the last transform | [`executor.rs:155-238`](../../adk-guardrail/src/executor.rs) | Sequential redaction |
| Console ignores held-call events | [`launcher.rs:773-781`](../../adk-cli/src/launcher.rs) | §8.1 |
| First-run setup offers uncompiled providers | [`setup.rs:212-230`](../../adk-cli/src/setup.rs) | `compiled_providers()` |
| Convention files discovered anywhere | [`discovery.rs:82`](../../adk-skill/src/discovery.rs) | §7.7 loader |

## 11. Testing and live verification

| Layer | Where | Cases |
|---|---|---|
| Unit | adk-harness | Mode × class × isolation table is exhaustive; `configure_run` idempotent over three calls; outer Deny wins; approval service installed only when the store is empty or identical; member lowering sets every policy field; plan compiler produces the expected graph shape |
| Property (`proptest`) | adk-harness | P1 `ShellPrefix`: a match implies no metacharacter and token prefix equality (alphabet weighted to `; & \| $ \` < > ( ) { } * ? [ ] ~ ! \n \\ ' "`, `${IFS}`, NUL, Unicode lookalikes); P2 `PathWithin`: a match implies resolvable, under the prefix, not protected (`..`, `//`, `./`, case and NFC/NFD variants); P3 mode lattice monotone: ReadOnly ⪰ Plan ⪰ Ask ⪰ Auto, Allow rules never change Deny or NW; P4 subject isolation; P5 any declaration mutation stops a rule matching; P6 expiry; P7 fingerprint separators (U+001F) cannot forge; P8 arbitrary JSON parses to ReadOnly or "rule ignored", never Allow or Auto |
| Conformance | adk-harness | `mode_store_conformance_tests!`, `standing_rule_store_conformance_tests!` over in-memory, file and SQLite (pattern [`adk-core/src/conformance/mod.rs:202`](../../adk-core/src/conformance/mod.rs)) |
| Integration | adk-harness/tests | `ScriptedLlm` + Runner on InMemory and SQLite sessions: member inherits the stricter mode; per-session todos; held `rm` in Ask → Auto executes zero times; `[exit_plan_mode, write_file]` batch denies the write; goal resumes after a simulated crash; plan graph rewind re-executes nothing; loop guard tightens the mode; payment never auto-allowed |
| adk-cli | unit + binary tests with `--features scripted-model` | clap parsing and aliases; `compiled_providers`; `ConsolePrompter` over scripted `ConsoleIo` (once, always, deny with reason, edit, Ctrl-C, no TTY); `StreamPrinter` renders held calls; `code --mode plan` writes nothing; exit 3 on held calls; `goal --resume` restores history; `build_app`: 401 without extractor, loopback admitted, held → decide → resume executes once |
| Templates | cargo-adk + CI script | §8.3 |

**`examples/harness_agent`** (standalone crate; results in a main-vs-branch table). Scripted scenarios, no keys: `mode_matrix`, `ask_allow_once`, `always_allow_creates_standing_rule` (prefix rule matches `cargo test --quiet`, not `cargo publish`, not another user), `rule_stale_after_redeclaration` (H2), `edit_args_rechecked_by_policy`, `todos_isolated_per_session`, `served_held_call_resumes_via_approvals` (executes once), `goal_resumes_after_crash` (H1), `ledger_prevents_double_bash` (H1), `plan_graph_rewind` (H4), `team_budget_stops_run` (H3), `acp_member_governed` (H3, scripted ACP server), `loop_guard_parks_run` (H7), `payment_requires_approval` (H8, payments test harness), `sandbox_fail_if_unavailable`, `unsandboxed_retry_is_held`, `agents_md_and_skills_loaded`, `memory_file_persists`, `identity_per_user_credentials` (H6, scripted broker). Live (`--live`, `#[ignore]` as tests) on `gemini-3.5-flash-lite`, `gpt-5.6-terra`, `claude-sonnet-4-6`: `live_fix_failing_test`, `live_plan_mode_writes_nothing` (tree hash unchanged, todos non-empty), `live_goal_until`, `live_served_approval_roundtrip` (file created exactly once after a REST approval), `live_standing_rule` (zero prompts for `cargo test` on the second task), `live_team_review` (local coordinator, ACP reviewer), `live_voice_approval` (3.1.x).

**Red-team scenarios** (`examples/harness_redteam`, modelled on `examples/autonomy_validation`): poisoned root and nested `AGENTS.md`; project config grants bash; `.mcp.json` server spawn; git-hook persistence from the sandbox; background survivor; `.env` grep canary; `~/.ssh` read on host vs sandbox; `git credential fill`; sandboxed `curl` canary; retry replay; digest drift; lying `readOnlyHint`; MCP `bash` shadow; cross-user rule; unauthenticated server; AG-UI state echo after revocation; delegate state write; member timeout; recursion; Plan escapes; held calls after loosening; freeze during prompt and run; approval timeout; ANSI in command; prompt flood; `MEMORY.md` poisoning; template injection; symlink TOCTOU; hard-link read; plugin short-circuit; goal-state tamper; stdin steal; TIOCSTI from bwrap. Pass means the harness blocked the effect, whatever the model chose to do.

**Must be proven live, and how.** Sandbox enforcement on real macOS (Seatbelt) and Linux (bwrap) runners (GitHub's `ubuntu-24.04` restricts unprivileged user namespaces through AppArmor: set `kernel.apparmor_restrict_unprivileged_userns=0` or add a bwrap profile; a host with neither proves I24 by refusing); process lifetime with `pgrep` by marker; terminal behaviour in a pty (`expectrl`/`portable-pty`); server identity and state echo over real HTTP with a JWT extractor and an AG-UI client; durable resume and expiry with SQLite stores and the background worker; real-model injection on three providers.

## 12. Phased delivery

| Phase | Release | Contents | Size (engineer-weeks) | Demonstrates |
|---|---|---|---|---|
| 0 Prerequisites | 3.0.x fixes; 3.1.0 additive APIs | A1 todos; plugin short-circuit audited (F1); `PluginContext` per invocation; `/runs` owner checks; first-run setup; console held-call rendering; C1–C3; D1–D3; S1–S2; F2, F6 | 4–5 | Existing `code` and `serve` safe to build on |
| 1 Core | 3.1.0 | `adk-harness`: builder, `HarnessAgent`/`configure_run`/`Harness`, modes and `ModeStore`, standing rules and stores, `HarnessPolicy`, todos, instructions and trust, skills, shell (host + OS sandbox), team lowering with local members, durability wiring, observability (spans, spend, loop guard), identity (RBAC map, broker), file memory, `coding` preset; scripted scenarios | 9–11 | H1, H2, H6, H7 scripted; `features = ["harness"]` |
| 2 CLI | 3.1.0 | §8.1; examples migrated; live scenarios 1–5 | 4–5 | `cargo install adk-cli` → REPL with approvals, `/mode`, `--session`, `goal` |
| 3 Server, graph, team | 3.1.0 | `serve` via `ServerBuilder` + approvals + background; plan graph and `Graph` goal engine; ACP and A2A members; `approvals` subcommand | 4–5 | H3, H4, H9 (CLI, server, Agent Engine) |
| 4 Scaffold, docs, eval | 3.1.0 | `harness` template; docs pages; eval suite and baselines; changelog fragments | 3 | `cargo adk new x --template harness && cargo run` |
| 5 Packs | 3.1.x | S3 then `auto_allow_sandboxed`; Docker (S4); payments pack; browser and computer-use packs; voice (after the realtime hardening rows); managed form | 5–7 | H5, H8, H9 (managed), H10 |
| 6 Follow-ups | 3.2.0 | `#[deprecated]` on `adk_agent::coding`; ACP server mapping "allow always" to a standing rule; Mermaid/DOT export; memory consolidation; shared mode/rule stores on Postgres/Redis; Stable review | 2–3 | IDE use; multi-replica servers |

Total for 3.1.0 (phases 0–4): roughly 24–29 engineer-weeks; with the packs, 32–38. Estimates are for one engineer and carry the usual uncertainty.

## 13. Open questions

1. Should an approved `exit_plan_mode` default to `ask` only, with `auto` requiring a second explicit confirmation in the CLI?
2. Multi-replica servers: the SQLite mode and rule stores are per host; shared backends through `adk-session` are a 3.2 item. Acceptable for 3.1?
3. Should `auto_allow_sandboxed` exist in the 3.1.0 API as "off and refuses to turn on until S3 is present", or be omitted until S3 lands?
4. Should a `BlackboardSpec` review round (model-selected speakers) replace the `ultracode` fan-out graph, or coexist with it?
5. Should the plan graph's checkpointer default to the session backend on the server, or always to SQLite beside the harness store?

## 14. Appendix: grounding against current APIs (`origin/main` @ `a24ebd8d`)

| Area | Items the spec builds on |
|---|---|
| Agent and builder | `LlmAgentBuilder::{instruction_provider 1002, toolset 1270, tool 1254, max_iterations 1204, tool_timeout 1217, parallelize_agent_delegations 1451, tool_confirmation_policy 1417, build 1626}`; per-call order 2565 → 2613 → 2653 → **2673** → 2694 → 2763 → 2797; tool re-resolution 4593-4606; `Agent::configure_run` ([`agent.rs:206`](../../adk-core/src/agent.rs)) called at [`runner.rs:1080, 1772`](../../adk-runner/src/runner.rs) and [`agent_tool.rs:867, 1331`](../../adk-tool/src/agent_tool.rs) |
| Governed path | `authorize_tool_call` 1527; `ToolGate` 1351-1446; `GovernedCall::for_tool` 1281; `ToolAuthorization` 1459; decision sources 1903-1978; `held_tool_calls` 2084; `confirmation_decision_available` 2444; fingerprint [`context.rs:1289`](../../adk-core/src/context.rs); `ToolOrigin` 1089-1148; `ToolEffect` [`tool.rs:494`](../../adk-core/src/tool.rs) |
| Policy and approvals | `PolicyDecision` 29, `ToolPolicyRequest` 93, `ToolPolicy` 229, `ArgPredicate` 262, `PolicyRule` 439-574, default deny 625 ([`policy.rs`](../../adk-core/src/policy.rs)); `ToolApproval` 774, `ApprovalScope` 857, `ApprovalStore` 931 ([`governance.rs`](../../adk-core/src/governance.rs)); `RunConfig` approval fields [`context.rs:1385-1503`](../../adk-core/src/context.rs); `ApprovalService` 1349, `ApprovalNotifier` 1199 ([`approval.rs`](../../adk-core/src/approval.rs)) |
| Identity | `ActingContext` [`principal.rs:335`](../../adk-core/src/principal.rs); `admit_subject` [`acting.rs:284`](../../adk-core/src/acting.rs); `ToolContext` [`tool.rs:317-476`](../../adk-core/src/tool.rs) |
| State and sessions | `State` [`context.rs:245`](../../adk-core/src/context.rs); prefixes [`event.rs:11-15`](../../adk-core/src/event.rs); tier split [`state_utils.rs:21-39`](../../adk-session/src/state_utils.rs); `SessionService` [`service.rs:316`](../../adk-session/src/service.rs) |
| Teams | `TeamSpec` 59, `RelationshipPolicy` 176-217, `TeamPolicy` 328-340, `TeamBudget` 352-384, `TeamTerminationPolicy` 389-403, `compile` 856, delegate lowering 1562-1697 ([`team.rs`](../../adk-agent/src/team.rs)); `TeamAgentRegistry` [`discovery.rs:61-76, 126`](../../adk-agent/src/team/discovery.rs); `BlackboardSpec` [`blackboard.rs:21-51`](../../adk-agent/src/team/blackboard.rs) |
| AgentTool | `new` 307, `timeout` 350, `session_snapshot` 368, `max_delegation_depth` 448, `output_state_keys` 512, delegation chain 871-882, held calls 1034-1038, timeout default 837-840 ([`agent_tool.rs`](../../adk-tool/src/agent_tool.rs)) |
| Graph | `StateGraph` [`graph.rs:14, 454`](../../adk-graph/src/graph.rs); `GraphAgentBuilder::interrupt_before/after` [`agent.rs:657-663`](../../adk-graph/src/agent.rs); `Checkpointer` [`checkpoint.rs:222`](../../adk-graph/src/checkpoint.rs); time travel [`time_travel.rs:266, 327, 390`](../../adk-graph/src/time_travel.rs); `WorkflowSchema` [`workflow.rs:62`](../../adk-graph/src/workflow.rs); `LoopAgent` [`loop_agent.rs:32, 65, 654`](../../adk-agent/src/workflow/loop_agent.rs) |
| Durability | `DurableRunStore` 620, `RunCheckpointRef` 123-148, `NewRun` 189-220 ([`run_store.rs`](../../adk-core/src/run_store.rs)); `DurableWorker::run_agent` [`worker.rs:789-979`](../../adk-server/src/background/worker.rs); ambient triggers [`ambient/`](../../adk-agent/src/ambient) |
| Skills | discovery [`discovery.rs:5-6, 73, 82`](../../adk-skill/src/discovery.rs); parser [`parser.rs:101-175`](../../adk-skill/src/parser.rs); `load_skill_index_with_extras` [`index.rs:94`](../../adk-skill/src/index.rs); `SkillToolset` [`progressive.rs:372-701`](../../adk-skill/src/progressive.rs) |
| Devtools | `Workspace` [`workspace.rs:24-330`](../../adk-devtools/src/workspace.rs); `DevToolset` [`toolset.rs:17-57`](../../adk-devtools/src/toolset.rs); bash [`bash.rs:48-143`](../../adk-devtools/src/tools/bash.rs) |
| Sandbox | `SandboxPolicy` and builder [`sandbox/mod.rs:134-197`](../../adk-sandbox/src/sandbox/mod.rs); `SandboxEnforcer` 310; `get_enforcer` 370; Seatbelt [`macos.rs`](../../adk-sandbox/src/sandbox/macos.rs); bwrap [`linux.rs`](../../adk-sandbox/src/sandbox/linux.rs); `ProcessBackend::with_sandbox` [`process.rs:317, 595-680`](../../adk-sandbox/src/process.rs); `SandboxClient`/`SandboxSession` [`workspace/client.rs:51`](../../adk-sandbox/src/workspace/client.rs), [`session.rs:40`](../../adk-sandbox/src/workspace/session.rs); `DockerClient` [`docker.rs:95`](../../adk-sandbox/src/workspace/docker.rs) |
| Realtime, payments, managed | `IntegratedRealtimeRunner` [`integration/mod.rs:85-153`](../../adk-realtime/src/integration/mod.rs), `tool_governance` [`builder.rs:283`](../../adk-realtime/src/integration/builder.rs); payments modules [`lib.rs:47-54`](../../adk-payments/src/lib.rs); `SpendLedger` [`spend.rs:105, 376`](../../adk-core/src/spend.rs); `RunBudget` [`budget.rs:105`](../../adk-core/src/budget.rs); `ManagedAgentDef` [`agent_def.rs:36`](../../adk-managed/src/types/agent_def.rs); `ManagedAgentRuntime` [`runtime.rs:194`](../../adk-managed/src/runtime.rs) |
| Observability | GenAI attributes [`semconv.rs:21-31`](../../adk-telemetry/src/semconv.rs); `TraceAnalyzer` [`trace_analyzer.rs:91`](../../adk-eval/src/trace_analyzer.rs); `BaselineStore` [`baseline.rs:62`](../../adk-eval/src/baseline.rs) |
| Surfaces | `Commands` [`cli.rs:54-167`](../../adk-cli/src/cli.rs); `Launcher` [`launcher.rs:159-619`](../../adk-cli/src/launcher.rs); `setup::resolve` [`setup.rs:104`](../../adk-cli/src/setup.rs); `ServerBuilder` [`rest/mod.rs:468-746`](../../adk-server/src/rest/mod.rs); per-request runner [`runtime.rs:1356-1379`](../../adk-server/src/rest/controllers/runtime.rs); `serve_agent_engine` [`entrypoint.rs:268`](../../adk-server/src/agent_engine/entrypoint.rs); templates [`registry.rs:177+`](../../cargo-adk/src/registry.rs), [`codegen.rs:180, 224`](../../cargo-adk/src/codegen.rs); `RemoteA2aAgent` [`remote_agent.rs:47`](../../adk-server/src/a2a/remote_agent.rs); `AcpAgentTool` [`tool.rs:59`](../../adk-acp/src/tool.rs), `AcpToolset` [`toolset.rs:27`](../../adk-acp/src/toolset.rs) |

**Net new code:** the `adk-harness` crate (policy, stores, matcher, prompter, toolset and tools, launchers, instructions, team lowering, plan compiler, goal engine, observability and identity wiring, memory, packs, preset), the CLI harness glue, the `harness` template, and the additive changes in [§10](#10-changes-outside-the-new-crate). Everything else is wiring of APIs that exist at `a24ebd8d`.
