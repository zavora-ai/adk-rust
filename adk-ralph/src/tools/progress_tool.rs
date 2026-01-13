//! Progress tracking tool for Ralph.
//!
//! This tool provides read, append, and summary operations for the progress.json file.
//! The progress log is append-only to preserve history and learnings.
//!
//! ## Operations
//!
//! - `read`: Read the entire progress log
//! - `append`: Add a new entry to the progress log (append-only)
//! - `summary`: Get a summary of progress statistics
//!
//! ## Requirements Validated
//!
//! - 7.1: WHEN a task is completed, THE Ralph_Loop_Agent SHALL append to `progress.json`
//! - 7.2: THE Progress_File SHALL include task completed, approach taken, lessons learned
//! - 7.3: THE Progress_File SHALL include any gotchas or patterns discovered

use crate::models::{ProgressEntry, ProgressLog, TestResults};
use crate::telemetry::{start_timing, tool_call_span};
use adk_core::{Result as AdkResult, Tool, ToolContext};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Input schema for the progress tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressToolInput {
    /// Operation to perform: "read", "append", or "summary"
    pub operation: String,
    /// Entry to append (required for "append" operation)
    #[serde(default)]
    pub entry: Option<ProgressEntryInput>,
}

/// Input for a new progress entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEntryInput {
    /// Task ID that was completed
    pub task_id: String,
    /// Task title for reference
    #[serde(default)]
    pub title: Option<String>,
    /// Description of the approach taken
    pub approach: String,
    /// Lessons learned during implementation
    #[serde(default)]
    pub learnings: Vec<String>,
    /// Gotchas or pitfalls discovered
    #[serde(default)]
    pub gotchas: Vec<String>,
    /// Files created during this task
    #[serde(default)]
    pub files_created: Vec<String>,
    /// Files modified during this task
    #[serde(default)]
    pub files_modified: Vec<String>,
    /// Test results if tests were run
    #[serde(default)]
    pub test_results: Option<TestResultsInput>,
    /// Git commit hash if committed
    #[serde(default)]
    pub commit_hash: Option<String>,
}

/// Input for test results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResultsInput {
    pub passed: usize,
    pub failed: usize,
    #[serde(default)]
    pub skipped: usize,
}

/// Tool for managing progress.json file.
///
/// This tool provides append-only access to the progress log,
/// ensuring that history is preserved and learnings accumulate.
pub struct ProgressTool {
    /// Path to the progress.json file
    path: PathBuf,
    /// Project name for creating new logs
    project: String,
    /// Cached progress log (for performance)
    cache: RwLock<Option<ProgressLog>>,
    /// Current iteration counter
    iteration: RwLock<u32>,
}

impl ProgressTool {
    /// Create a new progress tool.
    pub fn new(path: impl Into<PathBuf>, project: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            project: project.into(),
            cache: RwLock::new(None),
            iteration: RwLock::new(0),
        }
    }

    /// Set the current iteration number.
    pub async fn set_iteration(&self, iteration: u32) {
        *self.iteration.write().await = iteration;
    }

    /// Get the current iteration number.
    pub async fn get_iteration(&self) -> u32 {
        *self.iteration.read().await
    }

    /// Increment the iteration counter.
    pub async fn increment_iteration(&self) -> u32 {
        let mut iter = self.iteration.write().await;
        *iter += 1;
        *iter
    }

    /// Load the progress log from disk or create a new one.
    async fn load_or_create(&self) -> Result<ProgressLog, String> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(log) = cache.as_ref() {
                return Ok(log.clone());
            }
        }

        // Load from disk or create new
        let log = ProgressLog::load_or_create(&self.path, &self.project)?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            *cache = Some(log.clone());
        }

        Ok(log)
    }

    /// Save the progress log to disk and update cache.
    async fn save(&self, log: &ProgressLog) -> Result<(), String> {
        log.save(&self.path)?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            *cache = Some(log.clone());
        }

        Ok(())
    }

    /// Read the entire progress log.
    async fn read(&self) -> Result<Value, String> {
        let log = self.load_or_create().await?;

        Ok(json!({
            "success": true,
            "project": log.project,
            "started_at": log.started_at,
            "last_updated": log.last_updated,
            "total_iterations": log.total_iterations,
            "entry_count": log.entries.len(),
            "entries": log.entries,
            "summary": log.summary,
            "context": log.to_context(5)
        }))
    }

    /// Append a new entry to the progress log.
    async fn append(&self, input: ProgressEntryInput) -> Result<Value, String> {
        let mut log = self.load_or_create().await?;
        let iteration = self.get_iteration().await;

        // Get the count before appending for verification
        let count_before = log.entry_count();

        // Create the entry
        let mut entry = ProgressEntry::new(
            &input.task_id,
            input.title.as_deref().unwrap_or(&input.task_id),
            iteration,
            &input.approach,
        );

        // Add learnings
        for learning in &input.learnings {
            entry.add_learning(learning);
        }

        // Add gotchas
        for gotcha in &input.gotchas {
            entry.add_gotcha(gotcha);
        }

        // Add files
        for file in &input.files_created {
            entry.add_file_created(file);
        }
        for file in &input.files_modified {
            entry.add_file_modified(file);
        }

        // Add test results
        if let Some(results) = &input.test_results {
            entry = entry.with_test_results(TestResults::new(
                results.passed,
                results.failed,
                results.skipped,
            ));
        }

        // Add commit hash
        if let Some(hash) = &input.commit_hash {
            entry = entry.with_commit(hash);
        }

        // Append to log (append-only operation)
        log.append(entry.clone());

        // Verify append-only behavior
        let count_after = log.entry_count();
        if count_after != count_before + 1 {
            return Err(format!(
                "Append-only violation: expected {} entries, got {}",
                count_before + 1,
                count_after
            ));
        }

        // Save to disk
        self.save(&log).await?;

        Ok(json!({
            "success": true,
            "operation": "append",
            "task_id": input.task_id,
            "entry_count": count_after,
            "entry": entry
        }))
    }

    /// Get a summary of progress statistics.
    async fn summary(&self, tasks_remaining: usize) -> Result<Value, String> {
        let mut log = self.load_or_create().await?;
        log.update_summary(tasks_remaining);

        // Save updated summary
        self.save(&log).await?;

        let all_learnings = log.get_all_learnings();
        let all_gotchas = log.get_all_gotchas();

        Ok(json!({
            "success": true,
            "project": log.project,
            "total_iterations": log.total_iterations,
            "summary": {
                "tasks_completed": log.summary.tasks_completed,
                "tasks_remaining": log.summary.tasks_remaining,
                "total_commits": log.summary.total_commits,
                "total_files_created": log.summary.total_files_created,
                "total_files_modified": log.summary.total_files_modified,
                "total_tests_passed": log.summary.total_tests_passed,
                "total_tests_failed": log.summary.total_tests_failed
            },
            "all_learnings": all_learnings,
            "all_gotchas": all_gotchas
        }))
    }

    /// Clear the cache (useful for testing).
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        *cache = None;
    }
}

#[async_trait]
impl Tool for ProgressTool {
    fn name(&self) -> &str {
        "progress"
    }

    fn description(&self) -> &str {
        "Manage the progress.json file for tracking learnings and completed work. \
         Operations: 'read' (get full log), 'append' (add entry - append-only), \
         'summary' (get statistics). The progress log is append-only to preserve history."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["read", "append", "summary"],
                    "description": "Operation to perform"
                },
                "entry": {
                    "type": "object",
                    "description": "Entry to append (required for 'append' operation)",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "Task ID that was completed"
                        },
                        "title": {
                            "type": "string",
                            "description": "Task title for reference"
                        },
                        "approach": {
                            "type": "string",
                            "description": "Description of the approach taken"
                        },
                        "learnings": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Lessons learned during implementation"
                        },
                        "gotchas": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Gotchas or pitfalls discovered"
                        },
                        "files_created": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Files created during this task"
                        },
                        "files_modified": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Files modified during this task"
                        },
                        "test_results": {
                            "type": "object",
                            "properties": {
                                "passed": { "type": "integer" },
                                "failed": { "type": "integer" },
                                "skipped": { "type": "integer" }
                            }
                        },
                        "commit_hash": {
                            "type": "string",
                            "description": "Git commit hash if committed"
                        }
                    },
                    "required": ["task_id", "approach"]
                },
                "tasks_remaining": {
                    "type": "integer",
                    "description": "Number of tasks remaining (for 'summary' operation)"
                }
            },
            "required": ["operation"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        let operation = args["operation"]
            .as_str()
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'operation' field".to_string()))?;

        // Create span for tool call
        let span = tool_call_span("progress", operation);
        let _guard = span.enter();
        
        // Start timing
        let _timing = start_timing(format!("progress_tool_{}", operation));
        
        info!(operation = %operation, "Executing progress tool");

        match operation {
            "read" => self.read().await.map_err(adk_core::AdkError::Tool),
            "append" => {
                let entry: ProgressEntryInput = serde_json::from_value(args["entry"].clone())
                    .map_err(|e| adk_core::AdkError::Tool(format!("Invalid entry: {}", e)))?;
                self.append(entry).await.map_err(adk_core::AdkError::Tool)
            }
            "summary" => {
                let tasks_remaining = args["tasks_remaining"].as_u64().unwrap_or(0) as usize;
                self.summary(tasks_remaining).await.map_err(adk_core::AdkError::Tool)
            }
            _ => Err(adk_core::AdkError::Tool(format!(
                "Unknown operation '{}'. Valid operations: read, append, summary",
                operation
            ))),
        }
    }
}

impl std::fmt::Debug for ProgressTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressTool")
            .field("path", &self.path)
            .field("project", &self.project)
            .finish()
    }
}
