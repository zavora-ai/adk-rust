#!/usr/bin/env bash
# The page the app-shell layout test opens (spec 3.3 §5.1, §5.5): the web home's example,
# `fixture.html`, served by Vite with every screen over the example's data and no kernel.
# Run from the repository root by Playwright: fixture-server.sh <port> <dir>. It replaces
# itself with Vite, so stopping the test stops the server.
set -euo pipefail

port=$1
cd "$(dirname "$0")/.."
exec npx vite --port "$port" --strictPort --host 127.0.0.1
