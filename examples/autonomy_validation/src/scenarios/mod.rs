//! One module per Phase 0, Phase 1, and Phase 2 area; each scenario returns a
//! [`Verdict`](crate::common::Verdict).

pub mod anthropic;
pub mod ap2;
pub mod approvals;
pub mod audit;
pub mod budget;
pub mod builtin;
pub mod codeact;
pub mod commerce;
pub mod durable;
pub mod eval;
pub mod governance;
pub mod graph;
pub mod identity;
pub mod mcp;
pub mod payments;
pub mod policy;
pub mod providers;
pub mod state;
pub mod transfer;
pub mod wakeup;

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
    "payment_timeout_once",
    "codeact_payment_once",
    "team_budget_cap",
    "default_deny_policy",
    "approval_across_runs",
    "kill_switch",
    "spend_ledger_daily_cap",
    "ap2_payment_governed",
    "openai_tools_routing",
    "anthropic_thinking_replay",
    "builtin_tool_policy",
    "durable_crash_resume",
    "approval_service",
    "audit_trail",
    "agent_identity",
    "webhook_wakeup",
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
        "payment_timeout_once" => payments::payment_timeout_once(cfg, provider).await,
        "codeact_payment_once" => codeact::codeact_payment_once(cfg, provider).await,
        "team_budget_cap" => budget::team_budget_cap(cfg, provider).await,
        "default_deny_policy" => policy::default_deny_policy(cfg, provider).await,
        "approval_across_runs" => policy::approval_across_runs(cfg, provider).await,
        "kill_switch" => policy::kill_switch(cfg, provider).await,
        "spend_ledger_daily_cap" => budget::spend_ledger_daily_cap(cfg, provider).await,
        "ap2_payment_governed" => ap2::ap2_payment_governed(cfg, provider).await,
        "openai_tools_routing" => providers::openai_tools_routing(cfg, provider).await,
        "anthropic_thinking_replay" => providers::anthropic_thinking_replay(cfg, provider).await,
        "builtin_tool_policy" => builtin::builtin_tool_policy(cfg, provider).await,
        "durable_crash_resume" => durable::durable_crash_resume(cfg, provider).await,
        "approval_service" => approvals::approval_service(cfg, provider).await,
        "audit_trail" => audit::audit_trail(cfg, provider).await,
        "agent_identity" => identity::agent_identity(cfg, provider).await,
        "webhook_wakeup" => wakeup::webhook_wakeup(cfg, provider).await,
        other => Verdict::Fail(format!("unknown scenario {other}")),
    }
}
