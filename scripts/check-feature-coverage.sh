#!/usr/bin/env bash
# Compile and test the feature-gated modules the default workspace build skips.
#
# `cargo clippy --workspace` and `cargo nextest run --workspace` build every crate with
# its default features, so a module behind an opt-in feature is invisible to both. Each
# line of scripts/feature-coverage-pairs.txt names one such package/feature pair and the
# CI shard that covers it; this script runs clippy (-D warnings, all targets) and the
# tests for every pair in a shard, sharing one target directory so common dependencies
# compile once.
#
# Usage:
#   scripts/check-feature-coverage.sh all      # every pair
#   scripts/check-feature-coverage.sh 3        # shard 3 (CI matrix entry)
#   scripts/check-feature-coverage.sh --list   # print the pairs with their shard

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

PAIRS="$ROOT/scripts/feature-coverage-pairs.txt"
SELECT="${1:-all}"

if [[ "$SELECT" == "--list" ]]; then
    grep -vE '^\s*(#|$)' "$PAIRS"
    exit 0
fi

declare -a failed=()
declare -i ran=0

# Plain `read` keeps this runnable on the bash 3.2 that stock macOS ships.
while IFS=' ' read -r shard package features; do
    [[ -z "$shard" || "$shard" == \#* ]] && continue
    [[ "$SELECT" == "all" || "$SELECT" == "$shard" ]] || continue
    ran+=1

    printf '\n==> %s --features %s (shard %s)\n' "$package" "$features" "$shard"
    if cargo clippy -p "$package" --features "$features" --all-targets -- -D warnings \
        && cargo nextest run -p "$package" --features "$features"; then
        printf 'ok       %s --features %s\n' "$package" "$features"
    else
        printf 'FAILED   %s --features %s\n' "$package" "$features"
        failed+=("$package --features $features")
    fi
done < "$PAIRS"

if (( ran == 0 )); then
    printf 'error: no pairs match "%s" in %s\n' "$SELECT" "$PAIRS" >&2
    exit 2
fi

printf '\nfeature coverage: %d pair(s) checked, %d failed\n' "$ran" "${#failed[@]}"
for entry in ${failed[@]+"${failed[@]}"}; do
    printf '  FAILED  %s\n' "$entry"
done
(( ${#failed[@]} == 0 ))
