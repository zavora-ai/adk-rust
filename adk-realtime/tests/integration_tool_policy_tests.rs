//! A tool governed in the standard agent loop must be governed in realtime too.
//!
//! The builder wraps each ADK tool in a `ToolBridgeAdapter` and the live `next_event` path
//! called `RealtimeRunner::dispatch_tool_call`, which invokes that adapter directly. The
//! adapter creates a context and calls `Tool::execute` — no plugin pipeline, no callbacks, no
//! confirmation. The richer `execute_tool_with_plugins` existed but nothing on the live path
//! reached it, and its before-plugin error branch logged and then **executed the tool anyway**,
//! so a failing security plugin was fail-open.

#![cfg(feature = "integration")]

use adk_core::{Tool, ToolContext};
use adk_plugin::{BeforeToolCallResult, EnhancedPlugin, EnhancedPluginManager};
use adk_realtime::integration::IntegratedRealtimeRunnerBuilder;
use adk_realtime::{RealtimeConfig, RealtimeModel, RealtimeSession, Result as RtResult};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts executions so "the tool did not run" is measured, not inferred.
struct CountingTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "guarded"
    }
    fn description(&self) -> &str {
        "counts executions"
    }
    async fn execute(
        &self,
        _ctx: Arc<dyn ToolContext>,
        _args: serde_json::Value,
    ) -> adk_core::Result<serde_json::Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!({ "ran": true }))
    }
}

/// A plugin that denies by short-circuiting, as an authorization plugin would.
#[derive(Debug)]
struct DenyingPlugin;

#[async_trait]
impl EnhancedPlugin for DenyingPlugin {
    fn name(&self) -> &str {
        "denying"
    }

    async fn before_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: serde_json::Value,
        _ctx: Arc<dyn adk_core::CallbackContext>,
        _plugin_ctx: &adk_plugin::PluginContext,
    ) -> adk_core::Result<BeforeToolCallResult> {
        Ok(BeforeToolCallResult::ShortCircuit(serde_json::json!({ "error": "denied by policy" })))
    }
}

/// A plugin whose pipeline fails, standing in for a broken security plugin.
#[derive(Debug)]
struct FailingPlugin;

#[async_trait]
impl EnhancedPlugin for FailingPlugin {
    fn name(&self) -> &str {
        "failing"
    }

    async fn before_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: serde_json::Value,
        _ctx: Arc<dyn adk_core::CallbackContext>,
        _plugin_ctx: &adk_plugin::PluginContext,
    ) -> adk_core::Result<BeforeToolCallResult> {
        Err(adk_core::AdkError::tool("policy backend unreachable"))
    }
}

/// A model that never connects; these tests exercise dispatch, not transport.
struct MockModel;

#[async_trait]
impl RealtimeModel for MockModel {
    fn provider(&self) -> &str {
        "mock"
    }
    fn model_id(&self) -> &str {
        "mock"
    }
    fn supported_input_formats(&self) -> Vec<adk_realtime::AudioFormat> {
        vec![]
    }
    fn supported_output_formats(&self) -> Vec<adk_realtime::AudioFormat> {
        vec![]
    }
    fn available_voices(&self) -> Vec<&str> {
        vec![]
    }
    async fn connect(&self, _config: RealtimeConfig) -> RtResult<Box<dyn RealtimeSession>> {
        Err(adk_realtime::RealtimeError::connection("mock model does not connect"))
    }
}

/// Builds an integrated runner with one counting tool and the supplied plugin.
fn runner_with_plugin(
    plugin: Arc<dyn EnhancedPlugin>,
    executions: Arc<AtomicUsize>,
) -> adk_realtime::integration::IntegratedRealtimeRunner {
    let manager = EnhancedPluginManager::new(vec![plugin]);

    IntegratedRealtimeRunnerBuilder::new()
        .model(Arc::new(MockModel))
        .identity("test-app", "user-1", "session-1")
        .adk_tool(Arc::new(CountingTool { executions }) as Arc<dyn Tool>)
        .plugin_manager(Arc::new(manager))
        .build()
        .expect("builder should succeed")
}

#[tokio::test]
async fn a_denying_plugin_stops_the_tool_on_the_realtime_path() {
    let executions = Arc::new(AtomicUsize::new(0));
    let runner = runner_with_plugin(Arc::new(DenyingPlugin), Arc::clone(&executions));

    let call = adk_realtime::events::ToolCall {
        call_id: "call-1".to_string(),
        name: "guarded".to_string(),
        arguments: serde_json::json!({}),
    };
    let tool: Arc<dyn Tool> = Arc::new(CountingTool { executions: Arc::clone(&executions) });

    let result = runner.execute_tool_with_plugins_for_test(&tool, &call).await.expect("dispatch");

    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "an authorization plugin's denial must prevent execution: {result}"
    );
    assert_eq!(result["error"], "denied by policy");
}

#[tokio::test]
async fn a_failing_plugin_pipeline_refuses_the_tool_rather_than_running_it() {
    let executions = Arc::new(AtomicUsize::new(0));
    let runner = runner_with_plugin(Arc::new(FailingPlugin), Arc::clone(&executions));

    let call = adk_realtime::events::ToolCall {
        call_id: "call-1".to_string(),
        name: "guarded".to_string(),
        arguments: serde_json::json!({}),
    };
    let tool: Arc<dyn Tool> = Arc::new(CountingTool { executions: Arc::clone(&executions) });

    let result = runner.execute_tool_with_plugins_for_test(&tool, &call).await.expect("dispatch");

    // The old branch logged the error and executed the tool: a broken guard became no guard.
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a failed policy pipeline must fail closed: {result}"
    );
    let message = result["error"].as_str().unwrap_or_default();
    assert!(message.contains("refused"), "the refusal must be reported: {message}");
    assert!(
        message.contains("policy backend unreachable"),
        "the underlying failure must be reported so it can be fixed: {message}"
    );
}
