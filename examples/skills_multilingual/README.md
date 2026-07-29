# Multilingual Skill Matching

Demonstrates Unicode-aware skill selection across Chinese, Russian, and English. The agent dynamically selects the most relevant skill based on the user's query language and injects the skill's instructions into the prompt context.

Previously, non-ASCII characters in queries and skill metadata were silently dropped by the tokenizer, making skills written in CJK, Cyrillic, or accented Latin invisible to the selection engine. With Unicode-aware tokenization, skills in any language are correctly matched.

## Prerequisites

- Rust 1.95+
- `GOOGLE_API_KEY` environment variable set (for Scenario 6 live demo)

## Running

```bash
cargo run --manifest-path examples/skills_multilingual/Cargo.toml
```

## What It Does

1. **Loads multilingual skills** — 4 skills in Chinese (数据库操作, 电脑操作), Russian (поиск-документов), and English (code-review)
2. **Chinese DB query** → matches `数据库操作` skill (database operations)
3. **Chinese desktop query** → matches `电脑操作` skill (desktop control)
4. **Russian document query** → matches `поиск-документов` skill (document search)
5. **English code review query** → matches `code-review` skill
6. **Mixed query** → best match across languages
7. **Live agent demo** — injects the selected skill into a Gemini agent's context and runs a real query

## Skills Included

| File | Language | Purpose |
|------|----------|---------|
| `.skills/数据库操作.md` | Chinese | Database operations expert |
| `.skills/电脑操作.md` | Chinese | Desktop automation agent |
| `.skills/поиск-документов.md` | Russian | Document search assistant |
| `.skills/code-review.md` | English | Code review specialist |

## How It Works

The `adk-skill` tokenizer now recognizes non-ASCII alphanumeric characters:

- **Chinese**: Each CJK character becomes an individual token (`查`, `询`, `数`, `据`, `库`)
- **Russian**: Each Cyrillic character becomes an individual token (`п`, `о`, `и`, `с`, `к`)
- **English**: Words split on whitespace as before (`review`, `code`, `security`)

This enables cross-language skill matching where a Chinese query like `"查询数据库性能"` produces tokens that overlap with the Chinese skill's name, description, and body.

## Related

- [`adk-skill/src/select.rs`](../../adk-skill/src/select.rs) — Unicode tokenization implementation
- [`adk-skill/src/index.rs`](../../adk-skill/src/index.rs) — Unicode ID preservation
- [Issue #357](https://github.com/zavora-ai/adk-rust/issues/357) — Feature request
