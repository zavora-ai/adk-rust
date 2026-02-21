# `adk-skill` Design Document

> This document explains the architectural decisions behind `adk-skill`.
> For usage and API reference, see [README.md](./README.md).

---

## 1. Philosophy: Context Engineering, Not Prompt Injection

`adk-skill` treats agent context as a **structured, strongly-typed envelope** rather than a concatenated string. Every skill defines both the LLM's cognitive frame (instructions) and its physical capabilities (tools). The library guarantees that these two facets are always delivered together — an LLM is never told it can do something without the corresponding tool being bound.

This philosophy is informed by the state of the art:

| Framework | Approach | Key Insight |
|:---|:---|:---|
| Semantic Kernel | Plugin = Instructions + Functions | Context and tools are inseparable |
| LangGraph | `bind_tools` per graph node | Tools are scoped to execution context |
| AutoTool / ATLASS | Score → Select → Bind → Execute | Selection is decoupled from invocation |

**Our position**: `adk-skill` combines the determinism of static configuration with the flexibility of dynamic scoring. Skills are authored as data (Markdown files), scored at runtime, and their metadata drives both prompt construction *and* tool binding.

---

## 2. The Scoring Algorithm

### Design Goal
Provide a **fast, deterministic, reproducible** relevance score that requires no external model calls, no embeddings, and no network I/O.

### Algorithm: Weighted Lexical Overlap
Given a user query and a skill document, the algorithm:

1. **Tokenizes** both into lowercase alphanumeric tokens.
2. **Scores** each query token against four skill fields with fixed weights:

| Field | Weight | Rationale |
|:---|:---:|:---|
| `name` | **+4.0** | Exact name match is the strongest signal |
| `description` | **+2.5** | Semantic intent, concise by convention |
| `tags` | **+2.0** | Curated discovery labels |
| `body` | **+1.0** | Broad content match, low-precision |

3. **Normalizes** by `sqrt(body_token_count)` to prevent long documents from drowning concise, targeted skills.

### Why Not Embeddings?
- **Determinism**: Same query + same index = same score. Always.
- **Testability**: You can write unit tests asserting that `"gas leak"` always selects `emergency-plumber` with `score > 5.0`.
- **Zero latency**: No API calls, no model loading, instant scoring.
- **Auditability**: The score is a transparent, explainable number — not a black-box cosine similarity.

### Score Significance Scale

| Score | Interpretation | Typical Action |
|:---|:---|:---|
| 0.0 – 0.9 | Trace / noise | Below default `min_score`, filtered out |
| 1.0 – 2.4 | Broad match | Usable for fallback / generalist routing |
| 2.5 – 4.9 | Specific match | High-confidence single-skill selection |
| 5.0+ | Strong match | Name + description + tags aligned |

---

## 3. The Toolpath Guarantee

### The Problem: "Phantom Tools"
If a skill's body tells the LLM *"Use the `transfer_call` tool to connect the caller"*, but `transfer_call` is not bound to the model's function-calling schema, the LLM will either hallucinate a fake tool call or fail silently. This is the single most dangerous failure mode in skill-driven agents.

### The Contract
`adk-skill` guarantees: **If a skill is selected, its `allowed-tools` metadata is carried through every output type.**

The `SkillMatch` returned by `select_skills` contains a `SkillSummary` which includes `allowed_tools: Vec<String>`. The consumer is responsible for resolving these names to concrete tool instances before passing the context to the agent.

### Consumption Tiers

`adk-skill` supports three tiers of consumption, from simple to fully guaranteed:

#### Tier 1: Prompt-Only Injection (Current `SkillInjector`)
```
Query → score → inject body into prompt
```
- ✅ Simple, works as an `adk-plugin`.
- ⚠️ Does **not** configure tools. The caller's existing toolset must already cover the skill's needs.
- **Use when**: The agent has a fixed, universal toolset.

#### Tier 2: Score + Manual Tool Binding (Current `data_plane`)
```
Query → lookup skill → read allowed_tools → match/dispatch → build agent
```
- ✅ Full toolpath guarantee.
- ⚠️ Requires hardcoded `match` blocks in the consumer.
- **Use when**: You have a known, finite set of tools.

#### Tier 3: ContextCoordinator (Ideal / Target Architecture)
```
Query → score → resolve tools via registry → emit SkillContext → build agent
```
- ✅ Full toolpath guarantee.
- ✅ No hardcoding — tools are resolved dynamically.
- **Use when**: Building a generic, multi-skill agent platform.

---

## 4. Target Architecture: The `ContextCoordinator`

### The `SkillContext` Envelope
The output of the coordination pipeline is a single, atomic unit:

```rust
pub struct SkillContext {
    /// Structured system instruction, framed with persona and constraints.
    pub system_instruction: String,
    /// Resolved, executable tools guaranteed to match the instruction.  
    pub active_tools: Vec<Arc<dyn Tool>>,
    /// Provenance metadata for observability and auditing.
    pub provenance: SkillSummary,
}
```

### The `ToolRegistry` Trait
Host applications register their available tool factories:

```rust
pub trait ToolRegistry: Send + Sync {
    fn resolve(&self, tool_name: &str) -> Option<Arc<dyn Tool>>;
    fn available_tools(&self) -> Vec<String>;
}
```

### Pipeline

```
┌──────────┐     ┌────────────┐     ┌──────────────┐     ┌──────────────┐
│  Query   │────▶│  Scoring   │────▶│  Validation  │────▶│   Context    │
│          │     │            │     │              │     │ Engineering  │
└──────────┘     │ select_    │     │ Check tools  │     │              │
                 │ skills()   │     │ exist in     │     │ Build system │
                 │            │     │ registry     │     │ instruction  │
                 │ Returns    │     │              │     │ + bind tools │
                 │ SkillMatch │     │ Reject if    │     │              │
                 │            │     │ incomplete   │     │ Emit         │
                 │            │     │              │     │ SkillContext │
                 └────────────┘     └──────────────┘     └──────────────┘
```

### Validation Rules
When a `SkillMatch` requests tools that the registry cannot satisfy:

1. **Strict mode** (default): Reject the match entirely. Fall through to the next-best skill.
2. **Permissive mode**: Bind available tools, omit missing ones, log a warning.
3. **Fallback mode**: If no skill fully satisfies, return a default/generalist context.

---

## 5. Design Decisions & Trade-offs

### Why Lexical Over Semantic Matching?
We chose lexical overlap because the primary consumer is a **voice gateway** where latency is critical (<500ms). Embedding models add 50–200ms per query. Lexical scoring runs in microseconds.

**Trade-off**: We sacrifice fuzzy matching ("my kitchen is underwater" → "flood") for speed and determinism. The `MatchType.semantic` extension exists in the routing layer for cases that need it.

### Why `allowed-tools` in Metadata, Not Code?
Defining tool permissions in the skill's YAML frontmatter keeps the **Logic-as-Data** principle: the skill file is the single source of truth for what the agent can say *and* do. This enables:
- Non-developers to author and modify skills.
- Version control and diff-ability of capability changes.
- Runtime selection without recompilation.

### Why Content Hashing for IDs?
IDs are derived from `name + SHA256(content)`. This means:
- Renaming a skill changes its ID (intentional — it's a different skill).
- Editing instructions changes the hash suffix (detectable by CI/audit systems).
- Two identical files in different locations produce the same ID (deduplication).

### Why `sqrt` Normalization?
Without normalization, a 10,000-word skill body accumulates many low-weight body matches and outscores a focused 100-word skill. `sqrt(n)` dampens this bias sub-linearly — long documents are still rewarded for breadth, but not proportionally to their length.

---

## 6. Relationship to Other Crates

```
adk-skill          ← This crate: scoring, discovery, context engineering
    ↕
adk-core           ← Content, Part, Tool trait definitions
    ↕
adk-plugin         ← Plugin system (SkillInjector wraps as a plugin)
    ↕
adk-agent          ← Agent runtime that consumes SkillContext
    ↕
data_plane         ← Voice Gateway host application (provides ToolRegistry)
```

`adk-skill` intentionally has **no dependency on any model provider** (no Gemini, no OpenAI). It is a pure scoring and context-construction library. Model-specific logic belongs in the consumer.
