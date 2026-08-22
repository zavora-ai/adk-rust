use crate::skill_shim::{SelectionPolicy, SkillIndex, apply_skill_injection};
use adk_core::{
    Agent, CallbackContext, Content, InvocationContext, Memory, ReadonlyContext, Result, RunConfig,
    Session,
};
use async_trait::async_trait;
use std::sync::Arc;

pub(crate) fn with_skill_injected_context(
    ctx: Arc<dyn InvocationContext>,
    skills_index: Option<&Arc<SkillIndex>>,
    skill_policy: &SelectionPolicy,
    max_skill_chars: usize,
) -> Arc<dyn InvocationContext> {
    let Some(index) = skills_index else {
        return ctx;
    };

    let mut content = ctx.user_content().clone();
    if apply_skill_injection(&mut content, index.as_ref(), skill_policy, max_skill_chars).is_some()
    {
        with_user_content_override(ctx, content)
    } else {
        ctx
    }
}

pub(crate) fn with_user_content_override(
    ctx: Arc<dyn InvocationContext>,
    user_content: Content,
) -> Arc<dyn InvocationContext> {
    Arc::new(UserContentOverrideContext::new(ctx, user_content))
}

struct UserContentOverrideContext {
    parent: Arc<dyn InvocationContext>,
    user_content: Content,
}

impl UserContentOverrideContext {
    fn new(parent: Arc<dyn InvocationContext>, user_content: Content) -> Self {
        Self { parent, user_content }
    }
}

#[async_trait]
impl ReadonlyContext for UserContentOverrideContext {
    fn invocation_id(&self) -> &str {
        self.parent.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.parent.agent_name()
    }

    fn user_id(&self) -> &str {
        self.parent.user_id()
    }

    fn app_name(&self) -> &str {
        self.parent.app_name()
    }

    fn session_id(&self) -> &str {
        self.parent.session_id()
    }

    fn branch(&self) -> &str {
        self.parent.branch()
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for UserContentOverrideContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        self.parent.artifacts()
    }

    fn tool_outcome(&self) -> Option<adk_core::ToolOutcome> {
        self.parent.tool_outcome()
    }

    fn tool_name(&self) -> Option<&str> {
        self.parent.tool_name()
    }

    fn tool_input(&self) -> Option<&serde_json::Value> {
        self.parent.tool_input()
    }

    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.parent.shared_state()
    }
}

#[async_trait]
impl InvocationContext for UserContentOverrideContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.parent.agent()
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.parent.memory()
    }

    fn session(&self) -> &dyn Session {
        self.parent.session()
    }

    fn run_config(&self) -> &RunConfig {
        self.parent.run_config()
    }

    fn end_invocation(&self) {
        self.parent.end_invocation();
    }

    fn ended(&self) -> bool {
        self.parent.ended()
    }

    fn is_cancelled(&self) -> bool {
        self.parent.is_cancelled()
    }

    fn user_scopes(&self) -> Vec<String> {
        self.parent.user_scopes()
    }

    fn request_metadata(&self) -> std::collections::HashMap<String, serde_json::Value> {
        self.parent.request_metadata()
    }

    fn authoritative_transfer_targets(&self) -> bool {
        self.parent.authoritative_transfer_targets()
    }
    fn delegation_depth(&self) -> u32 {
        self.parent.delegation_depth()
    }
    fn max_delegation_depth(&self) -> Option<u32> {
        self.parent.max_delegation_depth()
    }

    fn orchestration_root_invocation_id(&self) -> &str {
        self.parent.orchestration_root_invocation_id()
    }

    fn orchestration_edge_id(&self) -> Option<&str> {
        self.parent.orchestration_edge_id()
    }

    fn requires_tool_confirmation(&self, tool_name: &str) -> bool {
        self.parent.requires_tool_confirmation(tool_name)
    }

    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.parent.get_secret(name).await
    }

    async fn get_secret_for(&self, request: &adk_core::SecretRequest) -> Result<Option<String>> {
        self.parent.get_secret_for(request).await
    }
}

#[allow(dead_code)]
fn _type_check_result(_: Result<()>) {}
