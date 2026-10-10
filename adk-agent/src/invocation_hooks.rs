//! Adapts a run's [`InvocationHooks`] into agent callbacks.
//!
//! Agents iterate these ahead of their own callbacks of the same kind, so a hook behaves exactly
//! like a callback registered first: a returned value short-circuits the rest, and an error is
//! handled as a failing callback.

use std::sync::Arc;

use adk_core::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, BeforeAgentCallback,
    BeforeModelCallback, BeforeToolCallback, InvocationHooks, OnToolErrorCallback,
};

/// One callback per hook object and kind, in the order the hooks are configured.
#[derive(Default)]
pub(crate) struct HookCallbacks {
    pub(crate) before_agent: Vec<BeforeAgentCallback>,
    pub(crate) after_agent: Vec<AfterAgentCallback>,
    pub(crate) before_model: Vec<BeforeModelCallback>,
    pub(crate) after_model: Vec<AfterModelCallback>,
    pub(crate) before_tool: Vec<BeforeToolCallback>,
    pub(crate) after_tool: Vec<AfterToolCallback>,
    pub(crate) on_tool_error: Vec<OnToolErrorCallback>,
}

impl HookCallbacks {
    pub(crate) fn new(hooks: &[Arc<dyn InvocationHooks>]) -> Self {
        let mut callbacks = Self::default();
        for hook in hooks {
            let h = Arc::clone(hook);
            callbacks.before_agent.push(Box::new(move |ctx| {
                let h = Arc::clone(&h);
                Box::pin(async move { h.before_agent(ctx).await })
            }));
            let h = Arc::clone(hook);
            callbacks.after_agent.push(Box::new(move |ctx| {
                let h = Arc::clone(&h);
                Box::pin(async move { h.after_agent(ctx).await })
            }));
            let h = Arc::clone(hook);
            callbacks.before_model.push(Box::new(move |ctx, request| {
                let h = Arc::clone(&h);
                Box::pin(async move { h.before_model(ctx, request).await })
            }));
            let h = Arc::clone(hook);
            callbacks.after_model.push(Box::new(move |ctx, response| {
                let h = Arc::clone(&h);
                Box::pin(async move { h.after_model(ctx, response).await })
            }));
            let h = Arc::clone(hook);
            callbacks.before_tool.push(Box::new(move |ctx| {
                let h = Arc::clone(&h);
                Box::pin(async move { h.before_tool(ctx).await })
            }));
            let h = Arc::clone(hook);
            callbacks.after_tool.push(Box::new(move |ctx| {
                let h = Arc::clone(&h);
                Box::pin(async move { h.after_tool(ctx).await })
            }));
            let h = Arc::clone(hook);
            callbacks.on_tool_error.push(Box::new(move |ctx, tool, args, error| {
                let h = Arc::clone(&h);
                Box::pin(async move { h.on_tool_error(ctx, tool, args, error).await })
            }));
        }
        callbacks
    }
}
