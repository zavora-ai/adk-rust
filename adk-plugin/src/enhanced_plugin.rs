//! Enhanced plugin trait with fine-grained hooks and default no-op implementations.
//!
//! The [`EnhancedPlugin`] trait provides a trait-based plugin interface that allows
//! plugin authors to implement only the hooks they need. All hook methods have default
//! implementations that pass through inputs unchanged (identity behavior).
//!
//! # Overview
//!
//! Unlike the closure-based [`PluginConfig`](crate::PluginConfig) approach, `EnhancedPlugin`
//! uses Rust's trait system for a more ergonomic and type-safe plugin authoring experience.
//! Plugins can:
//!
//! - Intercept and modify tool call arguments before execution
//! - Inspect and transform tool results after execution
//! - Modify LLM requests before they are sent
//! - Transform LLM responses after they are received
//! - Access shared state via [`PluginContext`] across all hook invocations
//! - Define execution priority for deterministic ordering
//!
//! # Examples
//!
//! ## Minimal plugin (no-op)
//!
//! ```rust
//! use adk_core::async_trait;
//! use adk_plugin::EnhancedPlugin;
//!
//! struct MyPlugin;
//!
//! #[async_trait]
//! impl EnhancedPlugin for MyPlugin {
//!     fn name(&self) -> &str {
//!         "my-plugin"
//!     }
//! }
//! ```
//!
//! ## Plugin with custom priority and before-tool hook
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_core::{async_trait, CallbackContext, Result, Tool};
//! use adk_plugin::{BeforeToolCallResult, EnhancedPlugin, PluginContext};
//! use serde_json::Value;
//!
//! struct ValidationPlugin;
//!
//! #[async_trait]
//! impl EnhancedPlugin for ValidationPlugin {
//!     fn name(&self) -> &str {
//!         "validation"
//!     }
//!
//!     fn priority(&self) -> i32 {
//!         10 // Run early in the pipeline
//!     }
//!
//!     async fn before_tool_call(
//!         &self,
//!         tool: Arc<dyn Tool>,
//!         args: Value,
//!         _ctx: Arc<dyn CallbackContext>,
//!         _plugin_ctx: &PluginContext,
//!     ) -> Result<BeforeToolCallResult> {
//!         // Inject a safety flag into all tool arguments
//!         let mut modified = args;
//!         if let Value::Object(ref mut map) = modified {
//!             map.insert("safe_mode".to_string(), Value::Bool(true));
//!         }
//!         Ok(BeforeToolCallResult::Continue(modified))
//!     }
//! }
//! ```
//!
//! ## Plugin using shared context for rate limiting
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_core::{async_trait, AdkError, CallbackContext, Result, Tool};
//! use adk_plugin::{BeforeToolCallResult, EnhancedPlugin, PluginContext};
//! use serde_json::Value;
//!
//! #[derive(Clone)]
//! struct RateLimitState {
//!     call_count: u32,
//! }
//!
//! struct RateLimitPlugin {
//!     max_calls: u32,
//! }
//!
//! #[async_trait]
//! impl EnhancedPlugin for RateLimitPlugin {
//!     fn name(&self) -> &str {
//!         "rate-limiter"
//!     }
//!
//!     fn priority(&self) -> i32 {
//!         5 // Security plugins run first
//!     }
//!
//!     async fn before_tool_call(
//!         &self,
//!         _tool: Arc<dyn Tool>,
//!         args: Value,
//!         _ctx: Arc<dyn CallbackContext>,
//!         plugin_ctx: &PluginContext,
//!     ) -> Result<BeforeToolCallResult> {
//!         let mut state = plugin_ctx.get::<RateLimitState>().await
//!             .unwrap_or(RateLimitState { call_count: 0 });
//!
//!         state.call_count += 1;
//!         plugin_ctx.insert(state.clone()).await;
//!
//!         if state.call_count > self.max_calls {
//!             return Err(AdkError::plugin("rate limit exceeded"));
//!         }
//!
//!         Ok(BeforeToolCallResult::Continue(args))
//!     }
//! }
//! ```

use std::sync::Arc;

use adk_core::{CallbackContext, LlmRequest, LlmResponse, Result, Tool, async_trait};
use serde_json::Value;

use crate::context::PluginContext;
use crate::hook_result::{
    AfterModelCallResult, AfterToolCallResult, BeforeModelCallResult, BeforeToolCallResult,
};

/// Enhanced plugin trait with fine-grained hooks and default no-op implementations.
///
/// Implement only the hooks you need. All methods have default implementations
/// that pass through inputs unchanged (identity function behavior).
///
/// # Priority
///
/// Plugins execute in ascending priority order (lower values run first).
/// The default priority is 100. Recommended ranges:
///
/// | Range | Use Case |
/// |-------|----------|
/// | 0–25 | Security plugins (auth, validation, rate limiting) |
/// | 26–50 | Caching plugins |
/// | 51–75 | Transformation plugins (sanitization, injection) |
/// | 76–100 | Logging and metrics plugins |
/// | 100+ | Application-specific plugins |
///
/// # Thread Safety
///
/// All implementations must be `Send + Sync` to support concurrent async execution.
/// The [`PluginContext`] provides thread-safe shared state access via
/// [`tokio::sync::RwLock`].
#[async_trait]
pub trait EnhancedPlugin: Send + Sync {
    /// Unique name identifying this plugin.
    ///
    /// Used for logging, debugging, and error messages. Should be a short,
    /// descriptive identifier (e.g., `"rate-limiter"`, `"cache"`, `"audit-log"`).
    fn name(&self) -> &str;

    /// Execution priority. Lower values execute first. Default: 100.
    ///
    /// When multiple plugins are registered, they execute hooks in ascending
    /// priority order. Plugins with the same priority execute in registration order.
    fn priority(&self) -> i32 {
        100
    }

    /// Called before a tool is executed.
    ///
    /// Receives the tool reference, call arguments, callback context, and shared plugin context.
    ///
    /// # Returns
    ///
    /// - `Ok(BeforeToolCallResult::Continue(args))` — pass (possibly modified) args to the
    ///   next plugin in the chain, and ultimately to the tool execution.
    /// - `Ok(BeforeToolCallResult::ShortCircuit(result))` — skip tool execution entirely
    ///   and use this synthetic result. No further plugins in the chain are invoked.
    /// - `Err(e)` — stop the pipeline and propagate the error. The tool is not executed.
    async fn before_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        args: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<BeforeToolCallResult> {
        Ok(BeforeToolCallResult::Continue(args))
    }

    /// Called after a tool executes successfully.
    ///
    /// Receives the tool reference, the arguments that were used for execution
    /// (after any modifications by before-hooks), the result, callback context,
    /// and shared plugin context.
    ///
    /// # Returns
    ///
    /// - `Ok(AfterToolCallResult::Continue(result))` — pass (possibly modified) result
    ///   to the next plugin in the chain, and ultimately return to the agent.
    /// - `Err(e)` — stop the pipeline and propagate the error.
    async fn after_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: &Value,
        result: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterToolCallResult> {
        Ok(AfterToolCallResult::Continue(result))
    }

    /// Called before a model (LLM) call is made.
    ///
    /// Receives the LLM request, callback context, and shared plugin context.
    ///
    /// # Returns
    ///
    /// - `Ok(BeforeModelCallResult::Continue(request))` — pass (possibly modified) request
    ///   to the next plugin in the chain, and ultimately to the LLM provider.
    /// - `Ok(BeforeModelCallResult::ShortCircuit(response))` — skip the model call entirely
    ///   and use this synthetic response. No further plugins in the chain are invoked.
    /// - `Err(e)` — stop the pipeline and propagate the error. The model is not called.
    async fn before_model_call(
        &self,
        request: LlmRequest,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<BeforeModelCallResult> {
        Ok(BeforeModelCallResult::Continue(request))
    }

    /// Called after a model (LLM) call completes.
    ///
    /// Receives the LLM response, callback context, and shared plugin context.
    ///
    /// # Returns
    ///
    /// - `Ok(AfterModelCallResult::Continue(response))` — pass (possibly modified) response
    ///   to the next plugin in the chain, and ultimately return to the agent.
    /// - `Err(e)` — stop the pipeline and propagate the error.
    async fn after_model_call(
        &self,
        response: LlmResponse,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterModelCallResult> {
        Ok(AfterModelCallResult::Continue(response))
    }

    /// Called when the plugin is being shut down.
    ///
    /// Override this method to perform cleanup operations such as flushing
    /// buffers, closing connections, or persisting state.
    ///
    /// The default implementation is a no-op.
    async fn close(&self) {}
}
