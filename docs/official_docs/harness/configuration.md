# Harness configuration

A harness is configured in three places: the builder in your code, the project file
`.adk/harness.toml`, and Cargo features. The CLI adds flags and a user config on top; see
[The harness in your terminal](cli.md#configuration-files).

## The builder

`HarnessAgent::builder(name)` returns a `HarnessAgentBuilder`. `model` and `workspace` are
required; everything else has a default.

### Core

| Method | Default | Effect |
|---|---|---|
| `model(llm)` | required | The model |
| `workspace(Workspace)` | required | The directory file tools are confined to; `bash` is offered when the workspace allows it (`Workspace::allow_bash(true)`) |
| `description(text)` | none | The agent description |
| `base_prompt(text)` | `HARNESS_SYSTEM_PROMPT` | Replaces the base prompt |
| `instruction(text)` | none | Operator text after the base prompt, not template-expanded |
| `project_instructions(InstructionsConfig)` | `AGENTS.md` from the git root down, `~/.adk/AGENTS.md`, trust required | Which instruction files load |
| `workspace_trust(store)` | in memory | Where workspace trust decisions are kept |
| `max_iterations(n)` | 100 | Most model turns per run |
| `tool_timeout(duration)` | 300 s | Timeout of one tool call |
| `history_max_events(n)` | all | Most recent events loaded per run |
| `compaction(config)` / `without_compaction()` | interval 10, overlap 4 | Event compaction for the harness's runner |
| `tool(tool)` / `toolset(toolset)` | none | Extra tools, governed as `External` |
| `customize_llm(f)` | none | The escape hatch to the underlying `LlmAgentBuilder`; its rustdoc states what it can bypass |

### Modes and approvals

| Method | Default | Effect |
|---|---|---|
| `mode(HarnessMode)` | `Ask` | The mode of sessions with no mode record |
| `mode_store(store)` | in memory | Where session modes are kept |
| `standing_rules(store)` | in memory | Where standing rules are kept |
| `operator_policy(policy)` | none | An operator `ToolPolicy`; the stricter decision wins |
| `prompter(prompter)` | none | Asks a person on interactive surfaces |
| `confirmation_handler(handler)` | the prompting handler | Replaces the confirmation handler |
| `approval_store(store)` | in memory | Where held calls and decisions are kept |
| `approval_service(service)` | none | The approval inbox held calls are submitted to |
| `confirmation_timeout(duration)` | 300 s | How long a person may take; a timeout denies |
| `trust_mcp_read_only_hints(bool)` | `false` | Trusts an MCP server's `readOnlyHint` for external tools |
| `require_authenticated_identity()` | off | Standing rules and `auto` apply only to authenticated users (served harnesses) |
| `config(HarnessConfig)` | none | Applies `.adk/harness.toml`; see [below](#the-project-file) |

See [Modes and approvals](modes-and-approvals.md).

### Shell

| Method | Default | Effect |
|---|---|---|
| `shell(ShellIsolation)` | `Host` | Where `bash` runs: the host, or `OsSandbox(..)` with feature `sandbox` |
| `fail_if_unavailable()` | off | Refuses to build, and each call, when the requested sandbox is unavailable |
| `auto_allow_sandboxed()` | off | Runs sandboxed commands without approval in `ask`, where the sandbox keeps protected paths read-only |
| `allow_unsandboxed_retry(bool)` | `true` | Offers a non-waivable unsandboxed retry of a failed sandboxed command |
| `allow_host_shell_in_auto()` | off | Runs host commands without approval in `auto` |
| `shell_ledger(ledger)` | in memory | Where the shell ledger is kept |
| `action_ledger(ledger)` | in memory | Where non-idempotent calls are recorded before they run |
| `without_shell()` | off | Leaves `bash` out |

See [Shell and sandbox](shell-and-sandbox.md) and [Durability](durability.md).

### Work tools

| Method | Default | Effect |
|---|---|---|
| `without_todos()` | off | Leaves `write_todos` and `update_todo` out |
| `skills(SkillsConfig)` | progressive, from `.skills/`, `.claude/skills/`, `.adk/skills/`, `~/.adk/skills/` | Skills (feature `skills`) |
| `member(MemberSpec)` | `explore` and `general` | Adds a team member (feature `team`) |
| `without_default_members()` | off | Leaves out `explore` and `general` |
| `member_registry(registry)`, `team_policy(policy)`, `member_model(llm)` | none | Discovery, the team's budget and termination, and the members' model |
| `plan_graph(bool)` | `true` with feature `graph` | Compiles approved plans with steps into a checkpointed graph |
| `plan_checkpointer(checkpointer)` | in memory | Where plan graphs are checkpointed |
| `goal_store(store)`, `goal_engine(GoalEngine)` | in memory; `Loop`, or `Graph` with a plan checkpointer | Where goals are kept and how they iterate |
| `memory(MemoryConfig)` | off | Working notes (feature `memory`) |
| `long_term_memory(config)`, `fact_store(store)`, `fact_index(index)`, `memory_embedder(embedder)` | off; in memory; a local lexical index; none | Long-term facts with provenance (feature `memory`) |
| `effort(EffortPolicy)` | plan high, members low, else medium | The reasoning effort per mode and role (spec 3.1 §7.23); surfaces build clients at it, `HarnessAgent::effort` reads it |
| `cache_capable(model)` / `without_context_caching()` | none; on | A Gemini model the runner caches explicitly with `ContextCacheConfig::default()` (spec 3.1 §7.23) |

See [Team](team.md), [Plans and goals](plan-and-goals.md) and [Memory](memory.md).

### Budget, identity and observability

| Method | Default | Effect |
|---|---|---|
| `budget(RunBudget)` | none | Resource limits for every run |
| `spend_ledger(ledger)` | none | The ledger model calls reserve budget against |
| `audit_log(log)` | an ephemeral signed log with feature `auth` | Where audit records go; the runner's own log takes precedence |
| `loop_guard(LoopGuard)` | off | Tightens a looping session to `read_only` and ends the turn (feature `observe`) |
| `rbac(RbacMap)` | none | A permission per tool class (feature `auth`) |
| `credentials(broker)`, `shell_credential(resource)` | none | Per-call shell credentials (feature `auth`) |

See [Identity](identity.md) and [Observability](observability.md).

## The project file

`.adk/harness.toml` in the workspace is read by `HarnessConfig::discover(dir)` and applied
with `HarnessAgentBuilder::config` (spec 3.1 §8.3). The file sits in the workspace the agent
works in, so it may only tighten (spec 3.1 §9.2 I3):

| Key | Applied |
|---|---|
| `mode` | When it is stricter than the mode set on the builder so far; an unknown mode reads as `read_only` |
| `sandbox = true` | Runs `bash` in the OS sandbox |
| `fail_if_unavailable = true` | Refuses to build without the sandbox |
| `[deploy]` | Not by the harness: deployment tooling reads it |
| `yes`, `allow_unsandboxed`, `trust`, `packs`, `members` | Never: they would loosen what the agent may do. Set them in code |

Ignored keys and a looser mode are logged as warnings and listed in `HarnessConfig::notes`.
A missing file is the default configuration.

```rust
use std::sync::Arc;

use adk_rust::harness::{HarnessAgent, HarnessConfig, HarnessMode, InstructionsConfig};
use adk_rust::model::{ScriptedLlm, ScriptedResponse};

fn main() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("harness-config-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".adk"))?;
    std::fs::write(dir.join(".adk/harness.toml"), "mode = \"plan\"\nyes = true\n")?;

    let config = HarnessConfig::discover(&dir)?;
    assert_eq!(config.notes.len(), 1); // `yes` is ignored
    let harness = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("done")])))
        .workspace(adk_devtools::Workspace::new(&dir))
        .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
        .config(config)
        .build()?;
    assert_eq!(harness.info().default_mode, HarnessMode::Plan);
    Ok(())
}
```

The CLI reads the same file with the same rule, plus `<config dir>/adk-rust/harness.toml`
for settings only you may make; see [Configuration files](cli.md#configuration-files).

## Features

On the umbrella crate:

| Feature | Effect |
|---|---|
| `harness` | `adk_rust::harness` (everything `adk-harness` exports) and `HarnessAgent`, `HarnessAgentBuilder`, `HarnessMode` in the prelude; turns on `agents`, `tools`, `skills`, `sandbox`, `runner`, `sessions` and `graph`. In `full` |
| `harness` with `cli` | `adk_rust::harness` also re-exports `HarnessLauncher`, `ConsolePrompter` and `ConsoleIo` from `adk-cli` |
| `harness-payments` | The payments pack: `HarnessAgentBuilder::pack_payments`; see [Payments](payments.md) |
| `harness-browser`, `harness-computer-use`, `harness-voice`, `harness-managed` | The other packs; the builder refuses them with `not yet implemented` until they land |
| `harness-encrypted`, `harness-memory-local` | The encrypted store and the local ONNX embedder (spec 3.1 §7.12, §7.15.2) |
| `mistralrs`, `cli-mistralrs` | `adk-mistralrs` in the umbrella, and `adk-rust --provider mistralrs`; a heavy build, in no tier |

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["harness", "cli"] }
```

On `adk-harness`:

| Feature | Default | Adds |
|---|---|---|
| `runner` | on | `Harness`, which owns a `Runner` |
| `skills` | on | Progressive skill disclosure |
| `team` | on | Team members |
| `graph` | on | The plan graph and the `Graph` goal engine |
| `sandbox` | on | The OS sandbox for `bash` |
| `observe` | on | Spans, spend and the loop guard |
| `auth` | on | RBAC, credentials and the signed audit log |
| `memory` | on | Working notes and long-term facts |
| `memory-embeddings` | off | OpenAI and Gemini embedders for the fact index |
| `memory-embeddings-local` | off | `RagEmbedder::local()`: an in-process ONNX embedder (`BAAI/bge-small-en-v1.5`); `ort` downloads the onnxruntime binary at build time |
| `encrypted` | off | The store envelope: `StoreKeyring`, `StoreKeySource`, sealed sessions, approvals and SQLite stores, key rotation (spec 3.1 §7.12) |
| `sqlite` | off | SQLite mode, rule, shell-ledger and goal stores |
| `ambient` | off | Goals started by cron, file-watch and webhook triggers |
| `team-acp`, `team-a2a` | off | ACP-compatible coding agents and remote A2A agents as members |
| `docker` | off | The container shell, `ShellIsolation::Container` (see [Shell and sandbox](shell-and-sandbox.md#the-container-shell)) |
| `skills-registry`, `memory-graph` | off | Registry skills, graph memory |

## The harness template

`cargo adk new my-harness --template harness` scaffolds a project around the harness
(spec 3.1 §8.3), on Gemini by default or with `--provider openai` or `--provider anthropic`:

| File | Purpose |
|---|---|
| `src/main.rs` | Builds the harness over SQLite sessions in `.adk/state.db`, with `ConsolePrompter` and `.config(HarnessConfig::discover(".")?)`, and runs it with `HarnessLauncher` |
| `.adk/harness.toml` | The project file, with a `[deploy]` table |
| `AGENTS.md` | Project instructions, loaded once you trust the workspace |
| `.skills/example/SKILL.md` | An example skill, valid for `adk-rust skills validate` |
| `tests/harness_smoke.rs` | A scripted test that plan mode refuses a write; no API key |
| `README.md`, `.gitignore` | Usage, and `.adk/state.db*` kept out of git |

```bash
cargo adk new my-harness --template harness
cd my-harness
cp .env.example .env     # add GOOGLE_API_KEY
cargo test               # the scripted smoke test
cargo run                # the REPL, ask mode, approvals in the terminal
cargo run -- serve       # the harness on 127.0.0.1:8080
```

The template takes the `telemetry`, `mcp` and other addons, and refuses `server` (the
launcher serves) and `docker` (the launcher refuses a non-loopback address without an
authenticating extractor, which the container would need).
