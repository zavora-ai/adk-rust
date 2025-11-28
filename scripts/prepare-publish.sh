#!/bin/bash
# Prepare Cargo.toml files for publishing to crates.io
# Converts path dependencies to version dependencies

set -e

echo "🔧 Preparing ADK-Rust for publishing..."
echo ""

# Backup original files
echo "📦 Creating backups..."
find . -name "Cargo.toml" -not -path "*/target/*" -not -path "*/.git/*" | while read f; do
    cp "$f" "$f.dev-backup"
done

echo "✅ Backups created (.dev-backup files)"
echo ""

# Convert path dependencies to version dependencies
echo "🔄 Converting path dependencies to version dependencies..."

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

for crate in "${CRATES[@]}"; do
    if [ -f "$crate/Cargo.toml" ]; then
        echo "  Processing $crate/Cargo.toml..."
        
        # Use perl for in-place editing (more reliable than sed across platforms)
        perl -i -pe 's/\{ path = "[^"]+", version = "([^"]+)"(, optional = true)? \}/"$1"$2/g' "$crate/Cargo.toml"
        
        # Also handle format without optional
        perl -i -pe 's/\{ path = "[^"]+", version = "([^"]+)" \}/"$1"/g' "$crate/Cargo.toml"
    fi
done

echo ""
echo "✅ Conversion complete!"
echo ""
echo "📋 Next steps:"
echo "  1. Review changes: git diff"
echo "  2. Test build: cargo build --workspace"
echo "  3. Test publish (dry-run): ./scripts/publish.sh dry-run"
echo "  4. Publish for real: ./scripts/publish.sh publish"
echo "  5. Revert to dev mode: ./scripts/revert-to-dev.sh"
echo ""
echo "⚠️  Backup files saved as: *.dev-backup"
