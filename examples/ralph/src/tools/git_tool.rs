//! Git operations tool

use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::process::Command;
use std::sync::Arc;

/// Tool for Git operations
pub struct GitTool {
    repo_path: String,
}

impl GitTool {
    pub fn new(repo_path: String) -> Self {
        Self { repo_path }
    }

    fn run_git(&self, args: &[&str]) -> std::result::Result<String, String> {
        let output = Command::new("git")
            .current_dir(&self.repo_path)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Git operations: add, commit, status, diff, checkout_branch"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["add", "commit", "status", "diff", "checkout_branch"],
                    "description": "Git command to execute"
                },
                "message": {
                    "type": "string",
                    "description": "Commit message for commit command"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files to add for add command"
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name for checkout_branch command"
                }
            },
            "required": ["command"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, params: Value) -> Result<Value> {
        let cmd = params["command"]
            .as_str()
            .ok_or_else(|| AdkError::Tool("Missing command".to_string()))?;

        match cmd {
            "add" => {
                let files: Vec<String> = params["files"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_else(|| vec![".".to_string()]);

                for file in &files {
                    self.run_git(&["add", file])
                        .map_err(|e| AdkError::Tool(format!("git add failed: {}", e)))?;
                }
                Ok(json!({"status": "added", "files": files}))
            }
            "commit" => {
                let message = params["message"]
                    .as_str()
                    .ok_or_else(|| AdkError::Tool("Missing message".to_string()))?;
                self.run_git(&["commit", "-m", message])
                    .map_err(|e| AdkError::Tool(format!("git commit failed: {}", e)))?;
                Ok(json!({
                    "status": "committed",
                    "message": message
                }))
            }
            "status" => {
                let status = self
                    .run_git(&["status", "--short"])
                    .map_err(|e| AdkError::Tool(format!("git status failed: {}", e)))?;
                Ok(json!({ "status": status }))
            }
            "diff" => {
                let diff = self
                    .run_git(&["diff", "--cached"])
                    .map_err(|e| AdkError::Tool(format!("git diff failed: {}", e)))?;
                Ok(json!({ "diff": diff }))
            }
            "checkout_branch" => {
                let branch = params["branch"]
                    .as_str()
                    .ok_or_else(|| AdkError::Tool("Missing branch".to_string()))?;
                // Try existing, or create new
                let result = self
                    .run_git(&["checkout", branch])
                    .or_else(|_| self.run_git(&["checkout", "-b", branch]));
                result.map_err(|e| AdkError::Tool(format!("git checkout failed: {}", e)))?;
                Ok(json!({
                    "status": "checked_out",
                    "branch": branch
                }))
            }
            _ => Err(AdkError::Tool(format!("Unknown command: {}", cmd))),
        }
    }
}
