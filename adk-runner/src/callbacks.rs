use adk_core::{CallbackContext, Content, Result};
use futures::future::BoxFuture;
use std::sync::Arc;

/// Callback executed before calling the model
pub type BeforeModelCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync,
>;

/// Callback executed after model response
pub type AfterModelCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync,
>;

/// Callback executed before tool execution
pub type BeforeToolCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync,
>;

/// Callback executed after tool execution
pub type AfterToolCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync,
>;

/// Collection of all callback types
pub struct Callbacks {
    pub before_model: Vec<BeforeModelCallback>,
    pub after_model: Vec<AfterModelCallback>,
    pub before_tool: Vec<BeforeToolCallback>,
    pub after_tool: Vec<AfterToolCallback>,
}

impl Default for Callbacks {
    fn default() -> Self {
        Self::new()
    }
}

impl Callbacks {
    pub fn new() -> Self {
        Self {
            before_model: Vec::new(),
            after_model: Vec::new(),
            before_tool: Vec::new(),
            after_tool: Vec::new(),
        }
    }

    pub fn add_before_model(&mut self, callback: BeforeModelCallback) {
        self.before_model.push(callback);
    }

    pub fn add_after_model(&mut self, callback: AfterModelCallback) {
        self.after_model.push(callback);
    }

    pub fn add_before_tool(&mut self, callback: BeforeToolCallback) {
        self.before_tool.push(callback);
    }

    pub fn add_after_tool(&mut self, callback: AfterToolCallback) {
        self.after_tool.push(callback);
    }

    /// Execute all before_model callbacks
    pub async fn execute_before_model(
        &self,
        ctx: Arc<dyn CallbackContext>,
    ) -> Result<Vec<Content>> {
        let mut results = Vec::new();
        for callback in &self.before_model {
            if let Some(content) = callback(ctx.clone()).await? {
                results.push(content);
            }
        }
        Ok(results)
    }

    /// Execute all after_model callbacks
    pub async fn execute_after_model(&self, ctx: Arc<dyn CallbackContext>) -> Result<Vec<Content>> {
        let mut results = Vec::new();
        for callback in &self.after_model {
            if let Some(content) = callback(ctx.clone()).await? {
                results.push(content);
            }
        }
        Ok(results)
    }

    /// Execute all before_tool callbacks
    pub async fn execute_before_tool(&self, ctx: Arc<dyn CallbackContext>) -> Result<Vec<Content>> {
        let mut results = Vec::new();
        for callback in &self.before_tool {
            if let Some(content) = callback(ctx.clone()).await? {
                results.push(content);
            }
        }
        Ok(results)
    }

    /// Execute all after_tool callbacks
    pub async fn execute_after_tool(&self, ctx: Arc<dyn CallbackContext>) -> Result<Vec<Content>> {
        let mut results = Vec::new();
        for callback in &self.after_tool {
            if let Some(content) = callback(ctx.clone()).await? {
                results.push(content);
            }
        }
        Ok(results)
    }
}
