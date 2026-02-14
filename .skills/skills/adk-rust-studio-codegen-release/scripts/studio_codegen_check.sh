#!/usr/bin/env bash
set -euo pipefail

cargo run -p adk-studio --example codegen_demo
cargo check --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "Studio codegen and release checks completed."
