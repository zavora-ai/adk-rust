//! Action-ledger bracketing shared by `LlmAgent` and `CodeActAgent`.
//!
//! Both agents record a [`ToolEffect::NonIdempotent`] call in
//! [`RunConfig::action_ledger`] after the governed path authorizes it and before
//! it executes, and record its outcome after the effect-gated retries end.

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, AdkError, InvocationContext, ToolEffect,
    ToolResultStatus,
};
use serde_json::Value;

/// The action-ledger key of a held tool call that a later run resumes.
///
/// A call's ledger key is normally its
/// [`ToolContext::idempotency_key`](adk_core::ToolContext::idempotency_key), which names
/// the invocation that runs it. A held call resumes in a later run, under that run's
/// invocation, so `LlmAgent` keys a resumed call by its original identity instead: the
/// session, the call ID the call was issued under, and a digest of the
/// [`ToolConfirmationRequest::fingerprint`](adk_core::ToolConfirmationRequest::fingerprint)
/// it was held with. Every resumption of the same held call shares one record, so a call
/// that ran in a run whose result was never saved is answered from its record instead of
/// running again (spec 3.1 §7.12, §9.2 I37).
///
/// # Example
///
/// ```rust
/// use adk_agent::held_call_ledger_key;
/// use adk_core::ToolConfirmationRequest;
/// use serde_json::json;
///
/// let request = ToolConfirmationRequest::new("transfer", json!({ "amount": 40 }))
///     .with_function_call_id("call-1");
/// let key = held_call_ledger_key("app", "alice", "s1", "call-1", &request.fingerprint());
/// assert!(key.starts_with("app/alice/s1/held/call-1/"));
/// assert_eq!(key, held_call_ledger_key("app", "alice", "s1", "call-1", &request.fingerprint()));
/// ```
pub fn held_call_ledger_key(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    function_call_id: &str,
    fingerprint: &str,
) -> String {
    let digest = adk_core::json_digest(&Value::String(fingerprint.to_string()));
    format!("{app_name}/{user_id}/{session_id}/held/{function_call_id}/{digest}")
}

/// A call begun in the action ledger, completed once the tool finishes.
pub(crate) struct LedgeredCall {
    ledger: Arc<dyn ActionLedger>,
    key: String,
}

/// The ledger's answer to a call that must not execute.
pub(crate) struct LedgerAnswer {
    /// The function response that answers the call.
    pub(crate) response: Value,
    /// What the answer reports: the recorded outcome, or `NotRun` when the ledger failed.
    pub(crate) status: ToolResultStatus,
    /// The ledger's error, when it could not be read or written.
    pub(crate) cause: Option<AdkError>,
}

/// Consults the action ledger before a call executes.
///
/// Returns `Ok(Ok(None))` when the call is not ledgered (no ledger configured, or an
/// effect other than [`ToolEffect::NonIdempotent`]), and `Ok(Ok(Some(_)))` once the
/// call has been begun under `key`. Returns `Ok(Err(answer))` when the call must
/// not execute: the ledger already holds a record for `key`, or cannot be read or
/// written.
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
    let ledger = match &ctx.run_config().action_ledger {
        Some(ledger) if effect == ToolEffect::NonIdempotent => ledger.clone(),
        _ => return Ok(Ok(None)),
    };
    let mut status = ToolResultStatus::NotRun;
    let mut cause = None;
    let response = match ledger.get(&key).await {
        Ok(Some(record)) => {
            status = ToolResultStatus::of_record(record.outcome.as_ref());
            match record.outcome {
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
                _ => {
                    record_outcome_unknown(ctx, name, args, "began_without_outcome").await?;
                    adk_core::outcome_unknown_response(format!(
                        "an earlier attempt of this call to '{name}' began but recorded no outcome, so it may have taken effect; it was not executed again"
                    ))
                }
            }
        }
        // A store error can name its connection, so the model is given its code only.
        Ok(None) => match ledger.begin(&ActionRecord::new(key.clone(), name, args, effect)).await {
            Ok(()) => return Ok(Ok(Some(LedgeredCall { ledger, key }))),
            Err(error) => {
                tracing::error!(tool.name = %name, error = %error, "action ledger could not record the call");
                let response = serde_json::json!({
                    "error": format!("tool '{name}' was not executed: the action ledger could not record the call: {}", error.redacted())
                });
                cause = Some(error);
                response
            }
        },
        Err(error) => {
            tracing::error!(tool.name = %name, error = %error, "action ledger could not be read");
            let response = serde_json::json!({
                "error": format!("tool '{name}' was not executed: the action ledger could not be read: {}", error.redacted())
            });
            cause = Some(error);
            response
        }
    };
    tracing::warn!(
        tool.name = %name,
        tool.idempotency_key = %key,
        response = %response,
        "non-idempotent call answered by the action ledger without executing"
    );
    Ok(Err(LedgerAnswer { response, status, cause }))
}

/// Records, through the run's [`RunConfig::audit`](adk_core::RunConfig::audit), a call
/// answered with an unknown outcome. `cause` is `interrupted` for a call that ended
/// mid-flight and `began_without_outcome` for a replay of one.
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
    let Some(trail) = &ctx.run_config().audit else {
        return Ok(());
    };
    let draft = AuditRecordDraft::new(AuditRecordKind::ToolCall, name)
        .actor(AuditActor::for_context(ctx))
        .args_digest(trail.digest(args))
        .outcome(AuditRecordOutcome::OutcomeUnknown)
        .ext("cause", Value::String(cause.to_string()))
        .ext("sessionId", Value::String(ctx.session_id().to_string()));
    trail.record(draft).await
}

impl LedgeredCall {
    /// Records the outcome of a call that ran to an end: `result` when the tool
    /// returned a value, otherwise `error`, the text the model was given for the
    /// failure, since a replay of the call is answered with it. A call the kill switch
    /// refused between `begin` and execution is completed here too, as `Failed` with its
    /// [`not_run_text`](adk_core::not_run_text): it never ran.
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
