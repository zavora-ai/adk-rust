# Publishing Scripts

Automation scripts for publishing ADK-Rust to crates.io.

## Scripts

### 1. `check-names.sh`
Check if crate names are available on crates.io.

```bash
./scripts/check-names.sh
```

### 2. `prepare-publish.sh`
Prepare Cargo.toml files for publishing (converts path to version dependencies).

```bash
./scripts/prepare-publish.sh
```

**What it does:**
- Creates `.dev-backup` files of all Cargo.toml
- Converts `{ path = "../crate", version = "0.1.0" }` → `"0.1.0"`
- Prepares for crates.io publishing

### 3. `publish.sh`
Publish crates to crates.io in correct dependency order.

```bash
# Dry run (test without publishing)
./scripts/publish.sh dry-run

# Actually publish
./scripts/publish.sh publish
```

**Publishing order:**
1. adk-core
2. adk-telemetry
3. adk-model
4. adk-tool
5. adk-session
6. adk-artifact
7. adk-memory
8. adk-agent
9. adk-runner
10. adk-server
11. adk-cli
12. adk-rust

### 4. `revert-to-dev.sh`
Restore development mode (path dependencies) from backups.

```bash
./scripts/revert-to-dev.sh
```

## Complete Publishing Workflow

```bash
# Step 1: Check availability (optional)
./scripts/check-names.sh

# Step 2: Prepare for publishing
./scripts/prepare-publish.sh

# Step 3: Test with dry-run
./scripts/publish.sh dry-run

# Step 4: Review changes
git diff

# Step 5: Publish (if dry-run passed)
./scripts/publish.sh publish

# Step 6: Create git tag
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0

# Step 7: Revert to development mode
./scripts/revert-to-dev.sh

# Step 8: Verify on crates.io
open https://crates.io/crates/adk-rust
```

## Prerequisites

1. **crates.io account**: Sign up at https://crates.io
2. **API token**: Get from https://crates.io/settings/tokens
3. **Login**: `cargo login <your-token>`

## Safety Features

- ✅ Dry-run mode to test before publishing
- ✅ Automatic backups of Cargo.toml files
- ✅ Dependency-order publishing
- ✅ Confirmation prompt for actual publishing
- ✅ Wait time between publishes for crates.io indexing
- ✅ Easy revert to development mode

## Troubleshooting

**Error: "crate already exists"**
- This version is already published
- Bump version number in workspace Cargo.toml

**Error: "path dependencies not allowed"**
- Run `prepare-publish.sh` first
- Verify all path deps converted to version deps

**Error: "dependency not found"**
- Previous crate in order failed to publish
- Wait for crates.io to index (15 seconds)
- Retry publishing

**Revert to development**
```bash
./scripts/revert-to-dev.sh
```
