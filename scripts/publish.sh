#!/bin/bash
# Publish ADK-Rust crates to crates.io
# Must run prepare-publish.sh first!

set -e

MODE=${1:-dry-run}

if [ "$MODE" != "dry-run" ] && [ "$MODE" != "publish" ]; then
    echo "Usage: $0 [dry-run|publish]"
    echo ""
    echo "  dry-run  - Test publishing without actually publishing (default)"
    echo "  publish  - Actually publish to crates.io"
    exit 1
fi

# Check if logged in to crates.io
if ! cargo login --help &>/dev/null; then
    echo "❌ Error: cargo not found or not configured"
    exit 1
fi

echo "📦 Publishing ADK-Rust crates to crates.io"
echo "   Mode: $MODE"
echo ""

if [ "$MODE" = "publish" ]; then
    echo "⚠️  WARNING: This will publish crates to crates.io!"
    echo "   Published versions are PERMANENT and cannot be deleted."
    echo ""
    read -p "Are you sure? (yes/no): " confirm
    if [ "$confirm" != "yes" ]; then
        echo "Aborted."
        exit 0
    fi
fi

# Crates in dependency order (most depended-upon first)
CRATES=(
    "adk-core"
    "adk-telemetry"
    "adk-model"
    "adk-tool"
    "adk-session"
    "adk-artifact"
    "adk-memory"
    "adk-agent"
    "adk-runner"
    "adk-server"
    "adk-cli"
    "adk-rust"
)

PUBLISHED=0
FAILED=0

for crate in "${CRATES[@]}"; do
    if [ ! -d "$crate" ]; then
        echo "⚠️  Skipping $crate (directory not found)"
        continue
    fi
    
    echo ""
    echo "📦 Publishing $crate..."
    cd "$crate"
    
    if [ "$MODE" = "dry-run" ]; then
        if cargo publish --dry-run; then
            echo "✅ $crate dry-run successful"
            ((PUBLISHED++)) || true
        else
            echo "❌ $crate dry-run failed"
            ((FAILED++)) || true
        fi
    else
        if cargo publish; then
            echo "✅ $crate published!"
            ((PUBLISHED++)) || true
            
            # Wait for crates.io to index the new crate
            # This prevents errors when the next crate depends on it
            echo "   Waiting 15 seconds for crates.io to index..."
            sleep 15
        else
            echo "❌ $crate publishing failed"
            ((FAILED++)) || true
            echo ""
            echo "⚠️  Publishing stopped due to error."
            echo "   You can retry - already published crates will be skipped."
            cd ..
            exit 1
        fi
    fi
    
    cd ..
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "📊 Summary"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Total crates: ${#CRATES[@]}"
echo "  Successful: $PUBLISHED"
echo "  Failed: $FAILED"
echo ""

if [ $FAILED -eq 0 ]; then
    if [ "$MODE" = "dry-run" ]; then
        echo "✅ All dry-runs passed!"
        echo ""
        echo "Next steps:"
        echo "  1. Review the dry-run output above"
        echo "  2. Run: ./scripts/publish.sh publish"
    else
        echo "🎉 All crates published successfully!"
        echo ""
        echo "Next steps:"
        echo "  1. Create git tag: git tag -a v0.1.0 -m 'Release v0.1.0'"
        echo "  2. Push tag: git push origin v0.1.0"
        echo "  3. Create GitHub release"
        echo "  4. Revert to dev mode: ./scripts/revert-to-dev.sh"
        echo "  5. Verify: https://crates.io/crates/adk-rust"
    fi
else
    echo "❌ Some crates failed to publish"
    echo "   Review errors above and fix before retrying"
    exit 1
fi
