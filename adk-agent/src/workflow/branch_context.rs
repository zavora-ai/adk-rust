//! Context wrapper that places a sub-agent on its own conversation branch.

use adk_core::{
    Agent, CallbackContext, Content, InvocationContext, Memory, ReadonlyContext, Result, RunConfig,
    Session, SharedState,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Derives a child branch name from a parent branch.
///
/// Segments are joined with `.`, matching ADK Python's `_BranchPath` and ADK
/// Go's `deriveSubBranch`. An empty parent yields just the segment, so a run
/// that starts unbranched produces `parallel.sub_agent` rather than
/// `.parallel.sub_agent`.
pub(crate) fn derive_sub_branch(parent: &str, segment: &str) -> String {
    if segment.is_empty() {
        return parent.to_string();
    }
    if parent.is_empty() {
        return segment.to_string();
    }
    format!("{parent}.{segment}")
}

/// Context wrapper that overrides [`ReadonlyContext::branch`].
///
/// `ParallelAgent` gives each sub-agent its own branch so that history reads
/// scoped by branch exclude what concurrent siblings produced. Every other
/// method delegates to the wrapped context — including the optional capability
/// methods (cancellation, secrets, scopes, request metadata, shared state),
/// which have permissive trait defaults and would otherwise be silently dropped.
pub(crate) struct BranchContext {
    inner: Arc<dyn InvocationContext>,
    branch: String,
}

impl BranchContext {
    pub(crate) fn new(inner: Arc<dyn InvocationContext>, branch: String) -> Self {
        Self { inner, branch }
    }
}

#[async_trait]
impl ReadonlyContext for BranchContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.inner.agent_name()
    }

    fn user_id(&self) -> &str {
        self.inner.user_id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn session_id(&self) -> &str {
        self.inner.session_id()
    }

    /// The only overridden method: this sub-agent's own branch.
    fn branch(&self) -> &str {
        &self.branch
    }

    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl CallbackContext for BranchContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        self.inner.artifacts()
    }

    fn tool_outcome(&self) -> Option<adk_core::ToolOutcome> {
        self.inner.tool_outcome()
    }

    fn tool_name(&self) -> Option<&str> {
        self.inner.tool_name()
    }

    fn tool_input(&self) -> Option<&serde_json::Value> {
        self.inner.tool_input()
    }

    fn shared_state(&self) -> Option<Arc<SharedState>> {
        self.inner.shared_state()
    }
}

#[async_trait]
impl InvocationContext for BranchContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.inner.agent()
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.inner.memory()
    }

    fn session(&self) -> &dyn Session {
        self.inner.session()
    }

    fn run_config(&self) -> &RunConfig {
        self.inner.run_config()
    }

    fn end_invocation(&self) {
        self.inner.end_invocation();
    }

    fn ended(&self) -> bool {
        self.inner.ended()
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn user_scopes(&self) -> Vec<String> {
        self.inner.user_scopes()
    }

    fn request_metadata(&self) -> HashMap<String, serde_json::Value> {
        self.inner.request_metadata()
    }

    fn authoritative_transfer_targets(&self) -> bool {
        self.inner.authoritative_transfer_targets()
    }
    fn delegation_depth(&self) -> u32 {
        self.inner.delegation_depth()
    }
    fn max_delegation_depth(&self) -> Option<u32> {
        self.inner.max_delegation_depth()
    }

    fn orchestration_root_invocation_id(&self) -> &str {
        self.inner.orchestration_root_invocation_id()
    }

    fn orchestration_edge_id(&self) -> Option<&str> {
        self.inner.orchestration_edge_id()
    }

    fn requires_tool_confirmation(&self, tool_name: &str) -> bool {
        self.inner.requires_tool_confirmation(tool_name)
    }

    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.inner.get_secret(name).await
    }

    async fn get_secret_for(&self, request: &adk_core::SecretRequest) -> Result<Option<String>> {
        self.inner.get_secret_for(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::derive_sub_branch;

    #[test]
    fn joins_parent_and_segment() {
        assert_eq!(derive_sub_branch("root", "parallel.a"), "root.parallel.a");
    }

    #[test]
    fn empty_parent_yields_the_segment_alone() {
        assert_eq!(derive_sub_branch("", "parallel.a"), "parallel.a");
    }

    #[test]
    fn empty_segment_yields_the_parent_unchanged() {
        assert_eq!(derive_sub_branch("root", ""), "root");
    }
}
