# Publishing ADK-Rust to crates.io

This guide covers how to publish the ADK-Rust crates to crates.io.

## Crate Name Availability

All crate names are **AVAILABLE** on crates.io:

- ✅ `adk-rust` (main facade)
- ✅ `adk-core`
- ✅ `adk-agent`
- ✅ `adk-model`
- ✅ `adk-tool`
- ✅ `adk-session`
- ✅ `adk-artifact`
- ✅ `adk-memory`
- ✅ `adk-runner`
- ✅ `adk-server`
- ✅ `adk-cli`
- ✅ `adk-telemetry`

## Publishing Strategy

### Option 1: Publish All Crates (Recommended) ⭐

Publish both the facade (`adk-rust`) AND all modular crates.

**Pros:**
- ✅ Maximum flexibility for users
- ✅ Advanced users can use specific crates
- ✅ Follows Rust ecosystem best practices (like `tokio`, `aws-sdk`)
- ✅ Better for the ecosystem

**Cons:**
- ⚠️ Need to manage versions for all crates
- ⚠️ More crates to publish (but can automate)

**User Experience:**
```toml
# Simple users
[dependencies]
adk-rust = "0.1"

# Advanced users (smaller dependency tree)
[dependencies]
adk-core = "0.1"
adk-agent = "0.1"
adk-model = "0.1"
```

### Option 2: Publish Only `adk-rust`

Keep modular crates internal, only publish the facade.

**Pros:**
- ✅ Simpler publishing process
- ✅ Only one crate to maintain

**Cons:**
- ❌ Users can't selectively use individual crates
- ❌ Larger dependency for simple use cases
- ❌ Less flexible

## Recommended: Option 1 (Publish All)

## Prerequisites

1. **crates.io Account**
   ```bash
   # Sign up at https://crates.io
   # Get your API token from https://crates.io/settings/tokens
   ```

2. **Login to cargo**
   ```bash
   cargo login <your-token>
   ```

3. **Update Cargo.toml files**
   
   Change path dependencies to version dependencies for crates.io:
   
   ```toml
   # Before (local development)
   adk-core = { path = "../adk-core", version = "0.1.0" }
   
   # After (for publishing)
   adk-core = { version = "0.1.0" }
   ```

## Publishing Order (Important!)

Publish in **dependency order** (dependencies first):

1. `adk-core` (no dependencies within ADK)
2. `adk-telemetry` (depends on adk-core)
3. `adk-model` (depends on adk-core)
4. `adk-tool` (depends on adk-core)
5. `adk-session` (depends on adk-core)
6. `adk-artifact` (depends on adk-core)
7. `adk-memory` (depends on adk-core)
8. `adk-agent` (depends on adk-core, adk-model)
9. `adk-runner` (depends on adk-core, adk-agent, adk-session, etc.)
10. `adk-server` (depends on adk-core, adk-runner)
11. `adk-cli` (depends on everything)
12. `adk-rust` (facade, depends on all)

## Step-by-Step Publishing

### 1. Prepare for Publishing

Update all Cargo.toml files to use version dependencies instead of path:

```bash
# We'll need to update each Cargo.toml
# This can be automated with a script
```

### 2. Publish Core Crates

```bash
# 1. Publish adk-core first
cd adk-core
cargo publish --dry-run  # Test first
cargo publish

# 2. Publish adk-telemetry
cd ../adk-telemetry
cargo publish --dry-run
cargo publish

# 3. Publish adk-model
cd ../adk-model
cargo publish --dry-run
cargo publish

# Continue in order...
```

### 3. Publish Facade

```bash
cd adk-rust
cargo publish --dry-run
cargo publish
```

### 4. Verify

```bash
# Check published crates
open https://crates.io/crates/adk-rust
open https://crates.io/crates/adk-core
# etc.

# Test installation
cargo new test-adk
cd test-adk
cargo add adk-rust
cargo check
```

## Automation Script

Create `scripts/publish.sh`:

```bash
#!/bin/bash
set -e

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

DRY_RUN=${1:-true}

for crate in "${CRATES[@]}"; do
    echo "📦 Publishing $crate..."
    cd "$crate"
    
    if [ "$DRY_RUN" = "true" ]; then
        cargo publish --dry-run
    else
        cargo publish
        # Wait for crates.io to update
        sleep 10
    fi
    
    cd ..
done

echo "✅ All crates published!"
```

Usage:
```bash
# Dry run first
./scripts/publish.sh true

# Actually publish
./scripts/publish.sh false
```

## Version Management

Use workspace version for consistency:

```toml
# Root Cargo.toml
[workspace.package]
version = "0.1.0"

# Each crate Cargo.toml
[package]
version.workspace = true
```

When releasing:
1. Update version in root `Cargo.toml`
2. All crates inherit the same version
3. Publish in order

## Before Publishing Checklist

- [ ] All tests pass: `cargo test --workspace`
- [ ] Documentation builds: `cargo doc --workspace --no-deps`
- [ ] Examples work: Test all examples
- [ ] README.md files are up to date
- [ ] LICENSE file exists in each crate
- [ ] Repository and homepage URLs are set
- [ ] Crate descriptions are clear
- [ ] Keywords and categories are set
- [ ] CHANGELOG.md is updated
- [ ] Version numbers are correct

## Post-Publishing

1. **Create Git Tag**
   ```bash
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```

2. **Create GitHub Release**
   - Go to GitHub releases
   - Create new release from tag
   - Add release notes from CHANGELOG

3. **Update Documentation**
   - docs.rs will automatically build docs
   - Verify at https://docs.rs/adk-rust

4. **Announce**
   - Post on Reddit r/rust
   - Tweet about it
   - Update README with crates.io badge

## Updating Published Crates

Use `cargo release` for managing versions:

```bash
cargo install cargo-release

# Bump version and publish
cargo release patch  # 0.1.0 -> 0.1.1
cargo release minor  # 0.1.0 -> 0.2.0
cargo release major  # 0.1.0 -> 1.0.0
```

## Converting from Path to Version Dependencies

Before publishing, we need to update Cargo.toml files to use crates.io versions instead of path dependencies.

### Manual Update

In each crate's `Cargo.toml`, change:

```toml
# FROM (development)
[dependencies]
adk-core = { path = "../adk-core", version = "0.1.0" }

# TO (publishing)
[dependencies]
adk-core = "0.1.0"
```

### Automated Script

Create `scripts/prepare-publish.sh`:

```bash
#!/bin/bash
# Convert path dependencies to version dependencies

find . -name "Cargo.toml" -not -path "*/target/*" | while read f; do
    echo "Processing $f"
    sed -i.bak 's/{ path = "[^"]*", version = "\([^"]*\)" }/"\1"/g' "$f"
    rm "$f.bak"
done
```

## CI/CD Publishing

Add to `.github/workflows/publish.yml`:

```yaml
name: Publish

on:
  push:
    tags:
      - 'v*'

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
          
      - name: Login to crates.io
        run: cargo login ${{ secrets.CARGO_TOKEN }}
        
      - name: Publish crates
        run: ./scripts/publish.sh false
```

## FAQ

**Q: Do I need to publish all crates?**  
A: No, but it's recommended for flexibility. You could publish only `adk-rust`, but then users can't use individual crates.

**Q: Can I use path dependencies in published crates?**  
A: No, crates.io requires version dependencies. Path dependencies only work locally.

**Q: What happens if publishing fails mid-way?**  
A: You can retry. Already published crates will be skipped. Versions are immutable.

**Q: Can I unpublish a crate?**  
A: You can only yank versions (they won't be installed by default but remain accessible). You cannot delete versions entirely.

## Next Steps

1. **Decide**: Publish all crates or just `adk-rust`
2. **Prepare**: Update Cargo.toml files for publishing
3. **Test**: Run `cargo publish --dry-run` on all crates
4. **Publish**: Execute publishing script
5. **Verify**: Test installation and functionality
6. **Announce**: Share with the community!

---

**Recommendation**: Publish all crates. It's the standard practice and provides maximum flexibility for users while maintaining the simple `cargo add adk-rust` experience.
