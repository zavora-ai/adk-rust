use adk_core::{
    Agent, CallbackContext, Content, InvocationContext, Memory, ReadonlyContext, Result, RunConfig,
    Session,
};
use adk_skill::{SelectionPolicy, SkillIndex, apply_skill_injection};
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
        Arc::new(UserContentOverrideContext::new(ctx, content))
    } else {
        ctx
    }
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
    fn identity(&self) -> &adk_core::types::AdkIdentity {
        self.parent.identity()
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        self.parent.metadata()
    }
}

#[async_trait]
impl CallbackContext for UserContentOverrideContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        self.parent.artifacts()
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
}

#[allow(dead_code)]
fn _type_check_result(_: Result<()>) {}
