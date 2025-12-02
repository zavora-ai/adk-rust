use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Default)]
pub struct ExitLoopTool;

impl ExitLoopTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ExitLoopTool {
    fn name(&self) -> &str {
        "exit_loop"
    }

    fn description(&self) -> &str {
        "Exits the loop.\nCall this function only when you are instructed to do so."
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let mut actions = ctx.actions();
        actions.escalate = true;
        actions.skip_summarization = true;
        ctx.set_actions(actions);
        Ok(json!({}))
    }
}
