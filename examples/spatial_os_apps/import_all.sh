#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${ADK_SPATIAL_OS_URL:-http://127.0.0.1:8199}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for app_dir in "$ROOT_DIR"/*; do
  if [[ ! -d "$app_dir" ]]; then
    continue
  fi

  echo "Importing $(basename "$app_dir")"
  curl -sS -X POST "${BASE_URL}/api/os/apps/import" \
    -H "content-type: application/json" \
    -d "{\"path\":\"${app_dir}\",\"source\":\"sample_pack\",\"on_conflict\":\"upsert\"}"
  echo
  echo

done
