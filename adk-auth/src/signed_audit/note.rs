//! C2SP signed notes and tlog checkpoints, compatible with Go's
//! `golang.org/x/mod/sumdb/note` package.
//!
//! A note is text, a blank line, and one signature line per signer:
//!
//! ```text
//! <origin>
//! <tree size>
//! <base64 root hash>
//!
//! — <key name> <base64(4-byte key hash ‖ signature)>
//! ```
//!
//! Verifier keys use the note format `<name>+<hex key hash>+<base64(0x01 ‖ public key)>`;
//! signer keys add a `PRIVATE+KEY+` prefix and carry the Ed25519 seed instead.

use std::fmt;
use std::sync::Arc;

use adk_core::audit_log::AuditHash;
use async_trait::async_trait;
use aws_lc_rs::signature::{self, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Algorithm byte for Ed25519 in note keys.
const ALG_ED25519: u8 = 0x01;
const SIGNATURE_PREFIX: &str = "\u{2014} ";

/// A signature algorithm usable for checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignatureAlgorithm {
    /// Ed25519 (RFC 8032).
    Ed25519,
}

impl SignatureAlgorithm {
    fn id(self) -> u8 {
        match self {
            Self::Ed25519 => ALG_ED25519,
        }
    }
}

/// Errors from signing, parsing, or opening notes and keys.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NoteError {
    /// A key string is malformed.
    #[error("invalid note key: {0}")]
    InvalidKey(String),
    /// A note or checkpoint is malformed.
    #[error("malformed note: {0}")]
    Malformed(String),
    /// A signer failed to sign.
    #[error("signing failed: {0}")]
    Signing(String),
}

/// Signs checkpoints. Implement it over a KMS or HSM to keep the private key out of
/// the process; [`Ed25519Signer`] holds the key in memory.
#[async_trait]
pub trait AuditSigner: Send + Sync {
    /// The key name that appears in signature lines and verifier keys.
    fn key_name(&self) -> &str;

    /// The signature algorithm.
    fn algorithm(&self) -> SignatureAlgorithm;

    /// The raw public key.
    fn public_key(&self) -> &[u8];

    /// Signs `message`.
    ///
    /// # Errors
    ///
    /// [`NoteError::Signing`] when the key cannot sign.
    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, NoteError>;

    /// The note verifier key string for this signer.
    fn verifier_key(&self) -> String {
        NoteVerifier::from_public_key(self.key_name(), self.algorithm(), self.public_key())
            .map(|verifier| verifier.to_string())
            .unwrap_or_default()
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('+') && !name.chars().any(char::is_whitespace)
}

fn key_hash(name: &str, key: &[u8]) -> u32 {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update(b"\n");
    hasher.update(key);
    let digest = hasher.finalize();
    u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]])
}

/// An Ed25519 [`AuditSigner`] holding its seed in memory (zeroized on drop).
///
/// # Example
///
/// ```rust
/// use adk_auth::signed_audit::{AuditSigner, Ed25519Signer, NoteVerifier};
///
/// let signer = Ed25519Signer::generate("audit.example.com").unwrap();
/// let verifier: NoteVerifier = signer.verifier_key().parse().unwrap();
/// assert_eq!(verifier.name(), "audit.example.com");
/// ```
pub struct Ed25519Signer {
    name: String,
    seed: Zeroizing<[u8; 32]>,
    key_pair: Ed25519KeyPair,
    public_key: [u8; 32],
}

impl fmt::Debug for Ed25519Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ed25519Signer").field("name", &self.name).finish_non_exhaustive()
    }
}

impl Ed25519Signer {
    /// Creates a signer from a 32-byte seed.
    ///
    /// # Errors
    ///
    /// [`NoteError::InvalidKey`] when `name` is empty or contains `+` or whitespace.
    pub fn from_seed(name: impl Into<String>, seed: [u8; 32]) -> Result<Self, NoteError> {
        let name = name.into();
        if !valid_name(&name) {
            return Err(NoteError::InvalidKey(format!(
                "key name '{name}' must be non-empty without '+' or whitespace"
            )));
        }
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|e| NoteError::InvalidKey(format!("ed25519 seed rejected: {e}")))?;
        let public_key: [u8; 32] = key_pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| NoteError::InvalidKey("ed25519 public key is not 32 bytes".into()))?;
        Ok(Self { name, seed: Zeroizing::new(seed), key_pair, public_key })
    }

    /// Generates a new random key.
    ///
    /// # Errors
    ///
    /// As [`from_seed`](Self::from_seed), or when the system RNG fails.
    pub fn generate(name: impl Into<String>) -> Result<Self, NoteError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        aws_lc_rs::rand::fill(seed.as_mut())
            .map_err(|_| NoteError::InvalidKey("system random number generator failed".into()))?;
        Self::from_seed(name, *seed)
    }

    /// Parses a note signer key, `PRIVATE+KEY+<name>+<hash>+<base64(0x01 ‖ seed)>`, as
    /// produced by Go's `note.GenerateKey`.
    ///
    /// # Errors
    ///
    /// [`NoteError::InvalidKey`] when the key is malformed or its hash does not match.
    pub fn from_note_key(key: &str) -> Result<Self, NoteError> {
        let rest = key.strip_prefix("PRIVATE+KEY+").ok_or_else(|| {
            NoteError::InvalidKey("signer key must start with PRIVATE+KEY+".into())
        })?;
        let (name, hash, encoded) = split_key(rest)?;
        let decoded = Zeroizing::new(
            BASE64
                .decode(encoded)
                .map_err(|e| NoteError::InvalidKey(format!("bad base64: {e}")))?,
        );
        let (&alg, seed) = decoded
            .split_first()
            .ok_or_else(|| NoteError::InvalidKey("signer key is empty".into()))?;
        if alg != ALG_ED25519 {
            return Err(NoteError::InvalidKey(format!("unsupported algorithm {alg}")));
        }
        let seed: [u8; 32] = seed
            .try_into()
            .map_err(|_| NoteError::InvalidKey("ed25519 seed is not 32 bytes".into()))?;
        let signer = Self::from_seed(name, seed)?;
        let verifier =
            NoteVerifier::from_public_key(name, SignatureAlgorithm::Ed25519, &signer.public_key)?;
        if verifier.key_hash != hash {
            return Err(NoteError::InvalidKey("key hash does not match the key".into()));
        }
        Ok(signer)
    }

    /// Returns the note signer key string. Treat it as a secret.
    pub fn to_note_key(&self) -> Zeroizing<String> {
        let mut encoded = Zeroizing::new(Vec::with_capacity(33));
        encoded.push(ALG_ED25519);
        encoded.extend_from_slice(self.seed.as_ref());
        let mut public = vec![ALG_ED25519];
        public.extend_from_slice(&self.public_key);
        Zeroizing::new(format!(
            "PRIVATE+KEY+{}+{:08x}+{}",
            self.name,
            key_hash(&self.name, &public),
            BASE64.encode(encoded.as_slice())
        ))
    }
}

#[async_trait]
impl AuditSigner for Ed25519Signer {
    fn key_name(&self) -> &str {
        &self.name
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::Ed25519
    }

    fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, NoteError> {
        Ok(self.key_pair.sign(message).as_ref().to_vec())
    }
}

fn split_key(text: &str) -> Result<(&str, u32, &str), NoteError> {
    let mut parts = text.splitn(3, '+');
    let (Some(name), Some(hash), Some(encoded)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(NoteError::InvalidKey("expected <name>+<hash>+<key>".into()));
    };
    if !valid_name(name) {
        return Err(NoteError::InvalidKey(format!("invalid key name '{name}'")));
    }
    if hash.len() != 8 {
        return Err(NoteError::InvalidKey("key hash must be 8 hex digits".into()));
    }
    let hash = u32::from_str_radix(hash, 16)
        .map_err(|_| NoteError::InvalidKey("key hash must be 8 hex digits".into()))?;
    Ok((name, hash, encoded))
}

/// A public key that verifies note signatures, parsed from a verifier key string.
///
/// # Example
///
/// ```rust
/// use adk_auth::signed_audit::NoteVerifier;
///
/// let verifier: NoteVerifier =
///     "PeterNeumann+c74f20a3+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW".parse().unwrap();
/// assert_eq!(verifier.key_hash(), 0xc74f20a3);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NoteVerifier {
    name: String,
    key_hash: u32,
    algorithm: SignatureAlgorithm,
    public_key: Vec<u8>,
}

impl NoteVerifier {
    /// Builds a verifier from a raw public key.
    ///
    /// # Errors
    ///
    /// [`NoteError::InvalidKey`] when the name is invalid or the key has the wrong length.
    pub fn from_public_key(
        name: &str,
        algorithm: SignatureAlgorithm,
        public_key: &[u8],
    ) -> Result<Self, NoteError> {
        if !valid_name(name) {
            return Err(NoteError::InvalidKey(format!("invalid key name '{name}'")));
        }
        if public_key.len() != 32 {
            return Err(NoteError::InvalidKey("ed25519 public key must be 32 bytes".into()));
        }
        let mut encoded = vec![algorithm.id()];
        encoded.extend_from_slice(public_key);
        Ok(Self {
            name: name.to_string(),
            key_hash: key_hash(name, &encoded),
            algorithm,
            public_key: public_key.to_vec(),
        })
    }

    /// The key name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The 32-bit key hash that identifies the key in signature lines.
    pub fn key_hash(&self) -> u32 {
        self.key_hash
    }

    /// Checks `signature` over `message`.
    pub fn verify(&self, message: &[u8], signature_bytes: &[u8]) -> bool {
        match self.algorithm {
            SignatureAlgorithm::Ed25519 => {
                UnparsedPublicKey::new(&signature::ED25519, &self.public_key)
                    .verify(message, signature_bytes)
                    .is_ok()
            }
        }
    }
}

impl std::str::FromStr for NoteVerifier {
    type Err = NoteError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (name, hash, encoded) = split_key(text.trim())?;
        let decoded = BASE64
            .decode(encoded)
            .map_err(|e| NoteError::InvalidKey(format!("bad base64: {e}")))?;
        let (&alg, key) = decoded
            .split_first()
            .ok_or_else(|| NoteError::InvalidKey("verifier key is empty".into()))?;
        if alg != ALG_ED25519 {
            return Err(NoteError::InvalidKey(format!("unsupported algorithm {alg}")));
        }
        let verifier = Self::from_public_key(name, SignatureAlgorithm::Ed25519, key)?;
        if verifier.key_hash != hash {
            return Err(NoteError::InvalidKey("key hash does not match the key".into()));
        }
        Ok(verifier)
    }
}

impl fmt::Display for NoteVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut encoded = vec![self.algorithm.id()];
        encoded.extend_from_slice(&self.public_key);
        write!(f, "{}+{:08x}+{}", self.name, self.key_hash, BASE64.encode(encoded))
    }
}

/// One signature line of a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteSignature {
    /// The key name.
    pub name: String,
    /// The key hash.
    pub key_hash: u32,
    /// The signature bytes, without the key hash.
    pub signature: Vec<u8>,
}

/// A parsed note: its signed text and its signature lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The signed text, ending in a newline.
    pub text: String,
    /// Every signature line, verified or not.
    pub signatures: Vec<NoteSignature>,
}

impl Note {
    /// Parses a signed note without verifying it.
    ///
    /// # Errors
    ///
    /// [`NoteError::Malformed`] when the note does not follow the signed-note format.
    pub fn parse(note: &str) -> Result<Self, NoteError> {
        let split = note
            .rfind("\n\n")
            .ok_or_else(|| NoteError::Malformed("no blank line before the signatures".into()))?;
        let text = &note[..=split];
        let signatures_text = &note[split + 2..];
        if !signatures_text.ends_with('\n') {
            return Err(NoteError::Malformed("signature block must end in a newline".into()));
        }
        let mut signatures = Vec::new();
        for line in signatures_text.lines() {
            let rest = line.strip_prefix(SIGNATURE_PREFIX).ok_or_else(|| {
                NoteError::Malformed("signature line must start with '— '".into())
            })?;
            let (name, encoded) = rest.split_once(' ').ok_or_else(|| {
                NoteError::Malformed("signature line needs a name and a value".into())
            })?;
            if !valid_name(name) {
                return Err(NoteError::Malformed(format!("invalid signer name '{name}'")));
            }
            let decoded = BASE64
                .decode(encoded)
                .map_err(|e| NoteError::Malformed(format!("bad signature base64: {e}")))?;
            if decoded.len() < 5 {
                return Err(NoteError::Malformed("signature is too short".into()));
            }
            signatures.push(NoteSignature {
                name: name.to_string(),
                key_hash: u32::from_be_bytes([decoded[0], decoded[1], decoded[2], decoded[3]]),
                signature: decoded[4..].to_vec(),
            });
        }
        if signatures.is_empty() {
            return Err(NoteError::Malformed("note has no signatures".into()));
        }
        Ok(Self { text: text.to_string(), signatures })
    }

    /// Returns the verifiers among `verifiers` whose signature on this note is valid.
    pub fn verified_by<'a>(&self, verifiers: &'a [NoteVerifier]) -> Vec<&'a NoteVerifier> {
        verifiers
            .iter()
            .filter(|verifier| {
                self.signatures.iter().any(|signature| {
                    signature.name == verifier.name
                        && signature.key_hash == verifier.key_hash
                        && verifier.verify(self.text.as_bytes(), &signature.signature)
                })
            })
            .collect()
    }
}

/// Signs `text` with every signer and returns the signed note.
///
/// # Errors
///
/// [`NoteError::Malformed`] when `text` is empty, does not end in a newline, or holds a
/// blank line; [`NoteError::Signing`] when a signer fails.
pub async fn sign_note(text: &str, signers: &[Arc<dyn AuditSigner>]) -> Result<String, NoteError> {
    if text.is_empty() || !text.ends_with('\n') || text.contains("\n\n") {
        return Err(NoteError::Malformed(
            "note text must be non-empty, end in a newline, and contain no blank line".into(),
        ));
    }
    let mut note = format!("{text}\n");
    for signer in signers {
        let verifier = NoteVerifier::from_public_key(
            signer.key_name(),
            signer.algorithm(),
            signer.public_key(),
        )?;
        let signature_bytes = signer.sign(text.as_bytes()).await?;
        let mut value = verifier.key_hash.to_be_bytes().to_vec();
        value.extend_from_slice(&signature_bytes);
        note.push_str(&format!(
            "{SIGNATURE_PREFIX}{} {}\n",
            signer.key_name(),
            BASE64.encode(value)
        ));
    }
    Ok(note)
}

/// The body of a C2SP tlog checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointBody {
    /// The log's origin line.
    pub origin: String,
    /// The tree size.
    pub size: u64,
    /// The root hash.
    pub root_hash: AuditHash,
}

impl CheckpointBody {
    /// Formats the checkpoint text that gets signed.
    pub fn to_text(&self) -> String {
        format!("{}\n{}\n{}\n", self.origin, self.size, BASE64.encode(self.root_hash.0))
    }

    /// Parses checkpoint text, ignoring extension lines after the root hash.
    ///
    /// # Errors
    ///
    /// [`NoteError::Malformed`] when a required line is missing or invalid.
    pub fn parse(text: &str) -> Result<Self, NoteError> {
        let mut lines = text.lines();
        let origin = lines.next().filter(|line| !line.is_empty());
        // Decimal digits only, no sign and no leading zero, as C2SP requires.
        let size = lines
            .next()
            .filter(|line| line.bytes().all(|b| b.is_ascii_digit()))
            .filter(|line| *line == "0" || !line.starts_with('0'))
            .and_then(|line| line.parse::<u64>().ok());
        let root = lines.next().and_then(|line| BASE64.decode(line).ok());
        match (origin, size, root) {
            (Some(origin), Some(size), Some(root)) => {
                let root: [u8; 32] = root
                    .try_into()
                    .map_err(|_| NoteError::Malformed("root hash is not 32 bytes".into()))?;
                Ok(Self { origin: origin.to_string(), size, root_hash: AuditHash(root) })
            }
            _ => Err(NoteError::Malformed(
                "checkpoint needs an origin, a decimal size, and a base64 root hash".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PETER_SIGNER: &str =
        "PRIVATE+KEY+PeterNeumann+c74f20a3+AYEKFALVFGyNhPJEMzD1QIDr+Y7hfZx09iUvxdXHKDFz";
    const PETER_VERIFIER: &str =
        "PeterNeumann+c74f20a3+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW";
    const PETER_TEXT: &str = "If you think cryptography is the answer to your problem,\n\
                              then you don't know what your problem is.\n";

    /// `TestSign` from golang.org/x/mod/sumdb/note.
    #[tokio::test]
    async fn go_note_sign_vector() {
        let signer = Ed25519Signer::from_note_key(PETER_SIGNER).unwrap();
        assert_eq!(signer.verifier_key(), PETER_VERIFIER);
        assert_eq!(signer.to_note_key().as_str(), PETER_SIGNER);
        let signers: Vec<Arc<dyn AuditSigner>> = vec![Arc::new(signer)];
        let note = sign_note(PETER_TEXT, &signers).await.unwrap();
        assert_eq!(
            note,
            format!(
                "{PETER_TEXT}\n— PeterNeumann x08go/ZJkuBS9UG/SffcvIAQxVBtiFupLLr8pAcElZInNIuGUgYN1FFYC2pZSNXgKvqfqdngotpRZb6KE6RyyBwJnAM=\n"
            )
        );
        let verifier: NoteVerifier = PETER_VERIFIER.parse().unwrap();
        let parsed = Note::parse(&note).unwrap();
        assert_eq!(parsed.text, PETER_TEXT);
        assert_eq!(parsed.verified_by(std::slice::from_ref(&verifier)).len(), 1);
    }

    #[test]
    fn tampered_notes_do_not_verify() {
        let verifier: NoteVerifier = PETER_VERIFIER.parse().unwrap();
        let good = format!(
            "{PETER_TEXT}\n— PeterNeumann x08go/ZJkuBS9UG/SffcvIAQxVBtiFupLLr8pAcElZInNIuGUgYN1FFYC2pZSNXgKvqfqdngotpRZb6KE6RyyBwJnAM=\n"
        );
        let edited = good.replacen("answer", "Answer", 1);
        let parsed = Note::parse(&edited).unwrap();
        assert!(parsed.verified_by(std::slice::from_ref(&verifier)).is_empty());
        assert!(Note::parse("no signatures\n").is_err());
    }

    #[test]
    fn bad_keys_are_refused() {
        // From golang.org/x/mod/sumdb/note TestNewVerifier.
        assert!(
            "PeterNeumann+cc469956+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TWBADKEY=="
                .parse::<NoteVerifier>()
                .is_err()
        );
        assert!(
            "PeterNeumann+173116ae+ZRpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW"
                .parse::<NoteVerifier>()
                .is_err()
        );
        assert!(
            "PeterNeumann+c74f20a4+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW"
                .parse::<NoteVerifier>()
                .is_err()
        );
        assert!(Ed25519Signer::generate("has space").is_err());
    }

    #[test]
    fn checkpoint_body_round_trips() {
        let body = CheckpointBody {
            origin: "example.com/log".into(),
            size: 42,
            root_hash: AuditHash([7; 32]),
        };
        let text = format!("{}extension line\n", body.to_text());
        assert_eq!(CheckpointBody::parse(&text).unwrap(), body);
        assert!(CheckpointBody::parse("origin\n042\nAAAA\n").is_err());
    }
}
