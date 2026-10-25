//! Detached JWS (EdDSA) over the JCS form of a document without its `proof` (spec 3.5 §7.2,
//! D-11; AWP 2.0 §5.4).

use adk_auth::signed_audit::{
    AuditSigner, Ed25519Signer, NoteVerifier, SignatureAlgorithm, canonicalize,
};
use async_trait::async_trait;
use awp_types::{Digest, Proof, PublicKeyJwk, without_proof};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::SigningError;

/// The JWS algorithm AWP 2.0 signs with.
pub const ALG: &str = "EdDSA";

/// A key that signs AWP documents; the private key never leaves it (spec 3.5 A-I5).
///
/// Implement it over a KMS or hardware token; [`Ed25519DocumentSigner`] holds a seed in
/// memory.
#[async_trait]
pub trait DocumentSigner: Send + Sync {
    /// The key id in `keys.json`.
    fn kid(&self) -> &str;

    /// The public key.
    fn public_key(&self) -> PublicKeyJwk;

    /// Signs `message` with Ed25519.
    ///
    /// # Errors
    ///
    /// [`SigningError::Key`] when the key cannot sign.
    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SigningError>;
}

/// An Ed25519 [`DocumentSigner`] holding its seed in memory, zeroed on drop. `Debug` shows the
/// key id only (spec 3.5 A-I5).
///
/// # Example
///
/// ```
/// use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
///
/// let signer = Ed25519DocumentSigner::from_seed("site-root-2026", [7; 32]).unwrap();
/// assert_eq!(signer.kid(), "site-root-2026");
/// assert_eq!(signer.public_key().crv, "Ed25519");
/// assert!(!format!("{signer:?}").contains("seed"));
/// ```
pub struct Ed25519DocumentSigner {
    kid: String,
    seed: Zeroizing<[u8; 32]>,
    inner: Ed25519Signer,
}

impl std::fmt::Debug for Ed25519DocumentSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ed25519DocumentSigner").field("kid", &self.kid).finish_non_exhaustive()
    }
}

impl Ed25519DocumentSigner {
    /// Creates a signer for key `kid` from a 32-byte Ed25519 seed.
    ///
    /// # Errors
    ///
    /// [`SigningError::Key`] when `kid` is empty or holds whitespace or `+`.
    pub fn from_seed(kid: impl Into<String>, seed: [u8; 32]) -> Result<Self, SigningError> {
        let kid = kid.into();
        let inner = Ed25519Signer::from_seed(kid.clone(), seed)
            .map_err(|error| SigningError::Key(error.to_string()))?;
        Ok(Self { kid, seed: Zeroizing::new(seed), inner })
    }

    /// Creates a signer for key `kid` with a fresh random seed.
    ///
    /// # Errors
    ///
    /// [`SigningError::Key`] when the kid is invalid or the random generator fails.
    pub fn generate(kid: impl Into<String>) -> Result<Self, SigningError> {
        let kid = kid.into();
        let inner = Ed25519Signer::generate(kid.clone())
            .map_err(|error| SigningError::Key(error.to_string()))?;
        let note_key = inner.to_note_key();
        // `PRIVATE+KEY+<name>+<hash>+<base64(alg || seed)>`; the name holds no `+`.
        let encoded = note_key.splitn(5, '+').nth(4).unwrap_or_default();
        let decoded = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| SigningError::Key(error.to_string()))?,
        );
        let seed: [u8; 32] = decoded
            .get(1..33)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| SigningError::Key("generated seed is not 32 bytes".to_owned()))?;
        Ok(Self { kid, seed: Zeroizing::new(seed), inner })
    }

    /// The raw 32-byte public key.
    pub fn public_key_bytes(&self) -> &[u8] {
        self.inner.public_key()
    }

    /// A C2SP note signer with the same key under `key_name`, for site log checkpoints.
    ///
    /// # Errors
    ///
    /// [`SigningError::Key`] when `key_name` is not a valid note key name.
    pub fn note_signer(&self, key_name: &str) -> Result<Ed25519Signer, SigningError> {
        Ed25519Signer::from_seed(key_name, *self.seed)
            .map_err(|error| SigningError::Key(error.to_string()))
    }
}

#[async_trait]
impl DocumentSigner for Ed25519DocumentSigner {
    fn kid(&self) -> &str {
        &self.kid
    }

    fn public_key(&self) -> PublicKeyJwk {
        public_key_jwk(self.inner.public_key())
    }

    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SigningError> {
        self.inner.sign(message).await.map_err(|error| SigningError::Key(error.to_string()))
    }
}

/// The JWK of a raw Ed25519 public key.
pub fn public_key_jwk(raw: &[u8]) -> PublicKeyJwk {
    PublicKeyJwk::ed25519(URL_SAFE_NO_PAD.encode(raw))
}

/// The raw bytes of an Ed25519 JWK.
///
/// # Errors
///
/// [`ProofError::Key`] when the JWK is not an `OKP` `Ed25519` key of 32 bytes.
pub fn public_key_bytes(jwk: &PublicKeyJwk) -> Result<Vec<u8>, ProofError> {
    if jwk.kty != "OKP" || jwk.crv != "Ed25519" {
        return Err(ProofError::Key("the key is not an OKP Ed25519 JWK".to_owned()));
    }
    let raw = URL_SAFE_NO_PAD
        .decode(&jwk.x)
        .map_err(|_| ProofError::Key("the JWK's x is not base64url".to_owned()))?;
    if raw.len() != 32 {
        return Err(ProofError::Key("an Ed25519 public key is 32 bytes".to_owned()));
    }
    Ok(raw)
}

/// The RFC 7638 SHA-256 thumbprint of an Ed25519 JWK, base64url (AWP 2.0 §5.1).
///
/// # Example
///
/// ```
/// use adk_awp::signing::jws::jwk_thumbprint;
/// use awp_types::PublicKeyJwk;
///
/// // RFC 8037 Appendix A.3.
/// let jwk = PublicKeyJwk::ed25519("11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo");
/// assert_eq!(jwk_thumbprint(&jwk), "kPrK_qmxVWaYVA9wwBF6Iuo3vVzz7TxHCTwXBygrS4k");
/// ```
pub fn jwk_thumbprint(jwk: &PublicKeyJwk) -> String {
    let members = format!(r#"{{"crv":"{}","kty":"{}","x":"{}"}}"#, jwk.crv, jwk.kty, jwk.x);
    URL_SAFE_NO_PAD.encode(Sha256::digest(members.as_bytes()))
}

/// The JCS form of `document` without its top-level `proof`: what a proof signs.
///
/// # Errors
///
/// [`SigningError::Canonicalize`] when the document is not I-JSON.
pub fn signing_payload<T: Serialize>(document: &T) -> Result<String, SigningError> {
    let value =
        without_proof(document).map_err(|error| SigningError::Canonicalize(error.to_string()))?;
    canonicalize(&value).map_err(|error| SigningError::Canonicalize(error.to_string()))
}

/// The digest of a JSON document: the SHA-256 of its JCS form (AWP 2.0 §1.2).
///
/// # Errors
///
/// [`SigningError::Canonicalize`] when the document is not I-JSON.
pub fn document_digest<T: Serialize>(document: &T) -> Result<Digest, SigningError> {
    let value = serde_json::to_value(document)
        .map_err(|error| SigningError::Canonicalize(error.to_string()))?;
    let canonical =
        canonicalize(&value).map_err(|error| SigningError::Canonicalize(error.to_string()))?;
    Ok(Digest::from_sha256_bytes(Sha256::digest(canonical.as_bytes()).into()))
}

/// Signs the JCS form of `document` without its `proof` and returns the proof
/// (AWP 2.0 §5.4).
///
/// # Errors
///
/// [`SigningError`] when the document is not I-JSON or the key cannot sign.
///
/// # Example
///
/// ```
/// use adk_awp::signing::jws::{Ed25519DocumentSigner, DocumentSigner, sign, verify};
/// use awp_types::{SiteContract, SiteRef};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let root = Ed25519DocumentSigner::from_seed("site-root-2026", [1; 32]).unwrap();
/// let mut contract = SiteContract::new(SiteRef {
///     id: uuid::Uuid::nil(), name: "Example".into(), domain: "example.com".into(), languages: vec![],
/// });
/// contract.proof = Some(sign(&contract, &root).await.unwrap());
/// let proof = contract.proof.clone().unwrap();
/// assert!(proof.jws.contains(".."));
/// assert!(verify(&contract, &proof, &root.public_key()).is_ok());
/// contract.site.name = "Changed".into();
/// assert!(verify(&contract, &proof, &root.public_key()).is_err());
/// # }
/// ```
pub async fn sign<T: Serialize>(
    document: &T,
    signer: &dyn DocumentSigner,
) -> Result<Proof, SigningError> {
    let payload = signing_payload(document)?;
    let header = format!(r#"{{"alg":"{ALG}","kid":{}}}"#, Value::String(signer.kid().to_owned()));
    let header = URL_SAFE_NO_PAD.encode(header.as_bytes());
    let input = format!("{header}.{}", URL_SAFE_NO_PAD.encode(payload.as_bytes()));
    let signature = signer.sign(input.as_bytes()).await?;
    Ok(Proof::eddsa(signer.kid(), format!("{header}..{}", URL_SAFE_NO_PAD.encode(signature))))
}

/// Why a proof does not verify (AWP 2.0 §5.4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProofError {
    /// The JWS is not `header..signature`.
    #[error("the proof is not a detached compact JWS (`header..signature`)")]
    Malformed,
    /// The payload part is not empty.
    #[error("the proof carries a payload; AWP proofs are detached")]
    NotDetached,
    /// The protected header is invalid: wrong `alg`, `crit`, `b64`, or a `kid` that differs
    /// from the proof's.
    #[error("the proof's protected header is invalid: {0}")]
    Header(String),
    /// The key is unusable.
    #[error("the verification key is invalid: {0}")]
    Key(String),
    /// The document is not I-JSON.
    #[error("the document cannot be canonicalised: {0}")]
    Canonicalize(String),
    /// The signature does not match.
    #[error("the signature does not match the document")]
    Signature,
}

/// Verifies `proof` over `document` with `key` (AWP 2.0 §5.4).
///
/// # Errors
///
/// [`ProofError`] naming what failed.
pub fn verify<T: Serialize>(
    document: &T,
    proof: &Proof,
    key: &PublicKeyJwk,
) -> Result<(), ProofError> {
    if proof.alg != ALG {
        return Err(ProofError::Header(format!("alg `{}` is not EdDSA", proof.alg)));
    }
    let mut parts = proof.jws.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ProofError::Malformed);
    };
    if !payload.is_empty() {
        return Err(ProofError::NotDetached);
    }
    let decoded = URL_SAFE_NO_PAD.decode(header).map_err(|_| ProofError::Malformed)?;
    let header_json: Value = serde_json::from_slice(&decoded).map_err(|_| ProofError::Malformed)?;
    if header_json.get("alg").and_then(Value::as_str) != Some(ALG) {
        return Err(ProofError::Header("alg must be EdDSA".to_owned()));
    }
    if header_json.get("crit").is_some() || header_json.get("b64").is_some() {
        return Err(ProofError::Header("crit and b64 are not allowed".to_owned()));
    }
    if header_json.get("kid").and_then(Value::as_str) != Some(proof.kid.as_str()) {
        return Err(ProofError::Header("the header kid differs from the proof's".to_owned()));
    }
    let payload =
        signing_payload(document).map_err(|error| ProofError::Canonicalize(error.to_string()))?;
    let input = format!("{header}.{}", URL_SAFE_NO_PAD.encode(payload.as_bytes()));
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| ProofError::Malformed)?;
    verify_signature(key, input.as_bytes(), &signature)
}

/// Verifies an Ed25519 `signature` over `message` with `key`.
///
/// # Errors
///
/// [`ProofError::Key`] for an unusable key; [`ProofError::Signature`] for a mismatch.
pub fn verify_signature(
    key: &PublicKeyJwk,
    message: &[u8],
    signature: &[u8],
) -> Result<(), ProofError> {
    let raw = public_key_bytes(key)?;
    let verifier = NoteVerifier::from_public_key("awp", SignatureAlgorithm::Ed25519, &raw)
        .map_err(|error| ProofError::Key(error.to_string()))?;
    if verifier.verify(message, signature) { Ok(()) } else { Err(ProofError::Signature) }
}

/// Verifies the `proof` member of a JSON document with `key`.
///
/// # Errors
///
/// [`ProofError::Malformed`] when the document has no proof; otherwise as [`verify`].
pub fn verify_value(document: &Value, key: &PublicKeyJwk) -> Result<(), ProofError> {
    let proof: Proof = document
        .get("proof")
        .and_then(|proof| serde_json::from_value(proof.clone()).ok())
        .ok_or(ProofError::Malformed)?;
    verify(document, &proof, key)
}
