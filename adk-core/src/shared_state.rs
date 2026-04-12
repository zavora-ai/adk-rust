//! Thread-safe shared state for parallel agent coordination.
//!
//! [`SharedState`] is a concurrent key-value store scoped to a single
//! `ParallelAgent::run()` invocation. Sub-agents use [`set_shared`],
//! [`get_shared`], and [`wait_for_key`] to exchange data and coordinate.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::Value;
use tokio::sync::{Notify, RwLock};

use crate::AdkError;

/// Minimum allowed timeout for [`SharedState::wait_for_key`].
const MIN_TIMEOUT: Duration = Duration::from_millis(1);
/// Maximum allowed timeout for [`SharedState::wait_for_key`].
const MAX_TIMEOUT: Duration = Duration::from_secs(300);
/// Maximum key length in bytes.
const MAX_KEY_LEN: usize = 256;

/// Errors from [`SharedState`] operations.
#[derive(Debug, thiserror::Error)]
pub enum SharedStateError {
    /// Key must not be empty.
    #[error("shared state key must not be empty")]
    EmptyKey,

    /// Key exceeds the maximum length.
    #[error("shared state key exceeds 256 bytes: {len} bytes")]
    KeyTooLong { len: usize },

    /// `wait_for_key` timed out.
    #[error("wait_for_key timed out after {timeout:?} for key \"{key}\"")]
    Timeout { key: String, timeout: Duration },

    /// Timeout value is outside the valid range.
    #[error("invalid timeout {timeout:?}: must be between 1ms and 300s")]
    InvalidTimeout { timeout: Duration },
}

impl From<SharedStateError> for AdkError {
    fn from(err: SharedStateError) -> Self {
        AdkError::agent(err.to_string())
    }
}

/// Thread-safe key-value store for parallel agent coordination.
///
/// Scoped to a single `ParallelAgent::run()` invocation. All sub-agents
/// share the same `Arc<SharedState>` instance.
///
/// # Example
///
/// ```rust,ignore
/// use adk_core::SharedState;
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// let state = Arc::new(SharedState::new());
///
/// // Agent A publishes a workbook handle
/// state.set_shared("workbook_id", serde_json::json!("wb-123")).await?;
///
/// // Agent B waits for the handle
/// let handle = state.wait_for_key("workbook_id", Duration::from_secs(30)).await?;
/// ```
#[derive(Debug)]
pub struct SharedState {
    data: RwLock<HashMap<String, Value>>,
    notifiers: RwLock<HashMap<String, Arc<Notify>>>,
}

impl SharedState {
    /// Creates a new empty `SharedState`.
    #[must_use]
    pub fn new() -> Self {
        Self { data: RwLock::new(HashMap::new()), notifiers: RwLock::new(HashMap::new()) }
    }

    /// Inserts a key-value pair. Notifies all waiters on that key.
    ///
    /// # Errors
    ///
    /// Returns [`SharedStateError::EmptyKey`] if key is empty.
    /// Returns [`SharedStateError::KeyTooLong`] if key exceeds 256 bytes.
    pub async fn set_shared(
        &self,
        key: impl Into<String>,
        value: Value,
    ) -> Result<(), SharedStateError> {
        let key = key.into();
        validate_key(&key)?;

        self.data.write().await.insert(key.clone(), value);

        // Notify all waiters for this key
        let notifiers = self.notifiers.read().await;
        if let Some(notify) = notifiers.get(&key) {
            notify.notify_waiters();
        }

        Ok(())
    }

    /// Returns the value for a key, or `None` if not present.
    pub async fn get_shared(&self, key: &str) -> Option<Value> {
        self.data.read().await.get(key).cloned()
    }

    /// Blocks until the key appears, or the timeout expires.
    ///
    /// If the key already exists, returns immediately.
    ///
    /// # Errors
    ///
    /// Returns [`SharedStateError::Timeout`] if the timeout expires.
    /// Returns [`SharedStateError::InvalidTimeout`] if timeout is outside [1ms, 300s].
    pub async fn wait_for_key(
        &self,
        key: &str,
        timeout: Duration,
    ) -> Result<Value, SharedStateError> {
        // Validate timeout range
        if timeout < MIN_TIMEOUT || timeout > MAX_TIMEOUT {
            return Err(SharedStateError::InvalidTimeout { timeout });
        }

        // Check if key already exists
        if let Some(value) = self.data.read().await.get(key).cloned() {
            return Ok(value);
        }

        // Get or create a Notify for this key
        let notify = {
            let mut notifiers = self.notifiers.write().await;
            notifiers.entry(key.to_string()).or_insert_with(|| Arc::new(Notify::new())).clone()
        };

        // Wait with timeout, re-checking after each notification
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(SharedStateError::Timeout { key: key.to_string(), timeout });
            }

            match tokio::time::timeout(remaining, notify.notified()).await {
                Ok(()) => {
                    // Check if our key was set
                    if let Some(value) = self.data.read().await.get(key).cloned() {
                        return Ok(value);
                    }
                    // Spurious wake or different key — loop and wait again
                }
                Err(_) => {
                    return Err(SharedStateError::Timeout { key: key.to_string(), timeout });
                }
            }
        }
    }

    /// Returns a snapshot of all current entries.
    pub async fn snapshot(&self) -> HashMap<String, Value> {
        self.data.read().await.clone()
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl Serialize for SharedState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Use try_read to avoid blocking in a sync context.
        // If the lock is held, serialize as empty map.
        match self.data.try_read() {
            Ok(data) => {
                let mut map = serializer.serialize_map(Some(data.len()))?;
                for (k, v) in data.iter() {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
            Err(_) => serializer.serialize_map(Some(0))?.end(),
        }
    }
}

/// Validates a shared state key.
fn validate_key(key: &str) -> Result<(), SharedStateError> {
    if key.is_empty() {
        return Err(SharedStateError::EmptyKey);
    }
    if key.len() > MAX_KEY_LEN {
        return Err(SharedStateError::KeyTooLong { len: key.len() });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_shared_state_is_empty() {
        let state = SharedState::new();
        assert!(state.snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn set_and_get() {
        let state = SharedState::new();
        state.set_shared("key", serde_json::json!("value")).await.unwrap();
        assert_eq!(state.get_shared("key").await, Some(serde_json::json!("value")));
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let state = SharedState::new();
        assert_eq!(state.get_shared("missing").await, None);
    }

    #[tokio::test]
    async fn overwrite_replaces_value() {
        let state = SharedState::new();
        state.set_shared("key", serde_json::json!(1)).await.unwrap();
        state.set_shared("key", serde_json::json!(2)).await.unwrap();
        assert_eq!(state.get_shared("key").await, Some(serde_json::json!(2)));
    }

    #[tokio::test]
    async fn empty_key_rejected() {
        let state = SharedState::new();
        let err = state.set_shared("", serde_json::json!(1)).await.unwrap_err();
        assert!(matches!(err, SharedStateError::EmptyKey));
    }

    #[tokio::test]
    async fn long_key_rejected() {
        let state = SharedState::new();
        let long_key = "x".repeat(257);
        let err = state.set_shared(long_key, serde_json::json!(1)).await.unwrap_err();
        assert!(matches!(err, SharedStateError::KeyTooLong { .. }));
    }

    #[tokio::test]
    async fn key_at_256_bytes_accepted() {
        let state = SharedState::new();
        let key = "x".repeat(256);
        state.set_shared(key.clone(), serde_json::json!(1)).await.unwrap();
        assert_eq!(state.get_shared(&key).await, Some(serde_json::json!(1)));
    }

    #[tokio::test]
    async fn wait_for_existing_key_returns_immediately() {
        let state = SharedState::new();
        state.set_shared("key", serde_json::json!("val")).await.unwrap();
        let val = state.wait_for_key("key", Duration::from_secs(1)).await.unwrap();
        assert_eq!(val, serde_json::json!("val"));
    }

    #[tokio::test]
    async fn wait_for_key_timeout() {
        let state = SharedState::new();
        let err = state.wait_for_key("missing", Duration::from_millis(10)).await.unwrap_err();
        assert!(matches!(err, SharedStateError::Timeout { .. }));
    }

    #[tokio::test]
    async fn wait_for_key_invalid_timeout_too_small() {
        let state = SharedState::new();
        let err = state.wait_for_key("key", Duration::from_nanos(1)).await.unwrap_err();
        assert!(matches!(err, SharedStateError::InvalidTimeout { .. }));
    }

    #[tokio::test]
    async fn wait_for_key_invalid_timeout_too_large() {
        let state = SharedState::new();
        let err = state.wait_for_key("key", Duration::from_secs(301)).await.unwrap_err();
        assert!(matches!(err, SharedStateError::InvalidTimeout { .. }));
    }

    #[tokio::test]
    async fn error_converts_to_adk_error() {
        let err = SharedStateError::EmptyKey;
        let adk_err: AdkError = err.into();
        assert!(adk_err.to_string().contains("empty"));
    }
}
