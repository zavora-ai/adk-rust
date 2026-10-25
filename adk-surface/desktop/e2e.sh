#!/usr/bin/env bash
# Runs one WebKitGTK test of the desktop shell (app/tests/webkitgtk.rs, spec 3.3 §5.5) under
# Xvfb and a D-Bus session, with tauri-driver started where the shell reads its kernel and token.
#
# Usage: e2e.sh <test> <kernel origin> <kernel command> [token]
# The kernel command may name `{workspace}`, which the test replaces with a new directory.
# Writes screenshots, logs and the JSON report under $DESKTOP_E2E_OUT/<test>.
set -euo pipefail

test=$1
origin=$2
kernel=$3
token=${4:-}
here=$(cd "$(dirname "$0")" && pwd)
out=${DESKTOP_E2E_OUT:?set DESKTOP_E2E_OUT}/$test
mkdir -p "$out"

export DESKTOP_E2E_SERVER=$origin DESKTOP_E2E_KERNEL=$kernel DESKTOP_E2E_TOKEN=$token
export DESKTOP_E2E_OUT=$out DESKTOP_E2E_SHELL_LOG=$out/shell.log
# The shell is started by the driver, so it reads these from the driver's environment.
export ADK_DESKTOP_SERVER=$origin ADK_DESKTOP_LOG=$out/shell.log
if [ -n "$token" ]; then export ADK_SERVE_TOKEN=$token; else unset ADK_SERVE_TOKEN; fi

# shellcheck disable=SC2016
dbus-run-session -- xvfb-run -a -s '-screen 0 1440x900x24' bash -c '
  tauri-driver --port 4444 > "$DESKTOP_E2E_OUT/driver.log" 2>&1 &
  driver=$!
  trap "kill $driver 2>/dev/null || true" EXIT
  for _ in $(seq 1 50); do curl -fsS -o /dev/null http://127.0.0.1:4444/status && break; sleep 0.2; done
  cargo test --manifest-path "$1/Cargo.toml" -p adk-desktop --test webkitgtk --locked -- \
    --ignored --exact "$2" --nocapture
' _ "$here" "$test"
