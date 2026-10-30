#!/usr/bin/env bash
# Compile and test the feature-gated modules the default workspace build skips.
#
# `cargo clippy --workspace` and `cargo nextest run --workspace` build every crate with
# its default features, so a module behind an opt-in feature is invisible to both. Each
# line of scripts/feature-coverage-pairs.txt names one such package/feature pair and the
# CI shard that covers it. This script lints every pair in a shard on its own (clippy,
# -D warnings, all targets), so each module must compile with only its own features on,
# then runs each crate's tests once with its pairs' features combined. One target
# directory serves the shard, so common dependencies compile once.
#
# A pair's optional fourth column holds comma-separated flags: `no-default` builds with
# `--no-default-features`, `doc` also runs the crate's doctests, which nextest skips, and
# `lib` lints the library alone and runs no tests, for a crate whose dev-dependency on
# itself turns its default features back on in every test target. A `solo` or
# `no-default` pair runs its tests on its own build instead of the combined one.
# A `-` in the features column passes no `--features`, so `- no-default` builds none.
#
# Environment:
#   AFFECTED_CRATES         space-separated package names whose pairs run (default: all)
#   FEATURE_COVERAGE_TESTS  `isolated` runs every pair's tests on its own build
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
AFFECTED=" ${AFFECTED_CRATES:-all} "
MODE="${FEATURE_COVERAGE_TESTS:-combined}"

if [[ "$SELECT" == "--list" ]]; then
    grep -vE '^\s*(#|$)' "$PAIRS"
    exit 0
fi

has_flag() { [[ ",$1," == *",$2,"* ]]; }

# Sets ARGS to the cargo feature arguments for <features> <flags>.
pair_args() {
    ARGS=()
    [[ "$1" != "-" ]] && ARGS=(--features "$1")
    has_flag "$2" no-default && ARGS=(--no-default-features ${ARGS[@]+"${ARGS[@]}"})
}

declare -a failed=()
run() { # run <label> <command...>
    local label="$1" started=$SECONDS
    shift
    printf '\n==> %s\n' "$label"
    if "$@" </dev/null; then
        printf 'ok       %s (%ds)\n' "$label" "$((SECONDS - started))"
    else
        printf 'FAILED   %s (%ds)\n' "$label" "$((SECONDS - started))"
        failed+=("$label")
    fi
}

# "<package> <features> <flags>" lines in file order. Plain `read` keeps this runnable on
# the bash 3.2 that stock macOS ships.
selected=""
declare -i in_shard=0
while IFS=' ' read -r shard package features flags; do
    [[ -z "$shard" || "$shard" == \#* ]] && continue
    [[ "$SELECT" == "all" || "$SELECT" == "$shard" ]] || continue
    in_shard+=1
    [[ "$AFFECTED" == " all " || "$AFFECTED" == *" $package "* ]] || continue
    selected+="$package $features ${flags:--}"$'\n'
done <"$PAIRS"

if ((in_shard == 0)); then
    printf 'error: no pairs match "%s" in %s\n' "$SELECT" "$PAIRS" >&2
    exit 2
fi
if [[ -z "$selected" ]]; then
    printf 'feature coverage: none of shard %s'"'"'s pairs belongs to an affected crate\n' "$SELECT"
    exit 0
fi

declare -i linted=0
while read -r package features flags; do
    [[ -n "$package" ]] || continue
    pair_args "$features" "$flags"
    targets=--all-targets
    has_flag "$flags" lib && targets=--lib
    run "clippy $package ${ARGS[*]-} $targets" \
        cargo clippy -p "$package" ${ARGS[@]+"${ARGS[@]}"} "$targets" -- -D warnings
    linted+=1
done <<<"$selected"

while read -r package; do
    combined=""
    doc=0
    while read -r pair_package features flags; do
        [[ "$pair_package" == "$package" ]] || continue
        has_flag "$flags" lib && continue
        if [[ "$MODE" == isolated ]] || has_flag "$flags" solo || has_flag "$flags" no-default; then
            pair_args "$features" "$flags"
            run "nextest $package ${ARGS[*]-}" cargo nextest run -p "$package" ${ARGS[@]+"${ARGS[@]}"}
            if has_flag "$flags" doc; then
                run "doctests $package ${ARGS[*]-}" \
                    cargo test -p "$package" ${ARGS[@]+"${ARGS[@]}"} --doc
            fi
            continue
        fi
        [[ "$features" == "-" ]] && continue
        combined="${combined:+$combined,}$features"
        has_flag "$flags" doc && doc=1
    done <<<"$selected"
    [[ -n "$combined" ]] || continue
    combined="$(tr ',' '\n' <<<"$combined" | awk '!seen[$0]++' | paste -sd, -)"
    run "nextest $package --features $combined" cargo nextest run -p "$package" --features "$combined"
    if ((doc)); then
        run "doctests $package --features $combined" \
            cargo test -p "$package" --features "$combined" --doc
    fi
done < <(awk '!seen[$1]++ { print $1 }' <<<"$selected")

printf '\nfeature coverage: %d pair(s) linted, %d step(s) failed\n' "$linted" "${#failed[@]}"
((${#failed[@]} == 0)) && exit 0
printf '  FAILED  %s\n' "${failed[@]}"
exit 1
