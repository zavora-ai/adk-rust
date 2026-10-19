# adk-skill

AgentSkills parser, index, matcher, and runtime injection helpers for ADK-Rust.

[![Crates.io](https://img.shields.io/crates/v/adk-skill.svg)](https://crates.io/crates/adk-skill)
[![Documentation](https://docs.rs/adk-skill/badge.svg)](https://docs.rs/adk-skill)
[![License](https://img.shields.io/crates/l/adk-skill.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

## Overview

`adk-skill` is the engine for **specification-driven agent skills**, implementing the [**agentskills.io**](https://agentskills.io) specification. It discovers, parses, indexes, and selects skills defined as Markdown with YAML frontmatter, and injects the selected instructions into an agent's context.

This crate is provider-agnostic and can be used through:

- `adk-agent` (`LlmAgentBuilder::with_skills`, `with_auto_skills`, `with_skills_from_root`)
- `adk-runner` (`Runner::with_auto_skills`)
- `adk-cli` (`adk-rust skills list`, `validate`, `match`, and, with `vertex-skill-registry`, `search` and `pull`)
- Direct API calls from custom runtimes

## Installation

```toml
[dependencies]
adk-skill = "3.0"
```

Or use the meta-crate, where the `skills` feature (included in `standard`) re-exports it:

```toml
[dependencies]
adk-rust = { version = "3.0", features = ["skills"] }
```

| Feature | Description |
|---------|-------------|
| `progressive-disclosure` | `SkillToolset`: `list_skills`, `load_skill`, and `load_skill_resource` tools instead of injecting a body into every matching message |
| `vertex-skill-registry` | Read-only Vertex AI Skill Registry client (`registry` module) with verified, bounded archive extraction |

No feature is enabled by default.

## Supported File Conventions

### `agentskills.io` Skills (`.skills/**/*.md`, `.claude/skills/**/*.md`)

Each skill is a Markdown file with YAML frontmatter following the `agentskills.io` standard. This structure allows for both human-readable instructions and machine-readable governance.

```md
---
name: zenith-voice-receptionist
description: Official voice persona for Mario's Plumbing Co. receptionist.
version: "1.1.0"
license: MIT
compatibility: Gemini Live (gemini-3.1-flash-live-preview)
allowed-tools:
  - user_profile
  - knowledge
  - reasoning_engine
references:
  - references/technicians.json
---
You are Zenith, the voice receptionist for Mario's Plumbing...
```

#### Specification Fields

| Field | Required | Description |
| :--- | :--- | :--- |
| `name` | Yes | Unique identifier (lowercase, numbers, hyphens). |
| `description` | Yes | Concise summary of the skill's purpose for agent selection. |
| `version` | No | Semantic version of the skill. |
| `license` | No | License identifier (e.g., MIT, Apache-2.0). |
| `compatibility` | No | Environment or model constraints. |
| `tags` | No | List of discovery and filtering labels. |
| `allowed-tools` | No | Tools the agent is permitted to use for this skill: a list, or one string of names separated by spaces or commas (`allowed-tools: Read Grep`). |
| `references` | No | Supporting resources (JSON, CSV) the skill uses. |
| `trigger` | No | Marks a skill meant to be invoked explicitly by name; exposed as `SkillDocument::trigger`. `select_skills` and the injector do not act on it. |
| `hint` | No | UI guidance for user input. |
| `metadata` | No | Arbitrary key-value map for extensions. |
| `triggers` | No | File glob patterns for activation, such as `["*.rs", "Cargo.toml"]`; exposed as `SkillDocument::triggers`. |

#### Parsing Strictness

`adk-skill` validates a file according to its location:

*   **Strict Mode (`.skills/**/*.md`, `.claude/skills/**/*.md`)**:
    - **Mandatory**: `name` and `description` must be present and non-empty.
    - **Validation**: A file without them fails with a `SkillError` and is not indexed.
    - **Optional**: All other fields (version, license, tools, etc.) are optional and default to empty or `None`.
    - Files inside `references/`, `agents/`, and `scripts/` subdirectories are skill resources, not skills, and are skipped.
*   **Permissive Mode (Convention Files)**:
    - Files like `AGENTS.md` or `SOUL.md` treat frontmatter as **entirely optional**.
    - If frontmatter is missing, the parser derives the skill name from the filename and assigns default convention tags.

### Instruction Convention Files

The index loader also discovers and ingests these markdown files:

- `AGENTS.md` and `AGENT.md`
- `CLAUDE.md`
- `GEMINI.md`
- `COPILOT.md`
- `SKILLS.md`
- `SOUL.md` (repository root only)

For these files, frontmatter is optional:

- If valid frontmatter is present, it is used.
- Otherwise the file is parsed as plain markdown instructions and converted into a skill document with convention tags (for example `agents-md`, `claude-md`).

## Logic-as-Data

`adk-skill` treats agent behavior as configuration. By parsing `allowed-tools` and
`references`, a runtime can instantiate the toolset for an agent session from the active skill:

- **Role-Based Access Control**: Limit an agent's capabilities based on the active skill.
- **Pluggable Personalities**: Swap personas by changing the active skill.
- **Resource Injection**: Load the reference data a specific flow needs.

## What The Crate Does

### 1. Discovery

- Scans `<root>/.skills/` and `<root>/.claude/skills/` recursively for frontmatter skills.
- Scans `<root>` recursively for supported convention files.
- Skips common heavy directories (`.git`, `target`, `node_modules`, `dist`, `build`, etc.).
- Returns deterministic sorted file paths with de-duplication.

API: `discover_skill_files(root)`, `discover_instruction_files(root)`, and the `_with_extras`
variants that add directories outside the root.

### 2. Parsing + Validation

- Strict path (`.skills/**`): parses required frontmatter as YAML with validation.
- Convention path (`AGENTS.md`, `CLAUDE.md`, etc.): parses plain markdown (or frontmatter if provided).
- Returns actionable errors with file path context for strict frontmatter paths.

API: `parse_skill_markdown(path, content)`, `parse_instruction_markdown(path, content)`

### 3. Indexing

- Builds `SkillIndex` from discovered files.
- Computes:
  - content hash (`SHA-256`)
  - `last_modified` (Unix timestamp seconds when available)
  - stable document id: `normalized-name + first-12-hash-chars`
- Sorts documents deterministically by `(name, path)`.

API: `load_skill_index(root)`, `load_skill_index_with_extras(root, extra_dirs)`

### 4. Selection (Scoring)

`select_skills` is a deterministic, token-based relevance engine. For every query token found
in a skill field, the score increases by that field's weight:

| Field | Weight |
| :--- | :--- |
| **Name** | `+4.0` |
| **Description** | `+2.5` |
| **Tags** | `+2.0` |
| **Body** | `+1.0` |

**Normalization**: the raw score is divided by the square root of the number of unique tokens
in the body (at least 1): `FinalScore = RawScore / sqrt(unique_body_tokens)`, so a long
instruction body does not drown out a concise skill. Tokenization is Unicode-aware, so skills
written in CJK, Cyrillic, or accented Latin scripts match.

`SelectionPolicy` then filters and ranks the scores:

1. **Tag filters**: a skill with any tag in `exclude_tags` is dropped; with `include_tags` set,
   only skills carrying one of them remain.
2. **Threshold**: a skill scoring below `min_score` is discarded.
3. **Ranking**: matches are sorted by descending score, ties broken by name and then by file
   path, and the first `top_k` are returned.

`SelectionPolicy` defaults:

- `top_k = 1`
- `min_score = 1.0`
- `include_tags = []`
- `exclude_tags = []`

The selection uses no random seed or model, so the same query against the same index always
yields the same scores, and every `SkillMatch` carries the document's content-derived `id`.

API: `select_skills(index, query, policy)`

### 5. The Context Coordinator

`ContextCoordinator` runs **selection → tool validation → context construction** and returns
a `SkillContext` holding the system instruction and the resolved `Arc<dyn Tool>` instances
together. It checks every tool in a skill's `allowed-tools` against the host's
`ToolRegistry` (re-exported from `adk-core`): `ValidationMode::Strict` (default) rejects a
skill that names a missing tool, and `ValidationMode::Permissive` omits the missing tool. The
model is therefore never told to use a tool that is not bound.

`resolve` tries `ResolutionStrategy::ByName`, `ByQuery`, and `ByTag` in order and returns the
first context that resolves.

```rust
use adk_core::{Tool, ToolRegistry};
use adk_skill::{
    ContextCoordinator, CoordinatorConfig, ResolutionStrategy, SkillDraft, SkillWriter,
    load_skill_index,
};
use std::sync::Arc;

/// A registry with no tools: only skills without `allowed-tools` resolve in strict mode.
struct NoTools;

impl ToolRegistry for NoTools {
    fn resolve(&self, _tool_name: &str) -> Option<Arc<dyn Tool>> {
        None
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    SkillWriter::new(root.path()).write(
        &SkillDraft::new("triage", "Triage incoming support tickets.")
            .with_body("Classify each ticket by urgency."),
    )?;

    let index = Arc::new(load_skill_index(root.path())?);
    let coordinator = ContextCoordinator::new(index, Arc::new(NoTools), CoordinatorConfig::default());

    let context = coordinator
        .resolve(&[
            ResolutionStrategy::ByName("triage".into()),
            ResolutionStrategy::ByQuery("support ticket".into()),
        ])
        .expect("the triage skill resolves");
    assert!(context.system_instruction.contains("Classify each ticket"));
    assert!(context.active_tools.is_empty());
    Ok(())
}
```

### 6. Injection

Injection helpers prepend the selected skill body to user content using:

```text
[skill:<name>]
<skill body>
[/skill]
```

Then original user text follows.

Behavior:

- Injection runs only when `Content.role == "user"`.
- Query text is extracted from text parts and joined with newlines.
- Only the top match is injected.
- Injected body is truncated to `max_injected_chars` (2000 in `SkillInjectorConfig::default()`).

APIs:

- `select_skill_prompt_block(...)`
- `apply_skill_injection(...)`
- `SkillInjector` / `SkillInjectorConfig`
- `SkillInjector::build_plugin(...)`
- `SkillInjector::build_plugin_manager(...)`

## Quick Start

`tempfile` is used here to give each example its own skills root.

### Write, Load, and Match Skills

`SkillWriter` writes into the `.skills` directory `load_skill_index` discovers. Writes go through
a unique sibling temporary file, are synchronized, and atomically replace the destination on Unix
and Windows, so a crash mid-write cannot leave a half-written skill that breaks the index load.

```rust
use adk_skill::{SelectionPolicy, SkillDraft, SkillWriter, load_skill_index, select_skills};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let writer = SkillWriter::new(root.path());

    writer.write(
        &SkillDraft::new("disk-triage", "Diagnose low disk space. Use when a disk alert fires.")
            .with_body("1. Check the largest directories.\n2. Report the growth rate.")
            .with_tags(["ops"])
            .with_metadata_entry("incidents", serde_json::json!(["INC-1", "INC-2"])),
    )?;

    let index = load_skill_index(root.path())?;
    assert!(index.find_by_name("disk-triage").is_some());

    let policy = SelectionPolicy { min_score: 0.1, ..SelectionPolicy::default() };
    let matches = select_skills(&index, "the disk alert fired", &policy);
    assert_eq!(matches[0].skill.name, "disk-triage");
    println!("{} ({:.2})", matches[0].skill.name, matches[0].score);
    Ok(())
}
```

A skill name becomes a filename, so `validate_skill_name` enforces the specification's
`[a-z0-9-]` rule (1–64 characters, no leading or trailing hyphen). That check is also the
path-safety boundary: `../escape` and `nested/name` are rejected before any file is touched.

Use `with_metadata_entry` to record provenance, such as the incidents and timestamp behind a
skill an agent derived from experience, so a change in behaviour is auditable.

### Inject Into User Content

```rust
use adk_core::Content;
use adk_skill::{
    SelectionPolicy, SkillDraft, SkillWriter, apply_skill_injection, load_skill_index,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    SkillWriter::new(root.path()).write(
        &SkillDraft::new("todo-finder", "Find TODO markers in a repository.")
            .with_body("Search for TODO and FIXME comments and list them by file."),
    )?;

    let index = load_skill_index(root.path())?;
    let policy = SelectionPolicy { min_score: 0.1, ..SelectionPolicy::default() };
    let mut content = Content::new("user").with_text("Search this repository for TODO markers");

    let matched = apply_skill_injection(&mut content, &index, &policy, 1500);
    assert_eq!(matched.map(|m| m.skill.name), Some("todo-finder".to_string()));
    assert!(content.parts[0].text().unwrap_or_default().starts_with("[skill:todo-finder]"));
    Ok(())
}
```

### Build A Plugin Manager

```rust
use adk_skill::{SkillInjector, SkillInjectorConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let injector = SkillInjector::from_root(root.path(), SkillInjectorConfig::default())?;
    let _plugin_manager = injector.build_plugin_manager("skills");

    // A plugin keeps the index it was built from; rebuild it to pick up skills written since.
    let injector = injector.reloaded()?;
    let _plugin = injector.build_plugin("skills");
    Ok(())
}
```

`SkillInjector` holds an index snapshotted at construction, and `build_plugin` captures it by
handle — a plugin already handed to a runner keeps using the index it was built from.
`reloaded` rescans the root; it returns an error when the injector was built with
`from_index`, since there is no root to rescan.

## Progressive Disclosure (Google ADK-style)

Enable the optional `progressive-disclosure` feature to expose skills as a
context-aware `Toolset` rather than injecting an instruction body into every
matching user message. It provides three always-available tools:

1. `list_skills` — L1: names and descriptions only.
2. `load_skill` — L2: loads one skill's instructions and activates its declared
   `allowed-tools` for the next model turn.
3. `load_skill_resource` — L3: reads a file from a skill's `references/`,
   `assets/`, or `scripts/` directory.

`SkillToolset` automatically injects the progressive-disclosure protocol into
each model request, so individual agents do not need to repeat it in their
instructions. The protocol is generated at runtime from the tools actually
available to the toolset. Use `build_skill_system_instruction(...)` to generate
a variant for a prefixed or filtered toolset, or
`DEFAULT_SKILL_SYSTEM_INSTRUCTION` for the default exported value.

The activation record contains the skill's content ID and hash, so a changed or
removed skill cannot silently retain its old permissions. Resources require
activation by default and reject path traversal. Set
`ResourceAccessPolicy::GoogleCompatible` only when direct resource reads are
required for interoperability.

The block is `ignore` because it needs the `progressive-disclosure` feature.

```rust,ignore
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, ToolRegistry};
use adk_skill::{SkillToolset, SkillToolsetConfig, load_skill_index};
use std::sync::Arc;

fn agent(model: Arc<dyn Llm>, tools: Arc<dyn ToolRegistry>) -> Result<LlmAgent, Box<dyn std::error::Error>> {
    let index = Arc::new(load_skill_index(".")?);
    let skills = SkillToolset::new(index, tools, SkillToolsetConfig::default());
    Ok(LlmAgentBuilder::new("assistant").model(model).toolset(Arc::new(skills)).build()?)
}
```

`SkillInjector` and `ContextCoordinator` remain available, and the toolset can coexist with
them.

When both `progressive-disclosure` and `vertex-skill-registry` are enabled,
attach a `SkillRegistryClient` through `SkillToolset::with_registry_client`. This adds the
registry's semantic `search_skills` tool and fetches a selected package only on `load_skill`.
Local names take precedence. `load_skill` and `load_skill_resource` accept only a name that
passes `validate_skill_name`, or the client's own `{parent}/skills/{name}`, so a model cannot
reach another project's registry with the agent's credentials. Registry activations retain
their fully qualified resource name and content hash, so they can be validated on later
model turns.

## Vertex AI Skill Registry (`vertex-skill-registry`)

The `registry` module is a read-only client for the Vertex AI Skill Registry (v1beta1):
`SkillRegistryClient::new_with_adc(SkillRegistryConfig::new(project, location))` gets, lists,
searches, and downloads skill packages. `fetch_skill_content` verifies the archive's SHA-256
digest and extracts it in memory within the `MAX_ARCHIVE_BYTES`, `MAX_UNCOMPRESSED_BYTES`,
`MAX_ENTRIES`, `MAX_COMPRESSION_RATIO`, and `MAX_DIRECTORY_DEPTH` bounds. Skill and revision
names that are not a bare ID or a well-formed resource name are rejected before any request.
Creating, updating, and deleting skills is left to platform tooling.

## Error Model

Main error type: `SkillError`

- `Io`
- `Yaml`
- `InvalidFrontmatter { path, message }`
- `MissingField { path, field }`
- `InvalidSkillsRoot(path)`
- `Validation(message)`
- `IndexError(message)`
- `Registry(AdkError)` (with `vertex-skill-registry`)

Type alias: `SkillResult<T> = Result<T, SkillError>`

## Current Limits

- No embedding/vector retrieval (lexical matching only).
- `SkillInjector::reloaded` rescans the whole root; there is no file-watching or incremental reload.
- Progressive disclosure reads skill scripts as resources but does not execute them; use an
  `adk-sandbox` or `adk-code` integration to run them.

## Application Integration Patterns

- **Confidence threshold**: in high-stakes applications, raise `min_score` so the agent acts
  only on a strong match.
- **Ambiguity handling**: with `top_k > 1`, close scores can be presented to the user as a
  choice instead of injecting one skill.
- **Generalist fallback**: keep a broad skill with a tag such as `fallback`, and list
  `ResolutionStrategy::ByTag("fallback")` after `ByQuery`, so it resolves when no skill matches.
- **Observability**: log `SkillMatch.score` with `SkillMatch.skill.id` and `.hash`, so a
  change in selection can be traced to a change in a skill.

## Optimizing Selection Score

The selection engine uses weighted token matches, so metadata steers discovery:

- **Name** (+4.0): use specific, unique terms.
- **Description** (+2.5): include the keywords users' queries contain; keep descriptions of
  different skills distinct so two skills do not match the same queries.
- **Tags** (+2.0): add synonyms or broad categories (e.g., `[plumbing, drainage]`).
- **Body** (+1.0): keep it concise; a longer body increases the normalization divisor
  (`sqrt(unique_body_tokens)`).
- **`allowed-tools`**: declare the tools a skill needs, so the coordinator and the
  progressive-disclosure toolset bind only those.

## Related Examples

From this repository:

- [`examples/skills_multilingual`](https://github.com/zavora-ai/adk-rust/tree/main/examples/skills_multilingual) — Unicode-aware selection across Chinese, Russian, and English
- [`examples/skill_memory_improvements`](https://github.com/zavora-ai/adk-rust/tree/main/examples/skill_memory_improvements) — `.skills` indexing and lexical matching

## Development

```bash
cargo test -p adk-skill
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
