//! Context wrapper that makes an agent run under a parent's context act as itself.

use adk_core::{
    Agent, CallbackContext, Content, InvocationContext, Memory, ReadonlyContext, Result, RunConfig,
    Session, SharedState,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Overrides [`ReadonlyContext::acting_context`] for an agent that runs under a context
/// naming another agent — a workflow or team sub-agent runs under its parent's context.
///
/// The agent acts as its own principal ([`ActingConfig::principal_for`] its name),
/// for the subject and through the delegation chain of the wrapped context. Every other
/// method delegates to the wrapped context, including the optional capability methods,
/// which have permissive trait defaults and would otherwise be silently dropped.
///
/// [`ActingConfig::principal_for`]: adk_core::ActingConfig::principal_for
pub(crate) struct ActingAgentContext {
    inner: Arc<dyn InvocationContext>,
    agent_name: String,
}

impl ActingAgentContext {
    /// `ctx` itself when it already names `agent_name`, otherwise `ctx` wrapped so it acts
    /// as `agent_name`.
    pub(crate) fn scope(
        ctx: Arc<dyn InvocationContext>,
        agent_name: &str,
    ) -> Arc<dyn InvocationContext> {
        if ctx.agent_name() == agent_name {
            ctx
        } else {
            Arc::new(Self { inner: ctx, agent_name: agent_name.to_string() })
        }
    }
}

#[async_trait]
impl ReadonlyContext for ActingAgentContext {
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

    /// The only overridden method: the wrapped agent's own principal.
    fn acting_context(&self) -> adk_core::ActingContext {
        let mut acting = self.inner.acting_context();
        acting.agent =
            self.inner.run_config().acting.principal_for(self.inner.app_name(), &self.agent_name);
        acting
    }

    fn state(&self) -> Option<&dyn adk_core::State> {
        self.inner.state()
    }
}

#[async_trait]
impl CallbackContext for ActingAgentContext {
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
impl InvocationContext for ActingAgentContext {
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
