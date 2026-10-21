#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# The generated projects and their one shared target directory live on the runner's
# main disk (RUNNER_TEMP on GitHub Actions): /tmp there carries a quota that the
# projects' builds overflow.
SCRATCH="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/adk-templates.XXXXXX")"
CHECK_TARGET_DIR="$SCRATCH/target"
trap 'rm -rf "$SCRATCH"' EXIT

# The generated projects are checked from TMPDIR, outside the repository where
# rustup would otherwise stop seeing rust-toolchain.toml and use a global
# default. Keep this gate on the same pinned compiler as the workspace.
if command -v rustup >/dev/null 2>&1; then
  RUST_TOOLCHAIN="$(awk -F'"' '/^channel = / { print $2; exit }' "$ROOT/rust-toolchain.toml")"
  export RUSTUP_TOOLCHAIN="${RUST_TOOLCHAIN}"
fi

# Entries are "template:provider[:addon1,addon2]".
# An empty provider means "omit --provider" (exercises the template's
# default provider, e.g. openai for the openai template).
templates=(
  "basic:gemini"
  "basic:openai"
  "basic:anthropic"
  "tools:gemini"
  "tools:anthropic"
  "rag:gemini"
  "api:gemini"
  "api:openai"
  "openai:"
  "graph:gemini"
  "realtime:"
  "a2a:gemini"
  "tools:gemini:telemetry,sessions"
  "llm:gemini:server,sessions"
  "harness:gemini"
  "harness:openai"
  "harness:anthropic"
  "harness:gemini:telemetry,mcp"
)

for entry in "${templates[@]}"; do
  IFS=':' read -r template provider addons <<<"$entry"
  name="adk_${template}_${provider:-default}_check"
  if [[ -n "$addons" ]]; then
    name="${name}_addons"
  fi

  args=(adk new "$name" --template "$template")
  if [[ -n "$provider" ]]; then
    args+=(--provider "$provider")
  fi
  if [[ -n "$addons" ]]; then
    IFS=',' read -ra addon_list <<<"$addons"
    for addon in "${addon_list[@]}"; do
      args+=(--addon "$addon")
    done
  fi

  (
    cd "$SCRATCH"
    cargo run --manifest-path "$ROOT/Cargo.toml" -p cargo-adk -- "${args[@]}"

    cat >> "$name/Cargo.toml" <<PATCH

[patch.crates-io]
adk-rust = { path = "$ROOT/adk-rust" }
adk-tool = { path = "$ROOT/adk-tool" }
adk-rag = { path = "$ROOT/adk-rag" }
adk-core = { path = "$ROOT/adk-core" }
adk-agent = { path = "$ROOT/adk-agent" }
adk-model = { path = "$ROOT/adk-model" }
adk-runner = { path = "$ROOT/adk-runner" }
adk-session = { path = "$ROOT/adk-session" }
adk-telemetry = { path = "$ROOT/adk-telemetry" }
adk-gemini = { path = "$ROOT/adk-gemini" }
adk-anthropic = { path = "$ROOT/adk-anthropic" }
adk-rust-macros = { path = "$ROOT/adk-rust-macros" }
adk-graph = { path = "$ROOT/adk-graph" }
adk-realtime = { path = "$ROOT/adk-realtime" }
adk-devtools = { path = "$ROOT/adk-devtools" }
PATCH

    # Every generated project builds into the shared target directory without debug
    # info, so the dependencies compile once and the build stays within the disk.
    export CARGO_TARGET_DIR="$CHECK_TARGET_DIR" CARGO_PROFILE_DEV_DEBUG=0
    cargo check --manifest-path "$name/Cargo.toml"
    # The harness template ships a scripted smoke test that needs no API key.
    if [[ "$template" == "harness" ]]; then
      cargo test --manifest-path "$name/Cargo.toml"
    fi
  )
done
