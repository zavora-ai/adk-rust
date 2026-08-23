//! OpenAI agent with deterministic service-inspection tools in the runtime UI.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, ToolContext};
use adk_tool::FunctionTool;
use runtime_ui_showcase_example::{openai_model, serve};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct ServiceArgs {
    /// Service name to inspect.
    service: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let inspect = FunctionTool::new(
        "inspect_service",
        "Inspect deterministic health, latency, deployment, and error-rate data for a service.",
        |_ctx: Arc<dyn ToolContext>, args| async move {
            let args: ServiceArgs = serde_json::from_value(args)
                .map_err(|error| adk_core::AdkError::tool(error.to_string()))?;
            Ok(json!({
                "service": args.service,
                "status": "degraded",
                "region": "us-east-1",
                "p95LatencyMs": 842,
                "errorRatePercent": 3.7,
                "deployment": "2026.08.23-rc3"
            }))
        },
    )
    .with_parameters_schema::<ServiceArgs>()
    .with_read_only(true)
    .with_concurrency_safe(true);
    let error_budget = FunctionTool::new(
        "calculate_error_budget",
        "Calculate the remaining monthly error budget for a service SLO.",
        |_ctx: Arc<dyn ToolContext>, args| async move {
            let args: ServiceArgs = serde_json::from_value(args)
                .map_err(|error| adk_core::AdkError::tool(error.to_string()))?;
            Ok(json!({
                "service": args.service,
                "sloPercent": 99.9,
                "budgetMinutes": 43.2,
                "consumedMinutes": 31.8,
                "remainingMinutes": 11.4
            }))
        },
    )
    .with_parameters_schema::<ServiceArgs>()
    .with_read_only(true)
    .with_concurrency_safe(true);

    let agent = LlmAgentBuilder::new("service_operator")
        .description("Diagnoses service health with typed, deterministic tools")
        .instruction(
            "You are an SRE assistant. For every service diagnosis, call inspect_service and \
             calculate_error_budget. Then return a concise Markdown incident brief with a \
             heading, status table, evidence bullets, and three recommended actions.",
        )
        .model(openai_model()?)
        .tool(Arc::new(inspect))
        .tool(Arc::new(error_budget))
        .build()?;
    serve(Arc::new(agent) as Arc<dyn Agent>, "runtime-ui-tools").await
}
