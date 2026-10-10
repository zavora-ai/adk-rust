//! The plugin never suggests repeating a refused call or a timed-out non-idempotent call.

use std::sync::Arc;

use adk_core::{
    CallbackContext, Content, ReadonlyContext, Result, Tool, ToolContext, ToolEffect, async_trait,
};
use adk_plugin::{AfterToolCallResult, EnhancedPlugin, PluginContext};
use adk_retry_reflect::RetryReflectPluginBuilder;
use serde_json::{Value, json};

struct EffectTool(ToolEffect);

#[async_trait]
impl Tool for EffectTool {
    fn name(&self) -> &str {
        "charge_card"
    }
    fn description(&self) -> &str {
        "charges a card"
    }
    fn effect(&self) -> ToolEffect {
        self.0
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(Value::Null)
    }
}

struct Ctx(Content);

#[async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        "inv"
    }
    fn agent_name(&self) -> &str {
        "agent"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn session_id(&self) -> &str {
        "session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.0
    }
}

#[async_trait]
impl CallbackContext for Ctx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

async fn after_tool(effect: ToolEffect, result: Value) -> Value {
    let plugin = RetryReflectPluginBuilder::new().build().expect("valid config");
    let outcome = plugin
        .after_tool_call(
            Arc::new(EffectTool(effect)),
            &json!({"amount": 50}),
            result,
            Arc::new(Ctx(Content::new("user"))),
            &PluginContext::new(),
        )
        .await
        .expect("plugin does not fail");
    match outcome {
        AfterToolCallResult::Continue(value) => value,
    }
}

#[tokio::test]
async fn a_timed_out_non_idempotent_call_is_not_suggested_for_retry() {
    let result = json!({"error": "Tool 'charge_card' timed out after 300 seconds"});
    assert_eq!(after_tool(ToolEffect::NonIdempotent, result.clone()).await, result);
}

#[tokio::test]
async fn a_timed_out_idempotent_call_is_still_suggested_for_retry() {
    let result = json!({"error": "Tool 'charge_card' timed out after 300 seconds"});
    let reflected = after_tool(ToolEffect::Idempotent, result).await;
    assert!(reflected.get("reflection").is_some(), "{reflected}");
}

#[tokio::test]
async fn an_authorization_denial_is_never_suggested_for_retry() {
    for message in [
        "tool.internal: missing required scopes: payments:checkout:complete",
        "Tool 'charge_card' execution denied by confirmation policy",
        "Access denied: user 'bob' cannot access tool 'charge_card'",
    ] {
        let result = json!({ "error": message });
        assert_eq!(after_tool(ToolEffect::ReadOnly, result.clone()).await, result, "{message}");
    }
}
