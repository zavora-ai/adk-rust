#!/usr/bin/env bash
# Installs `adk-rust up` as a real launchd agent (macOS) or systemd user unit (Linux) and
# proves what spec 3.2 §7.12 promises: the service manager starts the kernel as the user, it
# logs to an owner-only file in its home, it comes back after it is killed and after a
# restart with its process table intact, and --uninstall removes it.
#
# CI only (.github/workflows/kernel-service.yml): it installs a service for the current user.
#
# Usage: scripts/kernel-service-smoke.sh <path to adk-rust built with scripted-model>
set -euo pipefail

BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
WORK=$(mktemp -d)
KHOME="$WORK/kernel home"
PROJECT="$WORK/project"
PORT=18765
mkdir -p "$PROJECT"
printf '[{"text": "done"}]\n' > "$WORK/script.json"

adk() { "$BIN" --model-script "$WORK/script.json" "$@"; }
step() { printf '\n== %s\n' "$*"; }
fail() { printf 'FAILED: %s\n' "$*" >&2; [ -f "$KHOME/kernel.log" ] && tail -40 "$KHOME/kernel.log" >&2; exit 1; }

# Waits up to $1 seconds for the command after it to succeed.
wait_for() {
  local seconds=$1
  shift
  for _ in $(seq "$seconds"); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  return 1
}

# The service's command line, as the definition orders it; nothing else running has it.
kernel_pid() { pgrep -f -- "up --bind 127.0.0.1:$PORT --home $KHOME" | head -1; }
restarted() { local pid; pid=$(kernel_pid); [ -n "$pid" ] && [ "$pid" != "$FIRST" ]; }
stopped() { [ -z "$(kernel_pid)" ]; }
completed() { [ "$(adk ps --home "$KHOME" --state completed | grep -c '^[0-9a-f]\{8\}-')" -ge 1 ]; }

case "$(uname -s)" in
  Darwin)
    restart() { launchctl kickstart -k "gui/$(id -u)/$NAME"; }
    definitions() { echo "$HOME/Library/LaunchAgents"; }
    definition_file() { echo "$(definitions)/$NAME.plist"; }
    log_mode() { stat -f '%Lp' "$KHOME/kernel.log"; }
    ;;
  Linux)
    restart() { systemctl --user restart "$NAME.service"; }
    definitions() { echo "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"; }
    definition_file() { echo "$(definitions)/$NAME.service"; }
    log_mode() { stat -c '%a' "$KHOME/kernel.log"; }
    ;;
  *) fail "no service form on $(uname -s)" ;;
esac

step "--install --print renders the definition and installs nothing"
adk up --install --print --home "$KHOME" --dir "$PROJECT" --bind "127.0.0.1:$PORT" | tee "$WORK/definition"
grep -q -- "--home" "$WORK/definition" || fail "the definition does not run up --home"
if ls "$(definitions)"/adk-rust-kernel-* >/dev/null 2>&1; then fail "--print wrote a definition"; fi
[ ! -e "$KHOME" ] || fail "--print created the kernel home"

step "--install starts the kernel under the service manager"
adk up --install --home "$KHOME" --dir "$PROJECT" --bind "127.0.0.1:$PORT" | tee "$WORK/install"
NAME=$(grep -o 'adk-rust-kernel-[0-9a-f]\{16\}' "$WORK/install" | head -1)
[ -n "$NAME" ] || fail "--install named no service"
grep -q 'The kernel answers on' "$WORK/install" || fail "the installed kernel did not answer"
[ -f "$(definition_file)" ] || fail "no $(definition_file)"
adk ps --home "$KHOME"
[ "$(log_mode)" = 600 ] || fail "the log is mode $(log_mode), not 600"
wait_for 10 grep -q 'ADK-Rust kernel up' "$KHOME/kernel.log" || fail "the kernel's banner is not in its log"

step "a process the kernel runs survives what follows"
curl -fsS -X POST "http://127.0.0.1:$PORT/api/kernel/processes" \
  -H 'content-type: application/json' \
  -d '{"target": "adk-rust", "input": {"text": "say done"}}'
echo
wait_for 60 completed || fail "the spawned process did not complete"

step "a killed kernel is restarted by the service manager"
FIRST=$(kernel_pid)
[ -n "$FIRST" ] || fail "no kernel process"
kill -9 "$FIRST"
wait_for 60 restarted || fail "the kernel was not restarted after kill -9"
wait_for 60 adk ps --home "$KHOME" || fail "the restarted kernel does not answer"
completed || fail "the completed process is gone after the restart"

step "a restart through the service manager keeps the process table"
restart
wait_for 60 adk ps --home "$KHOME" || fail "the kernel does not answer after a restart"
completed || fail "the completed process is gone after the service restart"

step "--uninstall removes the service and keeps the home"
adk up --uninstall --home "$KHOME"
[ ! -e "$(definition_file)" ] || fail "$(definition_file) is still there"
wait_for 60 stopped || fail "the kernel still runs"
if adk ps --home "$KHOME" 2>/dev/null; then fail "a kernel still answers"; fi
[ -f "$KHOME/kernel.log" ] && [ -f "$KHOME/harness.db" ] || fail "the kernel home was not kept"
if adk up --uninstall --home "$KHOME" 2>"$WORK/again"; then fail "a second --uninstall succeeded"; fi
grep -q 'is installed' "$WORK/again" || fail "a second --uninstall did not say nothing is installed"

step "the kernel service installed, restarted and uninstalled as the user"
