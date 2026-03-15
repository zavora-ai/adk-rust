//! Quality check tool

use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::process::Command;
use std::sync::Arc;

/// Tool for running quality checks (cargo check, test, clippy, fmt)
pub struct TestTool {
    project_path: String,
}

impl TestTool {
    pub fn new(project_path: String) -> Self {
        Self { project_path }
    }

    fn run_cargo(&self, args: &[&str]) -> (bool, String, String) {
        match Command::new("cargo").current_dir(&self.project_path).args(args).output() {
            Ok(output) => (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            ),
            Err(e) => (false, String::new(), e.to_string()),
        }
    }
}

#[async_trait]
impl Tool for TestTool {
    fn name(&self) -> &str {
        "quality_check"
    }

    fn description(&self) -> &str {
        "Run quality checks: check, test, clippy, fmt, all"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "check_type": {
                    "type": "string",
                    "enum": ["check", "test", "clippy", "fmt", "all"],
                    "description": "Type of quality check to run"
                }
            },
            "required": ["check_type"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, params: Value) -> Result<Value> {
        let check_type = params["check_type"]
            .as_str()
            .ok_or_else(|| AdkError::Tool("Missing check_type".to_string()))?;

        let checks =
            if check_type == "all" { vec!["check", "test", "clippy"] } else { vec![check_type] };

        let mut results = vec![];

        for check in checks {
            let (passed, stdout, stderr) = match check {
                "check" => self.run_cargo(&["check"]),
                "test" => self.run_cargo(&["test", "--", "--test-threads=1"]),
                "clippy" => self.run_cargo(&["clippy", "--", "-D", "warnings"]),
                "fmt" => self.run_cargo(&["fmt", "--", "--check"]),
                _ => continue,
            };

            results.push(json!({
                "check": check,
                "passed": passed,
                "stdout": stdout,
                "stderr": stderr
            }));
        }

        let all_passed = results.iter().all(|r| r["passed"].as_bool().unwrap_or(false));

        Ok(json!({
            "all_passed": all_passed,
            "results": results
        }))
    }
}
