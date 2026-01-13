//! Design document data structures.
//!
//! This module provides data models for system architecture and design documents,
//! including component diagrams, interfaces, and file structure definitions.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// A component in the system architecture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Component {
    /// Component name
    pub name: String,
    /// Component purpose/description
    pub purpose: String,
    /// Public interface/API description
    #[serde(default)]
    pub interface: Vec<String>,
    /// Dependencies on other components
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// File path where this component is implemented
    #[serde(default)]
    pub file_path: Option<String>,
}

impl Component {
    /// Create a new component.
    pub fn new(name: impl Into<String>, purpose: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            purpose: purpose.into(),
            interface: Vec::new(),
            dependencies: Vec::new(),
            file_path: None,
        }
    }

    /// Add an interface method/function.
    pub fn add_interface(&mut self, interface: impl Into<String>) {
        self.interface.push(interface.into());
    }

    /// Add a dependency.
    pub fn add_dependency(&mut self, dependency: impl Into<String>) {
        self.dependencies.push(dependency.into());
    }

    /// Set the file path.
    pub fn with_file_path(mut self, path: impl Into<String>) -> Self {
        self.file_path = Some(path.into());
        self
    }
}

/// A file or directory in the project structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileStructure {
    /// File or directory name
    pub name: String,
    /// Description of the file/directory purpose
    #[serde(default)]
    pub description: String,
    /// Whether this is a directory
    #[serde(default)]
    pub is_directory: bool,
    /// Child files/directories (if this is a directory)
    #[serde(default)]
    pub children: Vec<FileStructure>,
}

impl FileStructure {
    /// Create a new file entry.
    pub fn file(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            is_directory: false,
            children: Vec::new(),
        }
    }

    /// Create a new directory entry.
    pub fn directory(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            is_directory: true,
            children: Vec::new(),
        }
    }

    /// Add a child file/directory.
    pub fn add_child(&mut self, child: FileStructure) {
        self.children.push(child);
    }

    /// Convert to a tree string representation.
    pub fn to_tree(&self, prefix: &str, is_last: bool) -> String {
        let mut result = String::new();
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };

        result.push_str(prefix);
        result.push_str(connector);
        result.push_str(&self.name);
        if self.is_directory {
            result.push('/');
        }
        result.push('\n');

        for (i, child) in self.children.iter().enumerate() {
            let is_last_child = i == self.children.len() - 1;
            result.push_str(&child.to_tree(&format!("{}{}", prefix, child_prefix), is_last_child));
        }

        result
    }
}

/// Technology stack information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TechnologyStack {
    /// Primary programming language
    pub language: String,
    /// Testing framework
    #[serde(default)]
    pub testing_framework: String,
    /// Build tool
    #[serde(default)]
    pub build_tool: String,
    /// External dependencies
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Additional tools/frameworks
    #[serde(default)]
    pub additional: HashMap<String, String>,
}

impl TechnologyStack {
    /// Create a new technology stack.
    pub fn new(language: impl Into<String>) -> Self {
        Self {
            language: language.into(),
            testing_framework: String::new(),
            build_tool: String::new(),
            dependencies: Vec::new(),
            additional: HashMap::new(),
        }
    }

    /// Set the testing framework.
    pub fn with_testing(mut self, framework: impl Into<String>) -> Self {
        self.testing_framework = framework.into();
        self
    }

    /// Set the build tool.
    pub fn with_build_tool(mut self, tool: impl Into<String>) -> Self {
        self.build_tool = tool.into();
        self
    }

    /// Add a dependency.
    pub fn add_dependency(&mut self, dep: impl Into<String>) {
        self.dependencies.push(dep.into());
    }
}

/// Design document containing system architecture and design decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DesignDocument {
    /// Project name (should match PRD)
    pub project: String,
    /// Architecture overview description
    pub overview: String,
    /// Mermaid diagram source (component diagram)
    #[serde(default)]
    pub component_diagram: Option<String>,
    /// System components
    #[serde(default)]
    pub components: Vec<Component>,
    /// Project file/folder structure
    #[serde(default)]
    pub file_structure: Option<FileStructure>,
    /// Technology stack
    #[serde(default)]
    pub technology_stack: Option<TechnologyStack>,
    /// Design decisions and rationale
    #[serde(default)]
    pub design_decisions: Vec<String>,
    /// Document version
    #[serde(default = "default_version")]
    pub version: String,
    /// Creation timestamp
    #[serde(default)]
    pub created_at: Option<String>,
    /// Last update timestamp
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn default_version() -> String {
    "1.0".to_string()
}

impl DesignDocument {
    /// Create a new design document.
    pub fn new(project: impl Into<String>, overview: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            overview: overview.into(),
            component_diagram: None,
            components: Vec::new(),
            file_structure: None,
            technology_stack: None,
            design_decisions: Vec::new(),
            version: default_version(),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            updated_at: None,
        }
    }

    /// Load a design document from a JSON file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read design file '{}': {}", path.display(), e))?;

        let design: DesignDocument = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse design JSON '{}': {}", path.display(), e))?;

        design.validate()?;
        Ok(design)
    }

    /// Load a design document from a Markdown file.
    ///
    /// This parses a markdown design document and extracts the key sections.
    /// Note: This is a simplified parser that extracts basic structure.
    pub fn load_markdown<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read design file '{}': {}", path.display(), e))?;

        Self::parse_markdown(&content)
    }

    /// Parse design document from markdown content.
    pub fn parse_markdown(content: &str) -> Result<Self, String> {
        let mut project = String::new();
        let mut overview = String::new();
        let mut diagram = None;
        let mut in_overview = false;
        let mut in_diagram = false;
        let mut diagram_content = String::new();

        for line in content.lines() {
            // Extract project name from title
            if line.starts_with("# ") {
                let title = line.trim_start_matches("# ");
                if let Some(name) = title.strip_prefix("System Design: ") {
                    project = name.to_string();
                } else {
                    project = title.to_string();
                }
                continue;
            }

            // Track sections
            if line.starts_with("## Overview") || line.starts_with("## Architecture Overview") {
                in_overview = true;
                in_diagram = false;
                continue;
            }
            if line.starts_with("## Component Diagram") || line.starts_with("## Architecture Diagram") {
                in_overview = false;
                in_diagram = false;
                continue;
            }
            if line.starts_with("## ") {
                in_overview = false;
                in_diagram = false;
                continue;
            }

            // Handle mermaid code blocks
            if line.starts_with("```mermaid") {
                in_diagram = true;
                continue;
            }
            if in_diagram && line.starts_with("```") {
                in_diagram = false;
                diagram = Some(diagram_content.trim().to_string());
                diagram_content.clear();
                continue;
            }

            // Collect content
            if in_overview && !line.is_empty() {
                if !overview.is_empty() {
                    overview.push('\n');
                }
                overview.push_str(line);
            }
            if in_diagram {
                diagram_content.push_str(line);
                diagram_content.push('\n');
            }
        }

        if project.is_empty() {
            return Err("Could not extract project name from design document".to_string());
        }

        let mut design = DesignDocument::new(project, overview);
        design.component_diagram = diagram;

        Ok(design)
    }

    /// Save the design document to a JSON file.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let path = path.as_ref();
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize design: {}", e))?;

        fs::write(path, content)
            .map_err(|e| format!("Failed to write design file '{}': {}", path.display(), e))?;

        Ok(())
    }

    /// Save the design document to a Markdown file.
    pub fn save_markdown<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let content = self.to_markdown();
        let path = path.as_ref();

        fs::write(path, content)
            .map_err(|e| format!("Failed to write design file '{}': {}", path.display(), e))?;

        Ok(())
    }

    /// Convert design document to markdown format.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str(&format!("# System Design: {}\n\n", self.project));
        md.push_str("## Architecture Overview\n\n");
        md.push_str(&self.overview);
        md.push_str("\n\n");

        if let Some(diagram) = &self.component_diagram {
            md.push_str("## Component Diagram\n\n");
            md.push_str("```mermaid\n");
            md.push_str(diagram);
            md.push_str("\n```\n\n");
        }

        if !self.components.is_empty() {
            md.push_str("## Components\n\n");
            for component in &self.components {
                md.push_str(&format!("### {}\n\n", component.name));
                md.push_str(&format!("**Purpose**: {}\n\n", component.purpose));

                if !component.interface.is_empty() {
                    md.push_str("**Interface**:\n");
                    for iface in &component.interface {
                        md.push_str(&format!("- {}\n", iface));
                    }
                    md.push('\n');
                }

                if !component.dependencies.is_empty() {
                    md.push_str(&format!(
                        "**Dependencies**: {}\n\n",
                        component.dependencies.join(", ")
                    ));
                }

                if let Some(path) = &component.file_path {
                    md.push_str(&format!("**File**: `{}`\n\n", path));
                }
            }
        }

        if let Some(structure) = &self.file_structure {
            md.push_str("## File Structure\n\n");
            md.push_str("```\n");
            md.push_str(&structure.name);
            md.push_str("/\n");
            for (i, child) in structure.children.iter().enumerate() {
                let is_last = i == structure.children.len() - 1;
                md.push_str(&child.to_tree("", is_last));
            }
            md.push_str("```\n\n");
        }

        if let Some(tech) = &self.technology_stack {
            md.push_str("## Technology Stack\n\n");
            md.push_str(&format!("- **Language**: {}\n", tech.language));
            if !tech.testing_framework.is_empty() {
                md.push_str(&format!("- **Testing**: {}\n", tech.testing_framework));
            }
            if !tech.build_tool.is_empty() {
                md.push_str(&format!("- **Build Tool**: {}\n", tech.build_tool));
            }
            if !tech.dependencies.is_empty() {
                md.push_str(&format!(
                    "- **Dependencies**: {}\n",
                    tech.dependencies.join(", ")
                ));
            }
            md.push('\n');
        }

        if !self.design_decisions.is_empty() {
            md.push_str("## Design Decisions\n\n");
            for decision in &self.design_decisions {
                md.push_str(&format!("- {}\n", decision));
            }
            md.push('\n');
        }

        md
    }

    /// Validate the design document.
    pub fn validate(&self) -> Result<(), String> {
        if self.project.is_empty() {
            return Err("Project name cannot be empty".to_string());
        }
        if self.overview.is_empty() {
            return Err("Architecture overview cannot be empty".to_string());
        }
        Ok(())
    }

    /// Add a component to the design.
    pub fn add_component(&mut self, component: Component) {
        self.components.push(component);
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Set the component diagram.
    pub fn set_diagram(&mut self, diagram: impl Into<String>) {
        self.component_diagram = Some(diagram.into());
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Set the file structure.
    pub fn set_file_structure(&mut self, structure: FileStructure) {
        self.file_structure = Some(structure);
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Set the technology stack.
    pub fn set_technology_stack(&mut self, tech: TechnologyStack) {
        self.technology_stack = Some(tech);
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Add a design decision.
    pub fn add_design_decision(&mut self, decision: impl Into<String>) {
        self.design_decisions.push(decision.into());
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Get a component by name.
    pub fn get_component(&self, name: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.name == name)
    }

    /// Check if the design has all required sections.
    pub fn is_complete(&self) -> bool {
        !self.overview.is_empty()
            && self.component_diagram.is_some()
            && !self.components.is_empty()
            && self.file_structure.is_some()
            && self.technology_stack.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_creation() {
        let mut component = Component::new("TestComponent", "Test purpose");
        component.add_interface("fn test() -> Result<()>");
        component.add_dependency("OtherComponent");

        assert_eq!(component.name, "TestComponent");
        assert_eq!(component.interface.len(), 1);
        assert_eq!(component.dependencies.len(), 1);
    }

    #[test]
    fn test_file_structure_tree() {
        let mut root = FileStructure::directory("project", "Root directory");
        let mut src = FileStructure::directory("src", "Source files");
        src.add_child(FileStructure::file("main.rs", "Entry point"));
        src.add_child(FileStructure::file("lib.rs", "Library"));
        root.add_child(src);
        root.add_child(FileStructure::file("Cargo.toml", "Manifest"));

        let tree = root.to_tree("", true);
        assert!(tree.contains("src/"));
        assert!(tree.contains("main.rs"));
    }

    #[test]
    fn test_design_document_validation() {
        let design = DesignDocument::new("Test", "Test overview");
        assert!(design.validate().is_ok());

        let empty_design = DesignDocument::new("", "Overview");
        assert!(empty_design.validate().is_err());
    }
}
