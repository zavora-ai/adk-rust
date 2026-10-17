//! How each scenario configures tool-result screening and error exposure.
//!
//! Tool-result guardrails are [`ToolGuardrail`](adk_guardrail::ToolGuardrail)s in a
//! [`ToolGuardrailSet`]: `LlmAgent` takes the set through `tool_guardrails`, and the realtime
//! runners take it as their [`ToolResultScreen`].

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::ToolResultScreen;
use adk_guardrail::{PiiRedactor, ResultTextGuardrail, ToolGuardrailSet};
use adk_realtime::RealtimeAgentBuilder;
use adk_realtime::runner::RealtimeRunnerBuilder;
use async_trait::async_trait;
use serde_json::Value;

/// The event metadata key under which low-severity guardrail findings are recorded.
pub const WARNINGS_KEY: &str = adk_guardrail::GUARDRAIL_WARNINGS_KEY;

/// Redacts PII from every text value of every tool result.
fn pii_result_guardrails() -> ToolGuardrailSet {
    ToolGuardrailSet::new().with(ResultTextGuardrail::new(PiiRedactor::new()))
}

/// A screen whose redaction backend is down, so it cannot vouch for any result.
struct UnavailableScreen;

#[async_trait]
impl ToolResultScreen for UnavailableScreen {
    async fn screen_result(
        &self,
        _tool_name: &str,
        _args: &Value,
        _result: &Value,
    ) -> Result<Value, String> {
        Err("the redaction service is unavailable".to_string())
    }
}

fn screen(failing: bool) -> Arc<dyn ToolResultScreen> {
    if failing { Arc::new(UnavailableScreen) } else { Arc::new(pii_result_guardrails()) }
}

pub fn llm_redacts_results(builder: LlmAgentBuilder) -> LlmAgentBuilder {
    builder.tool_guardrails(pii_result_guardrails())
}

pub fn realtime_agent_screen(builder: RealtimeAgentBuilder, failing: bool) -> RealtimeAgentBuilder {
    builder.tool_result_screen(screen(failing))
}

pub fn realtime_agent_exposes_errors(builder: RealtimeAgentBuilder) -> RealtimeAgentBuilder {
    builder.expose_tool_error_messages(true)
}

pub fn realtime_runner_screen(
    builder: RealtimeRunnerBuilder,
    failing: bool,
) -> RealtimeRunnerBuilder {
    builder.tool_result_screen(screen(failing))
}
