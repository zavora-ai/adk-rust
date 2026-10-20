//! `write_file` — create or overwrite a file in the workspace.

use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::DevToolError;
use crate::tools::read::require_str;
use crate::workspace::Workspace;

/// Creates or overwrites a file (creating parent directories as needed).
///
/// On Unix the parent directory is opened by descriptor and the file relative to it,
/// without following a symlink on any component of the target the containment check
/// resolved (spec 3.1 §10 D4).
pub struct WriteFileTool {
    workspace: Workspace,
}

impl WriteFileTool {
    /// Create a `write_file` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create a new file or overwrite an existing one with the given content. \
         Parent directories are created automatically."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root." },
                "content": { "type": "string", "description": "The full file content to write." }
            },
            "required": ["path", "content"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.workspace.is_writable() {
            return Err(DevToolError::ReadOnly("write_file").into());
        }
        let path = require_str(&args, "path")?;
        let content = require_str(&args, "content")?;
        let (resolved, target) = self.workspace.resolve_target(&path)?;
        self.workspace.write_contained(&target, content.as_bytes()).await?;
        // A freshly-written file is considered "read" (its content is known).
        self.workspace.mark_read(ctx.as_ref(), &resolved);

        Ok(json!({
            "path": self.workspace.display(&resolved),
            "bytes_written": content.len(),
            "message": format!("Wrote {} bytes to {}", content.len(), self.workspace.display(&resolved)),
        }))
    }
}
