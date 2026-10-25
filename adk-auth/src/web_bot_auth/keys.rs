//! Ed25519 keys, JWK thumbprints and the key directory document. Spec 3.5 §7.3.
//!
//! A key directory is a JSON Web Key Set ([RFC 7517] §5) of OKP keys ([RFC 8037]) served at
//! `/.well-known/http-message-signatures-directory` (draft-ietf-webbotauth-httpsig-protocol-00
//! §5.5; draft-meunier-http-message-signatures-directory-05 §3). A signature's `keyid` is the key's
//! JWK SHA-256 thumbprint ([RFC 7638] §3.2, [RFC 8037] Appendix A.3).
//!
//! [RFC 7517]: https://www.rfc-editor.org/rfc/rfc7517
//! [RFC 7638]: https://www.rfc-editor.org/rfc/rfc7638
//! [RFC 8037]: https://www.rfc-editor.org/rfc/rfc8037

use std::fmt;

use aws_lc_rs::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::message::{ComponentId, SignatureInput, signature_base};
use super::structured::{BareItem, Dictionary, ListEntry};
use super::{DIRECTORY_TAG, WebBotAuthError};

/// The public half of the RFC 9421 Appendix B.1.4 test key, which verifiers reject by default
/// (draft §6.8).
pub(crate) const RFC9421_TEST_KEY_X: &str = "JrQLj5P_89iXES9-vFgrIy29clF9CC_oPPsw3c5D0bs";

/// The JWK SHA-256 thumbprint of an Ed25519 public key (RFC 7638 §3.2, RFC 8037 Appendix A.3),
/// base64url without padding. Web Bot Auth uses it as the signature's `keyid`.
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::jwk_thumbprint;
/// use base64::Engine;
///
/// // RFC 8037 Appendix A.3.
/// let x = base64::engine::general_purpose::URL_SAFE_NO_PAD
///     .decode("11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo")
///     .unwrap();
/// assert_eq!(jwk_thumbprint(&x.try_into().unwrap()), "kPrK_qmxVWaYVA9wwBF6Iuo3vVzz7TxHCTwXBygrS4k");
/// ```
pub fn jwk_thumbprint(public_key: &[u8; 32]) -> String {
    let canonical =
        format!(r#"{{"crv":"Ed25519","kty":"OKP","x":"{}"}}"#, URL_SAFE_NO_PAD.encode(public_key));
    URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes()))
}

/// Verifies an Ed25519 signature (RFC 8032 §5.1.7, RFC 9421 §3.3.6).
pub(crate) fn verify_ed25519(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&ED25519, public_key).verify(message, signature).is_ok()
}

/// An Ed25519 signing key held in memory, its seed zeroised on drop.
///
/// Signing uses `aws-lc-rs`, as the signed audit log does. `Debug` never prints the seed.
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::Ed25519SigningKey;
///
/// let key = Ed25519SigningKey::generate().unwrap();
/// let restored = Ed25519SigningKey::from_jwk(&key.to_private_jwk()).unwrap();
/// assert_eq!(restored.thumbprint(), key.thumbprint());
/// assert!(!format!("{key:?}").contains(&*key.to_private_jwk()));
/// ```
pub struct Ed25519SigningKey {
    seed: Zeroizing<[u8; 32]>,
    key_pair: Ed25519KeyPair,
    public_key: [u8; 32],
    thumbprint: String,
}

impl fmt::Debug for Ed25519SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ed25519SigningKey")
            .field("thumbprint", &self.thumbprint)
            .finish_non_exhaustive()
    }
}

impl Ed25519SigningKey {
    /// Creates a key from its 32-byte seed (the RFC 8032 private key).
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidKey`] when the seed is rejected.
    pub fn from_seed(seed: [u8; 32]) -> Result<Self, WebBotAuthError> {
        let seed = Zeroizing::new(seed);
        let key_pair = Ed25519KeyPair::from_seed_unchecked(seed.as_ref())
            .map_err(|e| WebBotAuthError::InvalidKey { reason: e.to_string() })?;
        let public_key: [u8; 32] = key_pair.public_key().as_ref().try_into().map_err(|_| {
            WebBotAuthError::InvalidKey { reason: "public key is not 32 bytes".into() }
        })?;
        let thumbprint = jwk_thumbprint(&public_key);
        Ok(Self { seed, key_pair, public_key, thumbprint })
    }

    /// Generates a key from the system's secure random source.
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidKey`] when no randomness is available.
    pub fn generate() -> Result<Self, WebBotAuthError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        aws_lc_rs::rand::fill(seed.as_mut())
            .map_err(|_| WebBotAuthError::InvalidKey { reason: "no secure randomness".into() })?;
        Self::from_seed(*seed)
    }

    /// Reads a private OKP JWK (`kty` `OKP`, `crv` `Ed25519`, `d`, and `x` when present).
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidKey`] when the JWK is not an Ed25519 private key or its
    /// `x` does not match `d`.
    pub fn from_jwk(jwk: &str) -> Result<Self, WebBotAuthError> {
        #[derive(Deserialize)]
        struct PrivateJwk {
            kty: String,
            crv: String,
            x: Option<String>,
            d: Option<String>,
        }
        let invalid = |reason: &str| WebBotAuthError::InvalidKey { reason: reason.to_string() };
        let parsed: PrivateJwk =
            serde_json::from_str(jwk).map_err(|_| invalid("the JWK is not an OKP JSON object"))?;
        let d = Zeroizing::new(parsed.d.unwrap_or_default());
        if parsed.kty != "OKP" || parsed.crv != "Ed25519" {
            return Err(invalid("the JWK is not an OKP Ed25519 key"));
        }
        if d.is_empty() {
            return Err(invalid("the JWK has no private `d` member"));
        }
        let seed = Zeroizing::new(
            URL_SAFE_NO_PAD.decode(d.as_bytes()).map_err(|_| invalid("`d` is not base64url"))?,
        );
        let seed: [u8; 32] =
            seed.as_slice().try_into().map_err(|_| invalid("`d` is not 32 bytes"))?;
        let key = Self::from_seed(seed)?;
        if parsed.x.is_some_and(|x| x != URL_SAFE_NO_PAD.encode(key.public_key)) {
            return Err(invalid("`x` does not match `d`"));
        }
        Ok(key)
    }

    /// The private JWK, with `kid` set to the thumbprint. Store it as a secret.
    pub fn to_private_jwk(&self) -> Zeroizing<String> {
        let d = Zeroizing::new(URL_SAFE_NO_PAD.encode(self.seed.as_ref()));
        Zeroizing::new(format!(
            r#"{{"kty":"OKP","crv":"Ed25519","kid":"{}","x":"{}","d":"{}"}}"#,
            self.thumbprint,
            URL_SAFE_NO_PAD.encode(self.public_key),
            d.as_str()
        ))
    }

    /// The raw public key.
    pub fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    /// The JWK thumbprint, used as `keyid`.
    pub fn thumbprint(&self) -> &str {
        &self.thumbprint
    }

    /// The public JWK as a directory publishes it: `kid` is the thumbprint and `use` is `sig`.
    pub fn public_jwk(&self) -> DirectoryKey {
        DirectoryKey {
            kty: "OKP".to_string(),
            crv: "Ed25519".to_string(),
            kid: Some(self.thumbprint.clone()),
            x: URL_SAFE_NO_PAD.encode(self.public_key),
            key_use: Some("sig".to_string()),
            alg: None,
            nbf: None,
            exp: None,
        }
    }

    /// Signs `message` with Ed25519 (RFC 9421 §3.3.6).
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let mut out = [0u8; 64];
        out.copy_from_slice(self.key_pair.sign(message).as_ref());
        out
    }
}

/// One Ed25519 public key in a key directory: an OKP JWK (RFC 7517, RFC 8037).
///
/// `nbf` and `exp` bound the key's validity. They are read as Unix seconds; values of
/// 10^11 and above are read as milliseconds, as deployed directories publish them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryKey {
    /// Key type: `OKP`.
    pub kty: String,
    /// Curve: `Ed25519`.
    pub crv: String,
    /// Key id; when present it is the thumbprint (draft §5.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
    /// The public key, base64url.
    pub x: String,
    /// Intended use: `sig`.
    #[serde(rename = "use", default, skip_serializing_if = "Option::is_none")]
    pub key_use: Option<String>,
    /// Algorithm, restricted to HTTP signature algorithm names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
    /// Not valid before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<i64>,
    /// Not valid after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
}

impl DirectoryKey {
    /// The raw public key.
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidKey`] when this is not an Ed25519 key with a 32-byte `x`.
    pub fn public_key(&self) -> Result<[u8; 32], WebBotAuthError> {
        if self.kty != "OKP" || self.crv != "Ed25519" {
            return Err(WebBotAuthError::InvalidKey {
                reason: format!("key type {}/{} is not OKP/Ed25519", self.kty, self.crv),
            });
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.x)
            .map_err(|_| WebBotAuthError::InvalidKey { reason: "`x` is not base64url".into() })?;
        bytes
            .try_into()
            .map_err(|_| WebBotAuthError::InvalidKey { reason: "`x` is not 32 bytes".into() })
    }

    /// The key's JWK thumbprint, computed from `x` (never taken from `kid`).
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidKey`] when the key is malformed.
    pub fn thumbprint(&self) -> Result<String, WebBotAuthError> {
        self.public_key().map(|key| jwk_thumbprint(&key))
    }

    /// Whether the key's `nbf`/`exp` window includes `now` (Unix seconds).
    pub fn is_current(&self, now: i64) -> bool {
        let seconds = |value: i64| if value >= 100_000_000_000 { value / 1000 } else { value };
        self.nbf.is_none_or(|nbf| now >= seconds(nbf))
            && self.exp.is_none_or(|exp| now < seconds(exp))
    }
}

/// A key directory document: a JWKS of Ed25519 keys (draft §5.5.1).
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::{Ed25519SigningKey, KeyDirectory};
///
/// let key = Ed25519SigningKey::generate().unwrap();
/// let directory = KeyDirectory::from_keys([&key]);
/// let parsed = KeyDirectory::parse(directory.to_json().as_bytes(), 16).unwrap();
/// assert_eq!(parsed.find(key.thumbprint()).unwrap().x, directory.keys[0].x);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct KeyDirectory {
    /// The keys.
    pub keys: Vec<DirectoryKey>,
}

impl KeyDirectory {
    /// The directory a site publishes for its own signing keys (spec 3.5 §7.3 "Outbound").
    pub fn from_keys<'a>(keys: impl IntoIterator<Item = &'a Ed25519SigningKey>) -> Self {
        Self { keys: keys.into_iter().map(Ed25519SigningKey::public_jwk).collect() }
    }

    /// Parses a directory, keeping its well-formed Ed25519 keys and skipping other entries
    /// (draft §5.5.1: verifiers reject malformed entries).
    ///
    /// An entry is skipped when it is not an OKP/Ed25519 key, its `x` is not 32 bytes, it
    /// carries private material (`d`), its `use` is not `sig`, or its `alg` is not Ed25519.
    ///
    /// # Errors
    ///
    /// Returns a reason when the document is not a JWKS or lists more than `max_keys` entries.
    pub fn parse(bytes: &[u8], max_keys: usize) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Raw {
            keys: Vec<serde_json::Value>,
        }
        let raw: Raw = serde_json::from_slice(bytes)
            .map_err(|e| format!("the document is not a JWKS: {e}"))?;
        if raw.keys.len() > max_keys {
            return Err(format!(
                "it lists {} keys, more than the limit of {max_keys}",
                raw.keys.len()
            ));
        }
        let keys = raw
            .keys
            .into_iter()
            .filter_map(|entry| {
                if entry.get("d").is_some() {
                    tracing::warn!("key directory entry carries private key material; skipped");
                    return None;
                }
                let key: DirectoryKey = serde_json::from_value(entry).ok()?;
                let usable = key.public_key().is_ok()
                    && key.key_use.as_deref().is_none_or(|u| u == "sig")
                    && key.alg.as_deref().is_none_or(|a| matches!(a, "ed25519" | "Ed25519" | "EdDSA"));
                if !usable {
                    tracing::debug!(key.kty = %key.kty, "key directory entry is not a usable Ed25519 key; skipped");
                }
                usable.then_some(key)
            })
            .collect();
        Ok(Self { keys })
    }

    /// The key whose computed thumbprint is `thumbprint`.
    pub fn find(&self, thumbprint: &str) -> Option<&DirectoryKey> {
        self.keys.iter().find(|key| key.thumbprint().is_ok_and(|t| t == thumbprint))
    }

    /// The JSON document, compact, members in the order `kty`, `crv`, `kid`, `x`, `use`.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from(r#"{"keys":[]}"#))
    }

    /// The directory response with headers that prove possession of each key (draft
    /// Appendix B.1): one signature per key over `@authority;req` (the authority the directory
    /// was fetched from) and `content-digest`, tagged `http-message-signatures-directory`.
    ///
    /// Labels are `binding` for one key and `binding0`, `binding1`, … for several.
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidConfig`] when `keys` is empty or `expires` precedes
    /// `created`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_auth::web_bot_auth::{Ed25519SigningKey, KeyDirectory};
    ///
    /// let key = Ed25519SigningKey::generate().unwrap();
    /// let directory = KeyDirectory::from_keys([&key]);
    /// let response = directory
    ///     .signed_response(&[&key], "bot.example", 1_735_689_600, 1_735_776_000)
    ///     .unwrap();
    /// assert!(response.content_digest.starts_with("sha-256=:"));
    /// assert!(response.signature_input.starts_with("binding=(\"@authority\";req \"content-digest\")"));
    /// ```
    pub fn signed_response(
        &self,
        keys: &[&Ed25519SigningKey],
        authority: &str,
        created: i64,
        expires: i64,
    ) -> Result<DirectoryResponse, WebBotAuthError> {
        if keys.is_empty() || expires < created {
            return Err(WebBotAuthError::InvalidConfig {
                reason: "a directory proof needs at least one key and expires at or after created"
                    .into(),
            });
        }
        let body = self.to_json();
        let content_digest =
            format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body.as_bytes())));
        let authority = authority.to_ascii_lowercase();
        let mut inputs = Dictionary::new();
        let mut signatures = Dictionary::new();
        for (index, key) in keys.iter().enumerate() {
            let label =
                if keys.len() == 1 { "binding".to_string() } else { format!("binding{index}") };
            let mut input = SignatureInput::new(vec![
                ComponentId::new("@authority").with_param("req", BareItem::Boolean(true)),
                ComponentId::new("content-digest"),
            ]);
            input.params.insert("created", BareItem::Integer(created));
            input.params.insert("expires", BareItem::Integer(expires));
            input.params.insert("keyid", BareItem::String(key.thumbprint().to_string()));
            input.params.insert("tag", BareItem::String(DIRECTORY_TAG.to_string()));
            let base = signature_base(&input, |component| match component.name() {
                "@authority" => Ok(authority.clone()),
                _ => Ok(content_digest.clone()),
            })?;
            let signature = key.sign(base.as_bytes());
            inputs.insert(label.clone(), ListEntry::InnerList(input.to_inner_list()));
            signatures.insert(
                label,
                ListEntry::Item(super::structured::Item::new(BareItem::ByteSequence(
                    signature.to_vec(),
                ))),
            );
        }
        let serialize = |dict: Dictionary| {
            dict.serialize().map_err(|e| WebBotAuthError::InvalidConfig { reason: e.to_string() })
        };
        Ok(DirectoryResponse {
            body,
            content_digest,
            signature_input: serialize(inputs)?,
            signature: serialize(signatures)?,
        })
    }
}

/// A directory response with its possession proof (draft Appendix B.1).
///
/// Serve `body` as `application/http-message-signatures-directory+json` with the
/// `Content-Digest`, `Signature-Input` and `Signature` header values below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryResponse {
    /// The response body: the directory's JSON document.
    pub body: String,
    /// The `Content-Digest` value (RFC 9530): `sha-256=:<base64>:`.
    pub content_digest: String,
    /// The `Signature-Input` value.
    pub signature_input: String,
    /// The `Signature` value.
    pub signature: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_validity_reads_seconds_and_milliseconds() {
        let mut key = Ed25519SigningKey::generate().unwrap().public_jwk();
        key.nbf = Some(1_743_465_600_000);
        assert!(key.is_current(1_743_465_600));
        assert!(!key.is_current(1_743_465_599));
        key.nbf = Some(1_700_000_000);
        key.exp = Some(1_800_000_000);
        assert!(key.is_current(1_750_000_000));
        assert!(!key.is_current(1_800_000_000));
    }

    #[test]
    fn parse_skips_unusable_entries() {
        let good = Ed25519SigningKey::generate().unwrap();
        let doc = serde_json::json!({
            "keys": [
                {"kty": "RSA", "n": "abc", "e": "AQAB"},
                {"kty": "OKP", "crv": "Ed25519", "x": "short"},
                {"kty": "OKP", "crv": "Ed25519", "x": good.public_jwk().x, "d": "secret"},
                {"kty": "OKP", "crv": "Ed25519", "x": good.public_jwk().x, "use": "enc"},
                {"kty": "OKP", "crv": "Ed25519", "x": good.public_jwk().x, "alg": "ES256"},
                good.public_jwk(),
            ],
            "purpose": "rag"
        });
        let parsed = KeyDirectory::parse(doc.to_string().as_bytes(), 16).unwrap();
        assert_eq!(parsed.keys, vec![good.public_jwk()]);
        assert!(KeyDirectory::parse(doc.to_string().as_bytes(), 3).is_err());
        assert!(KeyDirectory::parse(b"[]", 3).is_err());
    }

    #[test]
    fn from_jwk_rejects_mismatched_public_key() {
        let a = Ed25519SigningKey::generate().unwrap();
        let b = Ed25519SigningKey::generate().unwrap();
        let mut jwk: serde_json::Value = serde_json::from_str(&a.to_private_jwk()).unwrap();
        jwk["x"] = serde_json::Value::String(b.public_jwk().x);
        assert!(Ed25519SigningKey::from_jwk(&jwk.to_string()).is_err());
        assert!(Ed25519SigningKey::from_jwk(r#"{"kty":"OKP","crv":"Ed25519"}"#).is_err());
    }

    #[test]
    fn signatures_verify() {
        let key = Ed25519SigningKey::generate().unwrap();
        let signature = key.sign(b"message");
        assert!(verify_ed25519(key.public_key(), b"message", &signature));
        assert!(!verify_ed25519(key.public_key(), b"other", &signature));
    }
}
