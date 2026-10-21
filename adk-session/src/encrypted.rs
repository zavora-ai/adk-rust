//! Transparent encryption wrapper for any [`SessionService`] implementation.
//!
//! [`EncryptedSession`] encrypts session state and event payloads with AES-256-GCM before
//! they reach the inner [`SessionService`], and decrypts them on every read path (`create`,
//! `get`, `list`).
//!
//! # What Reaches the Inner Service
//!
//! | Data | Stored as |
//! |------|-----------|
//! | Each state value, from `create` state and from event `state_delta` | An envelope string; the key name stays in plaintext |
//! | Event payload: content, LLM response metadata, actions, long-running tool IDs | One envelope in the single text part of the event `content` |
//! | Event ID, timestamp, invocation ID, branch, author | Plaintext, so the inner service can order and filter events |
//! | `llm_request` and event `provider_metadata` | Not persisted, matching the database backends |
//!
//! State keys stay in plaintext so the inner service keeps its tier semantics. `app:` and
//! `user:` values are stored encrypted in the shared app and user tiers and stay visible to
//! every session of that app or user. `temp:` keys are dropped before encryption.
//!
//! # Envelope Format
//!
//! An envelope is `adk-enc:v1:` followed by base64 of `[12-byte nonce][ciphertext + tag]`.
//! The associated data binds every ciphertext to where it is stored, so a value or event
//! copied to another session, user, app, state key, or event ID fails authentication:
//!
//! | Data | Bound to |
//! |------|----------|
//! | Session-scoped state value | app name, user ID, session ID, state key |
//! | `user:` state value | app name, user ID, state key |
//! | `app:` state value | app name, state key |
//! | Event payload | app name, user ID, session ID, event ID |
//!
//! Nonces are random, so rotate the key well before it has encrypted 2^32 values.
//!
//! # Key Rotation
//!
//! Writes always use the current key; reads try the current key, then each previous key.
//! When `get` finds state values under a previous key, it re-encrypts them with the current
//! key by appending a state-only rotation event through
//! [`append_event_for_identity`](SessionService::append_event_for_identity) and returns any
//! failure. Rotation events are not included in the events returned to callers.
//!
//! `SessionService` has no way to rewrite stored events, so events written under a previous
//! key stay readable only while that key remains in `previous_keys`. Keep a retired key until
//! the sessions that used it are deleted or expire. A concurrent write to a key between the
//! read and the rotation append can be overwritten with the re-encrypted older value.
//!
//! # Addressing
//!
//! Ciphertext is bound to the full `(app_name, user_id, session_id)` identity, which a bare
//! session ID does not determine, so [`append_event`](SessionService::append_event) returns a
//! `session.encryption.identity_required` error. Use
//! [`append_event_for_identity`](SessionService::append_event_for_identity), which the
//! runner uses. `create` without a `session_id` generates a UUID v4 in the wrapper so the
//! initial state can be bound to the session.
//!
//! `rewind` and `rewind_steps` forward to the inner service and decrypt its result.
//! `rewind_steps` counts only the events callers see, so key-rotation events are not steps.
//!
//! # Data Written by Earlier Releases
//!
//! Earlier releases stored `create` state as one `__encrypted_state` blob without associated
//! data and passed event content and state deltas through in plaintext. Reads reject such
//! data by default with `session.encryption.unencrypted_data`.
//! [`with_legacy_migration`](EncryptedSession::with_legacy_migration) accepts it: the blob is
//! decrypted, plaintext values and events are returned as stored, and `get` re-encrypts the
//! state into the current format. Plaintext events stay plaintext in the inner service, so a
//! session that holds them stays readable only with legacy migration enabled.
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};
//!
//! let inner = InMemorySessionService::new();
//! let key = EncryptionKey::generate();
//! let service = EncryptedSession::new(inner, key, vec![]);
//! ```

use crate::encryption_key::EncryptionKey;
use crate::service::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, ListRequest, SessionService,
    rewind_steps_exceed, validate_scope, validate_session_id,
};
use crate::session::{KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER, Session};
use crate::state::State;
use crate::{Event, Events};

use crate::envelope::{self, ENVELOPE_PREFIX};
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, Content, ErrorCategory, ErrorComponent, Part, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

/// State key that held the whole encrypted state in earlier releases.
const LEGACY_STATE_KEY: &str = "__encrypted_state";
/// Author of the state-only events that carry re-encrypted values.
const ROTATION_AUTHOR: &str = "__adk_session_key_rotation__";
/// Domain separator prepended to all associated data.
const AAD_DOMAIN: &[u8] = b"adk-session/encrypted-session/v1";

/// Transparent encryption wrapper for any [`SessionService`].
///
/// Encrypts state values and event payloads with AES-256-GCM before writing to the inner
/// service and decrypts them on read. See the [module documentation](self) for the storage
/// format, what stays in plaintext, and how key rotation behaves.
///
/// # Example
///
/// ```rust,no_run
/// use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};
///
/// # fn main() -> adk_core::Result<()> {
/// let current = EncryptionKey::from_env("SESSION_KEY")?;
/// let retired = EncryptionKey::from_env("SESSION_KEY_PREVIOUS")?;
/// let service = EncryptedSession::new(InMemorySessionService::new(), current, vec![retired]);
/// # Ok(())
/// # }
/// ```
pub struct EncryptedSession<S: SessionService> {
    inner: S,
    current_key: EncryptionKey,
    previous_keys: Vec<EncryptionKey>,
    legacy_migration: bool,
}

/// Borrowed `(app_name, user_id, session_id)` triple that ciphertext is bound to.
#[derive(Clone, Copy)]
struct SessionIdentity<'a> {
    app_name: &'a str,
    user_id: &'a str,
    session_id: &'a str,
}

impl<'a> From<&'a AdkIdentity> for SessionIdentity<'a> {
    fn from(identity: &'a AdkIdentity) -> Self {
        Self {
            app_name: identity.app_name.as_ref(),
            user_id: identity.user_id.as_ref(),
            session_id: identity.session_id.as_ref(),
        }
    }
}

/// Which key opened an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyUsed {
    Current,
    Previous,
}

/// Decrypted state plus what `get` must re-encrypt with the current key.
#[derive(Default)]
struct OpenedState {
    state: HashMap<String, Value>,
    /// Values read under a previous key or from legacy data.
    stale: HashMap<String, Value>,
    /// Whether a legacy `__encrypted_state` blob is still stored.
    legacy_blob: bool,
}

/// Builds length-prefixed associated data so distinct component lists never collide.
fn associated_data(parts: &[&str]) -> Vec<u8> {
    envelope::associated_data(AAD_DOMAIN, parts)
}

/// Associated data for a state value, scoped by the tier its key prefix selects.
fn state_aad(id: &SessionIdentity<'_>, key: &str) -> Vec<u8> {
    if key.starts_with(KEY_PREFIX_APP) {
        associated_data(&["state", "app", id.app_name, key])
    } else if key.starts_with(KEY_PREFIX_USER) {
        associated_data(&["state", "user", id.app_name, id.user_id, key])
    } else {
        associated_data(&["state", "session", id.app_name, id.user_id, id.session_id, key])
    }
}

fn event_aad(id: &SessionIdentity<'_>, event_id: &str) -> Vec<u8> {
    associated_data(&["event", id.app_name, id.user_id, id.session_id, event_id])
}

fn encryption_error(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Session, ErrorCategory::Internal, code, message)
}

fn decrypt_error(what: &str) -> AdkError {
    encryption_error(
        "session.encryption.decrypt_failed",
        format!(
            "failed to decrypt {what}: no configured key authenticates it; the data was written \
             with an unknown key or was moved from another session, key, or event"
        ),
    )
}

fn unencrypted_error(what: &str) -> AdkError {
    encryption_error(
        "session.encryption.unencrypted_data",
        format!(
            "{what} is not encrypted by EncryptedSession; data written by an earlier \
             adk-session release is read only with EncryptedSession::with_legacy_migration(true)"
        ),
    )
}

fn serialization_error(error: serde_json::Error) -> AdkError {
    encryption_error(
        "session.encryption.serialization_failed",
        format!("failed to (de)serialize encrypted session data: {error}"),
    )
}

/// Returns the envelope body of an event stored by this wrapper, if it has one.
fn event_envelope(stored: &Event) -> Option<&str> {
    match stored.llm_response.content.as_ref()?.parts.as_slice() {
        [Part::Text { text }] => text.strip_prefix(ENVELOPE_PREFIX),
        _ => None,
    }
}

impl<S: SessionService> EncryptedSession<S> {
    /// Create a new encrypted session wrapper.
    ///
    /// # Arguments
    ///
    /// * `inner` — the underlying session service to delegate to
    /// * `current_key` — the active encryption key for new writes
    /// * `previous_keys` — older keys to try during decryption (for rotation)
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};
    ///
    /// let service = EncryptedSession::new(
    ///     InMemorySessionService::new(),
    ///     EncryptionKey::generate(),
    ///     vec![],
    /// );
    /// ```
    pub fn new(inner: S, current_key: EncryptionKey, previous_keys: Vec<EncryptionKey>) -> Self {
        Self { inner, current_key, previous_keys, legacy_migration: false }
    }

    /// Accepts data written by adk-session releases before the envelope format.
    ///
    /// When enabled, reads decrypt the legacy `__encrypted_state` blob and return plaintext
    /// state values and events as stored; `get` then re-encrypts the state into the current
    /// format. Plaintext from the inner service is not authenticated, so enable this only
    /// while migrating existing sessions.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};
    ///
    /// let service = EncryptedSession::new(
    ///     InMemorySessionService::new(),
    ///     EncryptionKey::generate(),
    ///     vec![],
    /// )
    /// .with_legacy_migration(true);
    /// ```
    pub fn with_legacy_migration(mut self, enabled: bool) -> Self {
        self.legacy_migration = enabled;
        self
    }

    /// Returns the wrapped session service.
    ///
    /// Reads through it return ciphertext; writes through it bypass encryption.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};
    ///
    /// let service =
    ///     EncryptedSession::new(InMemorySessionService::new(), EncryptionKey::generate(), vec![]);
    /// let _raw: &InMemorySessionService = service.inner();
    /// ```
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Consumes the wrapper and returns the wrapped session service.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};
    ///
    /// let old = EncryptionKey::from_bytes(&[1; 32]).unwrap();
    /// let service = EncryptedSession::new(InMemorySessionService::new(), old, vec![]);
    ///
    /// // Re-wrap the same store with a new current key and the old key for reads.
    /// let rotated = EncryptedSession::new(
    ///     service.into_inner(),
    ///     EncryptionKey::generate(),
    ///     vec![EncryptionKey::from_bytes(&[1; 32]).unwrap()],
    /// );
    /// ```
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Encrypts `plaintext` with the current key and returns the envelope string.
    fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<String> {
        envelope::seal(&self.current_key, plaintext, aad)
    }

    /// Decrypts the envelope body `encoded` (without its prefix) with the first configured
    /// key that authenticates it.
    fn open(&self, encoded: &str, aad: &[u8]) -> Option<(Vec<u8>, KeyUsed)> {
        let keys = std::iter::once(&self.current_key).chain(&self.previous_keys);
        envelope::open(keys, &format!("{ENVELOPE_PREFIX}{encoded}"), aad).map(
            |(plaintext, index)| {
                (plaintext, if index == 0 { KeyUsed::Current } else { KeyUsed::Previous })
            },
        )
    }

    /// Encrypts every non-`temp:` value of `state`, keeping the key names.
    fn seal_state(
        &self,
        id: &SessionIdentity<'_>,
        state: &HashMap<String, Value>,
    ) -> Result<HashMap<String, Value>> {
        state
            .iter()
            .filter(|(key, _)| !key.starts_with(KEY_PREFIX_TEMP))
            .map(|(key, value)| {
                if key == LEGACY_STATE_KEY {
                    return Err(AdkError::new(
                        ErrorComponent::Session,
                        ErrorCategory::InvalidInput,
                        "session.encryption.reserved_key",
                        format!("state key '{LEGACY_STATE_KEY}' is reserved by EncryptedSession"),
                    ));
                }
                let plaintext = serde_json::to_vec(value).map_err(serialization_error)?;
                Ok((key.clone(), Value::String(self.seal(&plaintext, &state_aad(id, key))?)))
            })
            .collect()
    }

    /// Decrypts every state value returned by the inner service.
    fn open_state(
        &self,
        id: &SessionIdentity<'_>,
        raw: &HashMap<String, Value>,
    ) -> Result<OpenedState> {
        let mut opened = OpenedState::default();
        let mut legacy_state: HashMap<String, Value> = HashMap::new();

        for (key, value) in raw {
            if key == LEGACY_STATE_KEY {
                match (value, self.legacy_migration) {
                    // Tombstone written once the blob has been migrated.
                    (Value::Null, _) => {}
                    (Value::String(blob), true) => {
                        let (plaintext, _) = self
                            .open(blob, b"")
                            .ok_or_else(|| decrypt_error("the legacy encrypted state blob"))?;
                        legacy_state =
                            serde_json::from_slice(&plaintext).map_err(serialization_error)?;
                        opened.legacy_blob = true;
                    }
                    (_, _) => {
                        return Err(unencrypted_error(&format!(
                            "the legacy '{LEGACY_STATE_KEY}' state blob"
                        )));
                    }
                }
                continue;
            }

            match value.as_str().and_then(|s| s.strip_prefix(ENVELOPE_PREFIX)) {
                Some(encoded) => {
                    let (plaintext, key_used) = self
                        .open(encoded, &state_aad(id, key))
                        .ok_or_else(|| decrypt_error(&format!("state key '{key}'")))?;
                    let value: Value =
                        serde_json::from_slice(&plaintext).map_err(serialization_error)?;
                    if key_used == KeyUsed::Previous {
                        opened.stale.insert(key.clone(), value.clone());
                    }
                    opened.state.insert(key.clone(), value);
                }
                None if self.legacy_migration => {
                    opened.stale.insert(key.clone(), value.clone());
                    opened.state.insert(key.clone(), value.clone());
                }
                None => return Err(unencrypted_error(&format!("state key '{key}'"))),
            }
        }

        // Per-key values were written after the legacy blob, so they take precedence.
        for (key, value) in legacy_state {
            if key.starts_with(KEY_PREFIX_TEMP) || opened.state.contains_key(&key) {
                continue;
            }
            opened.stale.insert(key.clone(), value.clone());
            opened.state.insert(key, value);
        }

        Ok(opened)
    }

    /// Returns the form of `event` stored in the inner service.
    fn seal_event(&self, id: &SessionIdentity<'_>, event: &Event) -> Result<Event> {
        let mut payload = event.clone();
        payload.actions.state_delta.retain(|key, _| !key.starts_with(KEY_PREFIX_TEMP));
        payload.llm_request = None;
        payload.provider_metadata.clear();
        let plaintext = serde_json::to_vec(&payload).map_err(serialization_error)?;

        let mut stored = Event::with_id(event.id.clone(), event.invocation_id.clone());
        stored.timestamp = event.timestamp;
        stored.branch = event.branch.clone();
        stored.author = event.author.clone();
        stored.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: self.seal(&plaintext, &event_aad(id, &event.id))? }],
        });
        stored.actions.state_delta = self.seal_state(id, &payload.actions.state_delta)?;
        Ok(stored)
    }

    /// Recovers the original event from its stored form.
    fn open_event(&self, id: &SessionIdentity<'_>, stored: &Event) -> Result<Event> {
        let Some(encoded) = event_envelope(stored) else {
            if self.legacy_migration {
                return Ok(stored.clone());
            }
            return Err(unencrypted_error(&format!("event '{}'", stored.id)));
        };
        let (plaintext, _) = self
            .open(encoded, &event_aad(id, &stored.id))
            .ok_or_else(|| decrypt_error(&format!("event '{}'", stored.id)))?;
        serde_json::from_slice(&plaintext).map_err(serialization_error)
    }

    /// Decrypts a session returned by the inner service for identity `id`.
    fn open_session(
        &self,
        id: &SessionIdentity<'_>,
        session: &dyn Session,
    ) -> Result<(DecryptedSession, OpenedState)> {
        let mut opened = self.open_state(id, &session.state().all())?;
        let mut events = Vec::with_capacity(session.events().len());
        for stored in session.events().all() {
            let event = self.open_event(id, &stored)?;
            if event.author != ROTATION_AUTHOR {
                events.push(event);
            }
        }

        let decrypted = DecryptedSession {
            app_name: id.app_name.to_string(),
            user_id: id.user_id.to_string(),
            session_id: id.session_id.to_string(),
            state: std::mem::take(&mut opened.state),
            events,
            updated_at: session.last_update_time(),
        };
        Ok((decrypted, opened))
    }

    /// Re-encrypts `opened.stale` with the current key through a state-only event.
    async fn reencrypt(&self, id: &SessionIdentity<'_>, opened: &OpenedState) -> Result<()> {
        let mut rotation = Event::new("");
        rotation.author = ROTATION_AUTHOR.to_string();
        rotation.actions.state_delta = opened.stale.clone();
        let mut stored = self.seal_event(id, &rotation)?;
        if opened.legacy_blob {
            stored.actions.state_delta.insert(LEGACY_STATE_KEY.to_string(), Value::Null);
        }

        let identity = AdkIdentity::new(
            AppName::try_from(id.app_name)?,
            UserId::try_from(id.user_id)?,
            SessionId::try_from(id.session_id)?,
        );
        self.inner
            .append_event_for_identity(AppendEventRequest { identity, event: stored })
            .await?;
        tracing::info!(
            session.id = %id.session_id,
            state.keys = opened.stale.len(),
            "re-encrypted session state with the current key"
        );
        Ok(())
    }
}

/// Key rotation over the SQLite backend (spec 3.1 §7.12): the trait cannot rewrite stored
/// events, so the rows are re-sealed in place.
#[cfg(feature = "sqlite")]
impl EncryptedSession<crate::sqlite::SqliteSessionService> {
    /// Re-seals every state value and event a previous key sealed with the current key, in
    /// every session, app and user tier of the database, and returns how many rows changed.
    ///
    /// Build the wrapper with the new key as `current_key` and the retired keys as
    /// `previous_keys`; after this returns, the previous keys open nothing in the database
    /// and can be dropped.
    ///
    /// # Errors
    ///
    /// Returns `session.encryption.decrypt_failed` when a value opens under none of the
    /// keys, `session.encryption.unencrypted_data` when a row holds plaintext (unless
    /// legacy migration is on, which leaves it as stored), or the database error.
    pub async fn rewrap_sqlite(&self) -> Result<u64> {
        use sqlx::Row;
        let pool = self.inner.pool().clone();
        let db = |e: sqlx::Error| {
            encryption_error(
                "session.encryption.storage",
                format!("re-sealing the session store failed: {e}"),
            )
        };
        let mut rewritten = 0u64;

        let rows =
            sqlx::query("SELECT app_name, user_id, session_id, state, initial_state FROM sessions")
                .fetch_all(&pool)
                .await
                .map_err(db)?;
        for row in rows {
            let (app, user, session): (String, String, String) =
                (row.get("app_name"), row.get("user_id"), row.get("session_id"));
            let id = SessionIdentity { app_name: &app, user_id: &user, session_id: &session };
            let state: String = row.get("state");
            let initial: Option<String> = row.get("initial_state");
            let (state, changed_state) = self.rewrap_state_json(&id, &state, "")?;
            let (initial, changed_initial) = match initial {
                Some(initial) => {
                    let (text, changed) = self.rewrap_state_json(&id, &initial, "")?;
                    (Some(text), changed)
                }
                None => (None, false),
            };
            if changed_state || changed_initial {
                sqlx::query(
                    "UPDATE sessions SET state = ?, initial_state = ? \
                     WHERE app_name = ? AND user_id = ? AND session_id = ?",
                )
                .bind(state)
                .bind(initial)
                .bind(&app)
                .bind(&user)
                .bind(&session)
                .execute(&pool)
                .await
                .map_err(db)?;
                rewritten += 1;
            }
        }

        let rows = sqlx::query(
            "SELECT id, app_name, user_id, session_id, llm_response, actions FROM events",
        )
        .fetch_all(&pool)
        .await
        .map_err(db)?;
        for row in rows {
            let (event_id, app, user, session): (String, String, String, String) =
                (row.get("id"), row.get("app_name"), row.get("user_id"), row.get("session_id"));
            let id = SessionIdentity { app_name: &app, user_id: &user, session_id: &session };
            let response: String = row.get("llm_response");
            let actions: String = row.get("actions");
            let mut response: Value =
                serde_json::from_str(&response).map_err(serialization_error)?;
            let mut changed = false;
            let text = response
                .pointer_mut("/content/parts/0/text")
                .and_then(|text| text.as_str().map(str::to_string).map(|t| (text, t)));
            match text {
                Some((slot, text)) if text.starts_with(ENVELOPE_PREFIX) => {
                    let aad = event_aad(&id, &event_id);
                    if let Some(resealed) =
                        self.reseal(&text, &aad, &format!("event '{event_id}'"))?
                    {
                        *slot = Value::String(resealed);
                        changed = true;
                    }
                }
                _ if self.legacy_migration => {}
                _ => return Err(unencrypted_error(&format!("event '{event_id}'"))),
            }
            let mut actions: Value = serde_json::from_str(&actions).map_err(serialization_error)?;
            if let Some(delta) = actions.get_mut("state_delta").and_then(Value::as_object_mut)
                && self.rewrap_state_map(&id, delta, "")?
            {
                changed = true;
            }
            if changed {
                sqlx::query(
                    "UPDATE events SET llm_response = ?, actions = ? \
                     WHERE id = ? AND app_name = ? AND user_id = ? AND session_id = ?",
                )
                .bind(serde_json::to_string(&response).map_err(serialization_error)?)
                .bind(serde_json::to_string(&actions).map_err(serialization_error)?)
                .bind(&event_id)
                .bind(&app)
                .bind(&user)
                .bind(&session)
                .execute(&pool)
                .await
                .map_err(db)?;
                rewritten += 1;
            }
        }

        let rows = sqlx::query("SELECT app_name, state FROM app_states")
            .fetch_all(&pool)
            .await
            .map_err(db)?;
        for row in rows {
            let app: String = row.get("app_name");
            let id = SessionIdentity { app_name: &app, user_id: "", session_id: "" };
            let state: String = row.get("state");
            let (state, changed) = self.rewrap_state_json(&id, &state, KEY_PREFIX_APP)?;
            if changed {
                sqlx::query("UPDATE app_states SET state = ? WHERE app_name = ?")
                    .bind(state)
                    .bind(&app)
                    .execute(&pool)
                    .await
                    .map_err(db)?;
                rewritten += 1;
            }
        }

        let rows = sqlx::query("SELECT app_name, user_id, state FROM user_states")
            .fetch_all(&pool)
            .await
            .map_err(db)?;
        for row in rows {
            let (app, user): (String, String) = (row.get("app_name"), row.get("user_id"));
            let id = SessionIdentity { app_name: &app, user_id: &user, session_id: "" };
            let state: String = row.get("state");
            let (state, changed) = self.rewrap_state_json(&id, &state, KEY_PREFIX_USER)?;
            if changed {
                sqlx::query("UPDATE user_states SET state = ? WHERE app_name = ? AND user_id = ?")
                    .bind(state)
                    .bind(&app)
                    .bind(&user)
                    .execute(&pool)
                    .await
                    .map_err(db)?;
                rewritten += 1;
            }
        }
        Ok(rewritten)
    }

    /// Re-seals the envelopes of a stored JSON state map whose keys the backend stored
    /// without `prefix` (the app and user tiers); returns the text and whether any value
    /// changed.
    fn rewrap_state_json(
        &self,
        id: &SessionIdentity<'_>,
        json: &str,
        prefix: &str,
    ) -> Result<(String, bool)> {
        let mut state: Value = serde_json::from_str(json).map_err(serialization_error)?;
        let changed = match state.as_object_mut() {
            Some(map) => self.rewrap_state_map(id, map, prefix)?,
            None => false,
        };
        Ok((serde_json::to_string(&state).map_err(serialization_error)?, changed))
    }

    fn rewrap_state_map(
        &self,
        id: &SessionIdentity<'_>,
        map: &mut serde_json::Map<String, Value>,
        prefix: &str,
    ) -> Result<bool> {
        let mut changed = false;
        for (key, value) in map.iter_mut() {
            if key == LEGACY_STATE_KEY || value.is_null() {
                continue;
            }
            match value.as_str() {
                Some(text) if text.starts_with(ENVELOPE_PREFIX) => {
                    let aad = state_aad(id, &format!("{prefix}{key}"));
                    if let Some(resealed) =
                        self.reseal(text, &aad, &format!("state value '{key}'"))?
                    {
                        *value = Value::String(resealed);
                        changed = true;
                    }
                }
                _ if self.legacy_migration => {}
                _ => return Err(unencrypted_error(&format!("state value '{key}'"))),
            }
        }
        Ok(changed)
    }

    /// The envelope `text` sealed again with the current key when a previous key sealed it;
    /// `None` when the current key already did.
    fn reseal(&self, text: &str, aad: &[u8], what: &str) -> Result<Option<String>> {
        let encoded = text.strip_prefix(ENVELOPE_PREFIX).unwrap_or(text);
        let (plaintext, used) = self.open(encoded, aad).ok_or_else(|| decrypt_error(what))?;
        match used {
            KeyUsed::Current => Ok(None),
            KeyUsed::Previous => Ok(Some(self.seal(&plaintext, aad)?)),
        }
    }
}

#[async_trait]
impl<S: SessionService> SessionService for EncryptedSession<S> {
    async fn create(&self, mut req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.get_or_insert_with(|| Uuid::new_v4().to_string()).clone();
        req.try_identity()?;
        let app_name = req.app_name.clone();
        let user_id = req.user_id.clone();
        let id =
            SessionIdentity { app_name: &app_name, user_id: &user_id, session_id: &session_id };

        req.state = self.seal_state(&id, &req.state)?;
        let session = self.inner.create(req).await?;
        let (decrypted, _) = self.open_session(&id, session.as_ref())?;
        Ok(Box::new(decrypted))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        let app_name = req.app_name.clone();
        let user_id = req.user_id.clone();
        let session_id = req.session_id.clone();
        let id =
            SessionIdentity { app_name: &app_name, user_id: &user_id, session_id: &session_id };

        let session = self.inner.get(req).await?;
        let (decrypted, opened) = self.open_session(&id, session.as_ref())?;
        if !opened.stale.is_empty() || opened.legacy_blob {
            self.reencrypt(&id, &opened).await?;
        }
        Ok(Box::new(decrypted))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        validate_scope(&req.app_name, &req.user_id)?;
        let app_name = req.app_name.clone();
        let user_id = req.user_id.clone();
        let sessions = self.inner.list(req).await?;

        sessions
            .iter()
            .map(|session| {
                let id = SessionIdentity {
                    app_name: &app_name,
                    user_id: &user_id,
                    session_id: session.id(),
                };
                let (decrypted, _) = self.open_session(&id, session.as_ref())?;
                Ok(Box::new(decrypted) as Box<dyn Session>)
            })
            .collect()
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        req.try_identity()?;
        self.inner.delete(req).await
    }

    /// Always fails, with an invalid-input error for an invalid session ID: ciphertext is bound
    /// to the full session identity, which a bare session ID does not determine. Use
    /// [`append_event_for_identity`](SessionService::append_event_for_identity).
    async fn append_event(&self, session_id: &str, _event: Event) -> Result<()> {
        validate_session_id(session_id)?;
        Err(AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::Unsupported,
            "session.encryption.identity_required",
            format!(
                "EncryptedSession cannot append to session '{session_id}' by ID alone because \
                 ciphertext is bound to (app_name, user_id, session_id); call \
                 append_event_for_identity instead"
            ),
        ))
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let stored = self.seal_event(&SessionIdentity::from(&req.identity), &req.event)?;
        self.inner
            .append_event_for_identity(AppendEventRequest { identity: req.identity, event: stored })
            .await
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        validate_scope(app_name, user_id)?;
        self.inner.delete_all_sessions(app_name, user_id).await
    }

    async fn rewind(
        &self,
        identity: &AdkIdentity,
        target_event_id: &str,
    ) -> Result<Box<dyn Session>> {
        let session = self.inner.rewind(identity, target_event_id).await?;
        let (decrypted, _) =
            self.open_session(&SessionIdentity::from(identity), session.as_ref())?;
        Ok(Box::new(decrypted))
    }

    /// Counts `steps` over the events callers see; key-rotation events are stored but not
    /// returned, so they do not count.
    async fn rewind_steps(&self, identity: &AdkIdentity, steps: usize) -> Result<Box<dyn Session>> {
        if steps == 0 {
            return self.get_for_identity(identity).await;
        }
        let stored = self.inner.get_for_identity(identity).await?.events().all();
        let visible: Vec<&Event> =
            stored.iter().filter(|event| event.author != ROTATION_AUTHOR).collect();
        let keep = visible
            .len()
            .checked_sub(steps)
            .ok_or_else(|| rewind_steps_exceed(identity, steps, visible.len()))?;
        let session = match keep.checked_sub(1) {
            Some(last) => self.inner.rewind(identity, &visible[last].id).await?,
            None => self.inner.rewind_steps(identity, stored.len()).await?,
        };
        let (decrypted, _) =
            self.open_session(&SessionIdentity::from(identity), session.as_ref())?;
        Ok(Box::new(decrypted))
    }

    async fn health_check(&self) -> Result<()> {
        self.inner.health_check().await
    }
}

/// A session presenting decrypted state and events.
struct DecryptedSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for DecryptedSession {
    fn id(&self) -> &str {
        &self.session_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn events(&self) -> &dyn Events {
        self
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl State for DecryptedSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        if let Err(msg) = adk_core::validate_state_key(&key) {
            tracing::warn!(key = %key, "rejecting invalid state key: {msg}");
            return;
        }
        self.state.insert(key, value);
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.clone()
    }
}

impl Events for DecryptedSession {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn associated_data_is_unambiguous() {
        // Without length prefixes these would concatenate to the same bytes.
        assert_ne!(associated_data(&["ab", "c"]), associated_data(&["a", "bc"]));
    }

    #[test]
    fn state_values_are_bound_to_their_tier() {
        let alice = SessionIdentity { app_name: "app", user_id: "alice", session_id: "s1" };
        let alice_other_session = SessionIdentity { session_id: "s2", ..alice };
        let bob = SessionIdentity { user_id: "bob", ..alice };

        assert_eq!(state_aad(&alice, "app:x"), state_aad(&bob, "app:x"));
        assert_eq!(state_aad(&alice, "user:x"), state_aad(&alice_other_session, "user:x"));
        assert_ne!(state_aad(&alice, "user:x"), state_aad(&bob, "user:x"));
        assert_ne!(state_aad(&alice, "x"), state_aad(&alice_other_session, "x"));
        assert_ne!(state_aad(&alice, "x"), state_aad(&alice, "y"));
    }
}
