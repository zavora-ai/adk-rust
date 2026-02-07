//! Plugin definition
//!
//! A Plugin bundles related callbacks together for a specific purpose.

use crate::callbacks::*;
use adk_core::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, BeforeAgentCallback,
    BeforeModelCallback, BeforeToolCallback,
};
use std::future::Future;
use std::pin::Pin;

/// Configuration for creating a Plugin.
///
/// All callbacks are optional - only set the ones you need.
///
/// # Example
///
/// ```rust,ignore
/// let config = PluginConfig {
///     name: "my-plugin".to_string(),
///     on_user_message: Some(Box::new(|ctx, content| {
///         Box::pin(async move {
///             // Process user message
///             Ok(None)
///         })
///     })),
///     ..Default::default()
/// };
/// ```
pub struct PluginConfig {
    /// Unique name for this plugin
    pub name: String,

    // Run lifecycle callbacks
    /// Called when a user message is received (can modify)
    pub on_user_message: Option<OnUserMessageCallback>,
    /// Called for each event (can modify)
    pub on_event: Option<OnEventCallback>,
    /// Called before the run starts (can skip run)
    pub before_run: Option<BeforeRunCallback>,
    /// Called after the run completes (cleanup)
    pub after_run: Option<AfterRunCallback>,

    // Agent callbacks
    /// Called before agent execution
    pub before_agent: Option<BeforeAgentCallback>,
    /// Called after agent execution
    pub after_agent: Option<AfterAgentCallback>,

    // Model callbacks
    /// Called before LLM call (can modify request or skip)
    pub before_model: Option<BeforeModelCallback>,
    /// Called after LLM call (can modify response)
    pub after_model: Option<AfterModelCallback>,
    /// Called when LLM returns an error
    pub on_model_error: Option<OnModelErrorCallback>,

    // Tool callbacks
    /// Called before tool execution
    pub before_tool: Option<BeforeToolCallback>,
    /// Called after tool execution
    pub after_tool: Option<AfterToolCallback>,
    /// Called when tool returns an error
    pub on_tool_error: Option<OnToolErrorCallback>,

    /// Cleanup function called when plugin is closed
    pub close_fn: Option<Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            name: "unnamed".to_string(),
            on_user_message: None,
            on_event: None,
            before_run: None,
            after_run: None,
            before_agent: None,
            after_agent: None,
            before_model: None,
            after_model: None,
            on_model_error: None,
            before_tool: None,
            after_tool: None,
            on_tool_error: None,
            close_fn: None,
        }
    }
}

/// A Plugin bundles related callbacks for extending agent behavior.
///
/// Plugins are registered with a PluginManager which coordinates
/// callback execution across all registered plugins.
///
/// # Example
///
/// ```rust,ignore
/// use adk_plugin::{Plugin, PluginConfig};
///
/// // Create a caching plugin
/// let cache_plugin = Plugin::new(PluginConfig {
///     name: "cache".to_string(),
///     before_model: Some(Box::new(|ctx, request| {
///         Box::pin(async move {
///             // Check cache for this request
///             if let Some(cached) = check_cache(&request).await {
///                 return Ok(BeforeModelResult::Skip(cached));
///             }
///             Ok(BeforeModelResult::Continue(request))
///         })
///     })),
///     after_model: Some(Box::new(|ctx, response| {
///         Box::pin(async move {
///             // Store response in cache
///             store_in_cache(&response).await;
///             Ok(None)
///         })
///     })),
///     ..Default::default()
/// });
/// ```
pub struct Plugin {
    config: PluginConfig,
}

impl Plugin {
    /// Create a new plugin from configuration.
    pub fn new(config: PluginConfig) -> Self {
        Self { config }
    }

    /// Get the plugin name.
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Get the on_user_message callback if set.
    pub fn on_user_message(&self) -> Option<&OnUserMessageCallback> {
        self.config.on_user_message.as_ref()
    }

    /// Get the on_event callback if set.
    pub fn on_event(&self) -> Option<&OnEventCallback> {
        self.config.on_event.as_ref()
    }

    /// Get the before_run callback if set.
    pub fn before_run(&self) -> Option<&BeforeRunCallback> {
        self.config.before_run.as_ref()
    }

    /// Get the after_run callback if set.
    pub fn after_run(&self) -> Option<&AfterRunCallback> {
        self.config.after_run.as_ref()
    }

    /// Get the before_agent callback if set.
    pub fn before_agent(&self) -> Option<&BeforeAgentCallback> {
        self.config.before_agent.as_ref()
    }

    /// Get the after_agent callback if set.
    pub fn after_agent(&self) -> Option<&AfterAgentCallback> {
        self.config.after_agent.as_ref()
    }

    /// Get the before_model callback if set.
    pub fn before_model(&self) -> Option<&BeforeModelCallback> {
        self.config.before_model.as_ref()
    }

    /// Get the after_model callback if set.
    pub fn after_model(&self) -> Option<&AfterModelCallback> {
        self.config.after_model.as_ref()
    }

    /// Get the on_model_error callback if set.
    pub fn on_model_error(&self) -> Option<&OnModelErrorCallback> {
        self.config.on_model_error.as_ref()
    }

    /// Get the before_tool callback if set.
    pub fn before_tool(&self) -> Option<&BeforeToolCallback> {
        self.config.before_tool.as_ref()
    }

    /// Get the after_tool callback if set.
    pub fn after_tool(&self) -> Option<&AfterToolCallback> {
        self.config.after_tool.as_ref()
    }

    /// Get the on_tool_error callback if set.
    pub fn on_tool_error(&self) -> Option<&OnToolErrorCallback> {
        self.config.on_tool_error.as_ref()
    }

    /// Close the plugin, running cleanup if configured.
    pub async fn close(&self) {
        if let Some(ref close_fn) = self.config.close_fn {
            close_fn().await;
        }
    }
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("name", &self.config.name)
            .field("has_on_user_message", &self.config.on_user_message.is_some())
            .field("has_on_event", &self.config.on_event.is_some())
            .field("has_before_run", &self.config.before_run.is_some())
            .field("has_after_run", &self.config.after_run.is_some())
            .field("has_before_agent", &self.config.before_agent.is_some())
            .field("has_after_agent", &self.config.after_agent.is_some())
            .field("has_before_model", &self.config.before_model.is_some())
            .field("has_after_model", &self.config.after_model.is_some())
            .field("has_before_tool", &self.config.before_tool.is_some())
            .field("has_after_tool", &self.config.after_tool.is_some())
            .finish()
    }
}

/// Builder for creating plugins with a fluent API.
///
/// # Example
///
/// ```rust,ignore
/// let plugin = PluginBuilder::new("my-plugin")
///     .on_user_message(|ctx, content| Box::pin(async move { Ok(None) }))
///     .on_event(|ctx, event| Box::pin(async move { Ok(None) }))
///     .build();
/// ```
pub struct PluginBuilder {
    config: PluginConfig,
}

impl PluginBuilder {
    /// Create a new plugin builder with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self { config: PluginConfig { name: name.into(), ..Default::default() } }
    }

    /// Set the on_user_message callback.
    pub fn on_user_message(mut self, callback: OnUserMessageCallback) -> Self {
        self.config.on_user_message = Some(callback);
        self
    }

    /// Set the on_event callback.
    pub fn on_event(mut self, callback: OnEventCallback) -> Self {
        self.config.on_event = Some(callback);
        self
    }

    /// Set the before_run callback.
    pub fn before_run(mut self, callback: BeforeRunCallback) -> Self {
        self.config.before_run = Some(callback);
        self
    }

    /// Set the after_run callback.
    pub fn after_run(mut self, callback: AfterRunCallback) -> Self {
        self.config.after_run = Some(callback);
        self
    }

    /// Set the before_agent callback.
    pub fn before_agent(mut self, callback: BeforeAgentCallback) -> Self {
        self.config.before_agent = Some(callback);
        self
    }

    /// Set the after_agent callback.
    pub fn after_agent(mut self, callback: AfterAgentCallback) -> Self {
        self.config.after_agent = Some(callback);
        self
    }

    /// Set the before_model callback.
    pub fn before_model(mut self, callback: BeforeModelCallback) -> Self {
        self.config.before_model = Some(callback);
        self
    }

    /// Set the after_model callback.
    pub fn after_model(mut self, callback: AfterModelCallback) -> Self {
        self.config.after_model = Some(callback);
        self
    }

    /// Set the on_model_error callback.
    pub fn on_model_error(mut self, callback: OnModelErrorCallback) -> Self {
        self.config.on_model_error = Some(callback);
        self
    }

    /// Set the before_tool callback.
    pub fn before_tool(mut self, callback: BeforeToolCallback) -> Self {
        self.config.before_tool = Some(callback);
        self
    }

    /// Set the after_tool callback.
    pub fn after_tool(mut self, callback: AfterToolCallback) -> Self {
        self.config.after_tool = Some(callback);
        self
    }

    /// Set the on_tool_error callback.
    pub fn on_tool_error(mut self, callback: OnToolErrorCallback) -> Self {
        self.config.on_tool_error = Some(callback);
        self
    }

    /// Set the close function.
    pub fn close_fn(
        mut self,
        f: impl Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    ) -> Self {
        self.config.close_fn = Some(Box::new(f));
        self
    }

    /// Build the plugin.
    pub fn build(self) -> Plugin {
        Plugin::new(self.config)
    }
}
