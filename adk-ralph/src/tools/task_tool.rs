//! Task management tool for Ralph.
//!
//! This tool provides list, get_next, update_status, and complete operations
//! for the tasks.json file. It implements priority-based selection with
//! dependency checking.
//!
//! ## Operations
//!
//! - `list`: List all tasks with their status
//! - `get_next`: Get the next task to work on (priority-based with dependency checking)
//! - `update_status`: Update a task's status
//! - `complete`: Mark a task as completed
//!
//! ## Requirements Validated
//!
//! - 4.1: WHEN starting an iteration, THE Ralph_Loop_Agent SHALL read `tasks.json`
//! - 4.2: THE Ralph_Loop_Agent SHALL select the pending task with highest priority
//! - 4.3: THE Ralph_Loop_Agent SHALL consider task dependencies when selecting
//! - 4.4: IF a task is blocked by incomplete dependencies, THEN THE Ralph_Loop_Agent SHALL skip it
//! - 4.5: THE Ralph_Loop_Agent SHALL update task status to in_progress when starting

use crate::models::{Task, TaskList, TaskStatus};
use crate::telemetry::{start_timing, tool_call_span};
use adk_core::{Result as AdkResult, Tool, ToolContext};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Input schema for the task tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskToolInput {
    /// Operation to perform: "list", "get_next", "update_status", "complete"
    pub operation: String,
    /// Task ID (required for update_status and complete)
    #[serde(default)]
    pub task_id: Option<String>,
    /// New status (required for update_status)
    #[serde(default)]
    pub status: Option<String>,
    /// Commit hash (optional for complete)
    #[serde(default)]
    pub commit_hash: Option<String>,
}

/// Tool for managing tasks.json file.
///
/// This tool provides priority-based task selection with dependency checking,
/// ensuring that tasks are worked on in the correct order.
pub struct TaskTool {
    /// Path to the tasks.json file
    path: PathBuf,
    /// Cached task list (for performance)
    cache: RwLock<Option<TaskList>>,
}

impl TaskTool {
    /// Create a new task tool.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            cache: RwLock::new(None),
        }
    }

    /// Load the task list from disk.
    async fn load(&self) -> Result<TaskList, String> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(list) = cache.as_ref() {
                return Ok(list.clone());
            }
        }

        // Load from disk
        let list = TaskList::load(&self.path)?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            *cache = Some(list.clone());
        }

        Ok(list)
    }

    /// Save the task list to disk and update cache.
    async fn save(&self, list: &TaskList) -> Result<(), String> {
        list.save(&self.path)?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            *cache = Some(list.clone());
        }

        Ok(())
    }

    /// List all tasks with their status.
    async fn list(&self) -> Result<Value, String> {
        let list = self.load().await?;
        let stats = list.get_stats();

        // Group tasks by status
        let all_tasks = list.get_all_tasks();
        let pending: Vec<_> = all_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .collect();
        let in_progress: Vec<_> = all_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::InProgress)
            .collect();
        let completed: Vec<_> = all_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .collect();
        let blocked: Vec<_> = all_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Blocked)
            .collect();

        Ok(json!({
            "success": true,
            "project": list.project,
            "language": list.language,
            "stats": {
                "total": stats.total,
                "completed": stats.completed,
                "in_progress": stats.in_progress,
                "pending": stats.pending,
                "blocked": stats.blocked,
                "completion_rate": format!("{:.1}%", stats.completion_rate)
            },
            "tasks": {
                "pending": pending.iter().map(|t| task_summary(t)).collect::<Vec<_>>(),
                "in_progress": in_progress.iter().map(|t| task_summary(t)).collect::<Vec<_>>(),
                "completed": completed.iter().map(|t| task_summary(t)).collect::<Vec<_>>(),
                "blocked": blocked.iter().map(|t| task_summary(t)).collect::<Vec<_>>()
            },
            "is_complete": list.is_complete()
        }))
    }

    /// Get the next task to work on based on priority and dependencies.
    async fn get_next(&self) -> Result<Value, String> {
        let mut list = self.load().await?;

        // Find the next task using priority-based selection with dependency checking
        let next_task = list.get_next_task();

        match next_task {
            Some(task) => {
                let task_id = task.id.clone();
                let task_detail = task_detail(task);

                // Update status to in_progress
                list.update_task_status(&task_id, TaskStatus::InProgress)?;
                self.save(&list).await?;

                Ok(json!({
                    "success": true,
                    "has_next": true,
                    "task": task_detail,
                    "status_updated": true,
                    "message": format!("Task {} is now in progress", task_id)
                }))
            }
            None => {
                // Check if all tasks are complete or if there are blocked tasks
                let stats = list.get_stats();
                let all_complete = list.is_complete();

                if all_complete {
                    Ok(json!({
                        "success": true,
                        "has_next": false,
                        "all_complete": true,
                        "message": "All tasks have been completed!"
                    }))
                } else if stats.blocked > 0 {
                    Ok(json!({
                        "success": true,
                        "has_next": false,
                        "all_complete": false,
                        "blocked_count": stats.blocked,
                        "message": format!("{} tasks are blocked. Check dependencies.", stats.blocked)
                    }))
                } else {
                    // Tasks exist but none are selectable (dependency issues)
                    Ok(json!({
                        "success": true,
                        "has_next": false,
                        "all_complete": false,
                        "pending_count": stats.pending,
                        "message": "No tasks available. Pending tasks may have unmet dependencies."
                    }))
                }
            }
        }
    }

    /// Update a task's status.
    async fn update_status(&self, task_id: &str, status_str: &str) -> Result<Value, String> {
        let mut list = self.load().await?;

        let status = parse_status(status_str)?;
        list.update_task_status(task_id, status)?;
        self.save(&list).await?;

        Ok(json!({
            "success": true,
            "task_id": task_id,
            "new_status": status.to_string(),
            "message": format!("Task {} status updated to {}", task_id, status)
        }))
    }

    /// Mark a task as completed.
    async fn complete(&self, task_id: &str, commit_hash: Option<String>) -> Result<Value, String> {
        let mut list = self.load().await?;

        list.complete_task(task_id, commit_hash.clone())?;
        self.save(&list).await?;

        let stats = list.get_stats();
        let all_complete = list.is_complete();

        Ok(json!({
            "success": true,
            "task_id": task_id,
            "commit_hash": commit_hash,
            "stats": {
                "completed": stats.completed,
                "remaining": stats.pending + stats.in_progress,
                "completion_rate": format!("{:.1}%", stats.completion_rate)
            },
            "all_complete": all_complete,
            "message": if all_complete {
                "All tasks completed! Project is done.".to_string()
            } else {
                format!("Task {} completed. {} tasks remaining.", task_id, stats.pending + stats.in_progress)
            }
        }))
    }

    /// Get a specific task by ID.
    async fn get_task(&self, task_id: &str) -> Result<Value, String> {
        let list = self.load().await?;

        match list.get_task(task_id) {
            Some(task) => Ok(json!({
                "success": true,
                "task": task_detail(task)
            })),
            None => Err(format!("Task not found: {}", task_id)),
        }
    }

    /// Clear the cache (useful for testing).
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        *cache = None;
    }
}

/// Parse a status string into TaskStatus.
fn parse_status(s: &str) -> Result<TaskStatus, String> {
    match s.to_lowercase().as_str() {
        "pending" => Ok(TaskStatus::Pending),
        "in_progress" | "inprogress" => Ok(TaskStatus::InProgress),
        "completed" | "complete" | "done" => Ok(TaskStatus::Completed),
        "blocked" => Ok(TaskStatus::Blocked),
        "skipped" | "skip" => Ok(TaskStatus::Skipped),
        _ => Err(format!(
            "Invalid status '{}'. Valid: pending, in_progress, completed, blocked, skipped",
            s
        )),
    }
}

/// Create a summary of a task for listing.
fn task_summary(task: &Task) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "priority": task.priority,
        "status": task.status.to_string(),
        "dependencies": task.dependencies,
        "complexity": task.estimated_complexity.to_string()
    })
}

/// Create a detailed view of a task.
fn task_detail(task: &Task) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "description": task.description,
        "priority": task.priority,
        "status": task.status.to_string(),
        "dependencies": task.dependencies,
        "user_story_id": task.user_story_id,
        "complexity": task.estimated_complexity.to_string(),
        "files_created": task.files_created,
        "files_modified": task.files_modified,
        "commit_hash": task.commit_hash,
        "attempts": task.attempts,
        "notes": task.notes,
        "context": task.to_context()
    })
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "tasks"
    }

    fn description(&self) -> &str {
        "Manage the tasks.json file for tracking implementation tasks. \
         Operations: 'list' (show all tasks), 'get_next' (get highest priority pending task \
         with satisfied dependencies and mark as in_progress), 'update_status' (change task status), \
         'complete' (mark task as done with optional commit hash), 'get' (get specific task by ID). \
         Tasks are selected by priority (1=highest) with dependency checking."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["list", "get_next", "update_status", "complete", "get"],
                    "description": "Operation to perform"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID (required for update_status, complete, get)"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "blocked", "skipped"],
                    "description": "New status (required for update_status)"
                },
                "commit_hash": {
                    "type": "string",
                    "description": "Git commit hash (optional for complete)"
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
        let span = tool_call_span("tasks", operation);
        let _guard = span.enter();
        
        // Start timing
        let _timing = start_timing(format!("task_tool_{}", operation));
        
        info!(operation = %operation, "Executing task tool");

        match operation {
            "list" => self.list().await.map_err(adk_core::AdkError::Tool),
            "get_next" => self.get_next().await.map_err(adk_core::AdkError::Tool),
            "get" => {
                let task_id = args["task_id"]
                    .as_str()
                    .ok_or_else(|| adk_core::AdkError::Tool("Missing 'task_id' for get operation".to_string()))?;
                self.get_task(task_id).await.map_err(adk_core::AdkError::Tool)
            }
            "update_status" => {
                let task_id = args["task_id"]
                    .as_str()
                    .ok_or_else(|| adk_core::AdkError::Tool("Missing 'task_id' for update_status".to_string()))?;
                let status = args["status"]
                    .as_str()
                    .ok_or_else(|| adk_core::AdkError::Tool("Missing 'status' for update_status".to_string()))?;
                self.update_status(task_id, status).await.map_err(adk_core::AdkError::Tool)
            }
            "complete" => {
                let task_id = args["task_id"]
                    .as_str()
                    .ok_or_else(|| adk_core::AdkError::Tool("Missing 'task_id' for complete".to_string()))?;
                let commit_hash = args["commit_hash"].as_str().map(|s| s.to_string());
                self.complete(task_id, commit_hash).await.map_err(adk_core::AdkError::Tool)
            }
            _ => Err(adk_core::AdkError::Tool(format!(
                "Unknown operation '{}'. Valid operations: list, get_next, update_status, complete, get",
                operation
            ))),
        }
    }
}

impl std::fmt::Debug for TaskTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskTool")
            .field("path", &self.path)
            .finish()
    }
}
