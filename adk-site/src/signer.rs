//! Release signing: the key that signs site-log checkpoints (spec 3.5 §7.7, D-11, A-I5;
//! AWP 2.0 §14.5).
//!
//! A release is authenticated by its manifest digest in the site log, and the log by a
//! checkpoint signed by a delegated `release` key. [`ReleaseSigner`] is that key: callers
//! implement it over a KMS or hardware token so the key never leaves it, and
//! [`Ed25519ReleaseSigner`] holds an Ed25519 seed in memory, zeroed on drop. Signatures are
//! C2SP signed notes made with `adk-auth`, whose key name is the checkpoint origin.

use std::sync::Arc;

use adk_auth::signed_audit::{
    AuditSigner, Ed25519Signer, NoteError, NoteVerifier, SignatureAlgorithm,
};
use async_trait::async_trait;

use crate::error::{Result, SiteError};

/// Signs site-log checkpoints with a delegated `release` key; the key never leaves it
/// (spec 3.5 A-I5).
#[async_trait]
pub trait ReleaseSigner: Send + Sync {
    /// The note key name: the checkpoint origin, `{domain}/.well-known/awp/log`.
    fn key_name(&self) -> &str;

    /// The raw Ed25519 public key.
    fn public_key(&self) -> &[u8];

    /// Signs `message`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Signing`] when the key cannot sign.
    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>>;

    /// The note verifier key, `<name>+<hash>+<base64 key>`, for `keys.json` and verifiers.
    fn verifier_key(&self) -> String {
        NoteVerifier::from_public_key(
            self.key_name(),
            SignatureAlgorithm::Ed25519,
            self.public_key(),
        )
        .map(|v| v.to_string())
        .unwrap_or_default()
    }
}

/// The checkpoint origin of a site: `{domain}/.well-known/awp/log` (AWP 2.0 §14.5).
pub fn log_origin(domain: &str) -> String {
    format!("{domain}/.well-known/awp/log")
}

/// An Ed25519 [`ReleaseSigner`] over an `adk-auth` note signer.
///
/// # Example
///
/// ```rust
/// use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
///
/// let signer = Ed25519ReleaseSigner::from_seed("example.com", [7; 32]).unwrap();
/// assert_eq!(signer.key_name(), "example.com/.well-known/awp/log");
/// assert!(signer.verifier_key().starts_with("example.com/.well-known/awp/log+"));
/// ```
pub struct Ed25519ReleaseSigner {
    inner: Ed25519Signer,
}

impl std::fmt::Debug for Ed25519ReleaseSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ed25519ReleaseSigner")
            .field("key_name", &self.inner.key_name())
            .finish_non_exhaustive()
    }
}

fn note_error(e: NoteError) -> SiteError {
    SiteError::Signing(e.to_string())
}

impl Ed25519ReleaseSigner {
    /// A signer for `domain` from a 32-byte seed.
    ///
    /// # Errors
    ///
    /// [`SiteError::Signing`] when the domain makes an invalid key name.
    pub fn from_seed(domain: &str, seed: [u8; 32]) -> Result<Self> {
        Ok(Self { inner: Ed25519Signer::from_seed(log_origin(domain), seed).map_err(note_error)? })
    }

    /// A signer for `domain` with a new random key.
    ///
    /// # Errors
    ///
    /// As [`from_seed`](Self::from_seed), or when the system RNG fails.
    pub fn generate(domain: &str) -> Result<Self> {
        Ok(Self { inner: Ed25519Signer::generate(log_origin(domain)).map_err(note_error)? })
    }

    /// A signer from a note signer key (`PRIVATE+KEY+<origin>+…`), as Go's `note` tools write.
    ///
    /// # Errors
    ///
    /// [`SiteError::Signing`] when the key is malformed.
    pub fn from_note_key(key: &str) -> Result<Self> {
        Ok(Self { inner: Ed25519Signer::from_note_key(key).map_err(note_error)? })
    }
}

#[async_trait]
impl ReleaseSigner for Ed25519ReleaseSigner {
    fn key_name(&self) -> &str {
        self.inner.key_name()
    }

    fn public_key(&self) -> &[u8] {
        self.inner.public_key()
    }

    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>> {
        self.inner.sign(message).await.map_err(note_error)
    }
}

/// Presents a [`ReleaseSigner`] to `adk-auth`'s note signing.
pub(crate) struct NoteSigner(pub Arc<dyn ReleaseSigner>);

#[async_trait]
impl AuditSigner for NoteSigner {
    fn key_name(&self) -> &str {
        self.0.key_name()
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::Ed25519
    }

    fn public_key(&self) -> &[u8] {
        self.0.public_key()
    }

    async fn sign(&self, message: &[u8]) -> std::result::Result<Vec<u8>, NoteError> {
        self.0.sign(message).await.map_err(|e| NoteError::Signing(e.to_string()))
    }
}
