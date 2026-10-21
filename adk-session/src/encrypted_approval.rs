//! [`EncryptedApprovalStore`]: an [`ApprovalStore`](adk_core::ApprovalStore) whose payloads reach the inner store
//! sealed in the [`envelope`](crate::envelope) (spec 3.1 §7.12, §9.2 I57, §10 S5).
//!
//! | Data | Stored as |
//! |------|-----------|
//! | A held call's arguments and fingerprint (`ToolConfirmationRequest::args`) | One envelope in `args`, bound to (app, user, session, row key) |
//! | A decision's edited arguments, message and fingerprint | One envelope in `message`, bound to (app, user, session, row key) |
//! | An [`ApprovalRequest`](adk_core::ApprovalRequest)'s `fingerprint`, `args_redacted`, `summary`, `note` and each decision's `edited_args` and `reason` | Envelopes bound to (app, user, session, request id) |
//! | Row keys, request ids, tool names, agents, subjects, states, timestamps, delegations | Plaintext, so the inner store can key, order and filter |
//!
//! A call's fingerprint spells out its canonical arguments, so it cannot be the row key.
//! Rows are keyed by `adk-fp:v1:` and the HMAC-SHA256, under the store key, of the scope and
//! the fingerprint; the real fingerprint travels inside the sealed payload and comes back
//! from [`pending`](adk_core::ApprovalStore::pending). The inner store derives a pending row's key
//! from the request it is given, so the wrapper records through
//! [`ApprovalStore::record_pending_keyed`](adk_core::ApprovalStore::record_pending_keyed), which the stores in `adk-core` and this crate's
//! SQL stores implement.
//!
//! Reads try the current key, then each previous key; a value no key opens, or one stored
//! in plaintext, is an error, never a fallback (I57).
//!
//! # Example
//!
//! ```rust
//! use adk_core::InMemoryApprovalStore;
//! use adk_session::{EncryptedApprovalStore, EncryptionKey};
//!
//! let store = EncryptedApprovalStore::new(InMemoryApprovalStore::new(), EncryptionKey::generate(), vec![]);
//! # let _ = store;
//! ```

use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalDelegation, ApprovalRequest, ApprovalRequestQuery, ApprovalScope,
    ApprovalStore, ErrorCategory, ErrorComponent, PendingApproval, Result, ToolApproval,
    ToolConfirmationDecision, ToolConfirmationRequest,
};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

use crate::encryption_key::EncryptionKey;
use crate::envelope::{self, ENVELOPE_PREFIX};

/// Domain separator of every envelope this store writes.
const AAD_DOMAIN: &[u8] = b"adk-session/encrypted-approval-store/v1";
/// Domain separator of the row-key MAC.
const ROW_KEY_DOMAIN: &[u8] = b"adk-session/encrypted-approval-store/row-key/v1";
/// Prefix of every row key this store writes.
pub const ROW_KEY_PREFIX: &str = "adk-fp:v1:";

/// The sealed payload of a pending row: the call's fingerprint and arguments.
#[derive(Serialize, Deserialize)]
struct SealedCall {
    fingerprint: String,
    args: Value,
}

/// The sealed payload of a decision row: the call's fingerprint and what the approver
/// attached.
#[derive(Serialize, Deserialize)]
struct SealedDecision {
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edited_args: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// An [`ApprovalStore`] that seals arguments and decisions before they reach `S`.
///
/// See the [module documentation](self) for what is sealed and what stays plaintext.
///
/// # Example
///
/// ```rust
/// use adk_core::InMemoryApprovalStore;
/// use adk_session::{EncryptedApprovalStore, EncryptionKey};
///
/// let current = EncryptionKey::generate();
/// let retired = EncryptionKey::generate();
/// let store = EncryptedApprovalStore::new(InMemoryApprovalStore::new(), current, vec![retired]);
/// # let _ = store;
/// ```
pub struct EncryptedApprovalStore<S: ApprovalStore> {
    inner: S,
    current_key: EncryptionKey,
    previous_keys: Vec<EncryptionKey>,
    legacy_migration: bool,
}

impl<S: ApprovalStore> std::fmt::Debug for EncryptedApprovalStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedApprovalStore")
            .field("inner", &self.inner)
            .field("key_id", &self.current_key.id())
            .field("previous_keys", &self.previous_keys.len())
            .finish()
    }
}

fn error(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Session, ErrorCategory::Internal, code, message)
}

fn decrypt_error(what: &str) -> AdkError {
    error(
        "session.encryption.decrypt_failed",
        format!(
            "failed to decrypt the approval store's {what}: no configured key authenticates it; \
             it was written with an unknown key or moved from another session or request"
        ),
    )
}

fn unencrypted_error(what: &str) -> AdkError {
    error(
        "session.encryption.unencrypted_data",
        format!(
            "the approval store's {what} is not encrypted; rows written before the store key \
             was configured must be sealed first (`adk-rust store rotate-key`)"
        ),
    )
}

fn serialization_error(error_: serde_json::Error) -> AdkError {
    error(
        "session.encryption.serialization_failed",
        format!("failed to (de)serialize sealed approval data: {error_}"),
    )
}

/// The key a pending or decision row is stored under: a MAC of the scope and the
/// fingerprint, so equal calls in different sessions never share a row key and no row key
/// reveals the arguments.
fn row_key(key: &EncryptionKey, scope: &ApprovalScope, fingerprint: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC-SHA256 accepts a 32-byte key");
    mac.update(&envelope::associated_data(
        ROW_KEY_DOMAIN,
        &[&scope.app_name, &scope.user_id, &scope.session_id, fingerprint],
    ));
    let digest = mac.finalize().into_bytes();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{ROW_KEY_PREFIX}{hex}")
}

fn pending_aad(scope: &ApprovalScope, row_key: &str) -> Vec<u8> {
    envelope::associated_data(
        AAD_DOMAIN,
        &["pending", &scope.app_name, &scope.user_id, &scope.session_id, row_key],
    )
}

fn decision_aad(scope: &ApprovalScope, row_key: &str) -> Vec<u8> {
    envelope::associated_data(
        AAD_DOMAIN,
        &["decision", &scope.app_name, &scope.user_id, &scope.session_id, row_key],
    )
}

fn request_aad(scope: &ApprovalScope, id: &str) -> Vec<u8> {
    envelope::associated_data(
        AAD_DOMAIN,
        &["request", &scope.app_name, &scope.user_id, &scope.session_id, id],
    )
}

impl<S: ApprovalStore> EncryptedApprovalStore<S> {
    /// Wraps `inner`: writes seal with `current_key`; reads open with it, then with each
    /// of `previous_keys`.
    pub fn new(inner: S, current_key: EncryptionKey, previous_keys: Vec<EncryptionKey>) -> Self {
        Self { inner, current_key, previous_keys, legacy_migration: false }
    }

    /// Lets `rewrap_sqlite` seal rows written in plaintext before a key was configured,
    /// so a store can be encrypted in place. Reads stay strict: a plaintext row is an error.
    #[must_use]
    pub fn with_legacy_migration(mut self, enabled: bool) -> Self {
        self.legacy_migration = enabled;
        self
    }

    /// The wrapped store. Reads through it return envelopes; writes bypass sealing.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Consumes the wrapper and returns the wrapped store.
    pub fn into_inner(self) -> S {
        self.inner
    }

    fn keys(&self) -> impl Iterator<Item = &EncryptionKey> {
        std::iter::once(&self.current_key).chain(&self.previous_keys)
    }

    /// The row keys `fingerprint` may be stored under: the current key's first, then each
    /// previous key's (rows not yet re-sealed after a rotation).
    fn row_keys(&self, scope: &ApprovalScope, fingerprint: &str) -> Vec<String> {
        self.keys().map(|key| row_key(key, scope, fingerprint)).collect()
    }

    fn seal_text(&self, text: &str, aad: &[u8]) -> Result<String> {
        envelope::seal(&self.current_key, text.as_bytes(), aad)
    }

    fn open_text(&self, text: &str, what: &str, aad: &[u8]) -> Result<String> {
        if !text.starts_with(ENVELOPE_PREFIX) {
            return Err(unencrypted_error(what));
        }
        let (plaintext, _) =
            envelope::open(self.keys(), text, aad).ok_or_else(|| decrypt_error(what))?;
        String::from_utf8(plaintext).map_err(|e| {
            error("session.encryption.serialization_failed", format!("{what} is not UTF-8: {e}"))
        })
    }

    fn seal_json<T: Serialize>(&self, value: &T, aad: &[u8]) -> Result<Value> {
        let text = serde_json::to_string(value).map_err(serialization_error)?;
        Ok(Value::String(self.seal_text(&text, aad)?))
    }

    fn open_json<T: for<'de> Deserialize<'de>>(
        &self,
        value: &Value,
        what: &str,
        aad: &[u8],
    ) -> Result<T> {
        let Some(text) = value.as_str() else {
            return Err(unencrypted_error(what));
        };
        serde_json::from_str(&self.open_text(text, what, aad)?).map_err(serialization_error)
    }

    /// `request` with its fingerprint and arguments sealed under `row_key`.
    fn seal_call(
        &self,
        request: &ToolConfirmationRequest,
        fingerprint: &str,
        scope: &ApprovalScope,
        row_key: &str,
    ) -> Result<ToolConfirmationRequest> {
        let payload =
            SealedCall { fingerprint: fingerprint.to_string(), args: request.args.clone() };
        let mut sealed = request.clone();
        sealed.args = self.seal_json(&payload, &pending_aad(scope, row_key))?;
        Ok(sealed)
    }

    /// The call and its fingerprint from a pending row stored under `row_key`.
    fn open_call(
        &self,
        request: &ToolConfirmationRequest,
        scope: &ApprovalScope,
        row_key: &str,
    ) -> Result<(ToolConfirmationRequest, String)> {
        let payload: SealedCall =
            self.open_json(&request.args, "held call arguments", &pending_aad(scope, row_key))?;
        let mut opened = request.clone();
        opened.args = payload.args;
        Ok((opened, payload.fingerprint))
    }

    fn seal_approval(
        &self,
        approval: ToolApproval,
        fingerprint: &str,
        scope: &ApprovalScope,
        row_key: &str,
    ) -> Result<ToolApproval> {
        let payload = SealedDecision {
            fingerprint: fingerprint.to_string(),
            edited_args: approval.edited_args,
            message: approval.message,
        };
        let sealed = self.seal_json(&payload, &decision_aad(scope, row_key))?;
        Ok(ToolApproval {
            decision: approval.decision,
            expires_at: approval.expires_at,
            edited_args: None,
            message: sealed.as_str().map(str::to_string),
        })
    }

    fn open_approval(
        &self,
        approval: ToolApproval,
        scope: &ApprovalScope,
        row_key: &str,
    ) -> Result<(ToolApproval, String)> {
        let Some(blob) = &approval.message else {
            return Err(unencrypted_error("decision"));
        };
        let payload: SealedDecision = serde_json::from_str(&self.open_text(
            blob,
            "decision",
            &decision_aad(scope, row_key),
        )?)
        .map_err(serialization_error)?;
        Ok((
            ToolApproval {
                decision: approval.decision,
                expires_at: approval.expires_at,
                edited_args: payload.edited_args,
                message: payload.message,
            },
            payload.fingerprint,
        ))
    }

    fn seal_approval_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let aad = request_aad(&request.scope, &request.id);
        let mut sealed = request.clone();
        sealed.fingerprint = self.seal_text(&request.fingerprint, &aad)?;
        sealed.args_redacted = self.seal_json(&request.args_redacted, &aad)?;
        sealed.summary = self.seal_text(&request.summary, &aad)?;
        if let Some(note) = &request.note {
            sealed.note = Some(self.seal_text(note, &aad)?);
        }
        for decision in &mut sealed.decisions {
            if let Some(args) = &decision.edited_args {
                decision.edited_args = Some(self.seal_json(args, &aad)?);
            }
            if let Some(reason) = &decision.reason {
                decision.reason = Some(self.seal_text(reason, &aad)?);
            }
        }
        Ok(sealed)
    }

    fn open_approval_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let aad = request_aad(&request.scope, &request.id);
        let mut opened = request.clone();
        opened.fingerprint = self.open_text(&request.fingerprint, "request fingerprint", &aad)?;
        opened.args_redacted = self.open_json(&request.args_redacted, "request arguments", &aad)?;
        opened.summary = self.open_text(&request.summary, "request summary", &aad)?;
        if let Some(note) = &request.note {
            opened.note = Some(self.open_text(note, "request note", &aad)?);
        }
        for decision in &mut opened.decisions {
            if let Some(args) = &decision.edited_args {
                decision.edited_args = Some(self.open_json(args, "edited arguments", &aad)?);
            }
            if let Some(reason) = &decision.reason {
                decision.reason = Some(self.open_text(reason, "decision reason", &aad)?);
            }
        }
        Ok(opened)
    }

    /// The decrypted pending rows of `scope` with their row keys.
    async fn opened_pending(
        &self,
        scope: &ApprovalScope,
    ) -> Result<Vec<(String, PendingApproval)>> {
        self.inner
            .pending(scope)
            .await?
            .into_iter()
            .map(|entry| {
                let (request, fingerprint) =
                    self.open_call(&entry.request, scope, &entry.fingerprint)?;
                Ok((
                    entry.fingerprint,
                    PendingApproval { fingerprint, request, requested_at: entry.requested_at },
                ))
            })
            .collect()
    }

    /// The row key of the pending row for `fingerprint`, if one exists under any key.
    async fn pending_row_key(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .opened_pending(scope)
            .await?
            .into_iter()
            .find(|(_, entry)| entry.fingerprint == fingerprint)
            .map(|(row_key, _)| row_key))
    }
}

#[async_trait]
impl<S: ApprovalStore> ApprovalStore for EncryptedApprovalStore<S> {
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        self.record_pending_keyed(scope, &request.fingerprint(), request).await
    }

    async fn record_pending_keyed(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        // A call already pending under a previous key's row keeps that row, so recording
        // it again keeps its request time as the trait requires.
        let row_key = match self.pending_row_key(scope, fingerprint).await? {
            Some(existing) => existing,
            None => row_key(&self.current_key, scope, fingerprint),
        };
        let sealed = self.seal_call(request, fingerprint, scope, &row_key)?;
        self.inner.record_pending_keyed(scope, &row_key, &sealed).await
    }

    async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>> {
        Ok(self.opened_pending(scope).await?.into_iter().map(|(_, entry)| entry).collect())
    }

    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> Result<()> {
        // The decision clears the pending row, so it goes under the pending row's key.
        let row_key = match self.pending_row_key(scope, fingerprint).await? {
            Some(existing) => existing,
            None => row_key(&self.current_key, scope, fingerprint),
        };
        let sealed = self.seal_approval(approval, fingerprint, scope, &row_key)?;
        self.inner.decide(scope, &row_key, sealed).await
    }

    async fn take_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolConfirmationDecision>> {
        for row_key in self.row_keys(scope, fingerprint) {
            if let Some(decision) = self.inner.take_decision(scope, &row_key).await? {
                return Ok(Some(decision));
            }
        }
        Ok(None)
    }

    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        // The trait's default on real fingerprints: a call no longer pending may have a
        // decision; `take_approval` then finds it under whichever key sealed it.
        Ok(!self.pending(scope).await?.iter().any(|entry| entry.fingerprint == fingerprint))
    }

    async fn take_approval(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolApproval>> {
        for row_key in self.row_keys(scope, fingerprint) {
            if let Some(approval) = self.inner.take_approval(scope, &row_key).await? {
                let (approval, _) = self.open_approval(approval, scope, &row_key)?;
                return Ok(Some(approval));
            }
        }
        Ok(None)
    }

    async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let stored = self.inner.create_request(&self.seal_approval_request(request)?).await?;
        self.open_approval_request(&stored)
    }

    async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        match self.inner.get_request(id).await? {
            Some(request) => Ok(Some(self.open_approval_request(&request)?)),
            None => Ok(None),
        }
    }

    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> Result<bool> {
        self.inner.replace_request(&self.seal_approval_request(request)?, expected_version).await
    }

    async fn list_requests(&self, query: &ApprovalRequestQuery) -> Result<Vec<ApprovalRequest>> {
        self.inner
            .list_requests(query)
            .await?
            .iter()
            .map(|request| self.open_approval_request(request))
            .collect()
    }

    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
        self.inner.put_delegation(delegation).await
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        self.inner.delegations_to(to).await
    }

    async fn delete_delegation(&self, id: &str) -> Result<bool> {
        self.inner.delete_delegation(id).await
    }
}

/// Key rotation over the SQLite store (spec 3.1 §7.12).
#[cfg(feature = "sqlite")]
impl EncryptedApprovalStore<crate::SqliteApprovalStore> {
    /// Re-keys and re-seals every held call, decision and request a previous key sealed
    /// with the current key, and returns how many rows changed.
    ///
    /// # Errors
    ///
    /// Returns `session.encryption.decrypt_failed` when a value opens under none of the
    /// keys, `session.encryption.unencrypted_data` when a row holds plaintext, or the
    /// database error.
    pub async fn rewrap_sqlite(&self) -> Result<u64> {
        use sqlx::Row;
        let pool = self.inner.pool().clone();
        let db = |e: sqlx::Error| {
            error(
                "session.approval_store.storage",
                format!("re-sealing the approval store failed: {e}"),
            )
        };
        let under_current = |text: &str, aad: &[u8]| {
            envelope::open(std::iter::once(&self.current_key), text, aad).is_some()
        };
        let mut rewritten = 0u64;

        let rows = sqlx::query(
            "SELECT app_name, user_id, session_id, fingerprint, request FROM adk_approval_pending",
        )
        .fetch_all(&pool)
        .await
        .map_err(db)?;
        for row in rows {
            let scope = ApprovalScope::new(
                row.get::<String, _>("app_name"),
                row.get::<String, _>("user_id"),
                row.get::<String, _>("session_id"),
            );
            let old_row_key: String = row.get("fingerprint");
            let stored: ToolConfirmationRequest =
                serde_json::from_str(&row.get::<String, _>("request"))
                    .map_err(serialization_error)?;
            let sealed_args = stored.args.as_str().filter(|text| text.starts_with(ENVELOPE_PREFIX));
            let (opened, fingerprint) = match sealed_args {
                Some(text) if under_current(text, &pending_aad(&scope, &old_row_key)) => continue,
                Some(_) => self.open_call(&stored, &scope, &old_row_key)?,
                // A plaintext row written before the key: its row key is the fingerprint.
                None if self.legacy_migration => (stored.clone(), old_row_key.clone()),
                None => return Err(unencrypted_error("held call arguments")),
            };
            let new_row_key = row_key(&self.current_key, &scope, &fingerprint);
            let sealed = self.seal_call(&opened, &fingerprint, &scope, &new_row_key)?;
            sqlx::query(
                "UPDATE adk_approval_pending SET fingerprint = ?, request = ? \
                 WHERE app_name = ? AND user_id = ? AND session_id = ? AND fingerprint = ?",
            )
            .bind(&new_row_key)
            .bind(serde_json::to_string(&sealed).map_err(serialization_error)?)
            .bind(&scope.app_name)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .bind(&old_row_key)
            .execute(&pool)
            .await
            .map_err(db)?;
            rewritten += 1;
        }

        let rows = sqlx::query(
            "SELECT app_name, user_id, session_id, fingerprint, approval FROM adk_approval_decisions",
        )
        .fetch_all(&pool)
        .await
        .map_err(db)?;
        for row in rows {
            let scope = ApprovalScope::new(
                row.get::<String, _>("app_name"),
                row.get::<String, _>("user_id"),
                row.get::<String, _>("session_id"),
            );
            let old_row_key: String = row.get("fingerprint");
            let stored: ToolApproval = serde_json::from_str(&row.get::<String, _>("approval"))
                .map_err(serialization_error)?;
            let sealed_blob =
                stored.message.as_deref().filter(|text| text.starts_with(ENVELOPE_PREFIX));
            let (opened, fingerprint) = match sealed_blob {
                Some(blob) if under_current(blob, &decision_aad(&scope, &old_row_key)) => continue,
                Some(_) => self.open_approval(stored, &scope, &old_row_key)?,
                None if self.legacy_migration => (stored, old_row_key.clone()),
                None => return Err(unencrypted_error("decision")),
            };
            let new_row_key = row_key(&self.current_key, &scope, &fingerprint);
            let sealed = self.seal_approval(opened, &fingerprint, &scope, &new_row_key)?;
            sqlx::query(
                "UPDATE adk_approval_decisions SET fingerprint = ?, approval = ? \
                 WHERE app_name = ? AND user_id = ? AND session_id = ? AND fingerprint = ?",
            )
            .bind(&new_row_key)
            .bind(serde_json::to_string(&sealed).map_err(serialization_error)?)
            .bind(&scope.app_name)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .bind(&old_row_key)
            .execute(&pool)
            .await
            .map_err(db)?;
            rewritten += 1;
        }

        let rows = sqlx::query("SELECT id, record FROM adk_approval_requests")
            .fetch_all(&pool)
            .await
            .map_err(db)?;
        for row in rows {
            let id: String = row.get("id");
            let stored: ApprovalRequest = serde_json::from_str(&row.get::<String, _>("record"))
                .map_err(serialization_error)?;
            let opened = if stored.summary.starts_with(ENVELOPE_PREFIX) {
                if under_current(&stored.summary, &request_aad(&stored.scope, &stored.id)) {
                    continue;
                }
                self.open_approval_request(&stored)?
            } else if self.legacy_migration {
                stored
            } else {
                return Err(unencrypted_error("request summary"));
            };
            let resealed = self.seal_approval_request(&opened)?;
            sqlx::query("UPDATE adk_approval_requests SET record = ? WHERE id = ?")
                .bind(serde_json::to_string(&resealed).map_err(serialization_error)?)
                .bind(&id)
                .execute(&pool)
                .await
                .map_err(db)?;
            rewritten += 1;
        }
        Ok(rewritten)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::InMemoryApprovalStore;
    use serde_json::json;

    fn scope() -> ApprovalScope {
        ApprovalScope::new("app", "alice", "s1")
    }

    #[tokio::test]
    async fn arguments_and_decisions_reach_the_inner_store_sealed_under_a_keyed_row_key() {
        let key = EncryptionKey::generate();
        let store = EncryptedApprovalStore::new(InMemoryApprovalStore::new(), key, vec![]);
        let request = ToolConfirmationRequest::new("bash", json!({ "command": "rm -rf build" }))
            .with_function_call_id("call-1");
        let fingerprint = request.fingerprint();
        store.record_pending(&scope(), &request).await.unwrap();

        let raw = store.inner().pending(&scope()).await.unwrap();
        assert!(raw[0].fingerprint.starts_with(ROW_KEY_PREFIX), "{}", raw[0].fingerprint);
        assert!(!raw[0].fingerprint.contains("rm -rf"), "the row key spells no arguments");
        assert!(raw[0].request.args.as_str().unwrap().starts_with(ENVELOPE_PREFIX));
        assert!(!serde_json::to_string(&raw[0].request).unwrap().contains("rm -rf"));

        let opened = store.pending(&scope()).await.unwrap();
        assert_eq!(opened[0].request, request);
        assert_eq!(opened[0].fingerprint, fingerprint, "the real fingerprint comes back");
        assert!(!store.has_decision(&scope(), &fingerprint).await.unwrap());

        let approval = ToolApproval {
            edited_args: Some(json!({ "command": "rm -rf target" })),
            message: Some("only the build dir".into()),
            ..ToolApproval::approve()
        };
        store.decide(&scope(), &fingerprint, approval.clone()).await.unwrap();
        assert!(store.pending(&scope()).await.unwrap().is_empty(), "deciding clears the row");
        assert!(store.has_decision(&scope(), &fingerprint).await.unwrap());
        let raw_row = row_key(&store.current_key, &scope(), &fingerprint);
        let raw = store.inner().take_approval(&scope(), &raw_row).await.unwrap().unwrap();
        assert!(raw.edited_args.is_none() && raw.message.unwrap().starts_with(ENVELOPE_PREFIX));
        store.decide(&scope(), &fingerprint, approval.clone()).await.unwrap();
        assert_eq!(store.take_approval(&scope(), &fingerprint).await.unwrap(), Some(approval));
        assert_eq!(store.take_approval(&scope(), &fingerprint).await.unwrap(), None, "consumed");
    }

    #[tokio::test]
    async fn equal_calls_in_different_sessions_get_different_row_keys() {
        let key = EncryptionKey::generate();
        let request = ToolConfirmationRequest::new("bash", json!({ "command": "ls" }));
        let a = row_key(&key, &ApprovalScope::new("app", "alice", "s1"), &request.fingerprint());
        let b = row_key(&key, &ApprovalScope::new("app", "alice", "s2"), &request.fingerprint());
        assert_ne!(a, b);
        assert_eq!(
            a,
            row_key(&key, &ApprovalScope::new("app", "alice", "s1"), &request.fingerprint())
        );
    }

    #[tokio::test]
    async fn a_wrong_key_fails_the_read_instead_of_falling_back() {
        let store = EncryptedApprovalStore::new(
            InMemoryApprovalStore::new(),
            EncryptionKey::generate(),
            vec![],
        );
        let request = ToolConfirmationRequest::new("bash", json!({ "command": "ls" }));
        store.record_pending(&scope(), &request).await.unwrap();
        let other =
            EncryptedApprovalStore::new(store.into_inner(), EncryptionKey::generate(), vec![]);
        let error = other.pending(&scope()).await.unwrap_err();
        assert_eq!(error.code, "session.encryption.decrypt_failed");

        let plain = InMemoryApprovalStore::new();
        plain.record_pending(&scope(), &request).await.unwrap();
        let wrapped = EncryptedApprovalStore::new(plain, EncryptionKey::generate(), vec![]);
        assert_eq!(
            wrapped.pending(&scope()).await.unwrap_err().code,
            "session.encryption.unencrypted_data"
        );
    }

    #[tokio::test]
    async fn a_previous_key_still_opens_and_decides_what_it_sealed() {
        let old = EncryptionKey::generate();
        let new = EncryptionKey::generate();
        let store = EncryptedApprovalStore::new(InMemoryApprovalStore::new(), old, vec![]);
        let request = ToolConfirmationRequest::new("bash", json!({ "command": "ls" }));
        store.record_pending(&scope(), &request).await.unwrap();
        let old = EncryptionKey::from_bytes(store.current_key.as_bytes()).unwrap();
        let rotated = EncryptedApprovalStore::new(store.into_inner(), new, vec![old]);
        let pending = rotated.pending(&scope()).await.unwrap();
        assert_eq!(pending[0].request, request);
        // The decision clears the row the previous key wrote and is found afterwards.
        rotated.decide(&scope(), &request.fingerprint(), ToolApproval::approve()).await.unwrap();
        assert!(rotated.pending(&scope()).await.unwrap().is_empty());
        assert_eq!(
            rotated.take_approval(&scope(), &request.fingerprint()).await.unwrap(),
            Some(ToolApproval::approve())
        );
    }
}
