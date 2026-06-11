//! Agent template definitions for the composable scaffolding engine.
//!
//! Templates define the *agent structure* — the core agent type and its construction code.

use crate::addon::DependencySpec;

/// Category of a template in the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateCategory {
    /// A core agent type (llm, sequential, parallel, etc.)
    AgentType,
    /// An enterprise-grade pre-composed pattern
    EnterprisePattern,
    /// A legacy template preserved for backward compatibility
    Legacy,
}

/// A file to be generated as part of the scaffold.
#[derive(Debug, Clone)]
pub struct FileFragment {
    /// Relative path within the generated project.
    pub path: &'static str,
    /// File content to write.
    pub content: &'static str,
}

/// Code fragments that define the agent's structure in `main.rs`.
#[derive(Debug, Clone)]
pub struct AgentCodeFragments {
    /// Import statements needed at the top of `main.rs`.
    pub imports: Vec<&'static str>,
    /// The agent construction code (e.g., `LlmAgent::builder()...`).
    pub agent_construction: &'static str,
    /// Additional files to generate beyond `main.rs`.
    pub additional_files: Vec<FileFragment>,
}

/// Defines an agent type template (e.g., llm, sequential, graph).
#[derive(Debug, Clone)]
pub struct AgentTemplate {
    /// Template name used in CLI (e.g., "llm", "sequential").
    pub name: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Category for grouping in display.
    pub category: TemplateCategory,
    /// Default LLM provider for this template.
    pub default_provider: &'static str,
    /// Cargo features required by this template.
    pub required_features: Vec<&'static str>,
    /// Addons that are incompatible with this template.
    pub incompatible_addons: Vec<&'static str>,
    /// Additional crate dependencies beyond `adk-rust` (e.g., `adk-tool`,
    /// `schemars` for the `tools` template).
    pub additional_deps: Vec<DependencySpec>,
    /// Code fragments for generating the agent.
    pub code_fragments: AgentCodeFragments,
}
