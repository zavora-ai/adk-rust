#!/usr/bin/env bash
# Checks that adk-surface's generated OpenAPI document and TypeScript types match the Rust
# types they come from (spec 3.3 §6); `--write` regenerates them.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ "${1:-}" == "--write" ]]; then
  ADK_SURFACE_WRITE_API=1 cargo test -p adk-surface --features web --test api_contract --quiet
  ADK_SURFACE_WRITE_API=1 cargo test -p adk-surface --features website --test website_api_contract --quiet
  ADK_SURFACE_WRITE_API=1 cargo test -p adk-surface --features web,verb-ui --test verbs_api_contract --quiet
  ADK_SURFACE_WRITE_API=1 cargo test -p adk-surface --features web,workflow --test workflow_api_contract --quiet
  echo "wrote adk-surface/web/src/generated"
  exit 0
fi

cargo test -p adk-surface --features web --test api_contract --quiet
cargo test -p adk-surface --features website --test website_api_contract --quiet
cargo test -p adk-surface --features web,verb-ui --test verbs_api_contract --quiet
cargo test -p adk-surface --features web,workflow --test workflow_api_contract --quiet
git diff --exit-code -- adk-surface/web/src/generated
