//! AES-256-GCM encryption key management.
//!
//! Provides [`EncryptionKey`] for generating, loading, and managing
//! 256-bit encryption keys used by [`EncryptedSession`](super::EncryptedSession).

use adk_core::AdkError;

/// AES-256-GCM key material (256-bit / 32 bytes).
///
/// The internal bytes are never exposed through the `Debug` implementation
/// to prevent accidental key leakage in logs.
///
/// # Example
///
/// ```rust,no_run
/// use adk_session::EncryptionKey;
///
/// // Generate a random key
/// let key = EncryptionKey::generate();
///
/// // Load from environment variable
/// let key = EncryptionKey::from_env("MY_ENCRYPTION_KEY").unwrap();
///
/// // Create from raw bytes
/// let bytes = [0u8; 32];
/// let key = EncryptionKey::from_bytes(&bytes).unwrap();
/// ```
pub struct EncryptionKey {
    bytes: [u8; 32],
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionKey").field("bytes", &"[REDACTED]").finish()
    }
}

impl EncryptionKey {
    /// Generate a cryptographically random 256-bit key.
    pub fn generate() -> Self {
        use rand::Rng;
        let bytes: [u8; 32] = rand::rng().random();
        Self { bytes }
    }

    /// Read a base64-encoded key from the named environment variable.
    ///
    /// Returns an error if the variable is not set, the value is not valid
    /// base64, or the decoded bytes are not exactly 32 bytes.
    pub fn from_env(var_name: &str) -> adk_core::Result<Self> {
        use base64::Engine;

        let raw = std::env::var(var_name).map_err(|_| {
            AdkError::session(format!("environment variable {var_name} is not set"))
        })?;

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&raw)
            .map_err(|e| AdkError::session(format!("invalid base64 in {var_name}: {e}")))?;

        let len = decoded.len();
        if len != 32 {
            return Err(AdkError::session(format!(
                "{var_name} decoded to {len} bytes, expected 32"
            )));
        }

        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&decoded);
        Ok(Self { bytes })
    }

    /// Create from a byte slice that must be exactly 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> adk_core::Result<Self> {
        let len = bytes.len();
        if len != 32 {
            return Err(AdkError::session(format!(
                "encryption key must be exactly 32 bytes, got {len}"
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(Self { bytes: arr })
    }

    /// Return a reference to the raw key bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}
