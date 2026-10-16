//! The plugin never suggests repeating a refused call or a timed-out non-idempotent call.
//!
//! `LlmAgent` sends the model a tool error's category and code but not its message, so the
//! plugin classifies on the message the tool outcome keeps.

use std::sync::Arc;

use adk_core::{
    CallbackContext, Content, ReadonlyContext, Result, Tool, ToolContext, ToolEffect, ToolOutcome,
    async_trait,
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

struct Ctx(Content, Option<ToolOutcome>);

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

    fn tool_outcome(&self) -> Option<ToolOutcome> {
        self.1.clone()
    }
}

async fn after_tool(effect: ToolEffect, result: Value) -> Value {
    after_failed_tool(effect, result, None).await
}

/// Runs the plugin on `result` with a failed tool outcome whose full message is `message`.
async fn after_failed_tool(effect: ToolEffect, result: Value, message: Option<&str>) -> Value {
    let plugin = RetryReflectPluginBuilder::new().build().expect("valid config");
    let tool_outcome = message.map(|message| ToolOutcome {
        tool_name: "charge_card".to_string(),
        tool_args: json!({"amount": 50}),
        success: false,
        duration: std::time::Duration::from_millis(5),
        error_message: Some(message.to_string()),
        error: None,
        attempt: 0,
    });
    let outcome = plugin
        .after_tool_call(
            Arc::new(EffectTool(effect)),
            &json!({"amount": 50}),
            result,
            Arc::new(Ctx(Content::new("user"), tool_outcome)),
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

#[tokio::test]
async fn a_redacted_denial_is_classified_on_the_outcome_message() {
    let result = json!({"error": "tool 'charge_card' failed: tool.internal (tool.internal)"});
    let message = "tool.internal: missing required scopes: [payments:write] (tool requires: [payments:write])";
    assert_eq!(
        after_failed_tool(ToolEffect::ReadOnly, result.clone(), Some(message)).await,
        result
    );
}

#[tokio::test]
async fn a_redacted_non_idempotent_timeout_is_classified_on_the_outcome_message() {
    let result = json!({"error": "tool 'charge_card' failed: tool.internal (tool.gateway.failed)"});
    let message = "tool.internal: gateway request timed out after 30s";
    assert_eq!(
        after_failed_tool(ToolEffect::NonIdempotent, result.clone(), Some(message)).await,
        result
    );
}

#[tokio::test]
async fn the_reflection_prompt_quotes_the_response_not_the_outcome_message() {
    let result =
        json!({"error": "tool 'charge_card' failed: tool.unavailable (tool.gateway.down)"});
    let message = "tool.unavailable: connect postgres://admin:hunter2@db.internal/prod refused";
    let reflected = after_failed_tool(ToolEffect::Idempotent, result, Some(message)).await;
    let prompt = reflected["reflection"].as_str().expect("a retry is suggested");
    assert!(prompt.contains("tool.gateway.down"), "{prompt}");
    assert!(!prompt.contains("hunter2"), "{prompt}");
}
