//! Context wrapper that injects SharedState into the context chain.

use adk_core::{
    Agent, CallbackContext, Content, InvocationContext, Memory, ReadonlyContext, RunConfig,
    Session, SharedState,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Context wrapper that injects SharedState into the context chain.
///
/// Delegates all existing methods to the inner context and overrides
/// `shared_state()` to return the SharedState instance.
pub(crate) struct SharedStateContext {
    inner: Arc<dyn InvocationContext>,
    shared_state: Arc<SharedState>,
}

impl SharedStateContext {
    pub fn new(inner: Arc<dyn InvocationContext>, shared_state: Arc<SharedState>) -> Self {
        Self { inner, shared_state }
    }
}

#[async_trait]
impl ReadonlyContext for SharedStateContext {
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

    fn branch(&self) -> &str {
        self.inner.branch()
    }

    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl CallbackContext for SharedStateContext {
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
        Some(self.shared_state.clone())
    }
}

#[async_trait]
impl InvocationContext for SharedStateContext {
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

    async fn get_secret(&self, name: &str) -> adk_core::Result<Option<String>> {
        self.inner.get_secret(name).await
    }

    async fn get_secret_for(
        &self,
        request: &adk_core::SecretRequest,
    ) -> adk_core::Result<Option<String>> {
        self.inner.get_secret_for(request).await
    }
}
