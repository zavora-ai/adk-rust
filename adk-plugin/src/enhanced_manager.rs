//! Enhanced Plugin Manager with priority-based pipeline execution.
//!
//! [`EnhancedPluginManager`] manages a collection of [`EnhancedPlugin`] instances,
//! executing their hooks in priority order with pipeline semantics where each
//! plugin's output feeds the next plugin's input.
//!
//! # Overview
//!
//! The manager provides four pipeline methods:
//!
//! - [`run_before_tool_call`](EnhancedPluginManager::run_before_tool_call) — intercept tool calls before execution
//! - [`run_after_tool_call`](EnhancedPluginManager::run_after_tool_call) — transform tool results after execution
//! - [`run_before_model_call`](EnhancedPluginManager::run_before_model_call) — intercept model calls before execution
//! - [`run_after_model_call`](EnhancedPluginManager::run_after_model_call) — transform model responses after execution
//!
//! # Pipeline Semantics
//!
//! - **Continue**: The (possibly modified) value is passed to the next plugin in the chain.
//! - **ShortCircuit** (before-hooks only): Stops the pipeline immediately, skips the
//!   underlying operation, and returns the synthetic result.
//! - **Error**: Stops the pipeline immediately and propagates the error to the caller.
//!
//! # Priority Ordering
//!
//! Plugins execute in ascending priority order (lower values run first).
//! Plugins with the same priority execute in registration order (stable sort).
//!
//! # Examples
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_plugin::{EnhancedPluginManager, EnhancedPlugin};
//!
//! let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
//!     Arc::new(SecurityPlugin),   // priority = 10
//!     Arc::new(CachePlugin),      // priority = 50
//!     Arc::new(LoggingPlugin),    // priority = 100
//! ];
//!
//! let manager = EnhancedPluginManager::new(plugins);
//! // Plugins will execute in order: Security → Cache → Logging
//! ```

use std::sync::Arc;

use adk_core::{CallbackContext, LlmRequest, LlmResponse, Result, Tool};
use serde_json::Value;
use tracing::{debug, warn};

use crate::context::PluginContext;
use crate::enhanced_plugin::EnhancedPlugin;
use crate::hook_result::{
    AfterModelCallResult, AfterToolCallResult, BeforeModelCallResult, BeforeToolCallResult,
};
use crate::manager::PluginManagerConfig;

/// Manages enhanced plugins with priority-based pipeline execution.
///
/// Plugins are stored sorted by priority (ascending). All pipeline methods
/// iterate plugins in this order, passing each plugin's output as input to
/// the next plugin in the chain.
///
/// # Thread Safety
///
/// `EnhancedPluginManager` is `Send + Sync` and can be shared across async tasks
/// via `Arc<EnhancedPluginManager>`.
pub struct EnhancedPluginManager {
    /// Plugins sorted by priority (ascending). Same-priority preserves registration order.
    plugins: Vec<Arc<dyn EnhancedPlugin>>,
    /// Shared plugin context for the lifetime of this manager.
    context: Arc<PluginContext>,
    /// Configuration for the manager (e.g., close timeout).
    config: PluginManagerConfig,
}

impl EnhancedPluginManager {
    /// Creates a new `EnhancedPluginManager` with the given plugins.
    ///
    /// Plugins are sorted by priority in ascending order using a stable sort,
    /// so plugins with the same priority retain their registration order.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_plugin::{EnhancedPluginManager, EnhancedPlugin};
    ///
    /// let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
    ///     Arc::new(MyPlugin),
    /// ];
    /// let manager = EnhancedPluginManager::new(plugins);
    /// ```
    pub fn new(mut plugins: Vec<Arc<dyn EnhancedPlugin>>) -> Self {
        plugins.sort_by_key(|p| p.priority());
        Self {
            plugins,
            context: Arc::new(PluginContext::new()),
            config: PluginManagerConfig::default(),
        }
    }

    /// Creates a new `EnhancedPluginManager` with custom configuration.
    pub fn with_config(mut plugins: Vec<Arc<dyn EnhancedPlugin>>, config: PluginManagerConfig) -> Self {
        plugins.sort_by_key(|p| p.priority());
        Self {
            plugins,
            context: Arc::new(PluginContext::new()),
            config,
        }
    }

    /// Adds a plugin after construction, re-sorting by priority.
    ///
    /// The plugin is inserted and the entire list is re-sorted using a stable sort
    /// to maintain registration order for same-priority plugins.
    pub fn add_plugin(&mut self, plugin: Arc<dyn EnhancedPlugin>) {
        self.plugins.push(plugin);
        self.plugins.sort_by_key(|p| p.priority());
    }

    /// Returns a reference to the shared plugin context.
    ///
    /// The context is shared across all hook invocations and persists for the
    /// lifetime of this manager.
    pub fn context(&self) -> &Arc<PluginContext> {
        &self.context
    }

    /// Returns the number of registered plugins.
    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    /// Returns the names of all registered plugins in execution order.
    pub fn plugin_names(&self) -> Vec<&str> {
        self.plugins.iter().map(|p| p.name()).collect()
    }

    /// Executes the `before_tool_call` pipeline across all plugins in priority order.
    ///
    /// Each plugin receives the (possibly modified) arguments from the previous plugin.
    /// If a plugin returns `ShortCircuit`, the pipeline stops and the synthetic result
    /// is returned. If a plugin returns an error, the pipeline stops and the error
    /// is propagated.
    ///
    /// # Arguments
    ///
    /// * `tool` - The tool about to be executed
    /// * `args` - The initial tool call arguments
    /// * `ctx` - The callback context for the current invocation
    ///
    /// # Returns
    ///
    /// - `Ok(BeforeToolCallResult::Continue(args))` — final modified arguments for tool execution
    /// - `Ok(BeforeToolCallResult::ShortCircuit(result))` — synthetic result, skip tool execution
    /// - `Err(e)` — pipeline error, skip tool execution
    pub async fn run_before_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        args: Value,
        ctx: Arc<dyn CallbackContext>,
    ) -> Result<BeforeToolCallResult> {
        let mut current_args = args;

        for plugin in &self.plugins {
            debug!(plugin = plugin.name(), "running before_tool_call");
            match plugin
                .before_tool_call(tool.clone(), current_args, ctx.clone(), &self.context)
                .await?
            {
                BeforeToolCallResult::Continue(modified_args) => {
                    current_args = modified_args;
                }
                BeforeToolCallResult::ShortCircuit(result) => {
                    debug!(plugin = plugin.name(), "before_tool_call short-circuited");
                    return Ok(BeforeToolCallResult::ShortCircuit(result));
                }
            }
        }

        Ok(BeforeToolCallResult::Continue(current_args))
    }

    /// Executes the `after_tool_call` pipeline across all plugins in priority order.
    ///
    /// Each plugin receives the (possibly modified) result from the previous plugin.
    /// The `args` parameter contains the final modified arguments from the before-hook
    /// pipeline (not the original arguments).
    ///
    /// # Arguments
    ///
    /// * `tool` - The tool that was executed
    /// * `args` - The final arguments used for tool execution (after before-hook modifications)
    /// * `result` - The initial tool execution result
    /// * `ctx` - The callback context for the current invocation
    ///
    /// # Returns
    ///
    /// - `Ok(AfterToolCallResult::Continue(result))` — final modified result
    /// - `Err(e)` — pipeline error
    pub async fn run_after_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        args: &Value,
        result: Value,
        ctx: Arc<dyn CallbackContext>,
    ) -> Result<AfterToolCallResult> {
        let mut current_result = result;

        for plugin in &self.plugins {
            debug!(plugin = plugin.name(), "running after_tool_call");
            match plugin
                .after_tool_call(tool.clone(), args, current_result, ctx.clone(), &self.context)
                .await?
            {
                AfterToolCallResult::Continue(modified_result) => {
                    current_result = modified_result;
                }
            }
        }

        Ok(AfterToolCallResult::Continue(current_result))
    }

    /// Executes the `before_model_call` pipeline across all plugins in priority order.
    ///
    /// Each plugin receives the (possibly modified) request from the previous plugin.
    /// If a plugin returns `ShortCircuit`, the pipeline stops and the synthetic response
    /// is returned. If a plugin returns an error, the pipeline stops and the error
    /// is propagated.
    ///
    /// # Arguments
    ///
    /// * `request` - The initial LLM request
    /// * `ctx` - The callback context for the current invocation
    ///
    /// # Returns
    ///
    /// - `Ok(BeforeModelCallResult::Continue(request))` — final modified request for model call
    /// - `Ok(BeforeModelCallResult::ShortCircuit(response))` — synthetic response, skip model call
    /// - `Err(e)` — pipeline error, skip model call
    pub async fn run_before_model_call(
        &self,
        request: LlmRequest,
        ctx: Arc<dyn CallbackContext>,
    ) -> Result<BeforeModelCallResult> {
        let mut current_request = request;

        for plugin in &self.plugins {
            debug!(plugin = plugin.name(), "running before_model_call");
            match plugin
                .before_model_call(current_request, ctx.clone(), &self.context)
                .await?
            {
                BeforeModelCallResult::Continue(modified_request) => {
                    current_request = modified_request;
                }
                BeforeModelCallResult::ShortCircuit(response) => {
                    debug!(plugin = plugin.name(), "before_model_call short-circuited");
                    return Ok(BeforeModelCallResult::ShortCircuit(response));
                }
            }
        }

        Ok(BeforeModelCallResult::Continue(current_request))
    }

    /// Executes the `after_model_call` pipeline across all plugins in priority order.
    ///
    /// Each plugin receives the (possibly modified) response from the previous plugin.
    /// If a plugin returns an error, the pipeline stops and the error is propagated.
    ///
    /// # Arguments
    ///
    /// * `response` - The initial LLM response
    /// * `ctx` - The callback context for the current invocation
    ///
    /// # Returns
    ///
    /// - `Ok(AfterModelCallResult::Continue(response))` — final modified response
    /// - `Err(e)` — pipeline error
    pub async fn run_after_model_call(
        &self,
        response: LlmResponse,
        ctx: Arc<dyn CallbackContext>,
    ) -> Result<AfterModelCallResult> {
        let mut current_response = response;

        for plugin in &self.plugins {
            debug!(plugin = plugin.name(), "running after_model_call");
            match plugin
                .after_model_call(current_response, ctx.clone(), &self.context)
                .await?
            {
                AfterModelCallResult::Continue(modified_response) => {
                    current_response = modified_response;
                }
            }
        }

        Ok(AfterModelCallResult::Continue(current_response))
    }

    /// Closes all plugins, ignoring individual close errors.
    ///
    /// Each plugin's `close()` method is called in sequence. Errors during
    /// close are logged but do not prevent other plugins from being closed.
    pub async fn close(&self) {
        debug!("closing {} enhanced plugins", self.plugins.len());

        for plugin in &self.plugins {
            let close_future = plugin.close();
            match tokio::time::timeout(self.config.close_timeout, close_future).await {
                Ok(()) => {
                    debug!(plugin = plugin.name(), "enhanced plugin closed successfully");
                }
                Err(_) => {
                    warn!(plugin = plugin.name(), "enhanced plugin close timed out");
                }
            }
        }
    }
}

impl std::fmt::Debug for EnhancedPluginManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnhancedPluginManager")
            .field("plugin_count", &self.plugins.len())
            .field("plugin_names", &self.plugin_names())
            .field("close_timeout", &self.config.close_timeout)
            .finish()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{AdkError, LlmRequest, LlmResponse, async_trait};
    use adk_core::Content as AdkContent;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // --- Test helpers ---

    /// A simple plugin that passes through unchanged.
    struct NoOpPlugin {
        name: String,
        priority: i32,
    }

    impl NoOpPlugin {
        fn new(name: &str, priority: i32) -> Self {
            Self { name: name.to_string(), priority }
        }
    }

    #[async_trait]
    impl EnhancedPlugin for NoOpPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }
    }

    /// A plugin that appends a field to tool call arguments.
    struct ArgModifierPlugin {
        name: String,
        priority: i32,
        key: String,
        value: Value,
    }

    #[async_trait]
    impl EnhancedPlugin for ArgModifierPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeToolCallResult> {
            let mut modified = args;
            if let Value::Object(ref mut map) = modified {
                map.insert(self.key.clone(), self.value.clone());
            }
            Ok(BeforeToolCallResult::Continue(modified))
        }
    }

    /// A plugin that modifies tool results.
    struct ResultModifierPlugin {
        name: String,
        priority: i32,
        key: String,
        value: Value,
    }

    #[async_trait]
    impl EnhancedPlugin for ResultModifierPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn after_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            _args: &Value,
            result: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<AfterToolCallResult> {
            let mut modified = result;
            if let Value::Object(ref mut map) = modified {
                map.insert(self.key.clone(), self.value.clone());
            }
            Ok(AfterToolCallResult::Continue(modified))
        }
    }

    /// A plugin that short-circuits before_tool_call.
    struct ShortCircuitPlugin {
        name: String,
        priority: i32,
        result: Value,
    }

    #[async_trait]
    impl EnhancedPlugin for ShortCircuitPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            _args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeToolCallResult> {
            Ok(BeforeToolCallResult::ShortCircuit(self.result.clone()))
        }
    }

    /// A plugin that returns an error from before_tool_call.
    struct ErrorPlugin {
        name: String,
        priority: i32,
    }

    #[async_trait]
    impl EnhancedPlugin for ErrorPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            _args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeToolCallResult> {
            Err(AdkError::agent("test error from plugin"))
        }

        async fn after_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            _args: &Value,
            _result: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<AfterToolCallResult> {
            Err(AdkError::agent("test error from after_tool"))
        }

        async fn before_model_call(
            &self,
            _request: LlmRequest,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeModelCallResult> {
            Err(AdkError::agent("test error from before_model"))
        }

        async fn after_model_call(
            &self,
            _response: LlmResponse,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<AfterModelCallResult> {
            Err(AdkError::agent("test error from after_model"))
        }
    }

    /// A plugin that tracks whether it was called.
    struct TrackingPlugin {
        name: String,
        priority: i32,
        before_tool_called: AtomicBool,
        after_tool_called: AtomicBool,
        before_model_called: AtomicBool,
        after_model_called: AtomicBool,
    }

    impl TrackingPlugin {
        fn new(name: &str, priority: i32) -> Self {
            Self {
                name: name.to_string(),
                priority,
                before_tool_called: AtomicBool::new(false),
                after_tool_called: AtomicBool::new(false),
                before_model_called: AtomicBool::new(false),
                after_model_called: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl EnhancedPlugin for TrackingPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeToolCallResult> {
            self.before_tool_called.store(true, Ordering::SeqCst);
            Ok(BeforeToolCallResult::Continue(args))
        }

        async fn after_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            _args: &Value,
            result: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<AfterToolCallResult> {
            self.after_tool_called.store(true, Ordering::SeqCst);
            Ok(AfterToolCallResult::Continue(result))
        }

        async fn before_model_call(
            &self,
            request: LlmRequest,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeModelCallResult> {
            self.before_model_called.store(true, Ordering::SeqCst);
            Ok(BeforeModelCallResult::Continue(request))
        }

        async fn after_model_call(
            &self,
            response: LlmResponse,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<AfterModelCallResult> {
            self.after_model_called.store(true, Ordering::SeqCst);
            Ok(AfterModelCallResult::Continue(response))
        }
    }

    /// A plugin that short-circuits before_model_call.
    struct ModelShortCircuitPlugin {
        name: String,
        priority: i32,
    }

    #[async_trait]
    impl EnhancedPlugin for ModelShortCircuitPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_model_call(
            &self,
            _request: LlmRequest,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeModelCallResult> {
            Ok(BeforeModelCallResult::ShortCircuit(LlmResponse::default()))
        }
    }

    /// A plugin that records execution order via a shared counter.
    struct OrderTrackingPlugin {
        name: String,
        priority: i32,
        order_counter: Arc<AtomicUsize>,
        recorded_order: AtomicUsize,
    }

    impl OrderTrackingPlugin {
        fn new(name: &str, priority: i32, counter: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                priority,
                order_counter: counter,
                recorded_order: AtomicUsize::new(0),
            }
        }

        fn execution_order(&self) -> usize {
            self.recorded_order.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EnhancedPlugin for OrderTrackingPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeToolCallResult> {
            let order = self.order_counter.fetch_add(1, Ordering::SeqCst);
            self.recorded_order.store(order, Ordering::SeqCst);
            Ok(BeforeToolCallResult::Continue(args))
        }
    }

    // --- Mock Tool and CallbackContext ---

    struct MockTool;

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            "mock_tool"
        }

        fn description(&self) -> &str {
            "A mock tool for testing"
        }

        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: Value,
        ) -> Result<Value> {
            Ok(json!({"result": "mock"}))
        }
    }

    struct MockCallbackContext {
        content: AdkContent,
    }

    impl MockCallbackContext {
        fn new() -> Self {
            Self { content: AdkContent::new("user") }
        }
    }

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
            ""
        }

        fn user_content(&self) -> &AdkContent {
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for MockCallbackContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }

        fn tool_name(&self) -> Option<&str> {
            Some("mock_tool")
        }
    }

    fn mock_tool() -> Arc<dyn Tool> {
        Arc::new(MockTool)
    }

    fn mock_ctx() -> Arc<dyn CallbackContext> {
        Arc::new(MockCallbackContext::new())
    }

    fn mock_request() -> LlmRequest {
        LlmRequest::new("test-model", vec![])
    }

    // --- Tests ---

    #[test]
    fn test_new_sorts_by_priority() {
        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(NoOpPlugin::new("c", 100)),
            Arc::new(NoOpPlugin::new("a", 10)),
            Arc::new(NoOpPlugin::new("b", 50)),
        ];

        let manager = EnhancedPluginManager::new(plugins);
        assert_eq!(manager.plugin_names(), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_stable_sort_preserves_registration_order() {
        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(NoOpPlugin::new("first", 100)),
            Arc::new(NoOpPlugin::new("second", 100)),
            Arc::new(NoOpPlugin::new("third", 100)),
        ];

        let manager = EnhancedPluginManager::new(plugins);
        assert_eq!(manager.plugin_names(), vec!["first", "second", "third"]);
    }

    #[test]
    fn test_add_plugin_resorts() {
        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(NoOpPlugin::new("b", 50)),
            Arc::new(NoOpPlugin::new("c", 100)),
        ];

        let mut manager = EnhancedPluginManager::new(plugins);
        manager.add_plugin(Arc::new(NoOpPlugin::new("a", 10)));

        assert_eq!(manager.plugin_names(), vec!["a", "b", "c"]);
        assert_eq!(manager.plugin_count(), 3);
    }

    #[test]
    fn test_context_accessor() {
        let manager = EnhancedPluginManager::new(vec![]);
        let ctx = manager.context();
        // Just verify we can access it without panic
        assert!(Arc::strong_count(ctx) >= 1);
    }

    #[test]
    fn test_empty_manager() {
        let manager = EnhancedPluginManager::new(vec![]);
        assert_eq!(manager.plugin_count(), 0);
        assert!(manager.plugin_names().is_empty());
    }

    #[tokio::test]
    async fn test_before_tool_call_pipeline_propagation() {
        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(ArgModifierPlugin {
                name: "plugin1".to_string(),
                priority: 10,
                key: "added_by_1".to_string(),
                value: json!(true),
            }),
            Arc::new(ArgModifierPlugin {
                name: "plugin2".to_string(),
                priority: 20,
                key: "added_by_2".to_string(),
                value: json!("hello"),
            }),
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let result = manager
            .run_before_tool_call(mock_tool(), json!({"original": "value"}), mock_ctx())
            .await
            .unwrap();

        match result {
            BeforeToolCallResult::Continue(args) => {
                assert_eq!(args["original"], "value");
                assert_eq!(args["added_by_1"], true);
                assert_eq!(args["added_by_2"], "hello");
            }
            BeforeToolCallResult::ShortCircuit(_) => panic!("expected Continue"),
        }
    }

    #[tokio::test]
    async fn test_before_tool_call_short_circuit() {
        let tracking = Arc::new(TrackingPlugin::new("after_short_circuit", 50));
        let tracking_clone = tracking.clone();

        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(ShortCircuitPlugin {
                name: "short_circuit".to_string(),
                priority: 10,
                result: json!({"cached": true}),
            }),
            tracking_clone,
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let result = manager
            .run_before_tool_call(mock_tool(), json!({}), mock_ctx())
            .await
            .unwrap();

        match result {
            BeforeToolCallResult::ShortCircuit(value) => {
                assert_eq!(value, json!({"cached": true}));
            }
            BeforeToolCallResult::Continue(_) => panic!("expected ShortCircuit"),
        }

        // The tracking plugin should NOT have been called
        assert!(!tracking.before_tool_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_before_tool_call_error_propagation() {
        let tracking = Arc::new(TrackingPlugin::new("after_error", 50));
        let tracking_clone = tracking.clone();

        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(ErrorPlugin { name: "error_plugin".to_string(), priority: 10 }),
            tracking_clone,
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let result = manager
            .run_before_tool_call(mock_tool(), json!({}), mock_ctx())
            .await;

        assert!(result.is_err());
        // The tracking plugin should NOT have been called
        assert!(!tracking.before_tool_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_after_tool_call_pipeline_propagation() {
        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(ResultModifierPlugin {
                name: "plugin1".to_string(),
                priority: 10,
                key: "enriched_by_1".to_string(),
                value: json!(true),
            }),
            Arc::new(ResultModifierPlugin {
                name: "plugin2".to_string(),
                priority: 20,
                key: "enriched_by_2".to_string(),
                value: json!(42),
            }),
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let args = json!({"tool_arg": "test"});
        let result = manager
            .run_after_tool_call(mock_tool(), &args, json!({"status": "ok"}), mock_ctx())
            .await
            .unwrap();

        match result {
            AfterToolCallResult::Continue(value) => {
                assert_eq!(value["status"], "ok");
                assert_eq!(value["enriched_by_1"], true);
                assert_eq!(value["enriched_by_2"], 42);
            }
        }
    }

    #[tokio::test]
    async fn test_after_tool_call_error_propagation() {
        let tracking = Arc::new(TrackingPlugin::new("after_error", 50));
        let tracking_clone = tracking.clone();

        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(ErrorPlugin { name: "error_plugin".to_string(), priority: 10 }),
            tracking_clone,
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let result = manager
            .run_after_tool_call(mock_tool(), &json!({}), json!({}), mock_ctx())
            .await;

        assert!(result.is_err());
        assert!(!tracking.after_tool_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_before_model_call_pipeline_propagation() {
        // Use no-op plugins to verify the request passes through
        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(NoOpPlugin::new("plugin1", 10)),
            Arc::new(NoOpPlugin::new("plugin2", 20)),
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let request = mock_request();
        let result = manager
            .run_before_model_call(request, mock_ctx())
            .await
            .unwrap();

        match result {
            BeforeModelCallResult::Continue(_) => { /* pass */ }
            BeforeModelCallResult::ShortCircuit(_) => panic!("expected Continue"),
        }
    }

    #[tokio::test]
    async fn test_before_model_call_short_circuit() {
        let tracking = Arc::new(TrackingPlugin::new("after_short_circuit", 50));
        let tracking_clone = tracking.clone();

        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(ModelShortCircuitPlugin {
                name: "model_short_circuit".to_string(),
                priority: 10,
            }),
            tracking_clone,
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let result = manager
            .run_before_model_call(mock_request(), mock_ctx())
            .await
            .unwrap();

        match result {
            BeforeModelCallResult::ShortCircuit(_) => { /* pass */ }
            BeforeModelCallResult::Continue(_) => panic!("expected ShortCircuit"),
        }

        assert!(!tracking.before_model_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_before_model_call_error_propagation() {
        let tracking = Arc::new(TrackingPlugin::new("after_error", 50));
        let tracking_clone = tracking.clone();

        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(ErrorPlugin { name: "error_plugin".to_string(), priority: 10 }),
            tracking_clone,
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let result = manager
            .run_before_model_call(mock_request(), mock_ctx())
            .await;

        assert!(result.is_err());
        assert!(!tracking.before_model_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_after_model_call_pipeline_propagation() {
        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(NoOpPlugin::new("plugin1", 10)),
            Arc::new(NoOpPlugin::new("plugin2", 20)),
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let result = manager
            .run_after_model_call(LlmResponse::default(), mock_ctx())
            .await
            .unwrap();

        match result {
            AfterModelCallResult::Continue(_) => { /* pass */ }
        }
    }

    #[tokio::test]
    async fn test_after_model_call_error_propagation() {
        let tracking = Arc::new(TrackingPlugin::new("after_error", 50));
        let tracking_clone = tracking.clone();

        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(ErrorPlugin { name: "error_plugin".to_string(), priority: 10 }),
            tracking_clone,
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let result = manager
            .run_after_model_call(LlmResponse::default(), mock_ctx())
            .await;

        assert!(result.is_err());
        assert!(!tracking.after_model_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_empty_plugin_list_before_tool_call() {
        let manager = EnhancedPluginManager::new(vec![]);
        let result = manager
            .run_before_tool_call(mock_tool(), json!({"key": "value"}), mock_ctx())
            .await
            .unwrap();

        match result {
            BeforeToolCallResult::Continue(args) => {
                assert_eq!(args, json!({"key": "value"}));
            }
            BeforeToolCallResult::ShortCircuit(_) => panic!("expected Continue"),
        }
    }

    #[tokio::test]
    async fn test_empty_plugin_list_after_tool_call() {
        let manager = EnhancedPluginManager::new(vec![]);
        let result = manager
            .run_after_tool_call(mock_tool(), &json!({}), json!({"result": 42}), mock_ctx())
            .await
            .unwrap();

        match result {
            AfterToolCallResult::Continue(value) => {
                assert_eq!(value, json!({"result": 42}));
            }
        }
    }

    #[tokio::test]
    async fn test_empty_plugin_list_before_model_call() {
        let manager = EnhancedPluginManager::new(vec![]);
        let request = mock_request();
        let result = manager
            .run_before_model_call(request, mock_ctx())
            .await
            .unwrap();

        match result {
            BeforeModelCallResult::Continue(_) => { /* pass */ }
            BeforeModelCallResult::ShortCircuit(_) => panic!("expected Continue"),
        }
    }

    #[tokio::test]
    async fn test_empty_plugin_list_after_model_call() {
        let manager = EnhancedPluginManager::new(vec![]);
        let result = manager
            .run_after_model_call(LlmResponse::default(), mock_ctx())
            .await
            .unwrap();

        match result {
            AfterModelCallResult::Continue(_) => { /* pass */ }
        }
    }

    #[tokio::test]
    async fn test_priority_ordering_execution() {
        let counter = Arc::new(AtomicUsize::new(0));

        let p1 = Arc::new(OrderTrackingPlugin::new("high_priority", 10, counter.clone()));
        let p2 = Arc::new(OrderTrackingPlugin::new("medium_priority", 50, counter.clone()));
        let p3 = Arc::new(OrderTrackingPlugin::new("low_priority", 100, counter.clone()));

        let p1_clone = p1.clone();
        let p2_clone = p2.clone();
        let p3_clone = p3.clone();

        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![p3_clone, p1_clone, p2_clone];

        let manager = EnhancedPluginManager::new(plugins);
        manager
            .run_before_tool_call(mock_tool(), json!({}), mock_ctx())
            .await
            .unwrap();

        // Verify execution order: high (0), medium (1), low (2)
        assert_eq!(p1.execution_order(), 0);
        assert_eq!(p2.execution_order(), 1);
        assert_eq!(p3.execution_order(), 2);
    }

    #[tokio::test]
    async fn test_close_calls_all_plugins() {
        let closed = Arc::new(AtomicUsize::new(0));

        struct CloseTrackingPlugin {
            name: String,
            closed: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl EnhancedPlugin for CloseTrackingPlugin {
            fn name(&self) -> &str {
                &self.name
            }

            async fn close(&self) {
                self.closed.fetch_add(1, Ordering::SeqCst);
            }
        }

        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(CloseTrackingPlugin { name: "p1".to_string(), closed: closed.clone() }),
            Arc::new(CloseTrackingPlugin { name: "p2".to_string(), closed: closed.clone() }),
            Arc::new(CloseTrackingPlugin { name: "p3".to_string(), closed: closed.clone() }),
        ];

        let manager = EnhancedPluginManager::new(plugins);
        manager.close().await;

        assert_eq!(closed.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_debug_impl() {
        let plugins: Vec<Arc<dyn EnhancedPlugin>> = vec![
            Arc::new(NoOpPlugin::new("alpha", 10)),
            Arc::new(NoOpPlugin::new("beta", 20)),
        ];

        let manager = EnhancedPluginManager::new(plugins);
        let debug_str = format!("{manager:?}");
        assert!(debug_str.contains("EnhancedPluginManager"));
        assert!(debug_str.contains("plugin_count: 2"));
    }
}
