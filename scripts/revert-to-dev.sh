#!/bin/bash
# Revert Cargo.toml files back to development mode (path dependencies)

set -e

echo "🔄 Reverting to development mode..."
echo ""

# Restore from backups
RESTORED=0
SKIPPED=0

find . -name "Cargo.toml.dev-backup" -not -path "*/target/*" -not -path "*/.git/*" | while read backup; do
    original="${backup%.dev-backup}"
    
    if [ -f "$backup" ]; then
        echo "  Restoring $original"
        cp "$backup" "$original"
        rm "$backup"
        ((RESTORED++)) || true
    else
        echo "  Skipping $original (no backup found)"
        ((SKIPPED++)) || true
    fi
done

echo ""
echo "✅ Reverted to development mode!"
echo "  Restored: $RESTORED files"
if [ $SKIPPED -gt 0 ]; then
    echo "  Skipped: $SKIPPED files (no backup)"
fi
echo ""
echo "You can now continue local development with path dependencies."
