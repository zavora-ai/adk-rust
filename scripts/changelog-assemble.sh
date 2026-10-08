#!/usr/bin/env bash
# Assemble changelog.d/ fragments into CHANGELOG.md; `--check` only validates them.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/changelog-assemble.py" "$@"
