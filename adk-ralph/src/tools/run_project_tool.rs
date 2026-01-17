//! Run Project Tool for executing and testing generated projects.
//!
//! This tool detects the project language and executes appropriate
//! run or test commands, capturing stdout/stderr.
//!
//! ## Requirements Validated
//!
//! - 8.1: WHEN the user says "run the project" or "test it", THE Orchestrator_Agent SHALL invoke `run_project`
//! - 8.2: THE `run_project` tool SHALL detect the project language and use appropriate commands
//! - 8.3: THE System SHALL capture and display stdout/stderr from the executed project
//! - 8.5: THE System SHALL support running with arguments

use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

/// Supported programming languages for project execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Go,
    Python,
    Node,
    TypeScript,
    Java,
    Unknown,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::Go => write!(f, "go"),
            Language::Python => write!(f, "python"),
            Language::Node => write!(f, "node"),
            Language::TypeScript => write!(f, "typescript"),
            Language::Java => write!(f, "java"),
            Language::Unknown => write!(f, "unknown"),
        }
    }
}

/// Tool for running and testing generated projects.
///
/// Detects the project language from manifest files and executes
/// appropriate run/test commands.
///
/// # Input
///
/// ```json
/// {
///     "operation": "run",
///     "args": ["--help"]
/// }
/// ```
///
/// # Output
///
/// ```json
/// {
///     "success": true,
///     "exit_code": 0,
///     "stdout": "...",
///     "stderr": "",
///     "language": "rust",
///     "command": "cargo run -- --help"
/// }
/// ```
pub struct RunProjectTool {
    project_path: PathBuf,
}

impl RunProjectTool {
    /// Create a new RunProjectTool for the given project path.
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self {
            project_path: project_path.into(),
        }
    }

    /// Detect the programming language from project files.
    ///
    /// Checks for language-specific manifest files:
    /// - Cargo.toml → Rust
    /// - go.mod → Go
    /// - package.json → Node/TypeScript
    /// - requirements.txt / pyproject.toml → Python
    /// - pom.xml / build.gradle → Java
    pub fn detect_language(&self) -> Language {
        // Check for Rust
        if self.project_path.join("Cargo.toml").exists() {
            return Language::Rust;
        }

        // Check for Go
        if self.project_path.join("go.mod").exists() {
            return Language::Go;
        }

        // Check for Node/TypeScript
        if self.project_path.join("package.json").exists() {
            // Check if it's TypeScript
            if self.project_path.join("tsconfig.json").exists() {
                return Language::TypeScript;
            }
            return Language::Node;
        }

        // Check for Python
        if self.project_path.join("requirements.txt").exists()
            || self.project_path.join("pyproject.toml").exists()
            || self.project_path.join("setup.py").exists()
        {
            return Language::Python;
        }

        // Check for Java
        if self.project_path.join("pom.xml").exists()
            || self.project_path.join("build.gradle").exists()
            || self.project_path.join("build.gradle.kts").exists()
        {
            return Language::Java;
        }

        Language::Unknown
    }

    /// Get the run command for a language.
    pub fn get_run_command(&self, language: Language, args: &[String]) -> (String, Vec<String>) {
        let args_str = args.join(" ");
        
        match language {
            Language::Rust => {
                let mut cmd_args = vec!["run".to_string()];
                if !args.is_empty() {
                    cmd_args.push("--".to_string());
                    cmd_args.extend(args.iter().cloned());
                }
                ("cargo".to_string(), cmd_args)
            }
            Language::Go => {
                let mut cmd_args = vec!["run".to_string(), ".".to_string()];
                cmd_args.extend(args.iter().cloned());
                ("go".to_string(), cmd_args)
            }
            Language::Python => {
                // Try to find main.py or app.py
                let main_file = if self.project_path.join("main.py").exists() {
                    "main.py"
                } else if self.project_path.join("app.py").exists() {
                    "app.py"
                } else if self.project_path.join("src/main.py").exists() {
                    "src/main.py"
                } else {
                    "main.py" // Default
                };
                let mut cmd_args = vec![main_file.to_string()];
                cmd_args.extend(args.iter().cloned());
                ("python".to_string(), cmd_args)
            }
            Language::Node => {
                let mut cmd_args = vec!["start".to_string()];
                if !args.is_empty() {
                    cmd_args.push("--".to_string());
                    cmd_args.extend(args.iter().cloned());
                }
                ("npm".to_string(), cmd_args)
            }
            Language::TypeScript => {
                // Use ts-node or npm start
                let mut cmd_args = vec!["start".to_string()];
                if !args.is_empty() {
                    cmd_args.push("--".to_string());
                    cmd_args.extend(args.iter().cloned());
                }
                ("npm".to_string(), cmd_args)
            }
            Language::Java => {
                // Use Maven or Gradle
                if self.project_path.join("pom.xml").exists() {
                    let mut cmd_args = vec!["exec:java".to_string()];
                    if !args.is_empty() {
                        cmd_args.push(format!("-Dexec.args={}", args_str));
                    }
                    ("mvn".to_string(), cmd_args)
                } else {
                    let mut cmd_args = vec!["run".to_string()];
                    if !args.is_empty() {
                        cmd_args.push(format!("--args={}", args_str));
                    }
                    ("./gradlew".to_string(), cmd_args)
                }
            }
            Language::Unknown => {
                // Try to run a generic script
                ("echo".to_string(), vec!["Unknown project type".to_string()])
            }
        }
    }

    /// Get the test command for a language.
    pub fn get_test_command(&self, language: Language) -> (String, Vec<String>) {
        match language {
            Language::Rust => ("cargo".to_string(), vec!["test".to_string()]),
            Language::Go => ("go".to_string(), vec!["test".to_string(), "./...".to_string()]),
            Language::Python => ("pytest".to_string(), vec![]),
            Language::Node | Language::TypeScript => ("npm".to_string(), vec!["test".to_string()]),
            Language::Java => {
                if self.project_path.join("pom.xml").exists() {
                    ("mvn".to_string(), vec!["test".to_string()])
                } else {
                    ("./gradlew".to_string(), vec!["test".to_string()])
                }
            }
            Language::Unknown => ("echo".to_string(), vec!["No test command available".to_string()]),
        }
    }

    /// Execute a command and capture output.
    async fn execute_command(
        &self,
        program: &str,
        args: &[String],
    ) -> Result<(i32, String, String)> {
        use tokio::process::Command;

        let output = Command::new(program)
            .args(args)
            .current_dir(&self.project_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to execute command: {}", e)))?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok((exit_code, stdout, stderr))
    }
}

impl std::fmt::Debug for RunProjectTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunProjectTool")
            .field("project_path", &self.project_path)
            .finish()
    }
}

#[async_trait]
impl Tool for RunProjectTool {
    fn name(&self) -> &str {
        "run_project"
    }

    fn description(&self) -> &str {
        "Run or test the generated project. Automatically detects the project language and uses appropriate commands (cargo run, go run, python, npm start, etc.)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["run", "test"],
                    "description": "Whether to run the project or run tests"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Additional arguments to pass to the run command"
                }
            },
            "required": ["operation"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            operation: String,
            #[serde(default)]
            args: Vec<String>,
        }

        let args: Args = serde_json::from_value(args)
            .map_err(|e| AdkError::Tool(format!("Invalid arguments: {}", e)))?;

        // Detect language
        let language = self.detect_language();
        
        tracing::info!(
            operation = %args.operation,
            language = %language,
            project_path = %self.project_path.display(),
            "Executing project"
        );

        // Get the appropriate command
        let (program, cmd_args) = match args.operation.as_str() {
            "run" => self.get_run_command(language, &args.args),
            "test" => self.get_test_command(language),
            op => {
                return Err(AdkError::Tool(format!(
                    "Unknown operation: {}. Use 'run' or 'test'",
                    op
                )));
            }
        };

        // Build command string for logging
        let command_str = format!("{} {}", program, cmd_args.join(" "));
        
        tracing::debug!(command = %command_str, "Running command");

        // Execute the command
        let (exit_code, stdout, stderr) = self.execute_command(&program, &cmd_args).await?;

        let success = exit_code == 0;

        tracing::info!(
            exit_code = exit_code,
            success = success,
            "Command execution complete"
        );

        Ok(json!({
            "success": success,
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "language": language.to_string(),
            "command": command_str
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_run_project_tool_name() {
        let tool = RunProjectTool::new("/tmp/test");
        assert_eq!(tool.name(), "run_project");
    }

    #[test]
    fn test_run_project_tool_description() {
        let tool = RunProjectTool::new("/tmp/test");
        assert!(tool.description().contains("run"));
        assert!(tool.description().contains("test"));
    }

    #[test]
    fn test_run_project_tool_schema() {
        let tool = RunProjectTool::new("/tmp/test");
        let schema = tool.parameters_schema().unwrap();
        
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["operation"].is_object());
        assert!(schema["properties"]["args"].is_object());
    }

    #[test]
    fn test_detect_language_rust() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("Cargo.toml"), "[package]").unwrap();
        
        let tool = RunProjectTool::new(temp_dir.path());
        assert_eq!(tool.detect_language(), Language::Rust);
    }

    #[test]
    fn test_detect_language_go() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("go.mod"), "module test").unwrap();
        
        let tool = RunProjectTool::new(temp_dir.path());
        assert_eq!(tool.detect_language(), Language::Go);
    }

    #[test]
    fn test_detect_language_python() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("requirements.txt"), "").unwrap();
        
        let tool = RunProjectTool::new(temp_dir.path());
        assert_eq!(tool.detect_language(), Language::Python);
    }

    #[test]
    fn test_detect_language_node() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("package.json"), "{}").unwrap();
        
        let tool = RunProjectTool::new(temp_dir.path());
        assert_eq!(tool.detect_language(), Language::Node);
    }

    #[test]
    fn test_detect_language_typescript() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(temp_dir.path().join("tsconfig.json"), "{}").unwrap();
        
        let tool = RunProjectTool::new(temp_dir.path());
        assert_eq!(tool.detect_language(), Language::TypeScript);
    }

    #[test]
    fn test_detect_language_unknown() {
        let temp_dir = TempDir::new().unwrap();
        
        let tool = RunProjectTool::new(temp_dir.path());
        assert_eq!(tool.detect_language(), Language::Unknown);
    }

    #[test]
    fn test_get_run_command_rust() {
        let tool = RunProjectTool::new("/tmp/test");
        let (program, args) = tool.get_run_command(Language::Rust, &[]);
        
        assert_eq!(program, "cargo");
        assert_eq!(args, vec!["run"]);
    }

    #[test]
    fn test_get_run_command_rust_with_args() {
        let tool = RunProjectTool::new("/tmp/test");
        let (program, args) = tool.get_run_command(
            Language::Rust,
            &["--help".to_string(), "-v".to_string()],
        );
        
        assert_eq!(program, "cargo");
        assert_eq!(args, vec!["run", "--", "--help", "-v"]);
    }

    #[test]
    fn test_get_run_command_go() {
        let tool = RunProjectTool::new("/tmp/test");
        let (program, args) = tool.get_run_command(Language::Go, &[]);
        
        assert_eq!(program, "go");
        assert_eq!(args, vec!["run", "."]);
    }

    #[test]
    fn test_get_test_command_rust() {
        let tool = RunProjectTool::new("/tmp/test");
        let (program, args) = tool.get_test_command(Language::Rust);
        
        assert_eq!(program, "cargo");
        assert_eq!(args, vec!["test"]);
    }

    #[test]
    fn test_get_test_command_python() {
        let tool = RunProjectTool::new("/tmp/test");
        let (program, args) = tool.get_test_command(Language::Python);
        
        assert_eq!(program, "pytest");
        assert!(args.is_empty());
    }

    #[test]
    fn test_language_display() {
        assert_eq!(Language::Rust.to_string(), "rust");
        assert_eq!(Language::Go.to_string(), "go");
        assert_eq!(Language::Python.to_string(), "python");
        assert_eq!(Language::Node.to_string(), "node");
        assert_eq!(Language::TypeScript.to_string(), "typescript");
        assert_eq!(Language::Java.to_string(), "java");
        assert_eq!(Language::Unknown.to_string(), "unknown");
    }
}
