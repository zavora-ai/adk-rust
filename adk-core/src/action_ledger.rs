//! Durable record of tool calls whose side effects must not repeat.
//!
//! An [`ActionLedger`] records a [`ToolEffect::NonIdempotent`] call before it
//! executes and its outcome after. A call replayed under the same
//! [idempotency key](crate::ToolContext::idempotency_key) — after a crash, a
//! timeout, or a cancelled run — is answered from the ledger instead of
//! executing a second time:
//!
//! | Ledger state for the key | Agent behaviour |
//! |--------------------------|-----------------|
//! | No record | `begin`, execute, `complete` |
//! | Completed | The recorded outcome is returned; the tool does not run |
//! | Begun, not completed | An [`outcome_unknown_response`] is returned; the tool does not run |
//! | Ledger read or `begin` fails | An error is returned; the tool does not run |
//!
//! The governance kill switch is checked again between `begin` and execution. A call it
//! refuses there is completed as [`ActionOutcome::Failed`] with its [`not_run_text`], so the
//! record never reads as begun without an outcome and a replay is answered with that text.

use crate::{
    AdkError, ErrorCategory, ErrorComponent, ReadonlyContext, Result, RunConfig, ToolEffect,
    ToolResultStatus,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// The `status` value of a function response whose side effects may or may not
/// have happened.
pub const OUTCOME_UNKNOWN_STATUS: &str = "outcome_unknown";

/// Builds the function response for a call whose outcome is unknown.
///
/// The runtime returns this when a non-idempotent call timed out, panicked, was
/// cancelled mid-flight, or has a ledger record that was begun but never
/// completed. It tells the model and the operator that the side effect may have
/// happened, so the call must not simply be repeated.
///
/// # Example
///
/// ```rust
/// use adk_core::{OUTCOME_UNKNOWN_STATUS, outcome_unknown_response};
///
/// let response = outcome_unknown_response("the run was cancelled mid-call");
/// assert_eq!(response["status"], OUTCOME_UNKNOWN_STATUS);
/// assert_eq!(response["detail"], "the run was cancelled mid-call");
/// ```
pub fn outcome_unknown_response(detail: impl Into<String>) -> Value {
    serde_json::json!({ "status": OUTCOME_UNKNOWN_STATUS, "detail": detail.into() })
}

/// The prefix of the text a call the governance kill switch refused is answered with.
pub const NOT_RUN_PREFIX: &str = "not run: ";

/// The text a call the governance kill switch refused with `refusal` is answered with:
/// `not run: <refusal message>`. A call refused after [`ActionLedger::begin`] is completed
/// as [`ActionOutcome::Failed`] with this text, so a replay answers it the same way.
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, is_not_run, not_run_text};
///
/// let text = not_run_text(&AdkError::agent("execution is frozen"));
/// assert_eq!(text, "not run: execution is frozen");
/// assert!(is_not_run(&text));
/// ```
pub fn not_run_text(refusal: &AdkError) -> String {
    format!("{NOT_RUN_PREFIX}{}", refusal.message)
}

/// Returns `true` when `text` is a [`not_run_text`]: the call was refused and never ran.
///
/// # Example
///
/// ```rust
/// assert!(adk_core::is_not_run("not run: execution is frozen"));
/// assert!(!adk_core::is_not_run("card declined"));
/// ```
pub fn is_not_run(text: &str) -> bool {
    text.starts_with(NOT_RUN_PREFIX)
}

/// Returns `true` when `response` is an [`outcome_unknown_response`].
///
/// # Example
///
/// ```rust
/// use adk_core::{is_outcome_unknown, outcome_unknown_response};
///
/// assert!(is_outcome_unknown(&outcome_unknown_response("timed out")));
/// assert!(!is_outcome_unknown(&serde_json::json!({ "status": "ok" })));
/// ```
pub fn is_outcome_unknown(response: &Value) -> bool {
    response.get("status").and_then(Value::as_str) == Some(OUTCOME_UNKNOWN_STATUS)
}

/// Returns a stable digest of `value` in canonical JSON form, as `fnv1a128:<hex>`.
///
/// Object keys are sorted at every level, so structurally equal values produce
/// the same digest, in every process and release. The ledger stores digests
/// rather than arguments or results, so it never holds payment details or other
/// tool payloads. The digest identifies a payload; it is not a cryptographic
/// hash and does not prove that a payload was not altered.
///
/// # Example
///
/// ```rust
/// use adk_core::json_digest;
/// use serde_json::json;
///
/// let a = json_digest(&json!({ "amount": 50, "currency": "USD" }));
/// let b = json_digest(&json!({ "currency": "USD", "amount": 50 }));
/// assert_eq!(a, b);
/// assert!(a.starts_with("fnv1a128:"));
/// ```
pub fn json_digest(value: &Value) -> String {
    const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    let mut canonical = String::new();
    crate::context::write_canonical(value, &mut canonical);
    let hash = canonical
        .bytes()
        .fold(OFFSET_BASIS, |hash, byte| (hash ^ u128::from(byte)).wrapping_mul(PRIME));
    format!("fnv1a128:{hash:032x}")
}

/// The result of a ledgered call, recorded by [`ActionLedger::complete`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
#[non_exhaustive]
pub enum ActionOutcome {
    /// The tool returned a value.
    #[serde(rename_all = "camelCase")]
    Succeeded {
        /// [`json_digest`] of the value the tool returned.
        result_digest: String,
    },
    /// The tool returned an error, or the governance kill switch refused the call between
    /// [`ActionLedger::begin`] and execution, in which case `error` is its [`not_run_text`]
    /// and the tool never ran ([`is_not_run`]).
    #[serde(rename_all = "camelCase")]
    Failed {
        /// The error message the tool returned, or the [`not_run_text`] of a refused call.
        error: String,
    },
}

/// One ledgered tool call.
///
/// # Example
///
/// ```rust
/// use adk_core::{ActionRecord, ToolEffect};
/// use serde_json::json;
///
/// let record = ActionRecord::new(
///     "app/user/session/inv-1/call-1",
///     "payments_checkout_complete",
///     &json!({ "transactionId": "tx-1" }),
///     ToolEffect::NonIdempotent,
/// );
/// assert!(!record.is_completed());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionRecord {
    /// The call's [idempotency key](crate::ToolContext::idempotency_key).
    pub key: String,
    /// The name of the tool that was called.
    pub tool_name: String,
    /// [`json_digest`] of the call arguments.
    pub args_digest: String,
    /// The effect the tool declared when the call began.
    pub effect: ToolEffect,
    /// When the call began.
    pub started_at: DateTime<Utc>,
    /// The recorded outcome, or `None` while the call is in flight or after it
    /// ended without one (crash, timeout, cancellation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ActionOutcome>,
    /// When the outcome was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

impl ActionRecord {
    /// Creates a record for a call that begins now.
    pub fn new(
        key: impl Into<String>,
        tool_name: impl Into<String>,
        args: &Value,
        effect: ToolEffect,
    ) -> Self {
        Self {
            key: key.into(),
            tool_name: tool_name.into(),
            args_digest: json_digest(args),
            effect,
            started_at: Utc::now(),
            outcome: None,
            completed_at: None,
        }
    }

    /// Returns `true` once an outcome has been recorded.
    pub fn is_completed(&self) -> bool {
        self.outcome.is_some()
    }
}

/// Durable store of non-idempotent tool calls, keyed by idempotency key.
///
/// Set one on [`RunConfig::action_ledger`](crate::RunConfig::action_ledger).
/// `LlmAgent` and `CodeActAgent` call [`begin`](Self::begin) before executing a
/// [`ToolEffect::NonIdempotent`] tool and [`complete`](Self::complete) after,
/// and consults [`get`](Self::get) first so a replayed call never executes
/// twice. An error from `get` or `begin` fails the call closed: the tool is not
/// executed.
///
/// # Example
///
/// ```rust
/// use adk_core::{ActionLedger, ActionOutcome, ActionRecord, InMemoryActionLedger, ToolEffect};
/// use serde_json::json;
///
/// #[tokio::main(flavor = "current_thread")]
/// async fn main() -> adk_core::Result<()> {
///     let ledger = InMemoryActionLedger::new();
///     let record = ActionRecord::new("k1", "pay", &json!({}), ToolEffect::NonIdempotent);
///     ledger.begin(&record).await?;
///     assert!(ledger.begin(&record).await.is_err(), "a key begins once");
///
///     let outcome = ActionOutcome::Succeeded { result_digest: "fnv1a128:00".into() };
///     ledger.complete("k1", outcome.clone()).await?;
///     assert_eq!(ledger.get("k1").await?.and_then(|record| record.outcome), Some(outcome));
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait ActionLedger: std::fmt::Debug + Send + Sync {
    /// Records that a call is about to execute.
    ///
    /// # Errors
    ///
    /// Returns an error when a record already exists under `record.key`, or when
    /// the record cannot be stored. Either way the call must not execute.
    async fn begin(&self, record: &ActionRecord) -> Result<()>;

    /// Records the outcome of a call that [`begin`](Self::begin) recorded.
    ///
    /// # Errors
    ///
    /// Returns an error when no record exists under `key`, when one already has
    /// an outcome, or when the outcome cannot be stored.
    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()>;

    /// Returns the record stored under `key`, if any.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be read.
    async fn get(&self, key: &str) -> Result<Option<ActionRecord>>;
}

/// Builds the error a ledger returns when `begin` finds an existing record.
///
/// # Example
///
/// ```rust
/// let error = adk_core::action_ledger::duplicate_key_error("k1");
/// assert_eq!(error.code, "tool.action_ledger.duplicate_key");
/// ```
pub fn duplicate_key_error(key: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "tool.action_ledger.duplicate_key",
        format!(
            "action ledger already holds a record for key '{key}'; the call was not executed again"
        ),
    )
}

/// Builds the error a ledger returns when `complete` cannot find the record.
///
/// # Example
///
/// ```rust
/// let error = adk_core::action_ledger::missing_key_error("k1");
/// assert!(error.is_not_found());
/// ```
pub fn missing_key_error(key: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::NotFound,
        "tool.action_ledger.not_found",
        format!("action ledger has no record for key '{key}'; call begin before complete"),
    )
}

/// Builds the error a ledger returns when `complete` finds an outcome already recorded.
///
/// # Example
///
/// ```rust
/// let error = adk_core::action_ledger::already_completed_error("k1");
/// assert_eq!(error.code, "tool.action_ledger.already_completed");
/// ```
pub fn already_completed_error(key: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "tool.action_ledger.already_completed",
        format!("action ledger record '{key}' already has an outcome; outcomes are write-once"),
    )
}

/// Process-local [`ActionLedger`].
///
/// Records survive a cancelled or timed-out run within one process but not a
/// process restart. Use a durable implementation, such as
/// `adk_session::SqliteActionLedger`, when calls must not repeat across restarts.
///
/// # Example
///
/// ```rust
/// use adk_core::{InMemoryActionLedger, RunConfig};
/// use std::sync::Arc;
///
/// let config = RunConfig::builder()
///     .action_ledger(Arc::new(InMemoryActionLedger::new()))
///     .build();
/// assert!(config.action_ledger.is_some());
/// ```
#[derive(Debug, Default)]
pub struct InMemoryActionLedger {
    records: tokio::sync::RwLock<HashMap<String, ActionRecord>>,
}

impl InMemoryActionLedger {
    /// Creates an empty ledger.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ActionLedger for InMemoryActionLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        let mut records = self.records.write().await;
        if records.contains_key(&record.key) {
            return Err(duplicate_key_error(&record.key));
        }
        records.insert(record.key.clone(), record.clone());
        Ok(())
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        let mut records = self.records.write().await;
        let record = records.get_mut(key).ok_or_else(|| missing_key_error(key))?;
        if record.outcome.is_some() {
            return Err(already_completed_error(key));
        }
        record.outcome = Some(outcome);
        record.completed_at = Some(Utc::now());
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        Ok(self.records.read().await.get(key).cloned())
    }
}

/// The action-ledger key of a held tool call that a later run resumes.
///
/// A call's ledger key is normally its
/// [`ToolContext::idempotency_key`](crate::ToolContext::idempotency_key), which names
/// the invocation that runs it. A held call resumes in a later run, under that run's
/// invocation, so a resumed call is keyed by its original identity instead: the
/// session, the call ID the call was issued under, and a digest of the
/// [`ToolConfirmationRequest::fingerprint`](crate::ToolConfirmationRequest::fingerprint)
/// it was held with. Every resumption of the same held call shares one record, so a call
/// that ran in a run whose result was never saved is answered from its record instead of
/// running again (spec 3.1 §7.12, §9.2 I37).
///
/// # Example
///
/// ```rust
/// use adk_core::{ToolConfirmationRequest, held_call_ledger_key};
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
    let digest = json_digest(&Value::String(fingerprint.to_string()));
    format!("{app_name}/{user_id}/{session_id}/held/{function_call_id}/{digest}")
}

/// A call begun in the action ledger by [`begin_ledgered_call`], completed once the tool
/// finishes.
#[derive(Debug)]
pub struct LedgeredCall {
    ledger: Arc<dyn ActionLedger>,
    key: String,
}

/// The ledger's answer to a call that must not execute, from [`begin_ledgered_call`].
#[derive(Debug)]
#[non_exhaustive]
pub struct LedgerAnswer {
    /// The function response that answers the call.
    pub response: Value,
    /// What the answer reports: the recorded outcome, or `NotRun` when the ledger failed.
    pub status: ToolResultStatus,
    /// The ledger's error, when it could not be read or written.
    pub cause: Option<AdkError>,
}

/// Consults the run's action ledger ([`RunConfig::action_ledger`]) before a call executes.
///
/// Every runtime that dispatches tools — `LlmAgent`, `CodeActAgent` and the realtime
/// agents — brackets a [`ToolEffect::NonIdempotent`] call with this step after the
/// governed path authorizes it and before it executes.
///
/// Returns `Ok(Ok(None))` when the call is not ledgered (no ledger configured, or an
/// effect other than [`ToolEffect::NonIdempotent`]), and `Ok(Ok(Some(_)))` once the
/// call has been begun under `key`. Returns `Ok(Err(answer))` when the call must
/// not execute: the ledger already holds a record for `key`, or cannot be read or
/// written.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::{
///     InMemoryActionLedger, ReadonlyContext, RunConfig, ToolEffect, begin_ledgered_call,
/// };
/// use serde_json::json;
///
/// struct Call;
/// impl ReadonlyContext for Call {
///     fn invocation_id(&self) -> &str { "inv-1" }
///     fn agent_name(&self) -> &str { "voice" }
///     fn user_id(&self) -> &str { "alice" }
///     fn app_name(&self) -> &str { "app" }
///     fn session_id(&self) -> &str { "s1" }
///     fn branch(&self) -> &str { "" }
///     fn user_content(&self) -> &adk_core::Content {
///         static EMPTY: std::sync::OnceLock<adk_core::Content> = std::sync::OnceLock::new();
///         EMPTY.get_or_init(|| adk_core::Content::new("user"))
///     }
/// }
///
/// # futures::executor::block_on(async {
/// let config = RunConfig::builder().action_ledger(Arc::new(InMemoryActionLedger::new())).build();
/// let args = json!({ "to": "tel:+14155550100" });
/// let begun = begin_ledgered_call(&config, &Call, ToolEffect::NonIdempotent, "k1".into(), "transfer_call", &args)
///     .await?
///     .expect("no record yet")
///     .expect("ledgered");
/// begun.complete("transfer_call", Some(&json!({ "transferred": true })), "").await;
///
/// // A replay under the same key is answered from the record and does not execute.
/// let replay = begin_ledgered_call(&config, &Call, ToolEffect::NonIdempotent, "k1".into(), "transfer_call", &args)
///     .await?;
/// assert_eq!(replay.unwrap_err().response["status"], "already_succeeded");
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
///
/// # Errors
///
/// Returns `audit.record_failed`, which ends the run, when an outcome-unknown answer
/// cannot be audited under [`AuditFailureMode::Block`](crate::audit_log::AuditFailureMode::Block).
pub async fn begin_ledgered_call(
    config: &RunConfig,
    ctx: &dyn ReadonlyContext,
    effect: ToolEffect,
    key: String,
    name: &str,
    args: &Value,
) -> Result<std::result::Result<Option<LedgeredCall>, LedgerAnswer>> {
    let ledger = match &config.action_ledger {
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
                    record_outcome_unknown(config, ctx, name, args, "began_without_outcome")
                        .await?;
                    outcome_unknown_response(format!(
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

/// Records, through the run's [`RunConfig::audit`], a call answered with an unknown
/// outcome. `cause` is `interrupted` for a call that ended mid-flight and
/// `began_without_outcome` for a replay of one.
///
/// # Errors
///
/// Returns `audit.record_failed` when the record cannot be written under
/// [`AuditFailureMode::Block`](crate::audit_log::AuditFailureMode::Block).
pub async fn record_outcome_unknown(
    config: &RunConfig,
    ctx: &dyn ReadonlyContext,
    name: &str,
    args: &Value,
    cause: &str,
) -> Result<()> {
    use crate::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};

    let Some(trail) = &config.audit else {
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
    /// [`not_run_text`]: it never ran.
    ///
    /// A call that ended mid-flight (timeout, panic) must not be completed, so
    /// its begun record answers a replay with an unknown outcome.
    pub async fn complete(self, name: &str, result: Option<&Value>, error: &str) {
        let outcome = match result {
            Some(value) => ActionOutcome::Succeeded { result_digest: json_digest(value) },
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
///
/// # Example
///
/// ```rust
/// use adk_core::{interrupted_call_response, is_outcome_unknown};
///
/// assert!(is_outcome_unknown(&interrupted_call_response("Tool 'pay' timed out after 30s")));
/// ```
pub fn interrupted_call_response(error: &str) -> Value {
    outcome_unknown_response(format!(
        "{error}. The call may have taken effect; check its result before calling it again."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn in_memory_ledger_round_trips_a_completed_record() {
        let ledger = InMemoryActionLedger::new();
        let record = ActionRecord::new("k", "pay", &json!({"a": 1}), ToolEffect::NonIdempotent);
        ledger.begin(&record).await.unwrap();
        assert_eq!(ledger.get("k").await.unwrap(), Some(record.clone()));

        let outcome = ActionOutcome::Failed { error: "declined".into() };
        ledger.complete("k", outcome.clone()).await.unwrap();
        let stored = ledger.get("k").await.unwrap().unwrap();
        assert_eq!(stored.outcome, Some(outcome.clone()));
        assert!(stored.completed_at.is_some());

        let error = ledger.complete("k", outcome).await.unwrap_err();
        assert_eq!(error.code, "tool.action_ledger.already_completed");
    }

    #[tokio::test]
    async fn in_memory_ledger_rejects_duplicates_and_unknown_keys() {
        let ledger = InMemoryActionLedger::new();
        let record = ActionRecord::new("k", "pay", &json!({}), ToolEffect::NonIdempotent);
        ledger.begin(&record).await.unwrap();
        assert_eq!(
            ledger.begin(&record).await.unwrap_err().code,
            "tool.action_ledger.duplicate_key"
        );
        let missing = ledger
            .complete("other", ActionOutcome::Succeeded { result_digest: String::new() })
            .await
            .unwrap_err();
        assert!(missing.is_not_found());
        assert_eq!(ledger.get("other").await.unwrap(), None);
    }

    #[test]
    fn action_record_serializes_camel_case() {
        let mut record = ActionRecord::new("k", "pay", &json!({}), ToolEffect::NonIdempotent);
        record.outcome = Some(ActionOutcome::Succeeded { result_digest: "fnv1a128:ab".into() });
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["toolName"], "pay");
        assert_eq!(value["effect"], "non_idempotent");
        assert_eq!(value["outcome"], json!({"status": "succeeded", "resultDigest": "fnv1a128:ab"}));
        let back: ActionRecord = serde_json::from_value(value).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn json_digest_ignores_key_order_and_distinguishes_values() {
        assert_eq!(json_digest(&json!({"a": 1, "b": 2})), json_digest(&json!({"b": 2, "a": 1})));
        assert_ne!(json_digest(&json!({"a": 1})), json_digest(&json!({"a": 2})));
        assert_eq!(json_digest(&json!(null)).len(), "fnv1a128:".len() + 32);
        // The algorithm is fixed, so a digest stored by one release matches the next.
        assert_eq!(json_digest(&json!({"a": 1})), "fnv1a128:a930e708924ff78dd36054201cd38dc9");
    }
}
