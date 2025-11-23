use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type AsyncHandler = Box<dyn Fn(Arc<dyn ToolContext>, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>> + Send + Sync>;

pub struct FunctionTool {
    name: String,
    description: String,
    handler: AsyncHandler,
    long_running: bool,
}

impl FunctionTool {
    pub fn new<F, Fut>(name: impl Into<String>, description: impl Into<String>, handler: F) -> Self
    where
        F: Fn(Arc<dyn ToolContext>, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            handler: Box::new(move |ctx, args| Box::pin(handler(ctx, args))),
            long_running: false,
        }
    }

    pub fn with_long_running(mut self, long_running: bool) -> Self {
        self.long_running = long_running;
        self
    }
}

#[async_trait]
impl Tool for FunctionTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn is_long_running(&self) -> bool {
        self.long_running
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        (self.handler)(ctx, args).await
    }
}
