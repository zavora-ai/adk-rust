#!/bin/bash
# Updates {{version}} placeholders in all README.md files with the workspace version

VERSION=$(grep -A1 '\[workspace.package\]' Cargo.toml | grep 'version' | sed 's/.*"\(.*\)"/\1/')

if [ -z "$VERSION" ]; then
    echo "Error: Could not detect workspace version"
    exit 1
fi

echo "Updating READMEs to version: $VERSION"

# Find and replace in all markdown files
find . -name "*.md" -not -path "./target/*" -exec sed -i '' "s/{{version}}/$VERSION/g" {} \;

echo "Done. Updated $(grep -r "$VERSION" --include="*.md" | wc -l | tr -d ' ') occurrences."
