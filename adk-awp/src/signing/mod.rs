//! AWP 2.0 signatures (feature `signing`; spec 3.5 §7.2, §7.7, D-11, A-I5).
//!
//! | Module | Contents | AWP 2.0 |
//! |---|---|---|
//! | [`jws`] | Detached JWS (EdDSA) over the JCS form of a document without `proof`; `DocumentSigner`; digests and RFC 7638 thumbprints | §5.1, §5.4, §1.2 |
//! | [`keys`] | Delegations, revocations, root endorsement; verifying a key set; the key for a scope and date | §5.2–§5.5, §5.7 |
//! | [`anchor`] | The `_awp-root` DNS record and `DnsTxtResolver` | §5.6 |
//! | [`card`] | Signed A2A Agent Cards | §7.4.2 |
//! | [`log`] | The site log's entries, tiles and signed checkpoint | §14.5 |
//! | [`bundle`] | `StaticBundle`: the signed contract, `keys.json` and public terms | §4, §13.2 |
//!
//! JCS, Ed25519 and signed notes come from `adk-auth`. Keys never appear in `Debug`, errors or
//! logs (A-I5).

pub mod anchor;
pub mod bundle;
pub mod card;
pub mod jws;
pub mod keys;
pub mod log;

/// Why a document could not be signed (spec 3.5 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SigningError {
    /// The document is not I-JSON.
    #[error("the document cannot be canonicalised: {0}")]
    Canonicalize(String),
    /// The key is invalid or could not sign.
    #[error("the signing key failed: {0}")]
    Key(String),
    /// A log tile or entry bundle is malformed.
    #[error("a log tile is malformed: {0}")]
    Tile(String),
}
