//! The key hierarchy: an offline root, root-signed delegations with scopes and windows,
//! revocations and root rotation (spec 3.5 §7.2, D-11, A-I5; AWP 2.0 §5.2–§5.7).

use awp_types::{
    Delegation, KeyScope, KeySet, Proof, PublicKeyJwk, Revocation, RootEndorsement, RootKey,
};
use chrono::{DateTime, Utc};

use super::SigningError;
use super::jws::{DocumentSigner, sign, verify};

/// Why a key set or a signing key does not verify (AWP 2.0 §5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
    /// A delegation's proof does not verify with the root or a retired root.
    #[error("delegation `{kid}` is not signed by the root: {reason}")]
    Delegation {
        /// The delegated key id.
        kid: String,
        /// Why.
        reason: String,
    },
    /// A revocation's proof does not verify with the root.
    #[error("revocation of `{kid}` is not signed by the root: {reason}")]
    Revocation {
        /// The revoked key id.
        kid: String,
        /// Why.
        reason: String,
    },
    /// The root's endorsement does not verify with the previous root.
    #[error("the root's endorsement does not verify with the previous root: {0}")]
    Endorsement(String),
    /// No delegation has this key id.
    #[error("no delegated key `{0}`")]
    Unknown(String),
    /// The key is not delegated for the scope.
    #[error("key `{kid}` is not delegated for scope {scope:?}")]
    Scope {
        /// The key id.
        kid: String,
        /// The required scope.
        scope: KeyScope,
    },
    /// The document's time is outside the delegation's window.
    #[error("key `{0}` was not valid at the document's time")]
    Window(String),
    /// The key is revoked for this document.
    #[error("key `{0}` is revoked")]
    Revoked(String),
}

/// Signs a delegation of `public_key` as `kid` for `scopes` during `not_before..=not_after`
/// with the root key (AWP 2.0 §5.3).
///
/// # Errors
///
/// [`SigningError`] when the root cannot sign.
pub async fn delegate(
    root: &dyn DocumentSigner,
    kid: impl Into<String>,
    public_key: PublicKeyJwk,
    scopes: Vec<KeyScope>,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
) -> Result<Delegation, SigningError> {
    let mut delegation = Delegation {
        kid: kid.into(),
        public_key,
        scopes,
        not_before,
        not_after,
        proof: Proof::eddsa(root.kid(), ""),
    };
    delegation.proof = sign(&delegation, root).await?;
    Ok(delegation)
}

/// Signs a revocation of key `kid` from `revoked_at` with the root key (AWP 2.0 §5.5).
///
/// # Errors
///
/// [`SigningError`] when the root cannot sign.
pub async fn revoke(
    root: &dyn DocumentSigner,
    kid: impl Into<String>,
    revoked_at: DateTime<Utc>,
) -> Result<Revocation, SigningError> {
    let mut revocation =
        Revocation { kid: kid.into(), revoked_at, proof: Proof::eddsa(root.kid(), "") };
    revocation.proof = sign(&revocation, root).await?;
    Ok(revocation)
}

/// The root entry of a new root key, endorsed by the previous root over the JCS form of the
/// new root without its endorsement (AWP 2.0 §5.7).
///
/// # Errors
///
/// [`SigningError`] when the previous root cannot sign.
pub async fn endorse(
    previous: &dyn DocumentSigner,
    new_root: &dyn DocumentSigner,
) -> Result<RootKey, SigningError> {
    let mut root = RootKey {
        kid: new_root.kid().to_owned(),
        public_key: new_root.public_key(),
        endorsement: None,
    };
    let proof = sign(&root, previous).await?;
    root.endorsement = Some(RootEndorsement { previous: previous.public_key(), proof });
    Ok(root)
}

/// Verifies every proof in a key set: each delegation and revocation by the root (or, for a
/// delegation, a retired root), and the root's endorsement by its previous root (AWP 2.0 §5.2,
/// §5.7).
///
/// # Errors
///
/// [`KeyError`] naming the first entry that fails.
pub fn verify_key_set(keys: &KeySet) -> Result<(), KeyError> {
    if let Some(endorsement) = &keys.root.endorsement {
        let unendorsed = RootKey { endorsement: None, ..keys.root.clone() };
        verify(&unendorsed, &endorsement.proof, &endorsement.previous)
            .map_err(|error| KeyError::Endorsement(error.to_string()))?;
    }
    for delegation in &keys.delegations {
        let key = signer_key(keys, &delegation.proof.kid).ok_or_else(|| KeyError::Delegation {
            kid: delegation.kid.clone(),
            reason: format!("`{}` is neither the root nor a retired root", delegation.proof.kid),
        })?;
        verify(delegation, &delegation.proof, key).map_err(|error| KeyError::Delegation {
            kid: delegation.kid.clone(),
            reason: error.to_string(),
        })?;
    }
    for revocation in &keys.revocations {
        if revocation.proof.kid != keys.root.kid {
            return Err(KeyError::Revocation {
                kid: revocation.kid.clone(),
                reason: "only the current root revokes".to_owned(),
            });
        }
        verify(revocation, &revocation.proof, &keys.root.public_key).map_err(|error| {
            KeyError::Revocation { kid: revocation.kid.clone(), reason: error.to_string() }
        })?;
    }
    Ok(())
}

fn signer_key<'a>(keys: &'a KeySet, kid: &str) -> Option<&'a PublicKeyJwk> {
    if keys.root.kid == kid {
        return Some(&keys.root.public_key);
    }
    keys.retired_roots.iter().find(|root| root.kid == kid).map(|root| &root.public_key)
}

/// When a document is checked: a live document at verification time, a durable one at its
/// own time (AWP 2.0 §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Dated {
    /// An Agent Card, checkpoint or site status, checked at `now`; any revocation rejects it.
    Live(DateTime<Utc>),
    /// A receipt, quote or stream entry, checked at its own time.
    Durable(DateTime<Utc>),
}

/// Returns the public key of delegated key `kid` for a document needing `scope` (AWP 2.0
/// §5.3, §5.5). Call [`verify_key_set`] first.
///
/// # Errors
///
/// [`KeyError`] when the key is unknown, not delegated for `scope`, outside its window, or
/// revoked.
///
/// # Example
///
/// ```
/// use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
/// use adk_awp::signing::keys::{Dated, delegate, delegated_key, verify_key_set};
/// use awp_types::{KeyScope, KeySet, RootKey};
/// use chrono::{Duration, Utc};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let root = Ed25519DocumentSigner::from_seed("root", [1; 32]).unwrap();
/// let edge = Ed25519DocumentSigner::from_seed("edge", [2; 32]).unwrap();
/// let now = Utc::now();
/// let delegation = delegate(&root, "edge", edge.public_key(), vec![KeyScope::Receipt], now, now + Duration::days(30))
///     .await
///     .unwrap();
/// let keys = KeySet {
///     root: RootKey { kid: "root".into(), public_key: root.public_key(), endorsement: None },
///     delegations: vec![delegation],
///     retired_roots: vec![],
///     revocations: vec![],
/// };
/// verify_key_set(&keys).unwrap();
/// assert!(delegated_key(&keys, "edge", KeyScope::Receipt, Dated::Live(now)).is_ok());
/// assert!(delegated_key(&keys, "edge", KeyScope::Release, Dated::Live(now)).is_err());
/// # }
/// ```
pub fn delegated_key<'a>(
    keys: &'a KeySet,
    kid: &str,
    scope: KeyScope,
    dated: Dated,
) -> Result<&'a PublicKeyJwk, KeyError> {
    let delegation = keys
        .delegations
        .iter()
        .find(|delegation| delegation.kid == kid)
        .ok_or_else(|| KeyError::Unknown(kid.to_owned()))?;
    if !delegation.allows(scope) {
        return Err(KeyError::Scope { kid: kid.to_owned(), scope });
    }
    let (at, live) = match dated {
        Dated::Live(at) => (at, true),
        Dated::Durable(at) => (at, false),
    };
    if !delegation.is_valid_at(at) {
        return Err(KeyError::Window(kid.to_owned()));
    }
    let revoked = keys
        .revocations
        .iter()
        .any(|revocation| revocation.kid == kid && (live || revocation.revoked_at <= at));
    if revoked {
        return Err(KeyError::Revoked(kid.to_owned()));
    }
    Ok(&delegation.public_key)
}
