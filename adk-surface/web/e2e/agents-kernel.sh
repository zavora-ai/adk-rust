#!/usr/bin/env bash
# The kernel the agents end-to-end test opens (spec 3.3 §5.7, §12 phase 4): `adk-rust up` with a
# scripted model whose first turn suggests a Finance agent. Run from the repository root by
# Playwright: agents-kernel.sh <port> <dir>.
#
# `AGENTS_E2E_ADK` (or the memory test's `MEMORY_E2E_ADK`) names an `adk-rust` built with
# `scripted-model` and `audit-sql`; otherwise this builds one, the same build the memory test
# makes. It replaces itself with the kernel, so stopping the test stops the kernel.
set -euo pipefail

port=$1
dir=$2
bin=${AGENTS_E2E_ADK:-${MEMORY_E2E_ADK:-}}
if [[ -z "$bin" ]]; then
    cargo build -p adk-cli --features scripted-model,audit-sql --bin adk-rust
    bin="${CARGO_TARGET_DIR:-target}/debug/adk-rust"
fi
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")

# The kernel's socket lives in its home, and a Unix socket path holds at most 104 bytes, so the
# home is a short path under /tmp rather than under the runner's temporary directory.
home=$(mktemp -d /tmp/adk-agents.XXXXXX)
workspace="$home/ws"
mkdir -p "$workspace/Invoices" "$dir"
echo "INV-7 due 31 March" > "$workspace/Invoices/march.txt"
printf '%s' "$home" > "$dir/home"

# An environment of its own, so nothing reads or writes the real config or keys.
isolated=(env -i PATH="$PATH" HOME="$home" XDG_CONFIG_HOME="$home/.config" USER=ana TERM=dumb
    RUST_LOG="${AGENTS_E2E_LOG:-warn}")

# ADK-Rust suggests Finance and says so; every later turn, Finance's included, answers in text.
cat > "$home/script.json" <<'EOF'
[
  {"call": {"name": "propose_agent", "args": {
    "name": "Finance",
    "whatItDoes": "Reads your invoices and drafts payment reminders",
    "instructions": "Read the invoices in Invoices. Draft a polite reminder for each overdue one. Never send them.",
    "mode": "askFirst",
    "uses": {"files": "read", "folder": "Invoices"}
  }}},
  {"text": "I suggested a Finance agent. It waits for you in Decisions."},
  {"text": "Finance here."},
  {"text": "Nothing else to do."},
  {"text": "Nothing else to do."}
]
EOF

cd "$workspace"
exec "${isolated[@]}" "$bin" up --bind "127.0.0.1:$port" --dir "$workspace" \
    --mode ask --model-script "$home/script.json" --no-consolidation < /dev/null
