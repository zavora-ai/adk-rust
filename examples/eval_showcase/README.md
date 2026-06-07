# Eval Showcase

Demonstrates all 10 eval-competitive-parity features from the `adk-eval` crate without requiring API keys or external services.

## Features Demonstrated

| # | Feature | What's shown |
|---|---------|-------------|
| 1 | **StructuredJudge** | `extract_json_from_text()` parsing from markdown fences, raw JSON, and prose |
| 2 | **CostTracker** | `compute_cost()` for multiple model families with token counts |
| 3 | **TraceAnalyzer** | `analyze_tool_calls()` detecting redundant calls, showing efficiency score |
| 4 | **BaselineStore** | `save()`, `load()`, `check_regressions()` with tolerance-based detection |
| 5 | **JunitReporter** | `generate()` producing valid JUnit XML from a sample evaluation report |
| 6 | **AnnotationStore** | JSONL export/import round-trip with human verdicts |
| 7 | **Wilcoxon** | `wilcoxon_signed_rank()` for A/B agent statistical significance |
| 8 | **TestGenerator** | `generate_from_events()` creating eval cases from event logs (no LLM) |
| 9 | **ConversationScorer** | Config and metrics struct creation, serialization |
| 10 | **EmbeddingScorer** | `cosine_similarity()` with identical, similar, orthogonal, and edge-case vectors |

## Running

```bash
cargo run -p eval-showcase
```

No environment variables or API keys are required — all demos use sample data and synchronous computations.

## Dependencies

- `adk-eval` with features: `embedding`, `ci-helpers`, `statistics`
- `adk-core` for core types (Event, Content, Llm trait)
- `adk-memory` with `embedding-trait` for the EmbeddingProvider trait
- `tokio` (runtime, though most demos are synchronous)
- `tempfile` for temporary baseline/annotation files

## Architecture

The example is structured as a series of independent demo functions called sequentially from `main()`. Each demo:

1. Creates sample data
2. Calls the relevant `adk-eval` API
3. Prints the results with clear section headers

This makes it easy to copy individual demos into your own projects.
