#!/usr/bin/env bash
# Run the PR tier locally, scoped to what the current branch changes against a base.
#
# CI runs every job on every change; a maintainer rebasing a contributor's branch wants
# the same verdict in minutes. The script maps the changed files to workspace crates,
# adds every crate that depends on them, and runs the gates that can fail for that set:
#
#   fmt                      whole workspace, always
#   clippy / nextest         changed crates and their dependents
#   feature coverage         pairs from scripts/feature-coverage-pairs.txt whose package changed
#   semver                   changed Stable-tier crates, when cargo-semver-checks is installed
#   shell scripts and YAML   changed scripts and workflows
#   documentation gates      the `templates` job's checks, always
#   standalone examples      with --examples, every example that depends on a changed crate
#
# Usage:
#   scripts/validate-pr.sh                 # against origin/main
#   scripts/validate-pr.sh main --examples # against a local ref, examples included

set -uo pipefail

# The repository of the current directory, not of the script file: a maintainer runs
# this inside whichever worktree holds the branch under validation.
ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "not inside a git repository" >&2; exit 2; }
cd "$ROOT" || exit 1
export RUSTC_WRAPPER="${RUSTC_WRAPPER:-sccache}"
# Every worktree of the repository shares the main checkout's target directory: a
# workspace build with tests is tens of gigabytes, and one per worktree fills a disk.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$(git rev-parse --path-format=absolute --git-common-dir)/../target}"

BASE="origin/main"
WITH_EXAMPLES=0
for arg in "$@"; do
    case "$arg" in
        --examples) WITH_EXAMPLES=1 ;;
        --*) printf 'unknown option %s\n' "$arg" >&2; exit 2 ;;
        *) BASE="$arg" ;;
    esac
done

mapfile_compat() { # bash 3.2 has no mapfile
    while IFS= read -r line; do [ -n "$line" ] && printf '%s\n' "$line"; done
}

CHANGED_FILES="$(git diff --name-only "$BASE...HEAD" | mapfile_compat)"
if [ -z "$CHANGED_FILES" ]; then
    echo "no changes against $BASE"
    exit 0
fi
printf 'changes against %s: %s file(s)\n' "$BASE" "$(printf '%s\n' "$CHANGED_FILES" | grep -c .)"

# Workspace crates touched by the change, and every crate that depends on them.
CRATE_PLAN="$(python3 - "$CHANGED_FILES" <<'PY'
import json, subprocess, sys
from pathlib import Path
changed = sys.argv[1].split("\n")
meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1"], capture_output=True, text=True, check=True).stdout)
root = Path(meta["workspace_root"])
members = {p["id"]: p for p in meta["packages"] if p["id"] in set(meta["workspace_members"])}
by_dir = {}
for pid, p in members.items():
    rel = Path(p["manifest_path"]).parent.relative_to(root)
    by_dir[str(rel)] = pid
direct = set()
for f in changed:
    for d, pid in by_dir.items():
        if f == d or f.startswith(d + "/"):
            direct.add(pid)
names = {pid: p["name"] for pid, p in members.items()}
rdeps = {pid: set() for pid in members}
for node in meta["resolve"]["nodes"]:
    if node["id"] not in members: continue
    for dep in node["deps"]:
        if dep["pkg"] in members:
            rdeps[dep["pkg"]].add(node["id"])
affected, frontier = set(direct), list(direct)
while frontier:
    pid = frontier.pop()
    for d in rdeps[pid]:
        if d not in affected:
            affected.add(d); frontier.append(d)
print(" ".join(sorted(names[p] for p in direct)))
print(" ".join(sorted(names[p] for p in affected)))
PY
)"
DIRECT="$(printf '%s\n' "$CRATE_PLAN" | sed -n '1p')"
AFFECTED="$(printf '%s\n' "$CRATE_PLAN" | sed -n '2p')"
printf 'crates changed: %s\ncrates to check: %s\n' "${DIRECT:-none}" "${AFFECTED:-none}"

declare -a RESULTS=()
step() { # step <name> <command...>
    local name="$1"; shift
    printf '\n==> %s\n' "$name"
    if "$@"; then RESULTS+=("ok      $name"); else RESULTS+=("FAILED  $name"); fi
}

step "fmt" cargo fmt --all -- --check

if [ -n "$AFFECTED" ]; then
    # shellcheck disable=SC2046,SC2086
    step "clippy ($AFFECTED)" cargo clippy $(printf -- '-p %s ' $AFFECTED) --all-targets -- -D warnings
    # shellcheck disable=SC2046,SC2086
    step "nextest ($AFFECTED)" cargo nextest run $(printf -- '-p %s ' $AFFECTED)
fi

for crate in $DIRECT; do
    while IFS=' ' read -r shard package features; do
        [[ -n "$shard" && "$shard" != \#* && "$package" == "$crate" ]] || continue
        step "feature coverage $package --features $features" bash -c \
            "cargo clippy -p '$package' --features '$features' --all-targets -- -D warnings && cargo nextest run -p '$package' --features '$features'"
    done < scripts/feature-coverage-pairs.txt
done

STABLE="adk-core adk-agent adk-model adk-gemini adk-tool adk-runner adk-session adk-server adk-graph adk-memory adk-anthropic"
if command -v cargo-semver-checks >/dev/null 2>&1; then
    for crate in $DIRECT; do
        case " $STABLE " in *" $crate "*) step "semver $crate" cargo semver-checks check-release -p "$crate" --default-features --release-type minor ;; esac
    done
elif [ -n "$DIRECT" ]; then
    printf '\nnote: cargo-semver-checks is not installed; the semver gate runs in CI only\n'
fi

SCRIPTS="$(printf '%s\n' "$CHANGED_FILES" | grep -E '\.sh$' | tr '\n' ' ')"
if [ -n "$SCRIPTS" ] && command -v shellcheck >/dev/null 2>&1; then
    # shellcheck disable=SC2086
    step "shellcheck" shellcheck $SCRIPTS
fi
WORKFLOWS="$(printf '%s\n' "$CHANGED_FILES" | grep -E '^\.github/.*\.ya?ml$' | tr '\n' ' ')"
if [ -n "$WORKFLOWS" ]; then
    # shellcheck disable=SC2086
    step "workflow YAML" python3 -c 'import sys, yaml; [yaml.safe_load(open(p)) for p in sys.argv[1:]]' $WORKFLOWS
fi

step "documentation gates" bash -c '
    bash scripts/check-example-name-collisions.sh &&
    bash scripts/check-doc-examples.sh &&
    bash scripts/check-doc-versions.sh &&
    bash scripts/check-release-consistency.sh &&
    bash scripts/changelog-assemble.sh --check &&
    bash scripts/check-publish-order.sh'

if [ "$WITH_EXAMPLES" -eq 1 ] && [ -n "$DIRECT" ]; then
    export CARGO_TARGET_DIR="$CARGO_TARGET_DIR/examples-check"
    for manifest in examples/*/Cargo.toml; do
        for crate in $DIRECT; do
            if grep -qE "^$crate\s*=" "$manifest"; then
                step "example $(dirname "$manifest")" cargo check --locked --manifest-path "$manifest"
                break
            fi
        done
    done
fi

printf '\n== summary ==\n'
printf '%s\n' "${RESULTS[@]}"
! printf '%s\n' "${RESULTS[@]}" | grep -q '^FAILED'
