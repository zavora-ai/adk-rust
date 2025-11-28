#!/bin/bash
# Quick check: Verify all crate names are available on crates.io

echo "🔍 Checking crate name availability on crates.io..."
echo ""

CRATES=(
    "adk-rust"
    "adk-core"
    "adk-agent"
    "adk-model"
    "adk-tool"
    "adk-session"
    "adk-artifact"
    "adk-memory"
    "adk-runner"
    "adk-server"
    "adk-cli"
    "adk-telemetry"
)

AVAILABLE=0
TAKEN=0

for crate in "${CRATES[@]}"; do
    printf "  %-20s " "$crate"
    
    if curl -s "https://crates.io/api/v1/crates/$crate" | grep -q "does not exist"; then
        echo "✅ AVAILABLE"
        ((AVAILABLE++)) || true
    else
        echo "❌ TAKEN"
        ((TAKEN++)) || true
    fi
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Summary: $AVAILABLE available, $TAKEN taken"

if [ $TAKEN -eq 0 ]; then
    echo "✅ All crate names are available!"
else
    echo "⚠️  Some crate names are taken. You may need to choose different names."
fi
