use adk_core::identity::AdkIdentity;
use adk_core::{AdkError, Content, ErrorCategory, ErrorComponent, Event, Result};
use adk_session::KEY_PREFIX_APP;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::domain::{SafeTransactionSummary, TransactionId, TransactionRecord};
use crate::guardrail::redact_payment_content;

pub const TRANSACTION_KEY_PREFIX: &str = "payments:tx:";
pub const ACTIVE_INDEX_KEY: &str = "payments:index:active";
pub const COMPLETED_INDEX_KEY: &str = "payments:index:completed";

/// Locates one journal entry by session identity plus canonical transaction ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionLocator {
    pub identity: AdkIdentity,
    pub transaction_id: TransactionId,
}

/// Returns the app-scoped session-state key for one transaction record.
#[must_use]
pub fn transaction_state_key(identity: &AdkIdentity, transaction_id: &TransactionId) -> String {
    format!("{TRANSACTION_KEY_PREFIX}{}:{transaction_id}", identity_hash(identity))
}

/// Returns the app-scoped state key used to store unresolved transaction locators.
#[must_use]
pub fn active_index_state_key() -> String {
    format!("{KEY_PREFIX_APP}{ACTIVE_INDEX_KEY}")
}

/// Returns the app-scoped state key used to store completed transaction locators.
#[must_use]
pub fn completed_index_state_key() -> String {
    format!("{KEY_PREFIX_APP}{COMPLETED_INDEX_KEY}")
}

/// Returns the app-scoped state key used to store one transaction record.
#[must_use]
pub fn transaction_state_storage_key(
    identity: &AdkIdentity,
    transaction_id: &TransactionId,
) -> String {
    format!("{KEY_PREFIX_APP}{}", transaction_state_key(identity, transaction_id))
}

/// Builds the safe event that mirrors journal state into session storage.
///
/// # Errors
///
/// Returns an error if the journal state cannot be serialized.
pub fn build_journal_event(
    record: &TransactionRecord,
    active: &[TransactionLocator],
    completed: &[TransactionLocator],
) -> Result<Event> {
    let identity = record.session_identity.as_ref().ok_or_else(|| {
        AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::InvalidInput,
            "payments.journal.identity_required",
            "transaction journal writes require a session identity",
        )
    })?;

    let mut event = Event::new("payments.journal");
    event.author = "adk-payments".to_string();
    event.set_content(summary_content(&record.safe_summary));
    event.actions.state_delta.insert(
        transaction_state_storage_key(identity, &record.transaction_id),
        serialize_value(record, "payments.journal.record_serialize")?,
    );
    event.actions.state_delta.insert(
        active_index_state_key(),
        serialize_value(active, "payments.journal.active_index_serialize")?,
    );
    event.actions.state_delta.insert(
        completed_index_state_key(),
        serialize_value(completed, "payments.journal.completed_index_serialize")?,
    );
    Ok(event)
}

/// Parses one serialized transaction record from session state.
///
/// # Errors
///
/// Returns an error if the stored value is not a valid serialized transaction record.
pub fn parse_record(value: Value) -> Result<TransactionRecord> {
    serde_json::from_value(value).map_err(|err| {
        AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::Internal,
            "payments.journal.record_deserialize",
            format!("failed to deserialize stored transaction record: {err}"),
        )
    })
}

/// Parses one serialized locator index from session state.
///
/// # Errors
///
/// Returns an error if the stored value is not a valid locator list.
pub fn parse_locators(value: Option<Value>) -> Result<Vec<TransactionLocator>> {
    match value {
        Some(value) => serde_json::from_value(value).map_err(|err| {
            AdkError::new(
                ErrorComponent::Session,
                ErrorCategory::Internal,
                "payments.journal.index_deserialize",
                format!("failed to deserialize stored transaction index: {err}"),
            )
        }),
        None => Ok(Vec::new()),
    }
}

fn serialize_value<T: Serialize + ?Sized>(value: &T, code: &'static str) -> Result<Value> {
    serde_json::to_value(value).map_err(|err| {
        AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::Internal,
            code,
            format!("failed to serialize transaction journal state: {err}"),
        )
    })
}

fn summary_content(summary: &SafeTransactionSummary) -> Content {
    redact_payment_content(&Content::new("system").with_text(summary.transcript_text()))
}

fn identity_hash(identity: &AdkIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity.app_name.as_ref().as_bytes());
    hasher.update([0]);
    hasher.update(identity.user_id.as_ref().as_bytes());
    hasher.update([0]);
    hasher.update(identity.session_id.as_ref().as_bytes());
    hex::encode(hasher.finalize())[..24].to_string()
}
