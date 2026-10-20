# Spec: A Native Coding Agent for ADK-Rust

| | |
|---|---|
| **Status** | Draft / RFC — superseded by [`spec-3.1-harness-agent.md`](spec-3.1-harness-agent.md) for everything except `adk-devtools`, which shipped |
| **Tracking issue** | [#380 — feat: implement Code Agents](https://github.com/zavora-ai/adk-rust/issues/380) |
| **Author(s)** | ADK-Rust maintainers |
| **Target** | `adk-rust` 1.2.x |
| **Affected crates** | NEW `adk-devtools`, NEW `adk-coding-agent`; touches `adk-code`, `adk-skill`, `adk-cli`, `adk-server`, `adk-acp`, `adk-realtime`, `adk-managed` |

---

## 1. Summary

This spec defines a **first-class coding agent** for ADK-Rust: an agent that reads
and edits a codebase, runs commands and tests in a sandbox, plans multi-step work,
delegates to subagents, and remembers a project across sessions — usable from a
CLI, as a remote A2A service, by voice, or inside an IDE.

The central finding (see [§4](#4-background--prior-art)) is that ADK-Rust already
ships the *hard* infrastructure that 2026 coding-agent harnesses (LangChain Deep
Agents, Claude Code, Google ADK, Pi) had to build from scratch: a durable runtime,
multi-agent orchestration, context compaction, sandboxed code execution, a
bi-temporal knowledge-graph memory, A2A, ACP, and realtime/multimodal I/O.

Therefore this is **not a new framework**. It is **two new pieces** plus
**assembly**:

1. **`adk-devtools`** — the missing developer toolset (Read/Write/Edit/Grep/Glob/
   Bash/Git/GitHub) over the existing `adk-code` sandbox.
2. **`adk-coding-agent`** — a thin harness that wires those tools, a plan/todo
   loop, subagent spawning, compaction, skills, and a minimal prompt into a
   one-line `CodingAgent::builder()`.

Code-as-actions (CodeAct, the original framing of #380) is supported as an
**optional execution backend**, not the default. The default is tool-calling with
a sandboxed shell — matching what every shipping 2026 coding agent does.

## 2. Goals

- **G1.** A working coding agent in one builder call, on any model provider.
- **G2.** A native, sandbox-respecting developer toolset (file/search/shell/VCS).
- **G3.** Long-horizon work: planning, todos, subagents with context isolation,
  and automatic context compaction.
- **G4.** Durable, resumable, autonomous runs (`/goal`) via `adk-managed`.
- **G5.** One agent, many surfaces: CLI, A2A service, realtime voice, ACP/IDE — no
  rewrite.
- **G6.** Cross-session project memory via the bi-temporal knowledge graph + RAG.
- **G7.** Small footprint: minimal default build; everything else feature-gated.
- **G8.** Everything publishable to crates.io (no git deps in the default path).

## 3. Non-goals

- **N1.** Not a from-scratch agent runtime — reuse `LlmAgent` / `adk-runner`.
- **N2.** Not making a Python interpreter (Monty) load-bearing — it is at most one
  quarantined, non-published CodeAct backend (see [§7.6](#76-optional-codeact-execution-backend)).
- **N3.** Not an IDE or editor UI — but ship an ACP surface so existing IDEs can
  drive it.
- **N4.** Not reimplementing tools available via MCP unless they are core to the
  inner loop (file/search/shell/VCS are core; everything else is MCP/function tools).

## 4. Background & prior art

2026 coding-agent harnesses independently converged on the same shape — a
**harness** (the runtime/loop) distinct from the **agent** — with these primitives:

| Primitive | Deep Agents | Google ADK | Claude Code | Pi |
|---|---|---|---|---|
| Harness ≠ agent | ✅ (LangGraph) | ✅ (Runner) | ✅ | ✅ (`pi-agent-core`) |
| Planning (todos/plan) | `write_todos` | — | TodoWrite / plan mode | — |
| Subagents (fresh ctx → summary) | `task` tool | sub-agents | subagents / teams | — |
| Virtual filesystem / workspace | ✅ | — | real FS | Read/Write/Edit |
| Context compaction | ✅ | — | ✅ | ✅ |
| Skills (lazy markdown packs) | — | — | ✅ | ✅ "lazy skills" |
| Hooks (deterministic control) | — | callbacks | ✅ | extensions |
| Sandboxed code **as a tool** | Modal/Daytona | Agent Engine / GKE | Bash | Bash + Docker |
| Provider-agnostic | ✅ | Gemini-first | Anthropic | ✅ |
| Minimal core tools | — | — | small | Read/Write/Edit/Bash |

**Three lessons carried into this design:**

1. **Context isolation via subagents** is the scaling unlock — a subagent spends
   its own context budget and returns a single summary.
2. **Code runs as a sandboxed *tool*, not as the whole action space.** CodeAct is a
   power-option; modern models are post-trained for native tool calls.
3. **Differentiation is in skills + harness ergonomics + sandbox**, not novel
   agent theory.

See `docs/design/coding-agent-research.md` references at the end of this document.

## 5. Use cases

Organized into tiers so scope stays honest. Tier A is the MVP.

### Tier A — the core dev loop (table stakes)
- **UC-A1.** "Fix the failing test" — read files, run tests, edit, re-run.
- **UC-A2.** "Add feature X" — plan → edit across files → build → test.
- **UC-A3.** "Explain / find where Y happens" — grep/glob + read, summarize.
- **UC-A4.** "Open a PR for this change" — git commit/branch + GitHub PR.

### Tier B — orchestration (the moat)
- **UC-B1.** Swarm: orchestrator spawns subagents (e.g. one per failing module),
  each returns a summary; orchestrator integrates. Lifecycles tracked.
- **UC-B2.** Remote coding via A2A: a long-running refactor dispatched to a remote
  worker, streamed back.
- **UC-B3.** Autonomous `/goal`: "migrate the repo to edition 2024" runs durably,
  resumable across restarts.

### Tier C — interface (the unique surface)
- **UC-C1.** Voice: "Hey, what's breaking the build?" — realtime audio in/out.
- **UC-C2.** Multimodal: paste a screenshot of an error / a diagram of desired UI.
- **UC-C3.** CLI: `adk code "…"` in a terminal.
- **UC-C4.** IDE: drive the agent from any ACP-compatible editor; or delegate to
  Claude Code / Codex as a sub-tool.

### Tier D — governance & deployment
- **UC-D1.** Sandbox policy per deployment (strict in CI, relaxed locally).
- **UC-D2.** Guardrails (secret redaction, command allowlists), auth, audit.
- **UC-D3.** Eval & benchmark suites; telemetry/tracing of every run.

## 6. Architecture

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ INTERFACES   CLI (adk code) · A2A server · Realtime voice · ACP/IDE · AWP  │  Tier C
├──────────────────────────────────────────────────────────────────────────┤
│ ORCHESTRATION  CodingSwarm (multi-agent · adk-graph cycles · A2A remote)   │  Tier B
│                Managed durable/resumable runtime (/goal autonomous)        │
├──────────────────────────────────────────────────────────────────────────┤
│ THE CODING HARNESS  ◀── NEW (adk-coding-agent, thin)                       │
│   plan→act→observe loop · todo tracking · subagent.spawn(summary)          │
│   minimal prompt + adk-skill lazy skills · compaction wiring · permissions │
├───────────────────────────────┬──────────────────────────────────────────┤
│ DEV-TOOLS TOOLSET ◀── NEW      │ MEMORY & CONTEXT                          │
│  (adk-devtools)                │  GraphMemoryService (bi-temporal KG)      │  Tier A
│  Read/Write/Edit/Grep/Glob     │  adk-rag · adk-session · compaction       │
│  Bash/Test · Git · GitHub      │                                           │
├───────────────────────────────┴──────────────────────────────────────────┤
│ EXECUTION SUBSTRATE  adk-code (Rust sandbox · Docker · WASM · JS · Python) │  Tier A
│                      adk-sandbox · SandboxPolicy · Workspace               │
│   (optional) HostBridge/CallbackExecutor → CodeAct backend (issue #380)    │
├──────────────────────────────────────────────────────────────────────────┤
│ MODELS  adk-model — provider-agnostic (Claude, Gemini, GPT, local)         │  Tier 0
├──────────────────────────────────────────────────────────────────────────┤
│ GOVERNANCE  guardrails · auth · plugin hooks · eval · bench · telemetry    │  Tier D
└──────────────────────────────────────────────────────────────────────────┘
```

**Decisions:**
- **D-1.** Default action mode is **tool-calling + a sandboxed `bash` tool**.
  CodeAct is optional ([§7.6](#76-optional-codeact-execution-backend)).
- **D-2.** Subagents reuse `AgentTool` + multi-agent; the only addition is a
  `spawn(task) → summary` convention. The **same** interface dispatches remotely
  over A2A (UC-B2) by swapping the local agent for an A2A client tool.
- **D-3.** The harness is **configuration + loop policy** over `LlmAgent`, not a
  new agent type.
- **D-4.** All file/shell ops go through `adk-code`'s `Workspace` + `SandboxPolicy`
  — there is no unsandboxed path.

## 7. Component specifications

### 7.1 `adk-devtools` — the developer toolset

A new crate exposing a `Toolset` of the inner-loop tools. Each tool implements the
existing `adk_core::Tool` trait (`name` / `description` / `parameters_schema` /
`async execute(ctx, args)`), and every filesystem/shell operation is mediated by an
`adk_code::Workspace` scoped to a root and an `adk_code::SandboxPolicy`.

```rust
pub struct DevToolset {
    workspace: Arc<Workspace>,        // adk-code: root + collaboration bus
    policy: SandboxPolicy,            // adk-code: capability model
    executor: Arc<dyn CodeExecutor>,  // adk-code: bash/test backend (Docker | rust sandbox)
}

#[async_trait]
impl Toolset for DevToolset {
    fn name(&self) -> &str { "devtools" }
    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> { /* … */ }
}
```

**Tool catalog (MVP):**

| Tool | Params | Behavior | Sandbox concern |
|------|--------|----------|-----------------|
| `read_file` | `path`, optional `offset`/`limit` | Return file contents (line-numbered) | Path must resolve inside the workspace root |
| `write_file` | `path`, `content` | Create/overwrite a file | Workspace root; respects read-only policy |
| `edit_file` | `path`, `old_string`, `new_string`, `replace_all?` | Exact-match string replacement | Must read before edit (state tracked) |
| `glob` | `pattern`, `path?` | List matching paths | Read-only |
| `grep` | `pattern`, `path?`, `glob?`, flags | Content search (ripgrep semantics) | Read-only |
| `bash` | `command`, `timeout?` | Run a shell command in the sandbox | Gated by `SandboxPolicy` (network/fs/timeout) |
| `run_tests` | `selector?` | Convenience wrapper over the project's test runner | Same as `bash` |
| `git` | `op` (status/diff/add/commit/branch/log), args | Local VCS ops | `bash` under the hood; commit policy configurable |
| `github` | `op` (pr_create/issue/view), args | GitHub via `gh` or REST | Requires auth token; opt-in |

**Tool design rules:**
- Tools return a human-readable `message` plus structured fields, so the model can
  paraphrase and the UI can render (consistent with existing ADK tools).
- `edit_file` enforces **read-before-edit** (the file must have been read in the
  session) to avoid blind clobbering — mirrors Claude Code/Pi behavior.
- Destructive ops (`write_file`, `bash`, `git commit`, `github`) pass through the
  harness **permission layer** ([§7.2](#72-adk-coding-agent--the-harness)).
- The toolset is composable with MCP and function tools via the existing
  `MergedToolset` / `PrefixedToolset`.

**Feature flags:** `git`, `github`, `ripgrep` (vs. a pure-Rust fallback) are
individually gated so a minimal build ships only file + bash.

### 7.2 `adk-coding-agent` — the harness

A thin layer producing a configured `LlmAgent` plus a run policy. The builder wires
defaults so the common case is one call.

```rust
let agent = CodingAgent::builder()
    .model(model)                          // any adk-model provider
    .workspace("./my-repo")                // → Workspace + default SandboxPolicy
    .policy(SandboxPolicy::relaxed_local())// optional; CI uses strict
    .skills_dir("./.adk/skills")           // optional lazy skills (adk-skill)
    .memory(kg.clone())                    // optional KG project memory
    .permission_mode(PermissionMode::Ask)  // Ask | Auto | Plan | ReadOnly
    .max_iterations(50)
    .build()?;                             // -> CodingAgent (wraps Arc<dyn Agent>)
```

Internally this is roughly:

```rust
LlmAgentBuilder::new("coding-agent")
    .model(model)
    .instruction(MINIMAL_CODING_PROMPT)    // sub-1k tokens; Pi-style
    .toolset(Arc::new(devtools))           // §7.1
    .tool(Arc::new(TodoTool::new()))       // §7.2.1
    .tool(Arc::new(SpawnSubagentTool::new(/* … */)))  // §7.3
    .toolset(Arc::new(skill_toolset))      // §7.5, lazily resolved
    .build()?
```

and execution runs through `adk-runner` with `compaction_config(...)` and an
optional `memory_service(...)` set.

#### 7.2.1 Planning & todos
A `TodoTool` (`write_todos` / `update_todo`) maintains an explicit task list in
session state. The harness surfaces it to the UI and uses completion of the list as
a soft stop signal. (Equivalent to Deep Agents `write_todos` / Claude TodoWrite.)

#### 7.2.2 The loop (plan → act → observe)
1. Build prompt: minimal base + active todos + lazily-resolved skills + (optional)
   KG profile card for the project.
2. Model turn → tool calls (file/search/bash/git) or a subagent spawn.
3. Tool results appended as observations; on long histories, `adk-runner`
   compaction summarizes older turns.
4. Repeat until the todo list is complete, a `final_answer`-style signal is
   emitted, or `max_iterations` is hit.

#### 7.2.3 Permission modes (hooks)
A deterministic gate on tool calls (implemented as a `BeforeToolCallback` /
`adk-plugin` hook, so it cannot be reasoned around — Claude Code's hook lesson):

| Mode | Effect |
|------|--------|
| `ReadOnly` | Only read/search tools; edits/bash/VCS denied (plan/explore). |
| `Plan` | Agent may plan and read, but mutations require an explicit plan approval first. |
| `Ask` | Mutations prompt the caller/UI for confirmation. |
| `Auto` | Mutations allowed within `SandboxPolicy` (CI / autonomous). |

### 7.3 Subagents & swarms

A `spawn_subagent` tool delegates an isolated sub-task to a **fresh** agent context
and returns **only a summary** to the parent — the context-isolation pattern.

```rust
// Local subagent: wrap a child CodingAgent as a tool
let researcher = CodingAgent::builder().model(small_model).workspace(ws).build()?;
let spawn = SpawnSubagentTool::new(vec![
    SubagentSpec { name: "researcher", agent: researcher.into_agent(), model_hint: Small },
    // …
]);
```

- **Local swarm (UC-B1):** built on `AgentTool` + multi-agent; lifecycles tracked
  via the `Workspace` collaboration bus (`request_work` / `claim_work` /
  `signal_completed`). Cyclic/iterative coordination uses `adk-graph`.
- **Remote swarm (UC-B2):** the *same* `SubagentSpec` points at an **A2A client
  tool** (`adk-server`) instead of a local agent — remote, long-running, streamed.
  No change to the orchestrator's logic.
- **Model tiering:** subagents take a `model_hint` so grunt work runs on a cheap
  model and reasoning stays on the capable one (Claude Code lesson).

### 7.4 Memory & context

Two layers, both already in-tree:

- **Ephemeral (this task):** `adk-session` state + `Workspace` scratch + the
  todo list. `adk-runner` compaction keeps long runs within the context window.
- **Durable (across runs):** `adk-memory` **`GraphMemoryService`** (bi-temporal KG)
  holds project facts ("uses axum 0.8", "tests live in `tests/`", "user prefers
  small PRs"); injected as a profile card at session start and curated via the
  `remember` / `relate` tools. `adk-rag` provides retrieval over large corpora
  (docs, the codebase) when a vector index is preferred over the graph.

See `docs/official_docs/memory/` for the memory subsystem.

### 7.5 Skills (lazy capability packs)

The harness uses `adk-skill` (agentskills.io standard) so the base prompt stays
tiny and capabilities load on demand. A coding skills pack ships with the crate:

```text
.adk/skills/
  rust-testing.skill.md        # how to run/triage cargo nextest in this repo
  github-pr.skill.md           # PR conventions, commit style
  migration-writer.skill.md    # multi-file migration recipe
```

Skills are discovered, hashed, and resolved by `adk-skill`'s `ContextCoordinator`;
the harness injects only the relevant skill bodies for the current task.

### 7.6 Optional CodeAct execution backend

For users who want code-as-actions (issue #380), add the **minimal kernel** to
`adk-code` — and keep it backend-agnostic:

```rust
// adk-code (publishable, no new deps)
#[async_trait]
pub trait HostBridge: Send + Sync {
    async fn call(&self, name: &str, args: Vec<Value>, kwargs: Map<String, Value>)
        -> Result<Value, HostCallError>;
    fn function_names(&self) -> Vec<String> { vec![] }
}

#[async_trait]
pub trait CallbackExecutor: CodeExecutor {
    async fn execute_with_host(&self, request: ExecutionRequest, host: Arc<dyn HostBridge>)
        -> Result<ExecutionResult, ExecutionError>;
}
```

- A `ToolHostBridge` in the harness adapts `Arc<dyn Tool>` → `HostBridge`, reusing
  the permission layer.
- **Default backends are ADK-native:** (a) the **Docker/Python container** path
  (already in `adk-code`), and (b) the **Rust sandbox** as a `CallbackExecutor`
  (the LLM writes Rust that calls host tools).
- **Monty is one quarantined, non-published backend** (`adk-code-monty`, git dep),
  mirroring `adk-mistralrs`. The default feature set and the `adk-rust` umbrella
  never depend on it. This is the *only* acceptable place a git dep may appear.

This satisfies #380 without making CodeAct or Monty the headline.

## 8. Interfaces & deployment

One `CodingAgent`, many surfaces — no rewrite (G5):

| Surface | Mechanism | Crate |
|---|---|---|
| **CLI** `adk code "…"` | console launcher serves the harness | `adk-cli` |
| **Coding-as-a-service** (remote, long-running) | serve over A2A / REST | `adk-server` |
| **Autonomous `/goal`** (durable, resumable) | hand the def to the managed runtime | `adk-managed` |
| **Voice** ("talk to it") | wrap in the realtime bridge (`IntegratedRealtimeRunner`) | `adk-realtime` |
| **IDE** | expose the harness as an ACP agent | `adk-acp` |
| **Delegate to Claude Code / Codex** | wrap them as a sub-tool (`AcpAgentTool`) | `adk-acp` |

Example — CLI:

```bash
adk code "make the failing test in tests/auth.rs pass"
adk code --goal "migrate the workspace to edition 2024" --auto   # durable autonomous
```

Example — voice front-end reuses the realtime server-side bridge from
`docs/official_docs/realtime/building-web-apps.md`, with the `CodingAgent` as the
session's agent.

## 9. Security & sandboxing

- **No unsandboxed path.** Every file/shell op flows through `adk-code`'s
  `Workspace` + `SandboxPolicy`. `read_file`/`write_file`/`edit_file` resolve and
  reject paths outside the workspace root.
- **Policy presets:** `SandboxPolicy::strict_*` (CI/untrusted) vs a relaxed local
  preset; network and filesystem access are deny-by-default and opt-in.
- **Execution isolation:** `bash`/`run_tests` execute via the chosen
  `CodeExecutor` — Docker/container or the Rust sandbox; production deployments use
  containerized isolation (`ExecutionIsolation::Container*`).
- **Permission modes** ([§7.2.3](#723-permission-modes-hooks)) gate mutations
  deterministically via plugin hooks.
- **Guardrails & auth:** `adk-guardrail` (secret redaction, command allowlists),
  `adk-auth` (token scoping for `github`), audit via `adk-telemetry`.

## 10. Configuration & footprint

Default build is **models + devtools(file+bash) + harness**. Everything else is
feature-gated so a minimal coding agent stays small (G7):

| Feature | Pulls in |
|---|---|
| `git`, `github` | VCS tools (`adk-devtools`) |
| `swarm` | subagents + `adk-graph` |
| `memory` | `adk-memory` KG + `adk-rag` |
| `skills` | `adk-skill` |
| `voice` | `adk-realtime` |
| `acp` | `adk-acp` IDE surface |
| `managed` | `adk-managed` durable runtime |
| `codeact` | `HostBridge`/`CallbackExecutor` (publishable backends) |
| `codeact-monty` | the quarantined, non-published Monty backend |

## 11. Telemetry, evaluation, testing

- **Telemetry:** every tool call, subagent spawn, and compaction event traced via
  `adk-telemetry` (OTEL).
- **Eval:** a coding-task eval suite (`adk-eval`) — e.g. "make the test pass" tasks
  scored on success + steps + tokens; `adk-bench` for throughput/latency.
- **Testing:** `adk-devtools` tools unit-tested against a temp `Workspace`; the
  harness loop tested with a mock model and a mock executor; CodeAct tested with the
  `adk-code` `HostBridge` test double (no Monty dependency).

## 12. Phased roadmap

| Phase | Deliverable | Crates | Publishable |
|------:|-------------|--------|:-----------:|
| **1** | `adk-devtools` (file/search/bash) + minimal `CodingAgent` loop + todos + CLI | `adk-devtools`, `adk-coding-agent`, `adk-cli` | ✅ |
| **2** | Subagents/swarm + compaction defaults + KG/RAG memory + skills pack + git/github | + `adk-graph`, `adk-memory`, `adk-skill` | ✅ |
| **3** | A2A coding server + ACP/IDE surface + realtime voice + multimodal | + `adk-server`, `adk-acp`, `adk-realtime` | ✅ |
| **4** | Autonomous `/goal` on managed runtime + CodeAct kernel + (optional) Monty backend | + `adk-managed`, `adk-code` (+ `adk-code-monty`) | ✅ (Monty backend excluded) |

Phase 1 alone is a genuinely useful, publishable coding agent.

## 13. Open questions

- **Q1.** `adk-coding-agent` as a new crate vs. a `coding` feature/module in
  `adk-agent`? (Leaning: new crate, to keep `adk-agent` lean.)
- **Q2.** `edit_file` semantics — exact-string-replace (Claude/Pi) vs. structured
  patch/diff? (Leaning: exact-string-replace for model reliability.)
- **Q3.** Subagent transcript visibility — return summary only, or also expose the
  full child transcript on request?
- **Q4.** Persistent vs. fresh interpreter/session state across CodeAct steps
  (only relevant under `codeact`).
- **Q5.** Default test-runner detection (`run_tests`) — heuristic per ecosystem
  (cargo/npm/pytest) or explicit config?
- **Q6.** How much of the permission layer is policy (config) vs. interactive
  (callback to the surface)?

## 14. Appendix — grounding against current APIs

These signatures exist today and the spec builds on them directly:

- `adk_core::Tool` — `name` / `description` / `parameters_schema` / `async execute(ctx, args)`.
- `adk_core::Toolset` — `name` / `async tools(ctx) -> Vec<Arc<dyn Tool>>`.
- `adk_agent::LlmAgentBuilder` — `.instruction()` / `.model()` / `.tool()` /
  `.toolset()` / `.sub_agent()`.
- `adk_tool::AgentTool::new(Arc<dyn Agent>)` — agent-as-tool (subagents).
- `adk_runner` builder — `.agent()` / `.session_service()` / `.memory_service()` /
  `.plugin_manager()` / `.compaction_config()`.
- `adk_code::{Workspace, SandboxPolicy, CodeExecutor, ExecutionRequest, ExecutionResult}`
  — `Workspace::new(root)`, `SandboxPolicy::strict_rust()/strict_js()`, executor
  backends (rust sandbox, embedded JS, WASM, Docker), `extract_structured_output`.
- `adk_memory::GraphMemoryService` + `adk_tool::{RememberTool, RelateTool}`.
- `adk_skill::ContextCoordinator` — lazy skill discovery/resolution.
- `adk_acp::{AcpAgentTool}` — consume/expose ACP agents.

**Net new code:** the `adk-devtools` tools, the `CodingAgent`/`CodingAgentBuilder`
harness + `TodoTool` + `SpawnSubagentTool` + permission hook, and (optional) the
`HostBridge`/`CallbackExecutor` kernel in `adk-code`. Everything else is wiring.

---

### References

- LangChain Deep Agents — <https://blog.langchain.com/deep-agents/>, <https://github.com/langchain-ai/deepagents>, <https://docs.langchain.com/oss/python/deepagents/overview>
- Google ADK code execution — <https://google.github.io/adk-docs/integrations/code-execution/>, GKE executor <https://google.github.io/adk-docs/integrations/gke-code-executor/>
- Claude Code (skills/subagents/hooks, 2026) — <https://boringbot.substack.com/p/claude-code-skills-subagents-hooks>, <https://www.developersdigest.tech/blog/claude-code-agent-teams-subagents-2026>
- Pi / pi-mono — <https://github.com/badlogic/pi-mono>; Rust reimpl <https://github.com/Dicklesworthstone/pi_agent_rust>
- smolagents CodeAgent — <https://deepwiki.com/huggingface/smolagents/4.2-codeagent>
- CodeAct (Wang et al., ICML 2024) — <https://arxiv.org/abs/2402.01030>
- Pydantic Monty — <https://github.com/pydantic/monty>; hyper-mcp monty-plugin <https://github.com/hyper-mcp-rs/monty-plugin>
