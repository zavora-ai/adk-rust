//! `write_file` — create or overwrite a file in the workspace.

use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::DevToolError;
use crate::read_ledger::sha256_hex;
use crate::tools::read::require_str;
use crate::workspace::{Precondition, Workspace};

/// Creates or overwrites a file (creating parent directories as needed).
///
/// With `expect_sha256` the write happens only when the file's current sha256 is that
/// digest, or, given `absent`, only when no file exists yet; otherwise it is answered
/// `devtools.stale` and nothing is written (spec 3.1 §7.26, I66). The digest of what was
/// written becomes this session and agent's read record, so a following `edit_file`
/// needs no read.
///
/// On Unix the parent directory is opened by descriptor and the file relative to it,
/// without following a symlink on any component of the target the containment check
/// resolved, and the precondition is read through the same descriptor (spec 3.1 §10 D4).
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
/// use adk_devtools::{Workspace, WriteFileTool};
///
/// let tool = WriteFileTool::new(Workspace::new("./my-repo"));
/// assert!(tool.parameters_schema().unwrap()["properties"]["expect_sha256"].is_object());
/// ```
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

    /// Writes the workspace on disk.
    fn egress(&self, _args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        adk_core::egress::ToolEgress::None
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
                "content": { "type": "string", "description": "The full file content to write." },
                "expect_sha256": { "type": "string", "description": "Optional: the sha256 the file must have, or \"absent\" when it must not exist yet; the write is refused otherwise." }
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
        let precondition = match args.get("expect_sha256").and_then(Value::as_str) {
            Some(expect) => Precondition::from_expect(expect)?,
            None => Precondition::Any,
        };
        let (resolved, target) = self.workspace.resolve_target(&path)?;
        self.workspace.write_contained(&target, content.as_bytes(), precondition).await?;
        // A freshly-written file is considered "read" (its content is known).
        self.workspace
            .record_digest(ctx.as_ref(), &resolved, &sha256_hex(content.as_bytes()))
            .await;

        Ok(json!({
            "path": self.workspace.display(&resolved),
            "bytes_written": content.len(),
            "message": format!("Wrote {} bytes to {}", content.len(), self.workspace.display(&resolved)),
        }))
    }
}
