//! The `adk-enc:v1:` envelope: AES-256-GCM with associated data, as
//! [`EncryptedSession`](crate::EncryptedSession) stores it (spec 3.1 §7.12, §10 S5).
//!
//! An envelope is `adk-enc:v1:` followed by base64 of `[12-byte nonce][ciphertext + tag]`.
//! The associated data binds every ciphertext to where it is stored, so a value copied to
//! another row, key, session or store fails authentication. Nonces are random, so rotate a
//! key well before it has sealed 2^32 values.
//!
//! Other stores wrap their payloads with the same envelope — `adk-harness`'s mode, rule,
//! shell-ledger, goal and fact stores and [`EncryptedApprovalStore`](crate::EncryptedApprovalStore)
//! — each under its own domain, so a ciphertext never opens in a store it was not written to.
//!
//! # Example
//!
//! ```rust
//! use adk_session::EncryptionKey;
//! use adk_session::envelope::{associated_data, is_envelope, open, seal};
//!
//! let key = EncryptionKey::generate();
//! let aad = associated_data(b"my-store/v1", &["app", "alice", "row-1"]);
//! let sealed = seal(&key, b"payload", &aad).unwrap();
//! assert!(is_envelope(&sealed));
//! let (plaintext, key_index) = open([&key], &sealed, &aad).unwrap();
//! assert_eq!((plaintext.as_slice(), key_index), (&b"payload"[..], 0));
//! // Bound to its address: another row's associated data does not open it.
//! assert!(open([&key], &sealed, &associated_data(b"my-store/v1", &["app", "alice", "row-2"])).is_none());
//! ```

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::RngCore;

use crate::encryption_key::EncryptionKey;

/// Prefix of every envelope.
pub const ENVELOPE_PREFIX: &str = "adk-enc:v1:";
/// Length of the random nonce that opens each envelope.
pub const NONCE_LEN: usize = 12;

/// Whether `text` is an envelope this module can try to open.
pub fn is_envelope(text: &str) -> bool {
    text.starts_with(ENVELOPE_PREFIX)
}

/// Length-prefixed associated data under `domain`, so distinct component lists never
/// collide and a ciphertext sealed by one store never opens in another.
pub fn associated_data(domain: &[u8], parts: &[&str]) -> Vec<u8> {
    let mut aad = domain.to_vec();
    for part in parts {
        aad.extend_from_slice(&(part.len() as u64).to_be_bytes());
        aad.extend_from_slice(part.as_bytes());
    }
    aad
}

/// Seals `plaintext` under `key`, bound to `aad`, as an envelope string.
///
/// # Errors
///
/// Returns `session.encryption.encrypt_failed` when the cipher rejects the key or the
/// data.
pub fn seal(key: &EncryptionKey, plaintext: &[u8], aad: &[u8]) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes())
        .map_err(|e| encrypt_error(format!("invalid key: {e}")))?;
    let mut nonce = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .map_err(|_| encrypt_error("AES-256-GCM encryption failed"))?;
    let mut bytes = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    bytes.extend_from_slice(&nonce);
    bytes.extend_from_slice(&ciphertext);
    Ok(format!("{ENVELOPE_PREFIX}{}", BASE64.encode(bytes)))
}

/// Opens `envelope` bound to `aad` with the first of `keys` that authenticates it.
///
/// Returns the plaintext and the index of the key that opened it (0 is the first key
/// given, normally the current one), or `None` when `envelope` is not an envelope, is
/// malformed, or no key authenticates it under `aad`.
pub fn open<'a>(
    keys: impl IntoIterator<Item = &'a EncryptionKey>,
    envelope: &str,
    aad: &[u8],
) -> Option<(Vec<u8>, usize)> {
    let encoded = envelope.strip_prefix(ENVELOPE_PREFIX)?;
    let bytes = BASE64.decode(encoded).ok()?;
    if bytes.len() < NONCE_LEN {
        return None;
    }
    let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
    keys.into_iter().enumerate().find_map(|(index, key)| {
        Aes256Gcm::new_from_slice(key.as_bytes())
            .ok()?
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ciphertext, aad })
            .ok()
            .map(|plaintext| (plaintext, index))
    })
}

fn encrypt_error(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Internal,
        "session.encryption.encrypt_failed",
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_key_opens_what_the_first_sealed_and_reports_its_index() {
        let old = EncryptionKey::generate();
        let new = EncryptionKey::generate();
        let aad = associated_data(b"t/v1", &["a"]);
        let sealed = seal(&old, b"x", &aad).unwrap();
        assert_eq!(open([&new, &old], &sealed, &aad), Some((b"x".to_vec(), 1)));
        assert_eq!(open([&new], &sealed, &aad), None);
        assert_eq!(open([&old], "not an envelope", &aad), None);
        assert_eq!(open([&old], &format!("{ENVELOPE_PREFIX}!!"), &aad), None);
    }

    #[test]
    fn associated_data_is_length_prefixed_so_parts_never_slide() {
        assert_ne!(associated_data(b"d", &["ab", "c"]), associated_data(b"d", &["a", "bc"]));
        assert_ne!(associated_data(b"d", &["a"]), associated_data(b"e", &["a"]));
    }
}
