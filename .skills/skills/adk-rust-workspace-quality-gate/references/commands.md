# Command Matrix

Run from repository root:

```bash
cargo check --workspace --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Supplemental scans

```bash
rg -n "TODO|FIXME|HACK" adk-*/src --glob '!**/tests/**'
rg -n "unimplemented!\(|todo!\(" adk-*/src --glob '!**/tests/**'
```

## Severity rubric
- P0: data loss, security bypass, or crash on common path
- P1: merge-blocking correctness issue or broken gate
- P2: meaningful reliability, observability, or DX regression
