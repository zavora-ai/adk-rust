#!/usr/bin/env bash
# The kernel the memory end-to-end test opens (spec 3.3 §5.1, §12 phase 3): `adk-rust up` with
# a scripted model, in a home of its own, after a scripted session left the facts the page
# corrects. Run from the repository root by Playwright: memory-kernel.sh <port> <dir>.
#
# `MEMORY_E2E_BIN` (or `MEMORY_E2E_ADK`) names an `adk-rust` built with `scripted-model` and
# `audit-sql`; otherwise this builds one and runs the binary cargo names, wherever cargo's
# configuration puts its target directory. It writes the home it made to `<dir>/home` and the
# binary to `<dir>/bin`, so the test can export and verify the kernel's signed record as
# `adk-rust audit verify` does. The kernel runs as its child: stopping the test (SIGTERM)
# stops the kernel, then removes the home.
set -euo pipefail

port=$1
dir=$2
bin=${MEMORY_E2E_BIN:-${MEMORY_E2E_ADK:-}}
if [[ -z "$bin" ]]; then
    # Cargo's own artifact message names the binary, so a `build.target-dir` or a target
    # triple set in its configuration never leaves a stale or missing one in `target/debug`.
    bin=$(cargo build -p adk-cli --features scripted-model,audit-sql --bin adk-rust \
        --message-format=json-render-diagnostics |
        python3 -I -c '
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "adk-rust" and message.get("executable"):
        print(message["executable"])
')
    [[ -n "$bin" ]] || { echo "cargo built no adk-rust binary" >&2; exit 1; }
fi
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")

# The kernel's socket lives in its home, and a Unix socket path holds at most 104 bytes, so the
# home is a short path under /tmp rather than under the runner's temporary directory.
home=$(mktemp -d /tmp/adk-mem.XXXXXX)
trap 'rm -rf "$home"' EXIT
workspace="$home/ws"
mkdir -p "$workspace" "$dir"
echo "notes for the memory end-to-end test" > "$workspace/README.md"
printf '%s' "$home" > "$dir/home"
printf '%s' "$bin" > "$dir/bin"

# An environment of its own, so nothing reads or writes the real config or keys.
isolated=(env -i PATH="$PATH" HOME="$home" XDG_CONFIG_HOME="$home/.config" USER=ana TERM=dumb
    RUST_LOG="${MEMORY_E2E_LOG:-warn}")

# One session that ends with four facts: one the person said, three the agents guessed.
facts='{"facts":[
  {"text":"You bill [CLIENT] at [RATE] an hour","kind":"preference","source":"user_stated","quote":"bill [CLIENT] at [RATE] an hour","confidence":0.95,"supersedes":""},
  {"text":"Your accountant is [NAME]","kind":"project_fact","source":"tool_observed","quote":"","confidence":0.8,"supersedes":""},
  {"text":"You send invoices on the last working day","kind":"procedure","source":"tool_observed","quote":"","confidence":0.7,"supersedes":""},
  {"text":"Your bank is [BANK]","kind":"project_fact","source":"tool_observed","quote":"","confidence":0.6,"supersedes":""}]}'
seed=$(FACTS="$facts" python3 -c 'import json, os; print(json.dumps([{"text": "Noted."}, {"text": json.dumps(json.loads(os.environ["FACTS"]))}]))')
(cd "$workspace" && "${isolated[@]}" "$bin" code "From now on bill [CLIENT] at [RATE] an hour." \
    --dir "$workspace" --model-script "$seed" < /dev/null > "$home/seed.log" 2>&1)

# Each turn writes notes/a.txt, then says so; a refused write is answered without a retry.
cat > "$home/script.json" <<'EOF'
[
  {"call": {"name": "write_file", "args": {"path": "notes/a.txt", "content": "A"}}},
  {"text": "Note a is written."},
  {"call": {"name": "write_file", "args": {"path": "notes/a.txt", "content": "A"}}},
  {"text": "I did not write it."},
  {"text": "Nothing else to do."},
  {"text": "Nothing else to do."}
]
EOF

cd "$workspace"
"${isolated[@]}" "$bin" up --bind "127.0.0.1:$port" --dir "$workspace" \
    --mode ask --model-script "$home/script.json" --no-consolidation < /dev/null &
kernel=$!
# The home holds the kernel's database and signing key: removed once the kernel has stopped.
trap 'kill "$kernel" 2>/dev/null || true; wait "$kernel" 2>/dev/null || true; rm -rf "$home"' EXIT
trap 'exit 143' TERM INT HUP
wait "$kernel"
