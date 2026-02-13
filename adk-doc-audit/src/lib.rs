//! # adk-doc-audit
//!
//! Documentation audit system for ADK-Rust that validates documentation against actual crate implementations.
//!
//! This crate provides comprehensive documentation validation including:
//! - API reference validation against actual implementations
//! - Code example compilation testing
//! - Version consistency checking
//! - Cross-reference validation
//! - Automated fix suggestions
//! - Comprehensive audit reporting
//!
//! ## Features
//!
//! - **API Validation**: Ensures all API references match current implementations
//! - **Code Compilation**: Validates that documentation examples compile
//! - **Version Consistency**: Checks version references are current
//! - **Link Validation**: Validates internal documentation links
//! - **Automated Suggestions**: Provides fix suggestions for issues
//! - **Multiple Formats**: Supports console, JSON, and Markdown output
//! - **CI/CD Integration**: Designed for build pipeline integration
//! - **Incremental Audits**: Supports auditing only changed files
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_doc_audit::{AuditConfig, AuditOrchestrator};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = AuditConfig::builder()
//!         .workspace_path(".")
//!         .docs_path("docs/official_docs")
//!         .build()?;
//!     
//!     let orchestrator = AuditOrchestrator::new(config).await?;
//!     let report = orchestrator.run_full_audit().await?;
//!     
//!     println!("Audit complete: {} issues found", report.summary.total_issues);
//!     Ok(())
//! }
//! ```
//!
//! ## CLI Usage
//!
//! The crate also provides a command-line interface:
//!
//! ```bash
//! # Run full audit
//! adk-doc-audit audit --workspace . --docs docs/official_docs
//!
//! # Run incremental audit
//! adk-doc-audit incremental --workspace . --docs docs/official_docs --changed file1.md file2.md
//!
//! # Validate single file
//! adk-doc-audit validate docs/official_docs/getting-started.md
//! ```

pub mod analyzer;
pub mod cli;
pub mod config;
pub mod error;
pub mod orchestrator;
pub mod parser;
pub mod reporter;
pub mod suggestion;
pub mod validator;
pub mod version;

// Re-export commonly used types
pub use analyzer::{
    CodeAnalyzer, CrateInfo, CrateRegistry, Dependency, PublicApi, ValidationResult,
};
pub use cli::{AuditCli, AuditCommand, CliOutputFormat, CliSeverity};
pub use config::{AuditConfig, AuditConfigBuilder, IssueSeverity, OutputFormat};
pub use error::{AuditError, Result};
pub use orchestrator::AuditOrchestrator;
pub use parser::{
    ApiItemType, ApiReference, CodeExample, DocumentationParser, FeatureMention, InternalLink,
    ParsedDocument, VersionReference, VersionType,
};
pub use reporter::{
    AuditIssue, AuditReport, AuditReportConfig, AuditSummary, FileAuditResult, IssueCategory,
    ProblematicFile, Recommendation, RecommendationType, ReportGenerator,
};
pub use suggestion::{Suggestion, SuggestionConfig, SuggestionEngine, SuggestionType};
pub use validator::{
    AsyncValidationConfig, CompilationError, ErrorType, ExampleValidator, ValidationMetadata,
    ValidationResult as ExampleValidationResult,
};
pub use version::{
    DependencySpec, ValidationSeverity, VersionTolerance, VersionValidationConfig,
    VersionValidationResult, VersionValidator, WorkspaceVersionInfo,
};

/// Version information for the crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Name of the crate.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_info() {
        assert!(!VERSION.is_empty());
        assert_eq!(CRATE_NAME, "adk-doc-audit");
    }

    #[test]
    fn test_config_creation() {
        // Create temporary directories for testing
        let temp_dir = std::env::temp_dir();
        let workspace_path = temp_dir.join("test_workspace_2");
        let docs_path = temp_dir.join("test_docs_2");

        // Create the directories
        std::fs::create_dir_all(&workspace_path).unwrap();
        std::fs::create_dir_all(&docs_path).unwrap();

        let config =
            AuditConfig::builder().workspace_path(&workspace_path).docs_path(&docs_path).build();

        // This should succeed now that paths exist
        assert!(config.is_ok());

        // Clean up
        std::fs::remove_dir_all(&workspace_path).ok();
        std::fs::remove_dir_all(&docs_path).ok();
    }
}
