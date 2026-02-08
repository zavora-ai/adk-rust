# Configuration Guide

This guide covers all configuration options for the ADK documentation audit system.

## Configuration Methods

The audit system supports multiple configuration methods, in order of precedence:

1. **Command-line arguments** (highest priority)
2. **Environment variables**
3. **Configuration file** (`.adk-doc-audit.toml`)
4. **Default values** (lowest priority)

## Configuration File

Create a `.adk-doc-audit.toml` file in your project root:

```toml
# Basic paths
workspace_path = "."
docs_path = "docs/official_docs"

# Output configuration
output_format = "console"  # "console", "json", or "markdown"

# File filtering
excluded_files = [
    "docs/internal/*",
    "docs/drafts/*",
    "docs/archive/**/*.md"
]

excluded_crates = [
    "example-crate",
    "test-utils",
    "internal-tools"
]

# Severity and failure configuration
severity_threshold = "warning"  # "info", "warning", or "critical"
fail_on_critical = true

# Performance settings
example_timeout = "30s"
max_concurrent_validations = 4

# Feature flags
validate_api_references = true
validate_code_examples = true
validate_version_consistency = true
validate_internal_links = true
detect_missing_documentation = true
generate_suggestions = true

# Advanced settings
rust_version_override = "1.85.0"  # Override detected Rust version
workspace_version_override = "0.3.0"  # Override detected workspace version
```

## Environment Variables

All configuration options can be set via environment variables with the `ADK_DOC_AUDIT_` prefix:

```bash
export ADK_DOC_AUDIT_WORKSPACE_PATH="."
export ADK_DOC_AUDIT_DOCS_PATH="docs/official_docs"
export ADK_DOC_AUDIT_OUTPUT_FORMAT="json"
export ADK_DOC_AUDIT_SEVERITY_THRESHOLD="warning"
export ADK_DOC_AUDIT_FAIL_ON_CRITICAL="true"
export ADK_DOC_AUDIT_EXAMPLE_TIMEOUT="60s"
```

## Command-Line Arguments

### Full Audit Command

```bash
adk-doc-audit audit [OPTIONS]

OPTIONS:
    -w, --workspace <PATH>          Workspace root path [default: .]
    -d, --docs <PATH>               Documentation directory [default: docs]
    -f, --format <FORMAT>           Output format [default: console] [possible: console, json, markdown]
    -o, --output <FILE>             Output file (stdout if not specified)
    -c, --config <FILE>             Configuration file [default: .adk-doc-audit.toml]
    -v, --verbose                   Enable verbose logging
    -q, --quiet                     Suppress non-error output
        --severity <LEVEL>          Minimum severity to report [default: info]
        --fail-on-critical          Exit with error on critical issues [default: true]
        --no-fail-on-critical      Don't exit with error on critical issues
        --timeout <DURATION>        Example compilation timeout [default: 30s]
        --exclude-files <PATTERN>   File patterns to exclude (can be repeated)
        --exclude-crates <NAME>     Crate names to exclude (can be repeated)
        --no-api-validation         Disable API reference validation
        --no-example-validation     Disable code example validation
        --no-version-validation     Disable version consistency validation
        --no-link-validation        Disable internal link validation
        --no-missing-detection      Disable missing documentation detection
        --no-suggestions            Disable automated suggestions
```

### Incremental Audit Command

```bash
adk-doc-audit incremental [OPTIONS] --changed <FILE>...

OPTIONS:
    -w, --workspace <PATH>          Workspace root path [default: .]
    -d, --docs <PATH>               Documentation directory [default: docs]
    -f, --format <FORMAT>           Output format [default: console]
        --changed <FILE>...         Changed files to audit (required)
    [... other options same as audit command ...]
```

### Single File Validation

```bash
adk-doc-audit validate [OPTIONS] <FILE>

ARGS:
    <FILE>    Documentation file to validate

OPTIONS:
    -w, --workspace <PATH>          Workspace root path [default: .]
    -f, --format <FORMAT>           Output format [default: console]
    [... other options same as audit command ...]
```

## Configuration Options Reference

### Basic Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `workspace_path` | String | `"."` | Path to Rust workspace root |
| `docs_path` | String | `"docs"` | Path to documentation directory |
| `output_format` | Enum | `"console"` | Report format: `console`, `json`, `markdown` |

### File Filtering

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `excluded_files` | Array | `[]` | Glob patterns for files to exclude |
| `excluded_crates` | Array | `[]` | Crate names to exclude from analysis |

**Glob Pattern Examples:**
- `"docs/internal/*"` - All files in `docs/internal/`
- `"docs/**/*.draft.md"` - All `.draft.md` files recursively
- `"**/README.md"` - All README.md files
- `"docs/archive/**"` - Everything in `docs/archive/` recursively

### Severity and Failure Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `severity_threshold` | Enum | `"info"` | Minimum severity to report: `info`, `warning`, `critical` |
| `fail_on_critical` | Boolean | `true` | Exit with error code 1 if critical issues found |

### Performance Settings

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `example_timeout` | Duration | `"30s"` | Timeout for compiling code examples |
| `max_concurrent_validations` | Integer | `4` | Maximum parallel validation tasks |

**Duration Format:**
- `"30s"` - 30 seconds
- `"2m"` - 2 minutes  
- `"1h30m"` - 1 hour 30 minutes

### Feature Flags

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `validate_api_references` | Boolean | `true` | Validate API references against implementations |
| `validate_code_examples` | Boolean | `true` | Compile and validate code examples |
| `validate_version_consistency` | Boolean | `true` | Check version number consistency |
| `validate_internal_links` | Boolean | `true` | Validate internal documentation links |
| `detect_missing_documentation` | Boolean | `true` | Detect undocumented public APIs |
| `generate_suggestions` | Boolean | `true` | Generate automated fix suggestions |

### Advanced Settings

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `rust_version_override` | String | Auto-detected | Override Rust version for validation |
| `workspace_version_override` | String | Auto-detected | Override workspace version |

## Profile-Based Configuration

Create different configuration profiles for different environments:

### `.adk-doc-audit.toml` (Default)
```toml
workspace_path = "."
docs_path = "docs"
output_format = "console"
severity_threshold = "info"
```

### `.adk-doc-audit.ci.toml` (CI/CD)
```toml
workspace_path = "."
docs_path = "docs"
output_format = "json"
severity_threshold = "warning"
fail_on_critical = true
example_timeout = "60s"
excluded_files = ["docs/drafts/*"]
```

### `.adk-doc-audit.dev.toml` (Development)
```toml
workspace_path = "."
docs_path = "docs"
output_format = "console"
severity_threshold = "info"
fail_on_critical = false
generate_suggestions = true
```

Use with:
```bash
adk-doc-audit audit --config .adk-doc-audit.ci.toml
```

## Environment-Specific Configuration

### Development Environment

```toml
# Focus on helpful suggestions, don't fail builds
severity_threshold = "info"
fail_on_critical = false
generate_suggestions = true
validate_api_references = true
validate_code_examples = true
```

### CI/CD Environment

```toml
# Strict validation, fail on critical issues
severity_threshold = "warning"
fail_on_critical = true
output_format = "json"
example_timeout = "60s"  # Longer timeout for CI
excluded_files = [
    "docs/drafts/*",
    "docs/internal/*"
]
```

### Production Documentation

```toml
# Comprehensive validation for published docs
severity_threshold = "info"
fail_on_critical = true
validate_api_references = true
validate_code_examples = true
validate_version_consistency = true
validate_internal_links = true
detect_missing_documentation = true
generate_suggestions = true
```

## Integration Examples

### GitHub Actions

```yaml
name: Documentation Audit
on: [push, pull_request]

jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      
      - name: Full audit on main branch
        if: github.ref == 'refs/heads/main'
        run: |
          adk-doc-audit audit \
            --config .adk-doc-audit.ci.toml \
            --format json \
            --output audit-report.json
      
      - name: Incremental audit on PR
        if: github.event_name == 'pull_request'
        run: |
          # Get changed markdown files
          CHANGED_FILES=$(git diff --name-only origin/main...HEAD -- '*.md' | tr '\n' ' ')
          if [ -n "$CHANGED_FILES" ]; then
            adk-doc-audit incremental \
              --config .adk-doc-audit.ci.toml \
              --changed $CHANGED_FILES
          fi
      
      - name: Upload audit report
        if: always()
        uses: actions/upload-artifact@v3
        with:
          name: audit-report
          path: audit-report.json
```

### Pre-commit Hook

```bash
#!/bin/bash
# .git/hooks/pre-commit

# Get staged markdown files
STAGED_FILES=$(git diff --cached --name-only --diff-filter=ACM | grep '\.md$' | tr '\n' ' ')

if [ -n "$STAGED_FILES" ]; then
    echo "Running documentation audit on staged files..."
    
    adk-doc-audit incremental \
        --config .adk-doc-audit.dev.toml \
        --changed $STAGED_FILES \
        --severity warning
    
    if [ $? -ne 0 ]; then
        echo "Documentation audit failed. Fix issues before committing."
        exit 1
    fi
fi
```

### Makefile Integration

```makefile
# Makefile

.PHONY: docs-audit docs-audit-ci docs-audit-dev

docs-audit:
	adk-doc-audit audit

docs-audit-ci:
	adk-doc-audit audit \
		--config .adk-doc-audit.ci.toml \
		--format json \
		--output audit-report.json

docs-audit-dev:
	adk-doc-audit audit \
		--config .adk-doc-audit.dev.toml \
		--no-fail-on-critical

docs-audit-incremental:
	@if [ -n "$(FILES)" ]; then \
		adk-doc-audit incremental --changed $(FILES); \
	else \
		echo "Usage: make docs-audit-incremental FILES='file1.md file2.md'"; \
	fi
```

## Troubleshooting Configuration

### Validation Issues

1. **Check configuration loading:**
   ```bash
   adk-doc-audit audit --verbose
   ```

2. **Test configuration file:**
   ```bash
   adk-doc-audit audit --config .adk-doc-audit.toml --dry-run
   ```

3. **Validate glob patterns:**
   ```bash
   # Test file exclusion patterns
   find docs -name "*.md" | grep -E "docs/internal/.*"
   ```

### Common Configuration Errors

1. **Invalid duration format:**
   ```toml
   # Wrong
   example_timeout = "30"
   
   # Correct
   example_timeout = "30s"
   ```

2. **Invalid severity level:**
   ```toml
   # Wrong
   severity_threshold = "error"
   
   # Correct
   severity_threshold = "critical"
   ```

3. **Invalid output format:**
   ```toml
   # Wrong
   output_format = "yaml"
   
   # Correct
   output_format = "json"
   ```

### Debug Configuration

Enable debug logging to see configuration loading:

```bash
RUST_LOG=debug adk-doc-audit audit --verbose
```

This will show:
- Which configuration files are loaded
- How environment variables override settings
- Which files are excluded by patterns
- Performance metrics for validation tasks