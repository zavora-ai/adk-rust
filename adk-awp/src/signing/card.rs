//! Signed A2A Agent Cards: a JWS in the card's `signatures` member over the JCS form of the
//! card without `signatures`, by a delegated `card` key (spec 3.5 §7.4; A2A v1.0 §8.4;
//! AWP 2.0 §7.4.2).

use adk_auth::signed_audit::canonicalize;
use awp_types::{KeyScope, KeySet};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::SigningError;
use super::jws::{ALG, DocumentSigner, ProofError, verify_signature};
use super::keys::{Dated, KeyError, delegated_key};

/// Why an Agent Card's signature does not verify (AWP 2.0 §7.4.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CardError {
    /// The card is not a JSON object or carries no signature.
    #[error("the Agent Card carries no signature")]
    Unsigned,
    /// No signature verifies with a valid `card` delegation.
    #[error("no Agent Card signature verifies with a valid card key: {0}")]
    Invalid(String),
}

fn signing_input(card: &Value, protected: &str) -> Result<String, String> {
    let mut unsigned = card.clone();
    if let Some(object) = unsigned.as_object_mut() {
        object.remove("signatures");
    }
    let canonical = canonicalize(&unsigned).map_err(|error| error.to_string())?;
    Ok(format!("{protected}.{}", URL_SAFE_NO_PAD.encode(canonical.as_bytes())))
}

/// Appends a signature by `signer` to the card's `signatures` (A2A v1.0 §8.4).
///
/// # Errors
///
/// [`SigningError`] when the card is not an I-JSON object or the key cannot sign.
pub async fn sign_agent_card(
    card: &Value,
    signer: &dyn DocumentSigner,
) -> Result<Value, SigningError> {
    if !card.is_object() {
        return Err(SigningError::Canonicalize("an Agent Card is a JSON object".to_owned()));
    }
    let header = json!({ "alg": ALG, "kid": signer.kid(), "typ": "JOSE" });
    let protected = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());
    let input = signing_input(card, &protected).map_err(SigningError::Canonicalize)?;
    let signature = URL_SAFE_NO_PAD.encode(signer.sign(input.as_bytes()).await?);
    let mut signed = card.clone();
    let signatures = signed
        .as_object_mut()
        .map(|object| object.entry("signatures").or_insert_with(|| json!([])))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| SigningError::Canonicalize("`signatures` is not an array".to_owned()))?;
    signatures.push(json!({ "protected": protected, "signature": signature }));
    Ok(signed)
}

/// Verifies that one of the card's signatures is by a `card` key delegated in `keys` and valid
/// at `now` (AWP 2.0 §7.4.2, §5.5). Call `verify_key_set` on `keys` first.
///
/// # Errors
///
/// [`CardError`] when the card is unsigned or no signature verifies.
pub fn verify_agent_card(card: &Value, keys: &KeySet, now: DateTime<Utc>) -> Result<(), CardError> {
    let signatures = card
        .get("signatures")
        .and_then(Value::as_array)
        .filter(|signatures| !signatures.is_empty())
        .ok_or(CardError::Unsigned)?;
    let mut reasons = Vec::new();
    for entry in signatures {
        match verify_one(card, entry, keys, now) {
            Ok(()) => return Ok(()),
            Err(reason) => reasons.push(reason),
        }
    }
    Err(CardError::Invalid(reasons.join("; ")))
}

fn verify_one(
    card: &Value,
    entry: &Value,
    keys: &KeySet,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let protected = entry.get("protected").and_then(Value::as_str).ok_or("no protected header")?;
    let signature = entry.get("signature").and_then(Value::as_str).ok_or("no signature")?;
    let header: Value = URL_SAFE_NO_PAD
        .decode(protected)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or("the protected header is not base64url JSON")?;
    if header.get("alg").and_then(Value::as_str) != Some(ALG) || header.get("crit").is_some() {
        return Err("the protected header must be EdDSA without crit".to_owned());
    }
    let kid =
        header.get("kid").and_then(Value::as_str).ok_or("the protected header names no kid")?;
    let key = delegated_key(keys, kid, KeyScope::Card, Dated::Live(now))
        .map_err(|error: KeyError| error.to_string())?;
    let input = signing_input(card, protected)?;
    let signature =
        URL_SAFE_NO_PAD.decode(signature).map_err(|_| "the signature is not base64url")?;
    verify_signature(key, input.as_bytes(), &signature)
        .map_err(|error: ProofError| error.to_string())
}
