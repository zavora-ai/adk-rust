//! Environment validation for Ralph.
//!
//! This module checks that required tools and dependencies are installed
//! before starting the implementation phase.

use crate::output::RalphOutput;
use colored::Colorize;
use std::io::{self, Write};
use std::process::Command;

/// Supported languages and their required tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Java,
}

impl Language {
    /// Detect language from project description or design.
    pub fn detect(text: &str) -> Option<Self> {
        let lower = text.to_lowercase();
        
        if lower.contains("rust") {
            Some(Language::Rust)
        } else if lower.contains("python") {
            Some(Language::Python)
        } else if lower.contains("typescript") || lower.contains(" ts ") {
            Some(Language::TypeScript)
        } else if lower.contains("javascript") || lower.contains(" js ") {
            Some(Language::JavaScript)
        } else if lower.contains(" go ") || lower.contains("golang") {
            Some(Language::Go)
        } else if lower.contains("java") && !lower.contains("javascript") {
            Some(Language::Java)
        } else {
            None
        }
    }

    /// Get the display name.
    pub fn name(&self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Python => "Python",
            Language::TypeScript => "TypeScript",
            Language::JavaScript => "JavaScript",
            Language::Go => "Go",
            Language::Java => "Java",
        }
    }

    /// Get required tools for this language.
    pub fn required_tools(&self) -> Vec<ToolRequirement> {
        match self {
            Language::Rust => vec![
                ToolRequirement::new("rustc", "rustc --version", "Rust compiler")
                    .with_install_hint("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"),
                ToolRequirement::new("cargo", "cargo --version", "Rust package manager")
                    .with_install_hint("Installed with rustup"),
            ],
            Language::Python => vec![
                ToolRequirement::new("python3", "python3 --version", "Python interpreter")
                    .with_install_hint("brew install python3 (macOS) or apt install python3 (Linux)"),
                ToolRequirement::new("pip3", "pip3 --version", "Python package manager")
                    .with_install_hint("python3 -m ensurepip --upgrade"),
                ToolRequirement::new("pytest", "pytest --version", "Python test framework")
                    .with_install_hint("pip3 install pytest")
                    .with_install_cmd("pip3 install pytest"),
            ],
            Language::TypeScript => vec![
                ToolRequirement::new("node", "node --version", "Node.js runtime")
                    .with_install_hint("brew install node (macOS) or https://nodejs.org"),
                ToolRequirement::new("npm", "npm --version", "Node package manager")
                    .with_install_hint("Installed with Node.js"),
                ToolRequirement::new("npx", "npx --version", "Node package executor")
                    .with_install_hint("Installed with Node.js"),
            ],
            Language::JavaScript => vec![
                ToolRequirement::new("node", "node --version", "Node.js runtime")
                    .with_install_hint("brew install node (macOS) or https://nodejs.org"),
                ToolRequirement::new("npm", "npm --version", "Node package manager")
                    .with_install_hint("Installed with Node.js"),
            ],
            Language::Go => vec![
                ToolRequirement::new("go", "go version", "Go compiler")
                    .with_install_hint("brew install go (macOS) or https://golang.org/dl/"),
            ],
            Language::Java => vec![
                ToolRequirement::new("java", "java --version", "Java runtime")
                    .with_install_hint("brew install openjdk (macOS) or apt install default-jdk (Linux)"),
                ToolRequirement::new("mvn", "mvn --version", "Maven build tool")
                    .with_install_hint("brew install maven (macOS) or apt install maven (Linux)"),
            ],
        }
    }
}

/// A tool requirement with version check command.
#[derive(Debug, Clone)]
pub struct ToolRequirement {
    /// Tool name (command).
    pub name: &'static str,
    /// Command to check version.
    pub version_cmd: &'static str,
    /// Description of the tool.
    pub description: &'static str,
    /// Hint for manual installation.
    pub install_hint: Option<&'static str>,
    /// Command to auto-install (if possible).
    pub install_cmd: Option<&'static str>,
}

impl ToolRequirement {
    pub fn new(name: &'static str, version_cmd: &'static str, description: &'static str) -> Self {
        Self {
            name,
            version_cmd,
            description,
            install_hint: None,
            install_cmd: None,
        }
    }

    pub fn with_install_hint(mut self, hint: &'static str) -> Self {
        self.install_hint = Some(hint);
        self
    }

    pub fn with_install_cmd(mut self, cmd: &'static str) -> Self {
        self.install_cmd = Some(cmd);
        self
    }

    /// Check if the tool is installed.
    pub fn is_installed(&self) -> bool {
        let parts: Vec<&str> = self.version_cmd.split_whitespace().collect();
        if parts.is_empty() {
            return false;
        }

        Command::new(parts[0])
            .args(&parts[1..])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get the installed version.
    pub fn get_version(&self) -> Option<String> {
        let parts: Vec<&str> = self.version_cmd.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        Command::new(parts[0])
            .args(&parts[1..])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
    }

    /// Try to install the tool.
    pub fn try_install(&self) -> Result<(), String> {
        let cmd = self.install_cmd.ok_or_else(|| {
            format!(
                "No auto-install available. Please install manually: {}",
                self.install_hint.unwrap_or("See documentation")
            )
        })?;

        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return Err("Invalid install command".to_string());
        }

        let output = Command::new(parts[0])
            .args(&parts[1..])
            .output()
            .map_err(|e| format!("Failed to run install command: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "Install failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
}

/// Result of environment validation.
#[derive(Debug, Clone)]
pub struct EnvironmentStatus {
    /// Detected language.
    pub language: Option<Language>,
    /// Tools that are installed.
    pub installed: Vec<String>,
    /// Tools that are missing.
    pub missing: Vec<ToolRequirement>,
    /// Whether to skip tests/builds due to missing tools.
    pub skip_validation: bool,
}

impl EnvironmentStatus {
    /// Check if environment is fully ready.
    pub fn is_ready(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Validate the environment for a given language.
pub fn validate_environment(
    language: Language,
    output: &RalphOutput,
    interactive: bool,
) -> EnvironmentStatus {
    output.phase(&format!("Environment Check: {}", language.name()));

    let requirements = language.required_tools();
    let mut installed = Vec::new();
    let mut missing = Vec::new();

    // Check each tool
    for req in requirements {
        if req.is_installed() {
            let version = req.get_version().unwrap_or_else(|| "unknown".to_string());
            output.status(&format!("✓ {} - {}", req.name, version.bright_black()));
            installed.push(req.name.to_string());
        } else {
            output.status(&format!("✗ {} - {}", req.name.red(), "not found".red()));
            missing.push(req);
        }
    }

    // Handle missing tools
    let mut skip_validation = false;
    
    if !missing.is_empty() && interactive {
        println!();
        
        for req in &missing {
            // Check if we can auto-install
            if req.install_cmd.is_some() {
                print!(
                    "  {} Install {}? [y/N] ",
                    "?".bright_yellow(),
                    req.name.cyan()
                );
                io::stdout().flush().unwrap();

                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_ok() {
                    let answer = input.trim().to_lowercase();
                    if answer == "y" || answer == "yes" {
                        output.status(&format!("Installing {}...", req.name));
                        match req.try_install() {
                            Ok(()) => {
                                output.status(&format!("✓ {} installed", req.name.green()));
                                installed.push(req.name.to_string());
                                continue;
                            }
                            Err(e) => {
                                output.error(&format!("Failed to install {}: {}", req.name, e));
                            }
                        }
                    }
                }
            }

            // Show manual install hint
            if let Some(hint) = req.install_hint {
                println!("    {} {}", "Hint:".bright_black(), hint.bright_black());
            }
        }

        // Ask if user wants to continue without missing tools
        if !missing.is_empty() {
            println!();
            print!(
                "  {} Continue without {}? Tests/builds will be skipped. [y/N] ",
                "?".bright_yellow(),
                missing.iter().map(|r| r.name).collect::<Vec<_>>().join(", ").red()
            );
            io::stdout().flush().unwrap();

            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_ok() {
                let answer = input.trim().to_lowercase();
                if answer == "y" || answer == "yes" {
                    skip_validation = true;
                    output.warn("Continuing without full toolchain - tests/builds will be skipped");
                }
            }
        }
    } else if !missing.is_empty() {
        // Non-interactive mode - just warn
        skip_validation = true;
        output.warn(&format!(
            "Missing tools: {}. Tests/builds will be skipped.",
            missing.iter().map(|r| r.name).collect::<Vec<_>>().join(", ")
        ));
    }

    // Filter out tools that were installed
    let still_missing: Vec<ToolRequirement> = missing
        .into_iter()
        .filter(|r| !installed.contains(&r.name.to_string()))
        .collect();

    if still_missing.is_empty() {
        output.phase_complete("All tools available");
    }

    EnvironmentStatus {
        language: Some(language),
        installed,
        missing: still_missing,
        skip_validation,
    }
}

/// Initialize git repository if not exists.
pub fn ensure_git_repo(project_path: &std::path::Path, output: &RalphOutput) -> bool {
    let git_dir = project_path.join(".git");
    
    if git_dir.exists() {
        output.status("Git repository exists");
        return true;
    }

    output.status("Initializing git repository...");
    
    let result = Command::new("git")
        .args(["init"])
        .current_dir(project_path)
        .output();

    match result {
        Ok(o) if o.status.success() => {
            output.status("✓ Git repository initialized");
            true
        }
        Ok(o) => {
            output.warn(&format!(
                "Failed to init git: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
            false
        }
        Err(e) => {
            output.warn(&format!("Git not available: {}", e));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(Language::detect("Create a CLI in Rust"), Some(Language::Rust));
        assert_eq!(Language::detect("Build a Python script"), Some(Language::Python));
        assert_eq!(Language::detect("TypeScript web app"), Some(Language::TypeScript));
        assert_eq!(Language::detect("Go microservice"), Some(Language::Go));
        assert_eq!(Language::detect("Java REST API"), Some(Language::Java));
        assert_eq!(Language::detect("Something unknown"), None);
    }

    #[test]
    fn test_tool_requirement() {
        // This will vary by system, but should not panic
        let req = ToolRequirement::new("echo", "echo test", "Test tool");
        assert!(req.is_installed());
    }
}
