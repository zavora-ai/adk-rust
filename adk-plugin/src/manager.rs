//! Plugin Manager
//!
//! Coordinates execution of callbacks across all registered plugins.

use crate::Plugin;
use adk_core::{
    BeforeModelResult, CallbackContext, Content, Event, InvocationContext, LlmRequest, LlmResponse,
    Result, Tool,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Configuration for the PluginManager.
#[derive(Clone)]
pub struct PluginManagerConfig {
    /// Timeout for closing plugins during shutdown.
    pub close_timeout: Duration,
}

impl Default for PluginManagerConfig {
    fn default() -> Self {
        Self { close_timeout: Duration::from_secs(5) }
    }
}

/// Manages a collection of plugins and coordinates callback execution.
///
/// The PluginManager runs callbacks from all registered plugins in order.
/// For callbacks that can modify data (like on_user_message), the first
/// plugin to return a modification wins.
///
/// # Example
///
/// ```rust,ignore
/// use adk_plugin::{Plugin, PluginManager, PluginConfig};
///
/// let plugins = vec![
///     Plugin::new(PluginConfig {
///         name: "logging".to_string(),
///         on_event: Some(log_events()),
///         ..Default::default()
///     }),
///     Plugin::new(PluginConfig {
///         name: "metrics".to_string(),
///         before_run: Some(start_timer()),
///         after_run: Some(stop_timer()),
///         ..Default::default()
///     }),
/// ];
///
/// let manager = PluginManager::new(plugins);
/// ```
pub struct PluginManager {
    plugins: Vec<Plugin>,
    config: PluginManagerConfig,
}

impl PluginManager {
    /// Create a new plugin manager with the given plugins.
    pub fn new(plugins: Vec<Plugin>) -> Self {
        Self { plugins, config: PluginManagerConfig::default() }
    }

    /// Create a new plugin manager with custom configuration.
    pub fn with_config(plugins: Vec<Plugin>, config: PluginManagerConfig) -> Self {
        Self { plugins, config }
    }

    /// Get the number of registered plugins.
    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    /// Get plugin names.
    pub fn plugin_names(&self) -> Vec<&str> {
        self.plugins.iter().map(|p| p.name()).collect()
    }

    /// Run on_user_message callbacks from all plugins.
    ///
    /// Returns the modified content if any plugin modified it.
    pub async fn run_on_user_message(
        &self,
        ctx: Arc<dyn InvocationContext>,
        content: Content,
    ) -> Result<Option<Content>> {
        let mut current_content = content;
        let mut was_modified = false;

        for plugin in &self.plugins {
            if let Some(callback) = plugin.on_user_message() {
                debug!(plugin = plugin.name(), "Running on_user_message callback");
                match callback(ctx.clone(), current_content.clone()).await {
                    Ok(Some(modified)) => {
                        debug!(plugin = plugin.name(), "Content modified by plugin");
                        was_modified = true;
                        current_content = modified;
                    }
                    Ok(None) => {
                        // Continue with current content
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "on_user_message callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(if was_modified { Some(current_content) } else { None })
    }

    /// Run on_event callbacks from all plugins.
    ///
    /// Returns the modified event if any plugin modified it.
    pub async fn run_on_event(
        &self,
        ctx: Arc<dyn InvocationContext>,
        event: Event,
    ) -> Result<Option<Event>> {
        let mut current_event = event;
        let mut was_modified = false;

        for plugin in &self.plugins {
            if let Some(callback) = plugin.on_event() {
                debug!(plugin = plugin.name(), event_id = %current_event.id, "Running on_event callback");
                match callback(ctx.clone(), current_event.clone()).await {
                    Ok(Some(modified)) => {
                        debug!(plugin = plugin.name(), "Event modified by plugin");
                        was_modified = true;
                        current_event = modified;
                    }
                    Ok(None) => {
                        // Continue with current event
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "on_event callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(if was_modified { Some(current_event) } else { None })
    }

    /// Run before_run callbacks from all plugins.
    ///
    /// If any plugin returns content, the run should be skipped.
    pub async fn run_before_run(&self, ctx: Arc<dyn InvocationContext>) -> Result<Option<Content>> {
        for plugin in &self.plugins {
            if let Some(callback) = plugin.before_run() {
                debug!(plugin = plugin.name(), "Running before_run callback");
                match callback(ctx.clone()).await {
                    Ok(Some(content)) => {
                        debug!(plugin = plugin.name(), "before_run returned early exit content");
                        return Ok(Some(content));
                    }
                    Ok(None) => {
                        // Continue to next plugin
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "before_run callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Run after_run callbacks from all plugins.
    ///
    /// This does NOT emit events - it's for cleanup/metrics only.
    pub async fn run_after_run(&self, ctx: Arc<dyn InvocationContext>) {
        for plugin in &self.plugins {
            if let Some(callback) = plugin.after_run() {
                debug!(plugin = plugin.name(), "Running after_run callback");
                callback(ctx.clone()).await;
            }
        }
    }

    /// Run before_agent callbacks from all plugins.
    ///
    /// If any plugin returns content, the agent run should be skipped.
    pub async fn run_before_agent(&self, ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        for plugin in &self.plugins {
            if let Some(callback) = plugin.before_agent() {
                debug!(plugin = plugin.name(), "Running before_agent callback");
                match callback(ctx.clone()).await {
                    Ok(Some(content)) => {
                        debug!(plugin = plugin.name(), "before_agent returned early exit content");
                        return Ok(Some(content));
                    }
                    Ok(None) => {
                        // Continue to next plugin
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "before_agent callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Run after_agent callbacks from all plugins.
    pub async fn run_after_agent(&self, ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        for plugin in &self.plugins {
            if let Some(callback) = plugin.after_agent() {
                debug!(plugin = plugin.name(), "Running after_agent callback");
                match callback(ctx.clone()).await {
                    Ok(Some(content)) => {
                        debug!(plugin = plugin.name(), "after_agent returned content");
                        return Ok(Some(content));
                    }
                    Ok(None) => {
                        // Continue to next plugin
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "after_agent callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Run before_model callbacks from all plugins.
    ///
    /// Callbacks can modify the request or skip the model call.
    pub async fn run_before_model(
        &self,
        ctx: Arc<dyn CallbackContext>,
        request: LlmRequest,
    ) -> Result<BeforeModelResult> {
        let mut current_request = request;

        for plugin in &self.plugins {
            if let Some(callback) = plugin.before_model() {
                debug!(plugin = plugin.name(), "Running before_model callback");
                match callback(ctx.clone(), current_request.clone()).await {
                    Ok(BeforeModelResult::Continue(modified)) => {
                        current_request = modified;
                    }
                    Ok(BeforeModelResult::Skip(response)) => {
                        debug!(plugin = plugin.name(), "before_model skipped model call");
                        return Ok(BeforeModelResult::Skip(response));
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "before_model callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(BeforeModelResult::Continue(current_request))
    }

    /// Run after_model callbacks from all plugins.
    pub async fn run_after_model(
        &self,
        ctx: Arc<dyn CallbackContext>,
        response: LlmResponse,
    ) -> Result<Option<LlmResponse>> {
        let mut current_response = response;
        let mut was_modified = false;

        for plugin in &self.plugins {
            if let Some(callback) = plugin.after_model() {
                debug!(plugin = plugin.name(), "Running after_model callback");
                match callback(ctx.clone(), current_response.clone()).await {
                    Ok(Some(modified)) => {
                        was_modified = true;
                        current_response = modified;
                    }
                    Ok(None) => {
                        // Continue with current response
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "after_model callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(if was_modified { Some(current_response) } else { None })
    }

    /// Run on_model_error callbacks from all plugins.
    pub async fn run_on_model_error(
        &self,
        ctx: Arc<dyn CallbackContext>,
        request: LlmRequest,
        error: String,
    ) -> Result<Option<LlmResponse>> {
        for plugin in &self.plugins {
            if let Some(callback) = plugin.on_model_error() {
                debug!(plugin = plugin.name(), "Running on_model_error callback");
                match callback(ctx.clone(), request.clone(), error.clone()).await {
                    Ok(Some(response)) => {
                        debug!(plugin = plugin.name(), "on_model_error provided fallback response");
                        return Ok(Some(response));
                    }
                    Ok(None) => {
                        // Continue to next plugin
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "on_model_error callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Run before_tool callbacks from all plugins.
    pub async fn run_before_tool(&self, ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        for plugin in &self.plugins {
            if let Some(callback) = plugin.before_tool() {
                debug!(plugin = plugin.name(), "Running before_tool callback");
                match callback(ctx.clone()).await {
                    Ok(Some(content)) => {
                        debug!(plugin = plugin.name(), "before_tool returned early exit content");
                        return Ok(Some(content));
                    }
                    Ok(None) => {
                        // Continue to next plugin
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "before_tool callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Run after_tool callbacks from all plugins.
    pub async fn run_after_tool(&self, ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        for plugin in &self.plugins {
            if let Some(callback) = plugin.after_tool() {
                debug!(plugin = plugin.name(), "Running after_tool callback");
                match callback(ctx.clone()).await {
                    Ok(Some(content)) => {
                        debug!(plugin = plugin.name(), "after_tool returned content");
                        return Ok(Some(content));
                    }
                    Ok(None) => {
                        // Continue to next plugin
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "after_tool callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Run on_tool_error callbacks from all plugins.
    pub async fn run_on_tool_error(
        &self,
        ctx: Arc<dyn CallbackContext>,
        tool: Arc<dyn Tool>,
        args: serde_json::Value,
        error: String,
    ) -> Result<Option<serde_json::Value>> {
        for plugin in &self.plugins {
            if let Some(callback) = plugin.on_tool_error() {
                debug!(
                    plugin = plugin.name(),
                    tool = tool.name(),
                    "Running on_tool_error callback"
                );
                match callback(ctx.clone(), tool.clone(), args.clone(), error.clone()).await {
                    Ok(Some(result)) => {
                        debug!(plugin = plugin.name(), "on_tool_error provided fallback result");
                        return Ok(Some(result));
                    }
                    Ok(None) => {
                        // Continue to next plugin
                    }
                    Err(e) => {
                        warn!(plugin = plugin.name(), error = %e, "on_tool_error callback failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Close all plugins with timeout.
    pub async fn close(&self) {
        debug!("Closing {} plugins", self.plugins.len());

        for plugin in &self.plugins {
            let close_future = plugin.close();
            match tokio::time::timeout(self.config.close_timeout, close_future).await {
                Ok(()) => {
                    debug!(plugin = plugin.name(), "Plugin closed successfully");
                }
                Err(_) => {
                    warn!(plugin = plugin.name(), "Plugin close timed out");
                }
            }
        }
    }
}

impl std::fmt::Debug for PluginManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginManager")
            .field("plugin_count", &self.plugins.len())
            .field("plugin_names", &self.plugin_names())
            .field("close_timeout", &self.config.close_timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginConfig;

    #[test]
    fn test_plugin_manager_creation() {
        let plugins = vec![
            Plugin::new(PluginConfig { name: "test1".to_string(), ..Default::default() }),
            Plugin::new(PluginConfig { name: "test2".to_string(), ..Default::default() }),
        ];

        let manager = PluginManager::new(plugins);
        assert_eq!(manager.plugin_count(), 2);
        assert_eq!(manager.plugin_names(), vec!["test1", "test2"]);
    }
}
