//! `read_file` — return the (optionally sliced) contents of a file.

use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::DevToolError;
use crate::workspace::Workspace;

/// Reads a UTF-8 text file from the workspace and returns it with line numbers.
///
/// A file matching the workspace's exclusion patterns ([`Workspace::exclude_globs`]),
/// by its requested path or the path a symlink resolves to, is refused with
/// [`DevToolError::Excluded`]. On Unix the file is opened without following a symlink on
/// any component of the target the containment check resolved, and a file with more
/// than one hard link is refused (spec 3.1 §10 D4).
pub struct ReadFileTool {
    workspace: Workspace,
}

impl ReadFileTool {
    /// Create a `read_file` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a text file from the workspace and return its contents with line numbers. \
         Supports optional `offset` (1-based start line) and `limit` (number of lines)."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root." },
                "offset": { "type": "integer", "description": "1-based first line to return (optional)." },
                "limit": { "type": "integer", "description": "Maximum number of lines to return (optional)." }
            },
            "required": ["path"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let path = require_str(&args, "path")?;
        let (resolved, target) = self.workspace.resolve_target(&path)?;
        // A link inside the workspace can lead to an excluded file under another name.
        if self.workspace.is_excluded(&resolved) || self.workspace.is_excluded(&target) {
            return Err(DevToolError::Excluded(self.workspace.display(&resolved)).into());
        }

        let contents = self.workspace.read_contained(&target).await?;
        self.workspace.mark_read(ctx.as_ref(), &resolved);

        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
        let limit = args.get("limit").and_then(Value::as_u64).map(|v| v as usize);

        let mut lines = String::new();
        let mut total = 0usize;
        let mut shown = 0usize;
        for (idx, line) in contents.lines().enumerate() {
            total += 1;
            let line_no = idx + 1;
            if line_no < offset {
                continue;
            }
            if let Some(limit) = limit
                && shown >= limit
            {
                break;
            }
            lines.push_str(&format!("{line_no:>6}\t{line}\n"));
            shown += 1;
        }

        Ok(json!({
            "path": self.workspace.display(&resolved),
            "content": lines,
            "total_lines": total,
            "returned_lines": shown,
        }))
    }
}

pub(crate) fn require_str(args: &Value, key: &str) -> Result<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| {
        adk_core::AdkError::tool(format!("missing required string argument '{key}'"))
    })
}
