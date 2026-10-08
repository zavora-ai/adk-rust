#!/usr/bin/env bash
# Compile every standalone example crate under examples/.
#
# Each example is its own workspace with its own Cargo.lock and path
# dependencies back into this repo, so nothing in the root workspace build
# covers them: `cargo check --workspace` cannot see them and
# scripts/check-doc-examples.sh only validates that documented commands resolve
# in cargo metadata. Without this gate an example can stop compiling against a
# changed public API and no CI job notices.
#
# Usage:
#   scripts/check-examples-compile.sh              # every example
#   scripts/check-examples-compile.sh 0 4          # shard 0 of 4 (CI matrix)
#
# A shared CARGO_TARGET_DIR is used so the ADK crates and common dependencies
# are built once and reused across examples.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

SHARD_INDEX="${1:-0}"
SHARD_TOTAL="${2:-1}"

# Examples excluded from this gate (each entry names its own workflow/reason).
# Currently empty: the Monty example compiles here like any other since monty
# moved to crates.io and adk-codeact-monty joined the workspace.
SKIP_EXAMPLES=()

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/examples-check}"

# `mapfile` needs bash 4; stock macOS ships 3.2, so read the list in a loop.
MANIFESTS=()
while IFS= read -r manifest; do
    MANIFESTS+=("$manifest")
done < <(find examples -maxdepth 2 -name Cargo.toml | sort)

# Per-run scratch file so parallel shards never share one path.
ERR_FILE="$(mktemp "${TMPDIR:-/tmp}/example-check-err.XXXXXX")"
trap 'rm -f "$ERR_FILE"' EXIT

declare -a failed=()
declare -i checked=0 skipped=0 index=0

for manifest in ${MANIFESTS[@]+"${MANIFESTS[@]}"}; do
    dir="$(dirname "$manifest")"

    skip=0
    # `${arr[@]+...}` keeps `set -u` happy on an empty array under bash 3.2.
    for excluded in ${SKIP_EXAMPLES[@]+"${SKIP_EXAMPLES[@]}"}; do
        if [[ "$dir" == "$excluded" ]]; then skip=1; fi
    done
    if (( skip )); then
        printf 'skip     %s (own toolchain/workflow)\n' "$dir"
        skipped+=1
        continue
    fi

    if (( index % SHARD_TOTAL != SHARD_INDEX )); then
        index+=1
        continue
    fi
    index+=1

    # --locked also catches a lockfile that no longer matches its manifest.
    if cargo check --manifest-path "$manifest" --locked --quiet 2>"$ERR_FILE"; then
        printf 'ok       %s\n' "$dir"
    else
        printf 'FAILED   %s\n' "$dir"
        sed 's/^/         /' "$ERR_FILE" | head -20
        failed+=("$dir")
    fi
    checked+=1
done

printf '\nexamples checked: %d, skipped: %d, failed: %d' "$checked" "$skipped" "${#failed[@]}"
if (( SHARD_TOTAL > 1 )); then
    printf ' (shard %d of %d)' "$SHARD_INDEX" "$SHARD_TOTAL"
fi
printf '\n'

if (( ${#failed[@]} > 0 )); then
    printf '\nfailing examples:\n'
    printf '  %s\n' "${failed[@]}"
    exit 1
fi
