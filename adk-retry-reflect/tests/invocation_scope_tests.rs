//! One plugin serves every invocation: retry budgets apply per invocation, and a success
//! clears a tool's counts, so one user's exhausted budget never silences another's
//! reflections.

use std::sync::Arc;

use adk_core::{CallbackContext, Content, ReadonlyContext, Result, Tool, ToolContext, async_trait};
use adk_plugin::{AfterToolCallResult, EnhancedPlugin, PluginContext};
use adk_retry_reflect::{RetryReflectPlugin, RetryReflectPluginBuilder};
use serde_json::{Value, json};

struct Inventory;

#[async_trait]
impl Tool for Inventory {
    fn name(&self) -> &str {
        "check_inventory"
    }
    fn description(&self) -> &str {
        "checks the inventory"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(Value::Null)
    }
}

struct Ctx {
    invocation: &'static str,
    content: Content,
}

#[async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        self.invocation
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
        &self.content
    }
}

#[async_trait]
impl CallbackContext for Ctx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

/// Runs the plugin on one result of `check_inventory` in `invocation`, and returns whether it
/// added a reflection.
async fn reflects(plugin: &RetryReflectPlugin, invocation: &'static str, result: Value) -> bool {
    let outcome = plugin
        .after_tool_call(
            Arc::new(Inventory),
            &json!({}),
            result,
            Arc::new(Ctx { invocation, content: Content::new("user") }),
            &PluginContext::new(),
        )
        .await
        .expect("plugin does not fail");
    match outcome {
        AfterToolCallResult::Continue(value) => value.get("reflection").is_some(),
    }
}

fn failure() -> Value {
    json!({"error": "inventory service returned 503"})
}

#[tokio::test]
async fn each_invocation_has_its_own_retry_budget() {
    let plugin = RetryReflectPluginBuilder::new().max_retries(1).build().expect("valid config");
    assert!(reflects(&plugin, "inv-a", failure()).await);
    assert!(!reflects(&plugin, "inv-a", failure()).await, "inv-a used its budget of 1");
    assert!(reflects(&plugin, "inv-b", failure()).await, "inv-b starts with a full budget");
}

#[tokio::test]
async fn the_global_limit_applies_per_invocation() {
    let plugin = RetryReflectPluginBuilder::new()
        .max_retries(5)
        .global_limit(1)
        .build()
        .expect("valid config");
    assert!(reflects(&plugin, "inv-a", failure()).await);
    assert!(!reflects(&plugin, "inv-a", failure()).await);
    assert!(reflects(&plugin, "inv-b", failure()).await);
}

#[tokio::test]
async fn a_success_clears_the_tools_counts() {
    let plugin = RetryReflectPluginBuilder::new().max_retries(1).build().expect("valid config");
    assert!(reflects(&plugin, "inv", failure()).await);
    assert!(!reflects(&plugin, "inv", json!({"in_stock": 4})).await);
    assert!(reflects(&plugin, "inv", failure()).await, "the failure after a success is reflected");
}

#[tokio::test]
async fn a_success_closes_the_global_circuit() {
    let plugin = RetryReflectPluginBuilder::new()
        .max_retries(10)
        .enable_global_tracking(2)
        .build()
        .expect("valid config");
    assert!(reflects(&plugin, "inv-1", failure()).await);
    assert!(reflects(&plugin, "inv-2", failure()).await);
    assert!(!reflects(&plugin, "inv-3", failure()).await, "two failures break the circuit");
    reflects(&plugin, "inv-4", json!({"in_stock": 4})).await;
    assert!(reflects(&plugin, "inv-5", failure()).await, "a success closes the circuit");
}
