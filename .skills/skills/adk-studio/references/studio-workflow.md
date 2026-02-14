# ADK Studio Workflow Reference

## Primary commands

```bash
cargo test -p adk-studio
cargo run -p adk-studio --example codegen_demo
cargo check --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Focused test selection

```bash
cargo test -p adk-studio codegen::
cargo test -p adk-studio server::graph_runner::
cargo test -p adk-studio schema::deploy::
```

## Typical failure surfaces
- Invalid route target names in generated graph logic.
- Missing env var requirements for provider/tool nodes.
- Interrupt/resume serialization mismatches in graph runner state.
- Generated code compiles but drifts from schema expectations.

## Review output format
- P1/P2 findings first.
- Each finding includes path and line reference.
- Include tests run, passed, and skipped.
