# Coding Agent

> **Note:** new projects start from the [harness quick start](../harness/index.md#quick-start),
> or from `adk-rust code` in a terminal ([CLI](cli.md)). The harness (spec 3.1, in 3.0.0)
> supersedes `adk_agent::coding::CodingAgent`, the 2.x API: the same agent with modes,
> approvals, durable goals and team members on one governed path. `adk_agent::coding` is
> unchanged in 3.0.0 and carries no `#[deprecated]` attribute;
> [Migrating from `CodingAgent`](../migration/coding-agent-to-harness.md) moves a project to
> the harness.

Build agents that **work on a codebase**: read and edit files, run commands and
tests in a sandbox, plan multi-step work, iterate toward a goal autonomously, and
orchestrate parallel reviewers — all native to ADK-Rust, on any model provider.

This is assembled from a few focused pieces rather than a monolithic framework:

| Piece | What it gives you | Page |
|-------|-------------------|------|
| **The harness** (`adk-harness`) | The coding agent to start from: the dev tools, todos and plan mode under modes and approvals, a sandboxed shell, durable goals | [Harness](../harness/index.md) |
| **`adk-devtools`** | The inner-loop tools — `read_file`, `write_file`, `edit_file`, `glob`, `grep`, `bash` — scoped to a workspace directory | [Dev tools](devtools.md) |
| **CLI** (`adk-rust code` / `goal` / `ultracode`) | Native commands on the harness: one-shot tasks, autonomous goal mode, and parallel ultra-review | [CLI](cli.md) |
| **Graph workflows** (`adk-graph`) | Fan out to parallel specialist agents and synthesize — the "ultra" pattern | [Workflows](workflows.md) |
| **Examples** | Three runnable example crates | [Examples](examples.md) |
| **`CodingAgent`** (in `adk-agent`, feature `coding`) | The 2.x one-call agent: the dev tools, `write_todos` and a minimal prompt on an `LlmAgent`, with no approvals; superseded by the harness | [`CodingAgent` (2.x)](harness.md) |

> **New to ADK-Rust?** Read the [Introduction](../introduction.md) and
> [Quickstart](../quickstart.md) first. This section builds on agents, tools,
> sessions, and (optionally) [memory](../memory/index.md) and
> [graphs](../agents/graph-agents.md).

## What you can build

- **A one-shot coding task** — "make the failing test pass", "add a `/health`
  route". The agent explores, edits, runs tests, and reports.
- **Autonomous goal mode** — give a goal + a verifiable success condition (a
  command that must exit 0); the agent loops *plan → act → verify*, self-correcting
  until it passes or a budget runs out. Durable and resumable.
- **Ultra-review workflows** — implement, then fan out to parallel
  correctness/edge-case/style reviewers, synthesize their verdicts, and revise
  until they approve.

## Start

| Where | How |
|---|---|
| In a terminal | `cargo install adk-cli`, then `adk-rust code "make the failing test pass"` in the project; see the [CLI](cli.md) |
| In code | `adk-rust` with the `harness` feature and `HarnessAgent::builder`; see the [harness quick start](../harness/index.md#quick-start) |
| A new project | `cargo adk new my-harness --template harness`; see [Configuration](../harness/configuration.md#the-harness-template) |
| An existing `CodingAgent` | `adk_harness::coding::CodingAgent` keeps the builder; see [Migrating from `CodingAgent`](../migration/coding-agent-to-harness.md) |

## Where to go next

1. **[Harness](../harness/index.md)** — the agent, its modes and approvals, the shell sandbox.
2. **[Dev tools](devtools.md)** — the toolset and the sandboxed `Workspace`.
3. **[CLI](cli.md)** — `code`, `goal` (durable/resumable), `ultracode`.
4. **[Workflows](workflows.md)** — parallel ultra-review on `adk-graph`.
5. **[Examples](examples.md)** — three runnable example crates.
6. **[`CodingAgent` (2.x)](harness.md)** — the superseded one-call agent, for existing projects.

See also the full design rationale in
[`docs/design/coding-agent.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/coding-agent.md).
