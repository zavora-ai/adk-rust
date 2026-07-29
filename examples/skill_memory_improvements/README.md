# Skill and Memory Improvements Example

Validation example covering the skill-discovery and memory APIs added by the skill-memory-improvements spec.

## What This Shows

- **`Memory::add()` / `delete()`** — direct memory mutation from `adk-core`
- **Skill discovery** — `.skills` indexing and lexical matching
- **Prompt injection helpers** — compact skill blocks injected into instructions

## Prerequisites

- **Rust 1.95+** (edition 2024)
- No API key required

## Run

```bash
cargo run --manifest-path examples/skill_memory_improvements/Cargo.toml
```
