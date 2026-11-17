//! Action-ledger bracketing for `LlmAgent` and `CodeActAgent`.
//!
//! Both agents record a [`ToolEffect::NonIdempotent`] call in
//! [`RunConfig::action_ledger`](adk_core::RunConfig::action_ledger) after the governed path
//! authorizes it and before it executes, and record its outcome after the effect-gated
//! retries end. The step itself is [`adk_core::begin_ledgered_call`], which the realtime
//! agents share.

use adk_core::{InvocationContext, LedgerAnswer, LedgeredCall, ToolEffect};
use serde_json::Value;

pub use adk_core::held_call_ledger_key;
pub(crate) use adk_core::interrupted_call_response;

/// [`adk_core::begin_ledgered_call`] under the invocation's run configuration.
///
/// # Errors
///
/// Returns `audit.record_failed`, which ends the run, when an outcome-unknown answer
/// cannot be audited under [`AuditFailureMode::Block`](adk_core::audit_log::AuditFailureMode::Block).
pub(crate) async fn begin_ledgered_call(
    ctx: &dyn InvocationContext,
    effect: ToolEffect,
    key: String,
    name: &str,
    args: &Value,
) -> adk_core::Result<Result<Option<LedgeredCall>, LedgerAnswer>> {
    adk_core::begin_ledgered_call(ctx.run_config(), ctx, effect, key, name, args).await
}

/// [`adk_core::record_outcome_unknown`] under the invocation's run configuration.
///
/// # Errors
///
/// Returns `audit.record_failed` when the record cannot be written under
/// [`AuditFailureMode::Block`](adk_core::audit_log::AuditFailureMode::Block).
pub(crate) async fn record_outcome_unknown(
    ctx: &dyn InvocationContext,
    name: &str,
    args: &Value,
    cause: &str,
) -> adk_core::Result<()> {
    adk_core::record_outcome_unknown(ctx.run_config(), ctx, name, args, cause).await
}
