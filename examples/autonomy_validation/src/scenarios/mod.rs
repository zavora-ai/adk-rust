//! One module per Phase 0 area; each scenario returns a [`Verdict`](crate::common::Verdict).

pub mod anthropic;
pub mod eval;
pub mod governance;
pub mod graph;
pub mod mcp;
pub mod state;
pub mod transfer;

use crate::common::{Config, Provider, Verdict};
use governance::DenialForm;

/// Every scenario, in run order.
pub const ALL: &[&str] = &[
    "transfer_roundtrip",
    "runner_plugin_denies_tool",
    "runner_plugin_denies_tool_err",
    "path_guardrail_fail_closed",
    "failing_toolset_skipped",
    "shared_state_fresh",
    "graph_resume_once",
    "eval_judge_fail_closed",
    "anthropic_web_tools",
    "anthropic_cache_cost",
    "mcp_concurrency",
];

/// Runs the named scenario once.
pub async fn run(name: &str, cfg: &Config, provider: Provider) -> Verdict {
    match name {
        "transfer_roundtrip" => transfer::run(cfg, provider).await,
        "runner_plugin_denies_tool" => {
            governance::runner_plugin_denies_tool(cfg, provider, DenialForm::Content).await
        }
        "runner_plugin_denies_tool_err" => {
            governance::runner_plugin_denies_tool(cfg, provider, DenialForm::Error).await
        }
        "path_guardrail_fail_closed" => governance::path_guardrail_fail_closed(cfg, provider).await,
        "failing_toolset_skipped" => governance::failing_toolset_skipped(cfg, provider).await,
        "shared_state_fresh" => state::run(cfg, provider).await,
        "graph_resume_once" => graph::run(cfg, provider).await,
        "eval_judge_fail_closed" => eval::run(cfg, provider).await,
        "anthropic_web_tools" => anthropic::web_tools(cfg, provider).await,
        "anthropic_cache_cost" => anthropic::cache_cost(cfg, provider).await,
        "mcp_concurrency" => mcp::run(cfg, provider).await,
        other => Verdict::Fail(format!("unknown scenario {other}")),
    }
}
