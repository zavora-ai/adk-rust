//! Action-ledger bracketing shared by `LlmAgent` and `CodeActAgent`.
//!
//! Both agents record a [`ToolEffect::NonIdempotent`] call in
//! [`RunConfig::action_ledger`] after the governed path authorizes it and before
//! it executes, and record its outcome after the effect-gated retries end.

use std::sync::Arc;

use adk_core::{ActionLedger, ActionOutcome, ActionRecord, RunConfig, ToolEffect};
use serde_json::Value;

/// A call begun in the action ledger, completed once the tool finishes.
pub(crate) struct LedgeredCall {
    ledger: Arc<dyn ActionLedger>,
    key: String,
}

/// Consults the action ledger before a call executes.
///
/// Returns `Ok(None)` when the call is not ledgered (no ledger configured, or an
/// effect other than [`ToolEffect::NonIdempotent`]), and `Ok(Some(_))` once the
/// call has been begun under `key`. Returns `Err(response)` when the call must not
/// execute: the ledger already holds a record for `key`, or cannot be read or
/// written. `response` is the function response that answers the call.
pub(crate) async fn begin_ledgered_call(
    run_config: &RunConfig,
    effect: ToolEffect,
    key: String,
    name: &str,
    args: &Value,
) -> Result<Option<LedgeredCall>, Value> {
    let ledger = match &run_config.action_ledger {
        Some(ledger) if effect == ToolEffect::NonIdempotent => ledger.clone(),
        _ => return Ok(None),
    };
    let answer = match ledger.get(&key).await {
        Ok(Some(record)) => match record.outcome {
            Some(ActionOutcome::Succeeded { result_digest }) => serde_json::json!({
                "status": "already_succeeded",
                "result_digest": result_digest,
                "detail": format!("this call to '{name}' already completed; it was not executed again"),
            }),
            Some(ActionOutcome::Failed { error }) => serde_json::json!({
                "status": "already_failed",
                "error": error,
                "detail": format!("this call to '{name}' already failed; it was not executed again"),
            }),
            _ => adk_core::outcome_unknown_response(format!(
                "an earlier attempt of this call to '{name}' began but recorded no outcome, so it may have taken effect; it was not executed again"
            )),
        },
        Ok(None) => match ledger.begin(&ActionRecord::new(key.clone(), name, args, effect)).await {
            Ok(()) => return Ok(Some(LedgeredCall { ledger, key })),
            Err(error) => serde_json::json!({
                "error": format!("tool '{name}' was not executed: the action ledger could not record the call: {error}")
            }),
        },
        Err(error) => serde_json::json!({
            "error": format!("tool '{name}' was not executed: the action ledger could not be read: {error}")
        }),
    };
    tracing::warn!(
        tool.name = %name,
        tool.idempotency_key = %key,
        response = %answer,
        "non-idempotent call answered by the action ledger without executing"
    );
    Err(answer)
}

impl LedgeredCall {
    /// Records the outcome of a call that ran to an end: `result` when the tool
    /// returned a value, otherwise `error`.
    ///
    /// A call that ended mid-flight (timeout, panic) must not be completed, so
    /// its begun record answers a replay with an unknown outcome.
    pub(crate) async fn complete(self, name: &str, result: Option<&Value>, error: &str) {
        let outcome = match result {
            Some(value) => ActionOutcome::Succeeded { result_digest: adk_core::json_digest(value) },
            None => ActionOutcome::Failed { error: error.to_string() },
        };
        if let Err(error) = self.ledger.complete(&self.key, outcome).await {
            tracing::error!(
                tool.name = %name,
                tool.idempotency_key = %self.key,
                error = %error,
                "action ledger could not record the outcome; a replay will report it as unknown"
            );
        }
    }
}

/// The function response for a non-idempotent call that ended mid-flight with
/// `error`.
pub(crate) fn interrupted_call_response(error: &str) -> Value {
    adk_core::outcome_unknown_response(format!(
        "{error}. The call may have taken effect; check its result before calling it again."
    ))
}
