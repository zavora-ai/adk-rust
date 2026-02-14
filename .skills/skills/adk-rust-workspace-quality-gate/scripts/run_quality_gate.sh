#!/usr/bin/env bash
set -euo pipefail

OUT_DIR="output/adk-quality"
mkdir -p "$OUT_DIR"

echo "[1/3] cargo check"
cargo check --workspace --all-features 2>&1 | tee "$OUT_DIR/check.log"

echo "[2/3] cargo test"
cargo test --workspace --all-features 2>&1 | tee "$OUT_DIR/test.log"

echo "[3/3] cargo clippy"
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tee "$OUT_DIR/clippy.log"

echo "Quality gate completed. Logs: $OUT_DIR"
