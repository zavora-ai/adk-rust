//! PRD management tool

use crate::models::Prd;
use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Tool for managing PRD tasks
pub struct PrdTool {
    prd: Arc<Mutex<Prd>>,
    prd_path: String,
    progress_path: String,
}

impl PrdTool {
    pub fn new(prd: Arc<Mutex<Prd>>, prd_path: String, progress_path: String) -> Self {
        Self { prd, prd_path, progress_path }
    }
}

#[async_trait]
impl Tool for PrdTool {
    fn name(&self) -> &str {
        "prd_manager"
    }

    fn description(&self) -> &str {
        "Manage PRD: get_next_task, mark_complete, add_learning, get_stats"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get_next_task", "mark_complete", "add_learning", "get_stats"],
                    "description": "The action to perform"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID for mark_complete action"
                },
                "learning": {
                    "type": "string",
                    "description": "Learning text for add_learning action"
                }
            },
            "required": ["action"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, params: Value) -> Result<Value> {
        let action = params["action"]
            .as_str()
            .ok_or_else(|| AdkError::Tool("Missing action".to_string()))?;

        match action {
            "get_next_task" => {
                let prd = self.prd.lock().map_err(|e| AdkError::Tool(e.to_string()))?;
                match prd.get_next_task() {
                    Some(task) => Ok(json!({
                        "task": {
                            "id": task.id,
                            "title": task.title,
                            "description": task.description,
                            "acceptance_criteria": task.acceptance_criteria,
                            "priority": task.priority
                        }
                    })),
                    None => Ok(json!({
                        "task": null,
                        "message": "No tasks remaining"
                    })),
                }
            }
            "mark_complete" => {
                let task_id = params["task_id"]
                    .as_str()
                    .ok_or_else(|| AdkError::Tool("Missing task_id".to_string()))?;
                let mut prd = self.prd.lock().map_err(|e| AdkError::Tool(e.to_string()))?;
                prd.mark_complete(task_id).map_err(|e| AdkError::Tool(e.to_string()))?;
                prd.save(&self.prd_path).map_err(|e| AdkError::Tool(e.to_string()))?;
                Ok(json!({
                    "status": "marked_complete",
                    "task_id": task_id
                }))
            }
            "add_learning" => {
                let learning = params["learning"]
                    .as_str()
                    .ok_or_else(|| AdkError::Tool("Missing learning".to_string()))?;
                let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
                let entry = format!("\n[{}] {}\n", timestamp, learning);

                let result = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.progress_path)
                    .and_then(|mut f| f.write_all(entry.as_bytes()));

                match result {
                    Ok(_) => Ok(json!({"status": "added"})),
                    Err(e) => Ok(json!({"status": "failed", "error": e.to_string()})),
                }
            }
            "get_stats" => {
                let prd = self.prd.lock().map_err(|e| AdkError::Tool(e.to_string()))?;
                let (complete, total) = prd.stats();
                Ok(json!({
                    "complete": complete,
                    "total": total,
                    "is_complete": prd.is_complete()
                }))
            }
            _ => Err(AdkError::Tool(format!("Unknown action: {}", action))),
        }
    }
}
