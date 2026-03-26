# Action Nodes Example

Demonstrates all core action node types in `adk-graph` — deterministic, non-LLM programmatic nodes for workflow automation.

## Scenarios

| # | Node Type | What it demonstrates |
|---|-----------|---------------------|
| 1 | **Set** | Insert, deep-merge, and delete state variables; secret masking |
| 2 | **Transform** | Template interpolation (`{{var}}`), JSONPath extraction, type coercion |
| 3 | **Switch** | Conditional routing with `eq`, `contains` operators; default branches |
| 4 | **Loop** | `forEach` over arrays, `times` repetition, `while` conditions; result collection |
| 5 | **Merge** | `waitAll`/`waitN` with array, object, and first combine strategies |
| 6 | **Wait** | Fixed-duration delays, condition polling |
| 7 | **File** | Write/read JSON, write/read CSV, list directory with glob, delete |
| 8 | **Trigger** | Manual trigger with input, webhook metadata |
| 9 | **WorkflowSchema** | Load a complete graph from JSON and execute it |
| 10 | **Error Handling** | Continue mode, fallback values, skip conditions, timeout enforcement |

## Run

```bash
cargo run -p action-nodes-example
```

No external services or Docker required — all scenarios use in-process execution and temp files.

## Feature Flags

This example uses only the `action` feature (core nodes with zero extra dependencies beyond `adk-action`). For HTTP, database, email, RSS, and notification nodes, enable the corresponding feature flags:

```
action-http    → HTTP requests, notifications (reqwest)
action-db      → SQL databases (sqlx)
action-code    → JavaScript sandbox (quick-js)
action-email   → IMAP/SMTP (lettre, imap)
action-rss     → RSS/Atom feeds (feed-rs)
action-full    → All of the above
```
