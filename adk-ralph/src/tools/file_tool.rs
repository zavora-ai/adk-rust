//! File tools for reading and writing project files.
//!
//! These tools allow agents to interact with the filesystem
//! in a controlled way.
//!
//! Provides both:
//! - Unified `FileTool` with operation-based interface
//! - Individual tools (`ReadFileTool`, `WriteFileTool`, `ListFilesTool`)

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

/// Unified file tool with operation-based interface.
///
/// Supports operations: read, write, list, delete
pub struct FileTool {
    project_path: PathBuf,
}

impl FileTool {
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self {
            project_path: project_path.into(),
        }
    }

    fn validate_path(&self, rel_path: &str) -> Result<PathBuf> {
        let full_path = self.project_path.join(rel_path);

        // For new files, check parent exists or can be created
        if !full_path.exists() {
            if let Some(parent) = full_path.parent() {
                if !parent.exists() {
                    // Will create parent dirs on write
                    return Ok(full_path);
                }
            }
            return Ok(full_path);
        }

        // For existing files, verify within project
        let canonical = full_path.canonicalize().map_err(|e| {
            adk_core::AdkError::Tool(format!("Invalid path: {} - {}", rel_path, e))
        })?;

        let project_canonical = self.project_path.canonicalize().map_err(|e| {
            adk_core::AdkError::Tool(format!("Invalid project path: {}", e))
        })?;

        if !canonical.starts_with(&project_canonical) {
            return Err(adk_core::AdkError::Tool(
                "Access denied: path outside project directory".to_string(),
            ));
        }

        Ok(canonical)
    }
}

impl std::fmt::Debug for FileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileTool")
            .field("project_path", &self.project_path)
            .finish()
    }
}

#[async_trait]
impl Tool for FileTool {
    fn name(&self) -> &str {
        "file"
    }

    fn description(&self) -> &str {
        "File operations: read, write, list, delete files in the project directory."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["read", "write", "list", "delete"],
                    "description": "The operation to perform"
                },
                "path": {
                    "type": "string",
                    "description": "Relative path to the file or directory"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write (required for 'write' operation)"
                }
            },
            "required": ["operation", "path"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            operation: String,
            path: String,
            content: Option<String>,
        }

        let args: Args = serde_json::from_value(args)
            .map_err(|e| adk_core::AdkError::Tool(format!("Invalid arguments: {}", e)))?;

        match args.operation.as_str() {
            "read" => {
                let full_path = self.validate_path(&args.path)?;
                let content = std::fs::read_to_string(&full_path).map_err(|e| {
                    adk_core::AdkError::Tool(format!("Failed to read file: {}", e))
                })?;
                Ok(json!({
                    "success": true,
                    "operation": "read",
                    "path": args.path,
                    "content": content
                }))
            }
            "write" => {
                let content = args.content.ok_or_else(|| {
                    adk_core::AdkError::Tool("'content' is required for write operation".to_string())
                })?;

                let full_path = self.project_path.join(&args.path);

                // Create parent directories if needed
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        adk_core::AdkError::Tool(format!("Failed to create directories: {}", e))
                    })?;
                }

                std::fs::write(&full_path, &content).map_err(|e| {
                    adk_core::AdkError::Tool(format!("Failed to write file: {}", e))
                })?;

                Ok(json!({
                    "success": true,
                    "operation": "write",
                    "path": args.path,
                    "bytes_written": content.len()
                }))
            }
            "list" => {
                let full_path = self.project_path.join(&args.path);
                let entries: Vec<Value> = std::fs::read_dir(&full_path)
                    .map_err(|e| {
                        adk_core::AdkError::Tool(format!("Failed to read directory: {}", e))
                    })?
                    .filter_map(|entry| entry.ok())
                    .map(|entry| {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        json!({
                            "name": name,
                            "is_directory": is_dir
                        })
                    })
                    .collect();

                Ok(json!({
                    "success": true,
                    "operation": "list",
                    "path": args.path,
                    "entries": entries
                }))
            }
            "delete" => {
                let full_path = self.validate_path(&args.path)?;

                if full_path.is_dir() {
                    std::fs::remove_dir_all(&full_path).map_err(|e| {
                        adk_core::AdkError::Tool(format!("Failed to delete directory: {}", e))
                    })?;
                } else {
                    std::fs::remove_file(&full_path).map_err(|e| {
                        adk_core::AdkError::Tool(format!("Failed to delete file: {}", e))
                    })?;
                }

                Ok(json!({
                    "success": true,
                    "operation": "delete",
                    "path": args.path
                }))
            }
            op => Err(adk_core::AdkError::Tool(format!(
                "Unknown operation: {}. Use: read, write, list, delete",
                op
            ))),
        }
    }
}

/// Tool for reading files from the project directory.
pub struct ReadFileTool {
    project_path: PathBuf,
}

impl ReadFileTool {
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self { project_path: project_path.into() }
    }
}

impl std::fmt::Debug for ReadFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadFileTool")
            .field("project_path", &self.project_path)
            .finish()
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file from the project directory."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file (e.g., 'prd.md', 'src/main.rs')"
                }
            },
            "required": ["path"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
        }

        let args: Args = serde_json::from_value(args)
            .map_err(|e| adk_core::AdkError::Tool(format!("Invalid arguments: {}", e)))?;

        let full_path = self.project_path.join(&args.path);
        
        // Security: ensure path is within project directory
        let canonical = full_path.canonicalize().map_err(|e| {
            adk_core::AdkError::Tool(format!("File not found: {} - {}", args.path, e))
        })?;
        
        let project_canonical = self.project_path.canonicalize().map_err(|e| {
            adk_core::AdkError::Tool(format!("Invalid project path: {}", e))
        })?;
        
        if !canonical.starts_with(&project_canonical) {
            return Err(adk_core::AdkError::Tool(
                "Access denied: path outside project directory".to_string()
            ));
        }

        let content = std::fs::read_to_string(&canonical).map_err(|e| {
            adk_core::AdkError::Tool(format!("Failed to read file: {}", e))
        })?;

        Ok(json!({
            "success": true,
            "path": args.path,
            "content": content
        }))
    }
}

/// Tool for writing files to the project directory.
pub struct WriteFileTool {
    project_path: PathBuf,
}

impl WriteFileTool {
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self { project_path: project_path.into() }
    }
}

impl std::fmt::Debug for WriteFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteFileTool")
            .field("project_path", &self.project_path)
            .finish()
    }
}


#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file in the project directory. Creates parent directories if needed."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file (e.g., 'design.md', 'src/main.rs')"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            content: String,
        }

        let args: Args = serde_json::from_value(args)
            .map_err(|e| adk_core::AdkError::Tool(format!("Invalid arguments: {}", e)))?;

        let full_path = self.project_path.join(&args.path);
        
        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                adk_core::AdkError::Tool(format!("Failed to create directories: {}", e))
            })?;
        }

        std::fs::write(&full_path, &args.content).map_err(|e| {
            adk_core::AdkError::Tool(format!("Failed to write file: {}", e))
        })?;

        Ok(json!({
            "success": true,
            "path": args.path,
            "bytes_written": args.content.len()
        }))
    }
}

/// Tool for listing files in a directory.
pub struct ListFilesTool {
    project_path: PathBuf,
}

impl ListFilesTool {
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self { project_path: project_path.into() }
    }
}

impl std::fmt::Debug for ListFilesTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListFilesTool")
            .field("project_path", &self.project_path)
            .finish()
    }
}

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }

    fn description(&self) -> &str {
        "List files and directories in a path within the project."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to list (e.g., '.', 'src'). Defaults to project root."
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize, Default)]
        struct Args {
            #[serde(default)]
            path: Option<String>,
        }

        let args: Args = serde_json::from_value(args).unwrap_or_default();
        let rel_path = args.path.unwrap_or_else(|| ".".to_string());
        let full_path = self.project_path.join(&rel_path);

        let entries: Vec<Value> = std::fs::read_dir(&full_path)
            .map_err(|e| adk_core::AdkError::Tool(format!("Failed to read directory: {}", e)))?
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                json!({
                    "name": name,
                    "is_directory": is_dir
                })
            })
            .collect();

        Ok(json!({
            "path": rel_path,
            "entries": entries
        }))
    }
}
