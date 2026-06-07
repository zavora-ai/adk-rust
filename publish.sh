#!/bin/zsh
# Publish all workspace crates to crates.io in correct dependency order.
# Waits 10s after new publishes, 1s for skips.
#
# Dependency graph (verified May 2026):
#   Tier 1: adk-core, awp-types (no internal deps)
#   Tier 2: depends only on Tier 1
#   Tier 3: depends on Tier 1-2
#   Tier 4: depends on Tier 1-3
#   Tier 5: depends on Tier 1-4
#   Tier 6: depends on Tier 1-5
#   Tier 7: depends on Tier 1-6
#   Tier 8: depends on Tier 1-7
#   Tier 9: umbrella (depends on everything)

CRATES=(
  # Tier 1: no internal deps
  adk-core
  awp-types

  # Tier 2: depends on adk-core only (or awp-types)
  adk-telemetry
  adk-memory
  adk-artifact
  adk-plugin
  adk-guardrail
  adk-gemini
  adk-anthropic
  adk-rust-macros
  adk-sandbox
  adk-action
  adk-deploy
  adk-mistralrs    # depends on adk-core + adk-telemetry (now publishable)
  adk-awp          # depends on awp-types + adk-core

  # Tier 3: depends on Tier 2
  adk-skill        # depends on adk-plugin
  adk-code         # depends on adk-sandbox
  adk-realtime     # depends on adk-gemini (optional)
  adk-session      # depends on adk-core
  adk-model        # depends on adk-gemini, adk-telemetry

  # Tier 4: depends on Tier 3
  adk-tool         # depends on adk-code (optional), adk-rust-macros, adk-telemetry
  adk-browser      # depends on adk-core
  adk-audio        # depends on adk-realtime (optional)

  # Tier 5: depends on Tier 4
  adk-agent        # depends on adk-tool (dev), adk-skill, adk-plugin, adk-telemetry
  adk-graph        # depends on adk-action (optional)
  adk-runner       # depends on adk-session, adk-artifact, adk-plugin, adk-skill

  # Tier 6: depends on Tier 5
  adk-eval         # depends on adk-runner, adk-model
  adk-rag          # depends on adk-core
  adk-server       # depends on adk-runner, adk-agent
  adk-retry-reflect # depends on adk-runner, adk-plugin
  adk-bench        # depends on adk-runner, adk-model, adk-eval

  # Tier 7: depends on Tier 6
  adk-auth         # depends on adk-server (optional)
  adk-acp          # depends on adk-runner (optional server feature)
  cargo-adk        # depends on adk-deploy

  # Tier 8: depends on Tier 7
  adk-cli          # depends on adk-server, adk-deploy
  adk-payments     # depends on adk-auth
  adk-enterprise   # depends on adk-core (experimental)
  adk-managed      # depends on adk-runner, adk-session (experimental)

  # Tier 9: umbrella — always last
  adk-rust
)

echo "=== Publishing ADK-Rust ==="
echo "Total crates: ${#CRATES[@]}"
echo ""

PUBLISHED=0
SKIPPED=0
FAILED=0
FAILED_CRATES=()

for crate in "${CRATES[@]}"; do
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "📦 [$((PUBLISHED + SKIPPED + FAILED + 1))/${#CRATES[@]}] Publishing: $crate"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

  OUTPUT=$(cargo publish -p "$crate" 2>&1)
  STATUS=$?

  echo "$OUTPUT"
  echo ""

  if echo "$OUTPUT" | grep -q "already exists\|already uploaded"; then
    echo "⏭  Already published"
    SKIPPED=$((SKIPPED + 1))
    sleep 1
  elif [ $STATUS -eq 0 ]; then
    echo "✅ Published"
    PUBLISHED=$((PUBLISHED + 1))
    echo "⏳ Waiting 10s for indexing..."
    sleep 10
  else
    echo "❌ FAILED (exit $STATUS)"
    FAILED=$((FAILED + 1))
    FAILED_CRATES+=("$crate")
    sleep 2
  fi

  echo ""
done

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "=== SUMMARY ==="
echo "✅ Published: $PUBLISHED"
echo "⏭  Skipped:   $SKIPPED"
echo "❌ Failed:    $FAILED"
if [ ${#FAILED_CRATES[@]} -gt 0 ]; then
  echo ""
  echo "Failed crates:"
  for fc in "${FAILED_CRATES[@]}"; do
    echo "  - $fc"
  done
fi
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
