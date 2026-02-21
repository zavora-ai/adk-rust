# adk-skill

AgentSkills parser, index, matcher, and runtime injection helpers for ADK-Rust.

[![Crates.io](https://img.shields.io/crates/v/adk-skill.svg)](https://crates.io/crates/adk-skill)
[![Documentation](https://docs.rs/adk-skill/badge.svg)](https://docs.rs/adk-skill)
[![License](https://img.shields.io/crates/l/adk-skill.svg)](LICENSE)

## Overview

`adk-skill` is the engine for **specification-driven agent skills**, implementing the [**agentskills.io**](https://agentskills.io) specification. It provides the core building blocks to discover, parse, and index skill metadata, enabling agents to dynamically configure their behavior based on structured Markdown definitions.

This crate is provider-agnostic and can be used through:

- `adk-agent` (`LlmAgentBuilder::with_skills*`)
- `adk-runner` (`Runner::with_auto_skills`)
- Direct API calls from custom runtimes

## Supported File Conventions

### `agentskills.io` Compliance (`.skills/**/*.md`)

Each skill is a Markdown file with YAML frontmatter following the `agentskills.io` standard. This structure allows for both human-readable instructions and machine-readable governance.

```md
---
name: zenith-voice-receptionist
description: Official voice persona for Mario's Plumbing Co. receptionist.
version: "1.1.0"
license: MIT
compatibility: Gemini Live 2.5 Flash Native Audio
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
| `allowed-tools` | No | List of tools the agent is permitted to use for this skill. |
| `references` | No | External assets (JSON, CSV) required by the skill. |
| `trigger` | No | If true, requires explicit `@name` invocation. |
| `hint` | No | UI guidance for user input. |
| `metadata` | No | Arbitrary key-value map for extensions. |

#### Parsing Strictness

`adk-skill` employs a dual-validation strategy based on the file's location:

*   **Strict Mode (`.skills/**/*.md`)**: 
    - **Mandatory**: `name` and `description` must be present and non-empty.
    - **Validation**: Failing to provide these fields will result in a `SkillError`, and the file will not be indexed.
    - **Optional**: All other fields (version, license, tools, etc.) are optional and will default to empty or `None`.
*   **Permissive Mode (Convention Files)**:
    - Files like `AGENTS.md` or `SOUL.md` treat frontmatter as **entirely optional**.
    - If frontmatter is missing, the parser automatically derives the skill name from the filename and assigns default convention tags.

### Instruction Convention Files

The index loader also discovers and ingests these markdown files:

- `AGENTS.md` and `AGENT.md`
- `CLAUDE.md`
- `GEMINI.md`
- `COPILOT.md`
- `SKILLS.md`
- `SOUL.md` (root-level)

For these files, frontmatter is optional:

- If valid frontmatter is present, it is used.
- Otherwise the file is parsed as plain markdown instructions and converted into a skill document with convention tags (for example `agents-md`, `claude-md`).

## Logic-as-Data

The core philosophy of `adk-skill` is that **Agent behavior should be controlled by configuration, not just code.**

By parsing `allowed-tools` and `references`, the runtime (e.g., the `data_plane`) can dynamically instantiate the appropriate toolset for an agent session. This enables:
- **Role-Based Access Control**: Limit an agent's capabilities based on the active skill.
- **Pluggable Personalities**: Swap personas by simply changing the active skill metadata.
- **Resource Injection**: Automatically load the correct reference data for specific flows.

## What The Crate Does

### 1. Discovery

- Scans `<root>/.skills/` recursively for frontmatter skills.
- Scans `<root>` recursively for supported convention files.
- Skips common heavy directories (`.git`, `target`, `node_modules`, etc.).
- Returns deterministic sorted file paths with de-duplication.

API: `discover_skill_files(root)`
API: `discover_instruction_files(root)`

### Tool Discovery & Validation

While `adk-skill` handles the **declaration** of tools via `allowed-tools`, the **implementation** and **validation** are managed via core ADK traits.

- **`ToolRegistry` (Core)**: In [adk-core](file:///home/michael/src/voice_gateway/zenith/adk-rust/adk-core), use the `ToolRegistry` trait to map string identifiers (e.g., `user_profile`) to concrete `Arc<dyn Tool>` implementations.
- **`ValidationMode` (Core)**: Control whether the framework should strictly enforce tool availability or allow permissive binding.
- **Selective Injection**: Use the `ContextCoordinator` to filter available tools against a skill's `allowed_tools` list, ensuring the agent only sees authorized capabilities.

Example Flow:
1. `adk-skill` parses `allowed-tools: [weather]`.
2. Runtime looks up `"weather"` in its local registry.
3. Runtime injects the `WeatherTool` into the LLM context.

### 2. Parsing + Validation

- Strict path (`.skills/**`): parses required frontmatter as YAML with validation.
- Convention path (`AGENTS.md`, `CLAUDE.md`, etc.): parses plain markdown (or frontmatter if provided).
- Returns actionable errors with file path context for strict frontmatter paths.

API: `parse_skill_markdown(path, content)`
API: `parse_instruction_markdown(path, content)`

### 3. Indexing

- Builds `SkillIndex` from discovered files.
- Computes:
  - content hash (`SHA-256`)
  - `last_modified` (Unix timestamp seconds when available)
  - stable document id: `normalized-name + first-12-hash-chars`
- Sorts documents deterministically by `(name, path)`.

API: `load_skill_index(root)`

### 4. The Context Coordinator (Context Engineering)

The `ContextCoordinator` is the high-level engine that orchestrates the **Context Engineering Pipeline**. It bridges the gap between skill *selection* and agent *execution*, ensuring that any instruction given to the LLM is backed by concrete, validated capabilities.

#### The Role
1. **Orchestration**: It runs the full flow: `Selection` → `Validation` → `Engineering`.
2. **Preventing "Phantom Tools"**: It verifies that every tool listed in a skill's `allowed-tools` metadata exists in the host's `ToolRegistry`. If a tool is missing, it can reject the match (Strict) or omit the tool (Permissive), preventing the LLM from hallucinating an action it cannot perform.
3. **Atomic Delivery**: It emits a `SkillContext`, which encapsulates the final system instruction and the collection of executable `Arc<dyn Tool>` instances as a single unit.

#### Resolution Strategies
The coordinator supports a cascading **Resolution Strategy** pattern, allowing for flexible skill loading:
- **ByName**: Load a specific skill for dedicated flows.
- **ByQuery**: Find the best skill for a user's intent.
- **ByTag**: Find a skill categorised with specific labels (e.g., "emergency", "fallback").

API: `ContextCoordinator::new(index, registry, config)`
API: `coordinator.resolve(&[Strategy::ByName("..."), Strategy::ByQuery("...")])`

### 5. Selection (Scoring & Consumption)

The `select_skills` function implements a deterministic, token-based relevance engine. It calculates a score for each skill based on the query and then consumes that score to rank and filter the results.

#### The Scoring Algorithm

Relevance is determined by weighted lexical overlap. For every token in the query that is found in a skill field, the score increases by a specific weight:

| Field | Weight | Rationale |
| :--- | :--- | :--- |
| **Name** | `+4.0` | Exact name matches are highly intentional. |
| **Description** | `+2.5` | Concise summaries are the primary driver for relevance. |
| **Tags** | `+2.0` | Explicit labels provide strong categorization. |
| **Body** | `+1.0` | Mentions in instructions are relevant but can be noisy. |

**Normalization**: To prevent long-form instruction sets from unfairly drowning out concise skills, the raw score is divided by the square root of the unique tokens in the body: `FinalScore = RawScore / sqrt(unique_tokens)`.

#### Score Significance & Expression

Because the score is expressed as a weighted lexical distance, its significance should be interpreted as **Strength of Intent**:

-   **`0.0 - 0.9` (Trace)**: Incidental token overlap. Likely irrelevant to the user's current goal.
-   **`1.0 - 2.4` (Broad Match)**: Weak relevance. Found in the body or tags, but missing from primary identifiers (Name/Description).
-   **`2.5 - 4.9` (Specific Match)**: Strong relevance. Usually indicates a match in the `description` or multiple tags.
-   **`5.0+` (High Confidence)**: Direct hit. Indicates a match in the `name` or high-density overlap across all fields.

#### The Runtime Lifecycle of a Score

In a running application, the score follows this logical flow:

1.  **Query Arrival**: The user sends a message (e.g., "I need a plumber for a leak").
2.  **Lexical Calculation**: The `select_skills` engine tokenizes the query and performs a weighted lookup across the `SkillIndex`. Initial scores are generated for all candidate skills.
3.  **Policy Enforcement**: The `SelectionPolicy` is applied. 
    -   *Filtering*: If "Plumbing" scores `4.5` but the policy requires `min_score: 5.0`, it is discarded.
    -   *Clipping*: The sorted list is trimmed to `top_k`.
4.  **Action/Injection**:
    -   **Automated**: If a match survives, its `body` is injected into the prompt context using `apply_skill_injection`.
    -   **Manual/UI**: The application inspects the `SkillMatch.score` to decide whether to trigger a tool, log a warning, or ask for user confirmation.

API: `select_skills(index, query, policy)`

The skill engine uses the calculated score in two primary ways:

1.  **Filtering**: Any skill with a score below the `SelectionPolicy.min_score` (default: `1.0`) is immediately discarded. This ensures agents only receive highly relevant instructions.
2.  **Ranking & Top-K**: Resulting matches are sorted by descending score. If scores are tied, the engine applies deterministic **Tie-Breaking**:
    -   Lexicographical sort by **Name**.
    -   Lexicographical sort by **File Path**.

The engine then takes the top `top_k` results (default: `1`) for injection or inspection.

API: `select_skills(index, query, policy)`

`SelectionPolicy` defaults:

- `top_k = 1`
- `min_score = 1.0`
- `include_tags = []`
- `exclude_tags = []`

### 5. The Reliability Contract

What "makes the score well used" in a production app is its **predictability** and **reproducibility**.

-   **Deterministic Selection**: The lexical matching algorithm contains no random seeds or opaque model weights. The same query against the same skill index will *always* yield the same score.
-   **Stable Identification**: Every `SkillMatch` includes a unique `id` derived from the content hash. If the score changes, it's because the instructions (the data) changed.
-   **Policy-Enforced Boundaries**: By using `SelectionPolicy`, the application defines a "Safety Contract" where no agent ever receives instructions below a verified relevance threshold.

This transparency allows you to build **unit tests for your agent's persona**:
```rust
#[test]
fn verification_of_emergency_triage() {
    let index = load_skill_index("skills")?;
    let matches = select_skills(&index, "gas leak", &SelectionPolicy::default());
    assert!(matches[0].score > 5.0, "Emergency skill must have high confidence for 'gas leak'");
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
- Injected body is truncated to `max_injected_chars`.

APIs:

- `select_skill_prompt_block(...)`
- `apply_skill_injection(...)`
- `SkillInjector` / `SkillInjectorConfig`
- `SkillInjector::build_plugin(...)`
- `SkillInjector::build_plugin_manager(...)`

## Quick Start

### Load and Match Skills

```rust
use adk_skill::{SelectionPolicy, load_skill_index, select_skills};

let index = load_skill_index(".")?;
let policy = SelectionPolicy {
    top_k: 1,
    min_score: 0.1,
    include_tags: vec![],
    exclude_tags: vec![],
};

let matches = select_skills(&index, "find TODO markers in code", &policy);
for m in matches {
    println!("{} ({:.2})", m.skill.name, m.score);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Inject Into User Content

```rust
use adk_core::Content;
use adk_skill::{SelectionPolicy, apply_skill_injection, load_skill_index};

let index = load_skill_index(".")?;
let policy = SelectionPolicy { min_score: 0.1, ..SelectionPolicy::default() };
let mut content = Content::new("user").with_text("Search this repository for TODO markers");

let matched = apply_skill_injection(&mut content, &index, &policy, 1500);
if let Some(m) = matched {
    println!("Injected skill: {}", m.skill.name);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Build A Plugin Manager

```rust
use adk_skill::{SkillInjector, SkillInjectorConfig};

let injector = SkillInjector::from_root(".", SkillInjectorConfig::default())?;
let plugin_manager = injector.build_plugin_manager("skills");
# let _ = plugin_manager;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Error Model

Main error type: `SkillError`

- `Io`
- `Yaml`
- `InvalidFrontmatter { path, message }`
- `MissingField { path, field }`
- `InvalidSkillsRoot(path)`

Type alias: `SkillResult<T> = Result<T, SkillError>`

## Current Limits

- No embedding/vector retrieval (lexical matching only).
- No incremental file reload API yet.
- No remote catalog (`skills-ref`/MCP) in this crate yet.
- No script/file reference execution layer in this crate (selection + injection only).
- No standard CLI for skill management (use `adk-cli` wrapper if available).

## Application Integration Patterns

To ensure scores are well-consumed and the system is "designed for an app," consider these production-grade patterns:

### 1. The "Confidence Gatekeeper"
Don't always accept the top-1 match. In high-stakes applications (e.g., medical or financial), use a higher `min_score` (e.g., `5.0`) to ensure the agent only acts when it is highly confident in the match. 

### 2. Ambiguity Handling (The "Menu" Pattern)
If the top 3 skills have very close scores (e.g., within 0.5 of each other), instead of injecting one and potentially being wrong, use the scores to present a "Menu" to the user:
> "I found a few skills that could help. Would you like to use the **Account Recovery** skill or the **Security Update** skill?"

### 3. The "Generalist Fallback"
Always maintain a "Generalist" skill with broad tags and no strict `allowed-tools`. If `select_skills` returns an empty list, fall back to this base instruction set to ensure the agent doesn't simply fail or hallucinate a persona.

### 4. Observability & Evaluation
Log the `SkillMatch.score` along with the `SkillDocument.hash` for every interaction. This allows you to perform "Offline Evaluation":
- Re-run queries against newer instructor sets.
- Detect "Score Drift" where changes to a popular skill's description cause it to lose relevance for core queries.

## Best Practices for Skill Authors

### 1. Specification vs. Runtime Split
Maintain two versions of high-stakes skills:
- **`SKILL.md` (Design-Time)**: The comprehensive specification, including rationale, edge cases, and compliance data. Store this in your source-controlled `skills/` directory.
- **`.skills/<name>.md` (Runtime)**: An optimized, "distilled" version of the instructions. Strip out noise and focus on "Loud" instructions that the LLM can follow with minimal latency.

### 2. Explicit Tooling (Logic-as-Data)
Always define `allowed-tools` if your runtime supports dynamic loading. It acts as a safety barrier and reduces "tool hallucination" where the model tries to use a tool it isn't authorized for.

### 3. Descriptive Metadata
The `description` field is the primary driver for selection. If multiple skills are being matched, ensure descriptions are mutually exclusive to avoid "prompt pollution" where two conflicting personas are injected simultaneously.

### 5. Optimizing Selection Score

Because the selection engine uses weighted token matches, you can "steer" discovery by optimizing your metadata:
- **Title Power**: Use specific, unique terms in the `name` field (+4.0 weight).
- **Keyword Density**: Ensure the `description` (+2.5) contains the primary keywords you expect in users' queries.
- **Tag Categorization**: Use `tags` (+2.0) for synonyms or broad-brush categories (e.g., `[plumbing, drainage]`) that might not be in the name.
- **Normalization Strategy**: Keep your instructional `body` concise. A very long body increases the normalization factor (`sqrt(tokens)`), which can slightly penalize the overall score compared to a punchy, focused skill.

## Related Examples

From this repository:

- `examples/skills_llm_minimal`
- `examples/skills_auto_discovery`
- `examples/skills_policy_filters`
- `examples/skills_runner_injector`
- `examples/skills_workflow_minimal`

## Development

```bash
cargo test -p adk-skill
```

## License

Apache-2.0
