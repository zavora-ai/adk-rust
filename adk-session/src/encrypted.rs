//! Transparent encryption wrapper for any [`SessionService`] implementation.
//!
//! [`EncryptedSession`] encrypts session state at rest using AES-256-GCM before
//! delegating to an inner [`SessionService`]. On read, it decrypts the state
//! transparently. Key rotation is supported by accepting a current key and
//! optional previous keys.
//!
//! # Storage Format
//!
//! Encrypted state is serialized to JSON, encrypted with AES-256-GCM using a
//! random 96-bit nonce, and stored as a base64 string under the key
//! `__encrypted_state` in the inner service. The raw bytes are
//! `[12-byte nonce][ciphertext]`.
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
};
use crate::session::Session;
use crate::state::State;
use crate::{Event, Events};

use adk_core::{AdkError, Result};
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde_json::Value;
use std::collections::HashMap;

/// Key used to store the encrypted state blob in the inner service.
const ENCRYPTED_STATE_KEY: &str = "__encrypted_state";

/// Transparent encryption wrapper for any [`SessionService`].
///
/// Encrypts session state with AES-256-GCM before writing to the inner service
/// and decrypts after reading. Supports key rotation via `previous_keys`.
pub struct EncryptedSession<S: SessionService> {
    inner: S,
    current_key: EncryptionKey,
    previous_keys: Vec<EncryptionKey>,
}

impl<S: SessionService> EncryptedSession<S> {
    /// Create a new encrypted session wrapper.
    ///
    /// # Arguments
    ///
    /// * `inner` — the underlying session service to delegate to
    /// * `current_key` — the active encryption key for new writes
    /// * `previous_keys` — older keys to try during decryption (for rotation)
    pub fn new(inner: S, current_key: EncryptionKey, previous_keys: Vec<EncryptionKey>) -> Self {
        Self { inner, current_key, previous_keys }
    }

    /// Encrypt a state map into a single-entry map with the encrypted blob.
    fn encrypt_state(&self, state: &HashMap<String, Value>) -> Result<HashMap<String, Value>> {
        let plaintext = serde_json::to_vec(state)
            .map_err(|e| AdkError::session(format!("failed to serialize state: {e}")))?;

        let encrypted = encrypt_bytes(self.current_key.as_bytes(), &plaintext)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&encrypted);

        let mut wrapped = HashMap::new();
        wrapped.insert(ENCRYPTED_STATE_KEY.to_string(), Value::String(encoded));
        Ok(wrapped)
    }

    /// Decrypt a state map that contains an encrypted blob.
    ///
    /// Tries the current key first, then each previous key in order.
    /// Returns the decrypted state map on success.
    fn decrypt_state(&self, state: &HashMap<String, Value>) -> Result<HashMap<String, Value>> {
        let encoded = match state.get(ENCRYPTED_STATE_KEY) {
            Some(Value::String(s)) => s,
            _ => {
                // No encrypted state key — return as-is (unencrypted session)
                return Ok(state.clone());
            }
        };

        let encrypted = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| AdkError::session(format!("invalid base64 in encrypted state: {e}")))?;

        // Try current key first
        if let Ok(plaintext) = decrypt_bytes(self.current_key.as_bytes(), &encrypted) {
            return parse_state(&plaintext);
        }

        // Try previous keys in order
        for prev_key in &self.previous_keys {
            if let Ok(plaintext) = decrypt_bytes(prev_key.as_bytes(), &encrypted) {
                return parse_state(&plaintext);
            }
        }

        Err(AdkError::session("decryption failed: no matching key"))
    }

    /// Decrypt state, and if a previous key was used, re-encrypt with the
    /// current key. Returns `(decrypted_state, needs_reencrypt)`.
    fn decrypt_state_with_rotation(
        &self,
        state: &HashMap<String, Value>,
    ) -> Result<(HashMap<String, Value>, bool)> {
        let encoded = match state.get(ENCRYPTED_STATE_KEY) {
            Some(Value::String(s)) => s,
            _ => return Ok((state.clone(), false)),
        };

        let encrypted = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| AdkError::session(format!("invalid base64 in encrypted state: {e}")))?;

        // Try current key first
        if let Ok(plaintext) = decrypt_bytes(self.current_key.as_bytes(), &encrypted) {
            return Ok((parse_state(&plaintext)?, false));
        }

        // Try previous keys
        for prev_key in &self.previous_keys {
            if let Ok(plaintext) = decrypt_bytes(prev_key.as_bytes(), &encrypted) {
                return Ok((parse_state(&plaintext)?, true));
            }
        }

        Err(AdkError::session("decryption failed: no matching key"))
    }
}

/// Encrypt plaintext bytes with AES-256-GCM. Returns `[nonce || ciphertext]`.
fn encrypt_bytes(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| AdkError::session(format!("failed to create cipher: {e}")))?;

    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| AdkError::session(format!("encryption failed: {e}")))?;

    let mut result = Vec::with_capacity(12 + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Decrypt `[nonce || ciphertext]` with AES-256-GCM.
fn decrypt_bytes(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 12 {
        return Err(AdkError::session("encrypted data too short: missing nonce"));
    }

    let (nonce_bytes, ciphertext) = data.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| AdkError::session(format!("failed to create cipher: {e}")))?;
    let nonce = Nonce::from_slice(nonce_bytes);

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| AdkError::session(format!("decryption failed: {e}")))
}

/// Parse decrypted bytes back into a state map.
fn parse_state(plaintext: &[u8]) -> Result<HashMap<String, Value>> {
    serde_json::from_slice(plaintext)
        .map_err(|e| AdkError::session(format!("failed to deserialize decrypted state: {e}")))
}

#[async_trait]
impl<S: SessionService> SessionService for EncryptedSession<S> {
    async fn create(&self, mut req: CreateRequest) -> Result<Box<dyn Session>> {
        // Encrypt the state before delegating to inner
        if !req.state.is_empty() {
            req.state = self.encrypt_state(&req.state)?;
        }
        let session = self.inner.create(req).await?;
        // Decrypt the state in the returned session
        let decrypted = self.decrypt_state(&session.state().all())?;
        Ok(Box::new(DecryptedSession::new(session, decrypted)))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let session = self.inner.get(req).await?;
        let raw_state = session.state().all();
        let (decrypted, needs_reencrypt) = self.decrypt_state_with_rotation(&raw_state)?;

        if needs_reencrypt {
            // Re-encrypt with current key by updating the inner service
            let re_encrypted = self.encrypt_state(&decrypted)?;
            let update_req = CreateRequest {
                app_name: session.app_name().to_string(),
                user_id: session.user_id().to_string(),
                session_id: Some(session.id().to_string()),
                state: re_encrypted,
            };
            // Best-effort re-encryption — update inner store
            let _ = self.inner.create(update_req).await;
        }

        Ok(Box::new(DecryptedSession::new(session, decrypted)))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        // Delegate directly — list doesn't need decryption of state
        self.inner.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        self.inner.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        self.inner.append_event(session_id, event).await
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        self.inner.append_event_for_identity(req).await
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        self.inner.delete_all_sessions(app_name, user_id).await
    }

    async fn health_check(&self) -> Result<()> {
        self.inner.health_check().await
    }
}

/// A session wrapper that presents decrypted state while preserving
/// all other session data from the inner service.
struct DecryptedSession {
    inner: Box<dyn Session>,
    decrypted_state: HashMap<String, Value>,
}

impl DecryptedSession {
    fn new(inner: Box<dyn Session>, decrypted_state: HashMap<String, Value>) -> Self {
        Self { inner, decrypted_state }
    }
}

impl Session for DecryptedSession {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn user_id(&self) -> &str {
        self.inner.user_id()
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn events(&self) -> &dyn Events {
        self.inner.events()
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.inner.last_update_time()
    }
}

impl State for DecryptedSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.decrypted_state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        self.decrypted_state.insert(key, value);
    }

    fn all(&self) -> HashMap<String, Value> {
        self.decrypted_state.clone()
    }
}

impl Events for DecryptedSession {
    fn all(&self) -> Vec<Event> {
        self.inner.events().all()
    }

    fn len(&self) -> usize {
        self.inner.events().len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.inner.events().at(index)
    }
}
