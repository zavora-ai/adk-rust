//! Git tool for version control operations.
//!
//! Provides git operations: status, add, commit, diff

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

/// Git tool with operation-based interface.
///
/// Supports operations: status, add, commit, diff
pub struct GitTool {
    project_path: PathBuf,
}

impl GitTool {
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self {
            project_path: project_path.into(),
        }
    }

    fn run_git(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.project_path)
            .output()
            .map_err(|e| adk_core::AdkError::Tool(format!("Failed to run git: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(adk_core::AdkError::Tool(format!(
                "Git command failed: {}",
                if stderr.is_empty() { &stdout } else { &stderr }
            )));
        }

        Ok(stdout)
    }
}

impl std::fmt::Debug for GitTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitTool")
            .field("project_path", &self.project_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Git operations: status, add, commit, diff for version control."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["status", "add", "commit", "diff"],
                    "description": "The git operation to perform"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files to add (for 'add' operation). Use ['.'] for all files."
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (required for 'commit' operation)"
                },
                "path": {
                    "type": "string",
                    "description": "Path to diff (optional for 'diff' operation)"
                }
            },
            "required": ["operation"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            operation: String,
            files: Option<Vec<String>>,
            message: Option<String>,
            path: Option<String>,
        }

        let args: Args = serde_json::from_value(args)
            .map_err(|e| adk_core::AdkError::Tool(format!("Invalid arguments: {}", e)))?;

        match args.operation.as_str() {
            "status" => {
                let output = self.run_git(&["status", "--porcelain"])?;
                let changes: Vec<Value> = output
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(|line| {
                        let status = &line[..2];
                        let file = line[3..].trim();
                        json!({
                            "status": status.trim(),
                            "file": file
                        })
                    })
                    .collect();

                Ok(json!({
                    "success": true,
                    "operation": "status",
                    "changes": changes,
                    "clean": changes.is_empty()
                }))
            }
            "add" => {
                let files = args.files.unwrap_or_else(|| vec![".".to_string()]);
                let file_refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();

                let mut git_args = vec!["add"];
                git_args.extend(file_refs.iter());

                self.run_git(&git_args)?;

                Ok(json!({
                    "success": true,
                    "operation": "add",
                    "files": files
                }))
            }
            "commit" => {
                let message = args.message.ok_or_else(|| {
                    adk_core::AdkError::Tool(
                        "'message' is required for commit operation".to_string(),
                    )
                })?;

                let output = self.run_git(&["commit", "-m", &message])?;

                // Extract commit hash from output
                let commit_hash = output
                    .lines()
                    .find(|line| line.contains('['))
                    .and_then(|line| {
                        line.split_whitespace()
                            .find(|word| word.len() >= 7 && word.chars().all(|c| c.is_ascii_hexdigit()))
                    })
                    .map(|s| s.to_string());

                Ok(json!({
                    "success": true,
                    "operation": "commit",
                    "message": message,
                    "commit_hash": commit_hash,
                    "output": output.trim()
                }))
            }
            "diff" => {
                let mut git_args = vec!["diff"];
                if let Some(ref path) = args.path {
                    git_args.push(path.as_str());
                }

                let output = self.run_git(&git_args)?;

                Ok(json!({
                    "success": true,
                    "operation": "diff",
                    "path": args.path,
                    "diff": output
                }))
            }
            op => Err(adk_core::AdkError::Tool(format!(
                "Unknown operation: {}. Use: status, add, commit, diff",
                op
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_tool_metadata() {
        let tool = GitTool::new("/tmp");
        assert_eq!(tool.name(), "git");
        assert!(tool.description().contains("status"));
        assert!(tool.description().contains("commit"));
    }
}
