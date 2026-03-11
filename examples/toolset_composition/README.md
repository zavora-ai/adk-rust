# Toolset Composition Example

Demonstrates the reusable toolset composition utilities from the browser production hardening spec.

## Features

- **FilteredToolset** — wrap any toolset and filter tools by predicate (allow-list, custom logic)
- **MergedToolset** — combine multiple toolsets into one with deduplication
- **PrefixedToolset** — namespace tool names to avoid collisions
- **Full composition** — chain Prefix → Filter → Merge for complex configurations
- **BasicToolset** — group static tools into a named toolset
- **string_predicate** — convenience predicate for allow-listing by tool name

## Requirements

`GOOGLE_API_KEY` environment variable set.

## Running

```bash
cargo run --example toolset_composition
```
