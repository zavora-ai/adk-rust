#!/bin/bash
# Syncs docs/official_docs to GitHub Wiki
# GitHub Wiki uses flat structure with special naming conventions

set -e

REPO_ROOT=$(git rev-parse --show-toplevel)
WIKI_DIR="${REPO_ROOT}/../adk-rust.wiki"
DOCS_DIR="${REPO_ROOT}/docs/official_docs"

echo "📚 Syncing ADK-Rust documentation to GitHub Wiki..."

# Clone wiki if not exists
if [ ! -d "$WIKI_DIR" ]; then
    echo "📥 Cloning wiki repository..."
    git clone https://github.com/zavora-ai/adk-rust.wiki.git "$WIKI_DIR"
fi

cd "$WIKI_DIR"
git pull origin master 2>/dev/null || git pull origin main 2>/dev/null || true

# Clear existing markdown files (preserve .git)
echo "🧹 Cleaning wiki directory..."
find . -maxdepth 1 -name "*.md" -delete
rm -rf images/

# Function to convert path to wiki page name
# e.g., "agents/llm-agent.md" -> "Agents-LLM-Agent"
path_to_wiki_name() {
    local path="$1"
    # Remove .md extension
    path="${path%.md}"
    # Replace / with -
    path="${path//\//-}"
    # Title case each word (capitalize first letter of each segment)
    echo "$path" | sed 's/-/ /g' | awk '{for(i=1;i<=NF;i++) $i=toupper(substr($i,1,1)) tolower(substr($i,2))}1' | sed 's/ /-/g'
}

# Function to fix links in a markdown file
fix_wiki_links() {
    local file="$1"
    local temp_file="${file}.tmp"
    
    # Process the file line by line to handle links
    while IFS= read -r line || [[ -n "$line" ]]; do
        # Fix markdown links: [text](path.md) -> [text](Wiki-Page-Name)
        # Also handle relative paths like ../agents/llm-agent.md
        echo "$line" | sed -E '
            # Handle links with .md extension
            s/\]\(([^)]+)\.md\)/](WIKI_LINK_\1)/g
            # Handle links with .md#anchor
            s/\]\(([^)]+)\.md#([^)]+)\)/](WIKI_LINK_\1#\2)/g
        '
    done < "$file" > "$temp_file"
    
    # Now convert WIKI_LINK_ placeholders to proper wiki names
    # This is a simplified approach - complex paths may need manual adjustment
    sed -i '' 's|WIKI_LINK_\.\./||g' "$temp_file"
    sed -i '' 's|WIKI_LINK_\./||g' "$temp_file"
    sed -i '' 's|WIKI_LINK_||g' "$temp_file"
    
    # Convert remaining path separators to dashes and title case
    # e.g., agents/llm-agent -> Agents-Llm-Agent
    python3 -c "
import re
import sys

def path_to_wiki(match):
    path = match.group(1)
    anchor = match.group(2) if match.lastindex >= 2 else ''
    
    # Handle relative paths
    path = path.replace('../', '').replace('./', '')
    
    # Convert path to wiki name
    parts = path.replace('/', '-').split('-')
    wiki_name = '-'.join(p.capitalize() for p in parts if p)
    
    if anchor:
        return f']({wiki_name}#{anchor})'
    return f']({wiki_name})'

with open('$temp_file', 'r') as f:
    content = f.read()

# Fix links with anchors
content = re.sub(r'\]\(([^)#]+)#([^)]+)\)', path_to_wiki, content)
# Fix links without anchors  
content = re.sub(r'\]\(([^)]+)\)', lambda m: path_to_wiki(m) if '://' not in m.group(1) and not m.group(1).startswith('#') else m.group(0), content)

with open('$temp_file', 'w') as f:
    f.write(content)
" 2>/dev/null || true
    
    mv "$temp_file" "$file"
}

# Copy and flatten files
echo "📄 Copying and flattening documentation..."

# Copy index.md as Home.md (wiki homepage)
if [ -f "$DOCS_DIR/index.md" ]; then
    cp "$DOCS_DIR/index.md" "$WIKI_DIR/Home.md"
    fix_wiki_links "$WIKI_DIR/Home.md"
    echo "  ✓ Home.md (from index.md)"
fi

# Copy top-level files
for file in "$DOCS_DIR"/*.md; do
    if [ -f "$file" ] && [ "$(basename "$file")" != "index.md" ]; then
        basename=$(basename "$file" .md)
        # Title case the filename
        wiki_name=$(echo "$basename" | sed 's/-/ /g' | awk '{for(i=1;i<=NF;i++) $i=toupper(substr($i,1,1)) tolower(substr($i,2))}1' | sed 's/ /-/g')
        cp "$file" "$WIKI_DIR/${wiki_name}.md"
        fix_wiki_links "$WIKI_DIR/${wiki_name}.md"
        echo "  ✓ ${wiki_name}.md"
    fi
done

# Copy files from subdirectories with flattened names
for dir in "$DOCS_DIR"/*/; do
    if [ -d "$dir" ]; then
        dir_name=$(basename "$dir")
        # Title case directory name
        dir_prefix=$(echo "$dir_name" | sed 's/-/ /g' | awk '{for(i=1;i<=NF;i++) $i=toupper(substr($i,1,1)) tolower(substr($i,2))}1' | sed 's/ /-/g')
        
        for file in "$dir"*.md; do
            if [ -f "$file" ]; then
                basename=$(basename "$file" .md)
                # Title case the filename
                file_name=$(echo "$basename" | sed 's/-/ /g' | awk '{for(i=1;i<=NF;i++) $i=toupper(substr($i,1,1)) tolower(substr($i,2))}1' | sed 's/ /-/g')
                wiki_name="${dir_prefix}-${file_name}"
                cp "$file" "$WIKI_DIR/${wiki_name}.md"
                fix_wiki_links "$WIKI_DIR/${wiki_name}.md"
                echo "  ✓ ${wiki_name}.md"
            fi
        done
    fi
done

# Copy images if they exist
if [ -d "$DOCS_DIR/images" ]; then
    cp -r "$DOCS_DIR/images" "$WIKI_DIR/"
    echo "  ✓ images/"
fi

# Also check for images in subdirectories
for dir in "$DOCS_DIR"/*/images; do
    if [ -d "$dir" ]; then
        mkdir -p "$WIKI_DIR/images"
        cp -r "$dir"/* "$WIKI_DIR/images/" 2>/dev/null || true
    fi
done

# Generate sidebar (_Sidebar.md) for navigation
# Use exact file names (without .md) for wiki links
echo "📑 Generating sidebar..."
cat > "$WIKI_DIR/_Sidebar.md" << 'EOF'
**Getting Started**
* [[Home]]
* [[Introduction]]
* [[Quickstart]]

**Core**
* [[Core-Core|Core Types]]
* [[Core-Runner|Runner]]

**Models**
* [[Models-Providers|Model Providers]]
* [[Models-Ollama|Ollama]]
* [[Models-Mistralrs|mistral.rs]]

**Agents**
* [[Agents-Llm-Agent|LLM Agent]]
* [[Agents-Workflow-Agents|Workflow Agents]]
* [[Agents-Multi-Agent|Multi-Agent]]
* [[Agents-Graph-Agents|Graph Agents]]
* [[Agents-Realtime-Agents|Realtime Agents]]

**Tools**
* [[Tools-Function-Tools|Function Tools]]
* [[Tools-Built-In-Tools|Built-in Tools]]
* [[Tools-Mcp-Tools|MCP Tools]]
* [[Tools-Browser-Tools|Browser Tools]]
* [[Tools-Ui-Tools|UI Tools]]

**Sessions & State**
* [[Sessions-Sessions|Sessions]]
* [[Sessions-State|State Management]]

**Callbacks & Events**
* [[Callbacks-Callbacks|Callbacks]]
* [[Events-Events|Events]]

**Artifacts**
* [[Artifacts-Artifacts|Artifacts]]

**Observability**
* [[Observability-Telemetry|Telemetry]]

**Deployment**
* [[Deployment-Launcher|Launcher]]
* [[Deployment-Server|Server]]
* [[Deployment-A2a|A2A Protocol]]

**Evaluation**
* [[Evaluation-Evaluation|Agent Evaluation]]

**Security**
* [[Security-Access-Control|Access Control]]
* [[Security-Guardrails|Guardrails]]
* [[Security-Memory|Memory]]

**Studio**
* [[Studio-Studio|ADK Studio]]

**Development**
* [[Development-Development-Guidelines|Guidelines]]
EOF

echo "  ✓ _Sidebar.md"

# Commit and push
echo "📤 Pushing to GitHub Wiki..."
git add -A
git commit -m "Sync wiki from docs/official_docs ($(date +%Y-%m-%d))" || echo "No changes to commit"
git push origin master 2>/dev/null || git push origin main 2>/dev/null || echo "Push failed - check permissions"

echo ""
echo "✅ Wiki sync complete!"
echo "   View at: https://github.com/zavora-ai/adk-rust/wiki"
