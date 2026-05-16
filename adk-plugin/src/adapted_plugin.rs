//! Adapter wrapping a legacy closure-based [`Plugin`] as an [`EnhancedPlugin`].
//!
//! The [`AdaptedPlugin`] struct bridges the existing closure-based plugin system
//! to the new trait-based [`EnhancedPlugin`] interface, enabling legacy plugins
//! to participate in the enhanced pipeline without modification.
//!
//! # Overview
//!
//! Legacy plugins use callbacks that receive only a [`CallbackContext`] for tool hooks,
//! and `(CallbackContext, LlmRequest)` / `(CallbackContext, LlmResponse)` for model hooks.
//! They cannot modify tool arguments or results directly. The adapter:
//!
//! - Delegates `name()` to the inner [`Plugin::name()`]
//! - Uses a configurable priority (default 100)
//! - Invokes legacy `before_tool` / `after_tool` callbacks for side effects,
//!   but always returns `Continue` with unchanged args/result
//! - Maps legacy [`BeforeModelResult`] to [`BeforeModelCallResult`]
//! - Maps legacy `AfterModelCallback` results to [`AfterModelCallResult`]
//! - Delegates `close()` to the inner [`Plugin::close()`]
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_plugin::{AdaptedPlugin, Plugin, PluginConfig};
//!
//! let legacy_plugin = Plugin::new(PluginConfig {
//!     name: "my-legacy-plugin".to_string(),
//!     before_tool: Some(Box::new(|ctx| {
//!         Box::pin(async move {
//!             tracing::info!("tool starting");
//!             Ok(None)
//!         })
//!     })),
//!     ..Default::default()
//! });
//!
//! // Wrap with default priority (100)
//! let adapted = AdaptedPlugin::new(legacy_plugin, 100);
//! ```

use std::sync::Arc;

use adk_core::{
    BeforeModelResult, CallbackContext, LlmRequest, LlmResponse, Result, Tool, async_trait,
};
use serde_json::Value;

use crate::context::PluginContext;
use crate::enhanced_plugin::EnhancedPlugin;
use crate::hook_result::{
    AfterModelCallResult, AfterToolCallResult, BeforeModelCallResult, BeforeToolCallResult,
};
use crate::plugin::Plugin;

/// Wraps a legacy closure-based [`Plugin`] as an [`EnhancedPlugin`].
///
/// This adapter enables existing plugins to participate in the enhanced
/// pipeline without modification. Legacy callbacks are invoked for their
/// side effects, but the adapter does not modify tool arguments or results
/// (legacy callbacks don't have access to them).
///
/// For model hooks, the adapter maps between the legacy [`BeforeModelResult`]
/// and the new [`BeforeModelCallResult`], preserving short-circuit semantics.
pub struct AdaptedPlugin {
    inner: Plugin,
    priority: i32,
}

impl AdaptedPlugin {
    /// Create a new adapter wrapping a legacy plugin with the given priority.
    ///
    /// # Arguments
    ///
    /// * `plugin` - The legacy [`Plugin`] to wrap
    /// * `priority` - Execution priority (lower values execute first, default: 100)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_plugin::{AdaptedPlugin, Plugin, PluginConfig};
    ///
    /// let plugin = Plugin::new(PluginConfig {
    ///     name: "logger".to_string(),
    ///     ..Default::default()
    /// });
    ///
    /// let adapted = AdaptedPlugin::new(plugin, 50);
    /// assert_eq!(adapted.priority(), 50);
    /// ```
    pub fn new(plugin: Plugin, priority: i32) -> Self {
        Self {
            inner: plugin,
            priority,
        }
    }
}

#[async_trait]
impl EnhancedPlugin for AdaptedPlugin {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    async fn before_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        args: Value,
        ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<BeforeToolCallResult> {
        // Legacy before_tool callbacks only receive CallbackContext and return
        // Ok(None) to continue or Ok(Some(content)) to skip. They cannot modify
        // tool arguments. We invoke for side effects and always return Continue.
        if let Some(callback) = self.inner.before_tool() {
            // Invoke the legacy callback for its side effects (logging, etc.)
            // We ignore the return value since legacy callbacks can't modify args.
            let _ = callback(ctx).await?;
        }
        Ok(BeforeToolCallResult::Continue(args))
    }

    async fn after_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: &Value,
        result: Value,
        ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterToolCallResult> {
        // Legacy after_tool callbacks only receive CallbackContext and return
        // Ok(None) to continue or Ok(Some(content)). They cannot modify tool results.
        // We invoke for side effects and always return Continue with unchanged result.
        if let Some(callback) = self.inner.after_tool() {
            let _ = callback(ctx).await?;
        }
        Ok(AfterToolCallResult::Continue(result))
    }

    async fn before_model_call(
        &self,
        request: LlmRequest,
        ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<BeforeModelCallResult> {
        // Legacy before_model callbacks receive (CallbackContext, LlmRequest) and return
        // BeforeModelResult::Continue(request) or BeforeModelResult::Skip(response).
        // We map these to the new BeforeModelCallResult variants.
        if let Some(callback) = self.inner.before_model() {
            let legacy_result = callback(ctx, request).await?;
            match legacy_result {
                BeforeModelResult::Continue(req) => {
                    Ok(BeforeModelCallResult::Continue(req))
                }
                BeforeModelResult::Skip(response) => {
                    Ok(BeforeModelCallResult::ShortCircuit(response))
                }
            }
        } else {
            Ok(BeforeModelCallResult::Continue(request))
        }
    }

    async fn after_model_call(
        &self,
        response: LlmResponse,
        ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterModelCallResult> {
        // Legacy after_model callbacks receive (CallbackContext, LlmResponse) and return
        // Ok(Some(response)) to replace or Ok(None) to keep original.
        if let Some(callback) = self.inner.after_model() {
            let result = callback(ctx, response.clone()).await?;
            match result {
                Some(modified_response) => {
                    Ok(AfterModelCallResult::Continue(modified_response))
                }
                None => Ok(AfterModelCallResult::Continue(response)),
            }
        } else {
            Ok(AfterModelCallResult::Continue(response))
        }
    }

    async fn close(&self) {
        self.inner.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginConfig, plugin::Plugin};
    use adk_core::{BeforeModelResult, Content, LlmRequest, LlmResponse, Part};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Mock CallbackContext for testing
    struct MockCallbackContext;

    impl adk_core::ReadonlyContext for MockCallbackContext {
        fn invocation_id(&self) -> &str {
            "test-invocation"
        }

        fn agent_name(&self) -> &str {
            "test-agent"
        }

        fn user_id(&self) -> &str {
            "test-user"
        }

        fn app_name(&self) -> &str {
            "test-app"
        }

        fn session_id(&self) -> &str {
            "test-session"
        }

        fn branch(&self) -> &str {
            "main"
        }

        fn user_content(&self) -> &Content {
            static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
            CONTENT.get_or_init(|| Content::new("user"))
        }
    }

    #[async_trait]
    impl CallbackContext for MockCallbackContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    /// Mock Tool for testing
    struct MockTool;

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            "mock-tool"
        }

        fn description(&self) -> &str {
            "A mock tool for testing"
        }

        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    #[tokio::test]
    async fn test_name_delegates_to_inner() {
        let plugin = Plugin::new(PluginConfig {
            name: "my-legacy-plugin".to_string(),
            ..Default::default()
        });
        let adapted = AdaptedPlugin::new(plugin, 100);
        assert_eq!(adapted.name(), "my-legacy-plugin");
    }

    #[tokio::test]
    async fn test_priority_uses_configured_value() {
        let plugin = Plugin::new(PluginConfig {
            name: "test".to_string(),
            ..Default::default()
        });
        let adapted = AdaptedPlugin::new(plugin, 42);
        assert_eq!(adapted.priority(), 42);
    }

    #[tokio::test]
    async fn test_before_tool_call_invokes_legacy_callback() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let plugin = Plugin::new(PluginConfig {
            name: "test".to_string(),
            before_tool: Some(Box::new(move |_ctx| {
                let flag = called_clone.clone();
                Box::pin(async move {
                    flag.store(true, Ordering::SeqCst);
                    Ok(None)
                })
            })),
            ..Default::default()
        });

        let adapted = AdaptedPlugin::new(plugin, 100);
        let ctx: Arc<dyn CallbackContext> = Arc::new(MockCallbackContext);
        let plugin_ctx = PluginContext::new();
        let tool: Arc<dyn Tool> = Arc::new(MockTool);
        let args = serde_json::json!({"key": "value"});

        let result = adapted
            .before_tool_call(tool, args.clone(), ctx, &plugin_ctx)
            .await
            .unwrap();

        assert!(called.load(Ordering::SeqCst));
        match result {
            BeforeToolCallResult::Continue(returned_args) => {
                assert_eq!(returned_args, args);
            }
            _ => panic!("expected Continue"),
        }
    }

    #[tokio::test]
    async fn test_after_tool_call_invokes_legacy_callback() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let plugin = Plugin::new(PluginConfig {
            name: "test".to_string(),
            after_tool: Some(Box::new(move |_ctx| {
                let flag = called_clone.clone();
                Box::pin(async move {
                    flag.store(true, Ordering::SeqCst);
                    Ok(None)
                })
            })),
            ..Default::default()
        });

        let adapted = AdaptedPlugin::new(plugin, 100);
        let ctx: Arc<dyn CallbackContext> = Arc::new(MockCallbackContext);
        let plugin_ctx = PluginContext::new();
        let tool: Arc<dyn Tool> = Arc::new(MockTool);
        let args = serde_json::json!({"input": "test"});
        let result_val = serde_json::json!({"output": "done"});

        let result = adapted
            .after_tool_call(tool, &args, result_val.clone(), ctx, &plugin_ctx)
            .await
            .unwrap();

        assert!(called.load(Ordering::SeqCst));
        match result {
            AfterToolCallResult::Continue(returned_result) => {
                assert_eq!(returned_result, result_val);
            }
        }
    }

    #[tokio::test]
    async fn test_before_model_call_maps_continue() {
        let plugin = Plugin::new(PluginConfig {
            name: "test".to_string(),
            before_model: Some(Box::new(|_ctx, request| {
                Box::pin(async move { Ok(BeforeModelResult::Continue(request)) })
            })),
            ..Default::default()
        });

        let adapted = AdaptedPlugin::new(plugin, 100);
        let ctx: Arc<dyn CallbackContext> = Arc::new(MockCallbackContext);
        let plugin_ctx = PluginContext::new();
        let request = LlmRequest::new("test-model", vec![]);

        let result = adapted
            .before_model_call(request, ctx, &plugin_ctx)
            .await
            .unwrap();

        match result {
            BeforeModelCallResult::Continue(req) => {
                assert_eq!(req.model, "test-model");
            }
            _ => panic!("expected Continue"),
        }
    }

    #[tokio::test]
    async fn test_before_model_call_maps_skip_to_short_circuit() {
        let plugin = Plugin::new(PluginConfig {
            name: "test".to_string(),
            before_model: Some(Box::new(|_ctx, _request| {
                Box::pin(async move {
                    let response = LlmResponse {
                        content: Some(Content::new("model").with_text("cached")),
                        ..Default::default()
                    };
                    Ok(BeforeModelResult::Skip(response))
                })
            })),
            ..Default::default()
        });

        let adapted = AdaptedPlugin::new(plugin, 100);
        let ctx: Arc<dyn CallbackContext> = Arc::new(MockCallbackContext);
        let plugin_ctx = PluginContext::new();
        let request = LlmRequest::new("model", vec![]);

        let result = adapted
            .before_model_call(request, ctx, &plugin_ctx)
            .await
            .unwrap();

        match result {
            BeforeModelCallResult::ShortCircuit(resp) => {
                assert!(resp.content.is_some());
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[tokio::test]
    async fn test_after_model_call_maps_some_to_continue_modified() {
        let plugin = Plugin::new(PluginConfig {
            name: "test".to_string(),
            after_model: Some(Box::new(|_ctx, _response| {
                Box::pin(async move {
                    let modified = LlmResponse {
                        content: Some(Content::new("model").with_text("modified")),
                        ..Default::default()
                    };
                    Ok(Some(modified))
                })
            })),
            ..Default::default()
        });

        let adapted = AdaptedPlugin::new(plugin, 100);
        let ctx: Arc<dyn CallbackContext> = Arc::new(MockCallbackContext);
        let plugin_ctx = PluginContext::new();
        let response = LlmResponse::default();

        let result = adapted
            .after_model_call(response, ctx, &plugin_ctx)
            .await
            .unwrap();

        match result {
            AfterModelCallResult::Continue(resp) => {
                let content = resp.content.unwrap();
                assert!(content.parts.iter().any(|p| matches!(p, Part::Text { text } if text == "modified")));
            }
        }
    }

    #[tokio::test]
    async fn test_after_model_call_maps_none_to_continue_unchanged() {
        let plugin = Plugin::new(PluginConfig {
            name: "test".to_string(),
            after_model: Some(Box::new(|_ctx, _response| {
                Box::pin(async move { Ok(None) })
            })),
            ..Default::default()
        });

        let adapted = AdaptedPlugin::new(plugin, 100);
        let ctx: Arc<dyn CallbackContext> = Arc::new(MockCallbackContext);
        let plugin_ctx = PluginContext::new();
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("original")),
            ..Default::default()
        };

        let result = adapted
            .after_model_call(response, ctx, &plugin_ctx)
            .await
            .unwrap();

        match result {
            AfterModelCallResult::Continue(resp) => {
                let content = resp.content.unwrap();
                assert!(content.parts.iter().any(|p| matches!(p, Part::Text { text } if text == "original")));
            }
        }
    }

    #[tokio::test]
    async fn test_close_delegates_to_inner() {
        let closed = Arc::new(AtomicBool::new(false));
        let closed_clone = closed.clone();

        let plugin = Plugin::new(PluginConfig {
            name: "test".to_string(),
            close_fn: Some(Box::new(move || {
                let flag = closed_clone.clone();
                Box::pin(async move {
                    flag.store(true, Ordering::SeqCst);
                })
            })),
            ..Default::default()
        });

        let adapted = AdaptedPlugin::new(plugin, 100);
        adapted.close().await;

        assert!(closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_no_callbacks_returns_continue_unchanged() {
        let plugin = Plugin::new(PluginConfig {
            name: "empty".to_string(),
            ..Default::default()
        });

        let adapted = AdaptedPlugin::new(plugin, 100);
        let ctx: Arc<dyn CallbackContext> = Arc::new(MockCallbackContext);
        let plugin_ctx = PluginContext::new();
        let tool: Arc<dyn Tool> = Arc::new(MockTool);

        // before_tool_call with no callback
        let args = serde_json::json!({"x": 1});
        let result = adapted
            .before_tool_call(tool.clone(), args.clone(), ctx.clone(), &plugin_ctx)
            .await
            .unwrap();
        match result {
            BeforeToolCallResult::Continue(v) => assert_eq!(v, args),
            _ => panic!("expected Continue"),
        }

        // after_tool_call with no callback
        let res_val = serde_json::json!({"y": 2});
        let result = adapted
            .after_tool_call(tool.clone(), &args, res_val.clone(), ctx.clone(), &plugin_ctx)
            .await
            .unwrap();
        match result {
            AfterToolCallResult::Continue(v) => assert_eq!(v, res_val),
        }

        // before_model_call with no callback
        let request = LlmRequest::new("m", vec![]);
        let result = adapted
            .before_model_call(request, ctx.clone(), &plugin_ctx)
            .await
            .unwrap();
        match result {
            BeforeModelCallResult::Continue(req) => assert_eq!(req.model, "m"),
            _ => panic!("expected Continue"),
        }

        // after_model_call with no callback
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("hi")),
            ..Default::default()
        };
        let result = adapted
            .after_model_call(response, ctx, &plugin_ctx)
            .await
            .unwrap();
        match result {
            AfterModelCallResult::Continue(resp) => {
                assert!(resp.content.is_some());
            }
        }
    }
}
