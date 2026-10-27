//! The trust list packs are verified against (spec 3.5 §7.23): the publisher keys the owner
//! trusts, each named by its publisher and key id.
//!
//! A site keeps its list in `packs/trust.json`:
//!
//! ```json
//! {
//!   "keys": [
//!     {
//!       "publisher": "tables.example",
//!       "kid": "tables-2026",
//!       "publicKey": { "kty": "OKP", "crv": "Ed25519", "x": "…" },
//!       "note": "From tables.example/.well-known/packs-key.json, checked by phone"
//!     }
//!   ]
//! }
//! ```
//!
//! Nothing is trusted implicitly: a site with no list installs no pack, and the key the
//! built-in packs are signed with is trusted only by [`builtin_trust`](super::builtin::builtin_trust),
//! for the copies compiled into adk-website.

use std::path::Path;

use adk_awp::signing::jws::jwk_thumbprint;
use awp_types::PublicKeyJwk;
use serde::{Deserialize, Serialize};

use super::bundle::PackError;

/// One trusted publisher key (spec 3.5 §7.23).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrustedKey {
    /// The publisher's id, as its bundles name it.
    pub publisher: String,
    /// The key id its bundles' proofs name.
    pub kid: String,
    /// The Ed25519 public key.
    pub public_key: PublicKeyJwk,
    /// Where the key came from, for the people who keep the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl TrustedKey {
    /// Trusts `public_key` as key `kid` of `publisher`.
    pub fn new(
        publisher: impl Into<String>,
        kid: impl Into<String>,
        public_key: PublicKeyJwk,
    ) -> Self {
        Self { publisher: publisher.into(), kid: kid.into(), public_key, note: None }
    }

    /// With a note on where the key came from.
    #[must_use]
    pub fn noted(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// The RFC 7638 thumbprint of the key, for comparing it with the publisher's.
    pub fn thumbprint(&self) -> String {
        jwk_thumbprint(&self.public_key)
    }
}

/// The publisher keys a site trusts to sign the packs it installs (spec 3.5 §7.23).
///
/// # Example
///
/// ```rust
/// use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
/// use adk_website::packs::trust::{TrustList, TrustedKey};
///
/// let key = Ed25519DocumentSigner::from_seed("tables-2026", [4; 32]).unwrap();
/// let trust = TrustList::new().with(TrustedKey::new("tables.example", "tables-2026", key.public_key()));
/// assert!(trust.key("tables.example", "tables-2026").is_some());
/// assert!(trust.key("tables.example", "tables-2027").is_none());
/// assert!(trust.key("other.example", "tables-2026").is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrustList {
    /// The trusted keys.
    #[serde(default)]
    pub keys: Vec<TrustedKey>,
}

impl TrustList {
    /// An empty list: no publisher is trusted.
    pub fn new() -> Self {
        Self::default()
    }

    /// The list with `key` trusted.
    #[must_use]
    pub fn with(mut self, key: TrustedKey) -> Self {
        self.add(key);
        self
    }

    /// Trusts `key`; `false` when the list already holds the same key under the same names.
    /// A key of the same publisher and id with another public key replaces the old one.
    pub fn add(&mut self, key: TrustedKey) -> bool {
        if self.keys.iter().any(|held| {
            held.publisher == key.publisher
                && held.kid == key.kid
                && held.public_key == key.public_key
        }) {
            return false;
        }
        self.keys.retain(|held| !(held.publisher == key.publisher && held.kid == key.kid));
        self.keys.push(key);
        true
    }

    /// The public key `publisher` signs with as `kid`, when it is trusted.
    pub fn key(&self, publisher: &str, kid: &str) -> Option<&PublicKeyJwk> {
        self.keys
            .iter()
            .find(|key| key.publisher == publisher && key.kid == kid)
            .map(|key| &key.public_key)
    }

    /// Reads the list at `path`; an empty list when there is no file.
    ///
    /// # Errors
    ///
    /// [`PackError::Io`] for a file that cannot be read or is not a trust list.
    pub fn read(path: &Path) -> Result<Self, PackError> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                PackError::Io(format!("{} is not a pack trust list: {error}", path.display()))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(error) => Err(PackError::Io(format!("{}: {error}", path.display()))),
        }
    }

    /// Writes the list to `path`, creating its directory.
    ///
    /// # Errors
    ///
    /// [`PackError::Io`] when the file cannot be written.
    pub fn write(&self, path: &Path) -> Result<(), PackError> {
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|error| PackError::Io(error.to_string()))?;
        bytes.push(b'\n');
        super::installed::write_atomically(path, &bytes)
    }
}
