//! `edit_file` — exact-string replacement within a file.

use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::DevToolError;
use crate::tools::read::require_str;
use crate::workspace::Workspace;

/// Replaces an exact substring in a file. Requires the file to have been read
/// this session, and (unless `replace_all`) the target string to be unique.
pub struct EditFileTool {
    workspace: Workspace,
}

impl EditFileTool {
    /// Create an `edit_file` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. The file must have been read with read_file \
         first. By default the target string must occur exactly once; set replace_all=true \
         to replace every occurrence."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root." },
                "old_string": { "type": "string", "description": "The exact text to replace." },
                "new_string": { "type": "string", "description": "The replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)." }
            },
            "required": ["path", "old_string", "new_string"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.workspace.is_writable() {
            return Err(DevToolError::ReadOnly("edit_file").into());
        }
        let path = require_str(&args, "path")?;
        let old_string = require_str(&args, "old_string")?;
        let new_string = require_str(&args, "new_string")?;
        let replace_all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);

        let resolved = self.workspace.resolve(&path)?;
        if !self.workspace.was_read(&resolved) {
            return Err(DevToolError::NotRead(self.workspace.display(&resolved)).into());
        }

        let original = tokio::fs::read_to_string(&resolved).await.map_err(DevToolError::from)?;
        let count = original.matches(&old_string).count();
        let display = self.workspace.display(&resolved);
        if count == 0 {
            return Err(DevToolError::NoMatch(display).into());
        }
        if count > 1 && !replace_all {
            return Err(DevToolError::Ambiguous { path: display, count }.into());
        }

        let updated = if replace_all {
            original.replace(&old_string, &new_string)
        } else {
            original.replacen(&old_string, &new_string, 1)
        };
        self.workspace.write_contained(&resolved, updated.as_bytes()).await?;

        Ok(json!({
            "path": self.workspace.display(&resolved),
            "replacements": if replace_all { count } else { 1 },
            "message": format!(
                "Replaced {} occurrence(s) in {}",
                if replace_all { count } else { 1 },
                self.workspace.display(&resolved)
            ),
        }))
    }
}
