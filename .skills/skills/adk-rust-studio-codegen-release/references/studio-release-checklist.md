# Studio Release Checklist

## Codegen validation
```bash
cargo run -p adk-studio --example codegen_demo
```

## Workspace gate
```bash
cargo check --workspace --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Release evidence
- generated file list
- build/test/lint status
- open risks with severity and owner
