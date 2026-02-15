# adk-doc-audit

Documentation audit system for ADK-Rust that validates documentation against actual crate implementations.

[![Crates.io](https://img.shields.io/crates/v/adk-doc-audit.svg)](https://crates.io/crates/adk-doc-audit)
[![Documentation](https://docs.rs/adk-doc-audit/badge.svg)](https://docs.rs/adk-doc-audit)
[![License](https://img.shields.io/crates/l/adk-doc-audit.svg)](LICENSE)

## Overview

The `adk-doc-audit` crate provides a comprehensive system for ensuring that ADK-Rust's documentation stays accurate and up-to-date with the actual crate implementations. It performs static analysis of documentation files, validates code examples, and cross-references API signatures with actual implementations.

## Features

- **API Reference Validation**: Ensures all API references in documentation match actual implementations
- **Code Example Compilation**: Validates that code examples compile with current crate versions  
- **Version Consistency Checking**: Verifies version references are current and consistent
- **Cross-Reference Validation**: Validates internal links and references
- **Missing Feature Detection**: Identifies features that exist in code but are missing from documentation
- **Automated Suggestions**: Provides fix suggestions for identified issues
- **Comprehensive Reporting**: Generates detailed audit reports in multiple formats (Console, JSON, Markdown)
- **CI/CD Integration**: Supports integration with build pipelines with appropriate exit codes
- **Incremental Auditing**: Process only changed files for efficient CI/CD workflows

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-doc-audit = { git = "https://github.com/zavora-ai/adk-rust" }
```

Or use the main ADK-Rust crate with the `doc-audit` feature:

```toml
[dependencies]
adk-rust = { git = "https://github.com/zavora-ai/adk-rust", features = ["doc-audit"] }
```

## Quick Start

### Basic Usage

```rust
use adk_doc_audit::{AuditConfig, AuditOrchestrator, OutputFormat};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = AuditConfig::builder()
        .workspace_path(".")
        .docs_path("docs/official_docs")
        .output_format(OutputFormat::Console)
        .build();
    
    let orchestrator = AuditOrchestrator::new(config)?;
    let report = orchestrator.run_full_audit().await?;
    
    println!("Audit Results:");
    println!("  Total files: {}", report.summary.total_files);
    println!("  Files with issues: {}", report.summary.files_with_issues);
    println!("  Total issues: {}", report.summary.total_issues);
    println!("  Critical issues: {}", report.summary.critical_issues);
    println!("  Coverage: {:.1}%", report.summary.coverage_percentage);
    
    // Exit with error code if critical issues found
    if report.summary.critical_issues > 0 {
        std::process::exit(1);
    }
    
    Ok(())
}
```

### Single File Validation

```rust
use adk_doc_audit::{AuditConfig, AuditOrchestrator};
use std::path::Path;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = AuditConfig::builder()
        .workspace_path(".")
        .docs_path("docs")
        .build();
    
    let orchestrator = AuditOrchestrator::new(config)?;
    let result = orchestrator.validate_file(Path::new("docs/getting-started.md")).await?;
    
    for issue in &result.issues {
        println!("{:?}: {}", issue.severity, issue.message);
        if let Some(suggestion) = &issue.suggestion {
            println!("  Suggestion: {}", suggestion);
        }
    }
    
    Ok(())
}
```

### Incremental Audit

```rust
use adk_doc_audit::{AuditConfig, AuditOrchestrator};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = AuditConfig::builder()
        .workspace_path(".")
        .docs_path("docs")
        .build();
    
    let orchestrator = AuditOrchestrator::new(config)?;
    
    // Only audit changed files (useful for CI/CD)
    let changed_files = vec![
        PathBuf::from("docs/api-reference.md"),
        PathBuf::from("docs/examples.md"),
    ];
    
    let report = orchestrator.run_incremental_audit(&changed_files).await?;
    
    println!("Incremental audit complete: {} issues in {} files", 
             report.summary.total_issues, changed_files.len());
    
    Ok(())
}
```

## CLI Usage

The crate provides a command-line interface for easy integration with development workflows:

### Full Audit

Run a complete audit of all documentation:

```bash
# Basic audit with console output
adk-doc-audit audit --workspace . --docs docs/official_docs

# Generate JSON report for programmatic consumption
adk-doc-audit audit --workspace . --docs docs/official_docs --format json > audit-report.json

# Generate Markdown report for documentation
adk-doc-audit audit --workspace . --docs docs/official_docs --format markdown > audit-report.md
```

### Incremental Audit

Audit only specific files (useful for CI/CD when you know which files changed):

```bash
adk-doc-audit incremental --workspace . --docs docs/official_docs --changed file1.md file2.md
```

### Single File Validation

Validate a specific documentation file:

```bash
adk-doc-audit validate docs/official_docs/getting-started.md
```

### Configuration File

Create a `.adk-doc-audit.toml` configuration file:

```toml
workspace_path = "."
docs_path = "docs/official_docs"
output_format = "console"

# Files to exclude from auditing
excluded_files = [
    "docs/internal/*",
    "docs/drafts/*"
]

# Crates to exclude from analysis
excluded_crates = [
    "example-crate",
    "test-utils"
]

# Severity threshold for failing builds
severity_threshold = "warning"
fail_on_critical = true

# Timeout for code example compilation
example_timeout = "30s"
```

Then run without arguments:

```bash
adk-doc-audit audit
```

## Configuration

### AuditConfig Builder

```rust
use adk_doc_audit::{AuditConfig, OutputFormat, IssueSeverity};
use std::time::Duration;

let config = AuditConfig::builder()
    .workspace_path(".")
    .docs_path("docs/official_docs")
    .output_format(OutputFormat::Json)
    .excluded_files(vec!["docs/internal/*".to_string()])
    .excluded_crates(vec!["test-utils".to_string()])
    .severity_threshold(IssueSeverity::Warning)
    .fail_on_critical(true)
    .example_timeout(Duration::from_secs(30))
    .build();
```

### Configuration Options

| Option | Description | Default |
|--------|-------------|---------|
| `workspace_path` | Path to the Rust workspace root | `"."` |
| `docs_path` | Path to documentation directory | `"docs"` |
| `output_format` | Report format (Console, Json, Markdown) | `Console` |
| `excluded_files` | Glob patterns for files to exclude | `[]` |
| `excluded_crates` | Crate names to exclude from analysis | `[]` |
| `severity_threshold` | Minimum severity to report | `Info` |
| `fail_on_critical` | Exit with error on critical issues | `true` |
| `example_timeout` | Timeout for compiling examples | `30s` |

## Issue Types and Severity

### Issue Categories

- **ApiMismatch**: API references that don't match actual implementations
- **VersionInconsistency**: Version numbers that don't match workspace versions
- **CompilationError**: Code examples that fail to compile
- **BrokenLink**: Internal links that point to non-existent files
- **MissingDocumentation**: Public APIs not documented
- **DeprecatedApi**: References to deprecated APIs

### Severity Levels

- **Critical**: Issues that make documentation incorrect or unusable
- **Warning**: Issues that may confuse users but don't break functionality
- **Info**: Minor issues or suggestions for improvement

## CI/CD Integration

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
      - name: Run documentation audit
        run: |
          cargo install --git https://github.com/zavora-ai/adk-rust adk-doc-audit
          adk-doc-audit audit --workspace . --docs docs --format json
```

### Exit Codes

- `0`: Audit passed (no critical issues)
- `1`: Audit failed (critical issues found or configuration error)

### Incremental CI/CD

For faster CI/CD, audit only changed files:

```bash
# Get changed files from git
CHANGED_FILES=$(git diff --name-only HEAD~1 HEAD -- '*.md' | tr '\n' ' ')

if [ -n "$CHANGED_FILES" ]; then
    adk-doc-audit incremental --workspace . --docs docs --changed $CHANGED_FILES
else
    echo "No documentation files changed"
fi
```

## Integration with ADK-Rust Workflows

### Spec-Driven Development

The audit system integrates with ADK-Rust's spec-driven development workflow:

1. **Requirements Phase**: Validate that all requirements are documented
2. **Design Phase**: Ensure design documents reference valid APIs
3. **Implementation Phase**: Verify code examples compile with new implementations
4. **Testing Phase**: Validate that property tests are documented

### Hook Integration

Set up automatic audits when crates are updated:

```rust
use adk_doc_audit::{AuditOrchestrator, AuditConfig};

// Register hook for crate updates
let config = AuditConfig::builder()
    .workspace_path(".")
    .docs_path("docs")
    .build();

let orchestrator = AuditOrchestrator::new(config)?;

// This would be called by the hook system when a crate is updated
orchestrator.register_hook("crate_updated", |crate_name| async move {
    println!("Running audit after {} was updated", crate_name);
    // Run incremental audit on affected documentation
});
```

## Report Formats

### Console Output

Human-readable output with colored indicators:

```
=== Documentation Audit Report ===
📊 Summary:
  Total files: 15
  Files with issues: 3
  Total issues: 7
  Critical: 2, Warning: 3, Info: 2
  Coverage: 87.5%

❌ docs/api-reference.md:42
   [Critical] API reference 'GeminiModel::new_with_config' not found in crate
   💡 Suggestion: Use 'GeminiModel::with_config' instead

⚠️  docs/examples.md:15
   [Warning] Version reference '0.1.8' doesn't match workspace version '0.2.0'
   💡 Suggestion: Update to version '0.2.0'
```

### JSON Output

Machine-readable format for integration:

```json
{
  "summary": {
    "total_files": 15,
    "files_with_issues": 3,
    "total_issues": 7,
    "critical_issues": 2,
    "warning_issues": 3,
    "info_issues": 2,
    "coverage_percentage": 87.5
  },
  "issues": [
    {
      "file_path": "docs/api-reference.md",
      "line_number": 42,
      "severity": "Critical",
      "category": "ApiMismatch",
      "message": "API reference 'GeminiModel::new_with_config' not found in crate",
      "suggestion": "Use 'GeminiModel::with_config' instead"
    }
  ],
  "timestamp": "2024-01-15T10:30:00Z"
}
```

### Markdown Output

Documentation-friendly format:

```markdown
# Documentation Audit Report

Generated: 2024-01-15 10:30:00 UTC

## Summary

- **Total files**: 15
- **Files with issues**: 3  
- **Total issues**: 7
- **Critical issues**: 2
- **Coverage**: 87.5%

## Issues by File

### docs/api-reference.md

#### ❌ Line 42 - Critical
**API reference 'GeminiModel::new_with_config' not found in crate**
- Category: ApiMismatch
- Suggestion: Use 'GeminiModel::with_config' instead
```

## Advanced Usage

### Custom Validators

Extend the audit system with custom validation logic:

```rust
use adk_doc_audit::{Validator, ValidationResult, CodeExample};
use async_trait::async_trait;

struct CustomValidator;

#[async_trait]
impl Validator for CustomValidator {
    async fn validate_example(&self, example: &CodeExample) -> ValidationResult {
        // Custom validation logic
        ValidationResult {
            success: true,
            errors: vec![],
            warnings: vec![],
            suggestions: vec!["Consider adding error handling".to_string()],
        }
    }
}

// Register custom validator
let mut orchestrator = AuditOrchestrator::new(config)?;
orchestrator.add_validator(Box::new(CustomValidator));
```

### Programmatic Report Processing

Process audit reports programmatically:

```rust
use adk_doc_audit::{AuditReport, IssueSeverity, IssueCategory};

fn process_report(report: &AuditReport) {
    // Filter critical API mismatches
    let critical_api_issues: Vec<_> = report.issues
        .iter()
        .filter(|issue| {
            matches!(issue.severity, IssueSeverity::Critical) &&
            matches!(issue.category, IssueCategory::ApiMismatch)
        })
        .collect();
    
    if !critical_api_issues.is_empty() {
        println!("Found {} critical API issues that need immediate attention:", 
                 critical_api_issues.len());
        
        for issue in critical_api_issues {
            println!("  - {}: {}", issue.file_path.display(), issue.message);
        }
    }
    
    // Generate metrics for tracking
    let metrics = serde_json::json!({
        "total_issues": report.summary.total_issues,
        "critical_issues": report.summary.critical_issues,
        "coverage": report.summary.coverage_percentage,
        "timestamp": report.timestamp
    });
    
    // Send to monitoring system, save to database, etc.
}
```

## Troubleshooting

### Common Issues

1. **"Workspace not found"**: Ensure the workspace path points to a directory with `Cargo.toml`
2. **"Documentation directory not found"**: Check that the docs path exists and contains markdown files
3. **"Compilation timeout"**: Increase `example_timeout` for complex examples
4. **"Permission denied"**: Ensure the process has read access to workspace and docs directories

### Debug Mode

Enable debug logging for troubleshooting:

```rust
use tracing_subscriber;

// Initialize debug logging
tracing_subscriber::fmt()
    .with_max_level(tracing::Level::DEBUG)
    .init();

// Run audit with debug output
let report = orchestrator.run_full_audit().await?;
```

### Performance Tuning

For large documentation sets:

```rust
let config = AuditConfig::builder()
    .workspace_path(".")
    .docs_path("docs")
    .example_timeout(Duration::from_secs(60))  // Increase timeout
    .excluded_files(vec![
        "docs/generated/*",  // Exclude auto-generated docs
        "docs/archive/*",    // Exclude archived content
    ])
    .build();
```

## Contributing

Contributions are welcome! Please see the [ADK-Rust contributing guide](https://github.com/zavora-ai/adk-rust/blob/main/CONTRIBUTING.md) for details.

### Adding New Validators

To add new validation capabilities:

1. Implement the `Validator` trait
2. Add configuration options if needed
3. Write property tests for the validator
4. Update documentation and examples

### Extending Report Formats

To add new report formats:

1. Implement the `ReportFormatter` trait
2. Add the format to the `OutputFormat` enum
3. Update CLI argument parsing
4. Add tests and documentation

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE) for details.