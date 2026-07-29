//! Tool binding logic per sandbox capability.
//!
//! This module provides the [`bind_tools`] function that creates the appropriate
//! set of tools based on the enabled [`Capability`]
//! set from the agent's [`SandboxConfig`](adk_sandbox::workspace::SandboxConfig).
//!
//! # Binding Rules
//!
//! - [`Capability::Shell`] → binds [`ExecCommandTool`]
//! - [`Capability::Filesystem`] → binds [`ReadFileTool`], [`WriteFileTool`], [`ListDirTool`],
//!   [`ApplyPatchTool`]

use super::tools::{ApplyPatchTool, ExecCommandTool, ListDirTool, ReadFileTool, WriteFileTool};
use adk_sandbox::workspace::{Capability, SandboxSession};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Binds sandbox tools to the agent based on the enabled capabilities.
///
/// Returns a vector of trait objects implementing [`adk_core::Tool`], corresponding
/// to the enabled capabilities. The caller is responsible for registering these
/// with the agent's tool set.
///
/// # Arguments
///
/// * `session` - The live sandbox session that tools will operate against
/// * `capabilities` - The set of enabled capabilities from the `SandboxConfig`
/// * `command_timeout` - The default timeout for shell command execution
///
/// # Examples
///
/// ```rust,ignore
/// use std::collections::HashSet;
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_sandbox::workspace::Capability;
///
/// let mut caps = HashSet::new();
/// caps.insert(Capability::Shell);
/// caps.insert(Capability::Filesystem);
///
/// let tools = bind_tools(session, &caps, Duration::from_secs(120));
/// assert_eq!(tools.len(), 5);
/// ```
pub fn bind_tools(
    session: Arc<dyn SandboxSession>,
    capabilities: &HashSet<Capability>,
    command_timeout: Duration,
) -> Vec<Arc<dyn adk_core::Tool>> {
    let mut tools: Vec<Arc<dyn adk_core::Tool>> = Vec::new();

    if capabilities.contains(&Capability::Shell) {
        tools.push(Arc::new(ExecCommandTool {
            session: Arc::clone(&session),
            timeout: command_timeout,
        }));
    }

    if capabilities.contains(&Capability::Filesystem) {
        tools.push(Arc::new(ReadFileTool { session: Arc::clone(&session) }));
        tools.push(Arc::new(WriteFileTool { session: Arc::clone(&session) }));
        tools.push(Arc::new(ListDirTool { session: Arc::clone(&session) }));
        tools.push(Arc::new(ApplyPatchTool { session: Arc::clone(&session) }));
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_sandbox::SandboxError;
    use adk_sandbox::workspace::{DirEntry, ExecOutput};
    use async_trait::async_trait;

    /// A mock sandbox session for testing tool binding logic.
    struct MockSession;

    #[async_trait]
    impl SandboxSession for MockSession {
        async fn exec_command(
            &self,
            _command: &str,
            _working_dir: Option<&str>,
        ) -> Result<ExecOutput, SandboxError> {
            unimplemented!("mock")
        }

        async fn read_file(&self, _path: &str) -> Result<Vec<u8>, SandboxError> {
            unimplemented!("mock")
        }

        async fn write_file(&self, _path: &str, _content: &[u8]) -> Result<(), SandboxError> {
            unimplemented!("mock")
        }

        async fn list_dir(&self, _path: &str) -> Result<Vec<DirEntry>, SandboxError> {
            unimplemented!("mock")
        }

        async fn apply_patch(&self, _patch: &str) -> Result<(), SandboxError> {
            unimplemented!("mock")
        }
    }

    fn mock_session() -> Arc<dyn SandboxSession> {
        Arc::new(MockSession)
    }

    #[test]
    fn test_empty_capabilities_returns_empty_vec() {
        let session = mock_session();
        let capabilities = HashSet::new();
        let tools = bind_tools(session, &capabilities, Duration::from_secs(120));
        assert!(tools.is_empty());
    }

    #[test]
    fn test_shell_only_returns_one_tool() {
        let session = mock_session();
        let mut capabilities = HashSet::new();
        capabilities.insert(Capability::Shell);
        let tools = bind_tools(session, &capabilities, Duration::from_secs(120));
        assert_eq!(tools.len(), 1);
    }

    #[test]
    fn test_filesystem_only_returns_four_tools() {
        let session = mock_session();
        let mut capabilities = HashSet::new();
        capabilities.insert(Capability::Filesystem);
        let tools = bind_tools(session, &capabilities, Duration::from_secs(120));
        assert_eq!(tools.len(), 4);
    }

    #[test]
    fn test_both_capabilities_returns_five_tools() {
        let session = mock_session();
        let mut capabilities = HashSet::new();
        capabilities.insert(Capability::Shell);
        capabilities.insert(Capability::Filesystem);
        let tools = bind_tools(session, &capabilities, Duration::from_secs(120));
        assert_eq!(tools.len(), 5);
    }
}
