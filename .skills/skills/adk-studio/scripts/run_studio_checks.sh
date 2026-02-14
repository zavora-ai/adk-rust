#!/usr/bin/env bash
set -euo pipefail

cargo test -p adk-studio
cargo run -p adk-studio --example codegen_demo
cargo check --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "ADK Studio checks complete"
