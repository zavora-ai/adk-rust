//! [`DevToolset`] — the developer tools as a single [`Toolset`].

use std::sync::Arc;

use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;

use crate::tools::{
    ApplyPatchTool, BashTool, EditFileTool, GlobTool, GrepTool, ReadFileTool, WriteFileTool,
};
use crate::workspace::Workspace;

/// Bundles the developer tools (`read_file`, `write_file`, `edit_file`, `apply_patch`,
/// `glob`, `grep`, `bash`) so they can be attached to an agent in one call.
///
/// Mutating tools (`write_file`, `edit_file`, `apply_patch`) are omitted when the workspace is
/// read-only, and `bash` is omitted when bash is disabled — so the toolset the
/// model sees always matches what the workspace actually permits.
pub struct DevToolset {
    workspace: Workspace,
    include_bash: bool,
}

impl DevToolset {
    /// Create a toolset over `workspace` with all permitted tools enabled.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace, include_bash: true }
    }

    /// Enable or disable exposing the `bash` tool (independent of the workspace
    /// bash flag; both must allow it).
    pub fn with_bash(mut self, include: bool) -> Self {
        self.include_bash = include;
        self
    }
}

#[async_trait]
impl Toolset for DevToolset {
    fn name(&self) -> &str {
        "devtools"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let ws = self.workspace.clone();
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(ReadFileTool::new(ws.clone())),
            Arc::new(GlobTool::new(ws.clone())),
            Arc::new(GrepTool::new(ws.clone())),
        ];
        if ws.is_writable() {
            tools.push(Arc::new(WriteFileTool::new(ws.clone())));
            tools.push(Arc::new(EditFileTool::new(ws.clone())));
            tools.push(Arc::new(ApplyPatchTool::new(ws.clone())));
        }
        if self.include_bash && ws.bash_allowed() {
            tools.push(Arc::new(BashTool::new(ws.clone())));
        }
        Ok(tools)
    }
}
