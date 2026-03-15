//! File operations tool

use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

/// Tool for file operations
pub struct FileTool {
    base_path: String,
}

impl FileTool {
    pub fn new(base_path: String) -> Self {
        Self { base_path }
    }
}

#[async_trait]
impl Tool for FileTool {
    fn name(&self) -> &str {
        "file"
    }

    fn description(&self) -> &str {
        "File operations: read, write, append, list"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["read", "write", "append", "list"],
                    "description": "File operation to perform"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory path (relative to project root)"
                },
                "content": {
                    "type": "string",
                    "description": "Content for write/append operations"
                }
            },
            "required": ["operation", "path"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, params: Value) -> Result<Value> {
        let operation = params["operation"]
            .as_str()
            .ok_or_else(|| AdkError::Tool("Missing operation".to_string()))?;
        let path_str =
            params["path"].as_str().ok_or_else(|| AdkError::Tool("Missing path".to_string()))?;

        let full_path = Path::new(&self.base_path).join(path_str);

        match operation {
            "read" => {
                let content = fs::read_to_string(&full_path)?;
                Ok(json!({
                    "path": path_str,
                    "content": content
                }))
            }
            "write" => {
                let content = params["content"]
                    .as_str()
                    .ok_or_else(|| AdkError::Tool("Missing content".to_string()))?;

                // Create parent directories if they don't exist
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                fs::write(&full_path, content)?;
                Ok(json!({
                    "status": "written",
                    "path": path_str
                }))
            }
            "append" => {
                let content = params["content"]
                    .as_str()
                    .ok_or_else(|| AdkError::Tool("Missing content".to_string()))?;

                let mut file = OpenOptions::new().create(true).append(true).open(&full_path)?;

                file.write_all(content.as_bytes())?;

                Ok(json!({
                    "status": "appended",
                    "path": path_str
                }))
            }
            "list" => {
                let entries: Vec<String> = fs::read_dir(&full_path)?
                    .filter_map(|entry| {
                        entry.ok().and_then(|e| {
                            e.file_name().to_str().map(|s| {
                                if e.path().is_dir() { format!("{}/", s) } else { s.to_string() }
                            })
                        })
                    })
                    .collect();

                Ok(json!({
                    "path": path_str,
                    "entries": entries
                }))
            }
            _ => Err(AdkError::Tool(format!("Unknown operation: {}", operation))),
        }
    }
}
