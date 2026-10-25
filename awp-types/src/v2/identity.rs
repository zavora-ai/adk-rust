//! Keys, delegations and proofs (spec 3.5 §7.2) and requester identity and trust tiers
//! (spec 3.5 §7.3).

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A detached JWS (spec 3.5 §7.2).
///
/// `jws` is the compact serialization with an empty payload; the signed payload is the JCS
/// form of the enclosing document without its `proof`.
///
/// # Example
///
/// ```
/// use awp_types::Proof;
///
/// let proof = Proof::eddsa("site-root-2026", "eyJhbGciOiJFZERTQSJ9..c2ln");
/// assert_eq!(proof.alg, "EdDSA");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Proof {
    /// JWS algorithm, `EdDSA` in AWP 2.0.
    pub alg: String,
    /// Key id of the signing key in `keys.json`.
    pub kid: String,
    /// Detached compact JWS.
    pub jws: String,
}

impl Proof {
    /// The JWS algorithm AWP 2.0 signs with.
    pub const EDDSA: &'static str = "EdDSA";

    /// Creates an `EdDSA` proof.
    pub fn eddsa(kid: impl Into<String>, jws: impl Into<String>) -> Self {
        Self { alg: Self::EDDSA.to_owned(), kid: kid.into(), jws: jws.into() }
    }
}

/// An Ed25519 public key as a JWK (RFC 8037) (spec 3.5 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PublicKeyJwk {
    /// Key type, `OKP`.
    pub kty: String,
    /// Curve, `Ed25519`.
    pub crv: String,
    /// The public key, base64url without padding.
    pub x: String,
}

impl PublicKeyJwk {
    /// Creates an Ed25519 JWK from a base64url public key.
    pub fn ed25519(x: impl Into<String>) -> Self {
        Self { kty: "OKP".to_owned(), crv: "Ed25519".to_owned(), x: x.into() }
    }
}

/// `keys.json`: the site root key and its delegations (spec 3.5 §7.2).
///
/// Verification walks root → delegation → signature. A new root carries an endorsement by
/// the previous root, which moves to `retiredRoots`; recovery publishes a root through the
/// `_awp-root.<domain>` DNS TXT record, which a person changes. A compromised delegated key is
/// withdrawn by a root-signed revocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KeySet {
    /// The site root key, which signs the contract and delegations.
    pub root: RootKey,
    /// Root-signed delegations to release, card, receipt and payment keys.
    #[serde(default)]
    pub delegations: Vec<Delegation>,
    /// Earlier roots, kept so the delegations they signed still verify.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_roots: Vec<RetiredRoot>,
    /// Root-signed revocations of delegated keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revocations: Vec<Revocation>,
}

impl KeySet {
    /// Returns the delegations allowed to sign for `scope` on a document dated `at`: within
    /// their window and not revoked at or before `at`.
    pub fn delegates_for(
        &self,
        scope: KeyScope,
        at: DateTime<Utc>,
    ) -> impl Iterator<Item = &Delegation> {
        self.delegations
            .iter()
            .filter(move |d| d.allows(scope) && d.is_valid_at(at) && !self.is_revoked(&d.kid, at))
    }

    /// Returns `true` when key `kid` is revoked for a document dated `at`: a revocation names
    /// it with `revokedAt` at or before `at`.
    pub fn is_revoked(&self, kid: &str, at: DateTime<Utc>) -> bool {
        self.revocations
            .iter()
            .any(|revocation| revocation.kid == kid && revocation.revoked_at <= at)
    }
}

/// An earlier root key, kept after a rotation (spec 3.5 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RetiredRoot {
    /// Key id.
    pub kid: String,
    /// The public key.
    pub public_key: PublicKeyJwk,
    /// When it stopped being the root.
    pub retired_at: DateTime<Utc>,
}

/// A root-signed withdrawal of a delegated key (spec 3.5 §7.2).
///
/// A signature by the key on a document dated at or after `revokedAt`, or on any live
/// document, is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Revocation {
    /// Key id of the revoked key.
    pub kid: String,
    /// When the revocation takes effect.
    pub revoked_at: DateTime<Utc>,
    /// The root key's detached JWS over the JCS form of the revocation without `proof`.
    pub proof: Proof,
}

/// Returns `document` as JSON without its top-level `proof` member: the value whose JCS form
/// a proof signs (spec 3.5 §7.2).
///
/// Canonicalisation and signing happen in `adk-awp`.
///
/// # Errors
///
/// Returns the serialization error when `document` does not serialize to JSON.
///
/// # Example
///
/// ```
/// use awp_types::{Proof, without_proof};
///
/// let proof = Proof::eddsa("edge-2026-10", "e30..c2ln");
/// let value = without_proof(&proof).unwrap();
/// assert_eq!(value["kid"], "edge-2026-10");
/// ```
pub fn without_proof<T: Serialize>(document: &T) -> Result<serde_json::Value, serde_json::Error> {
    let mut value = serde_json::to_value(document)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("proof");
    }
    Ok(value)
}

/// The site root key (spec 3.5 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RootKey {
    /// Key id.
    pub kid: String,
    /// The public key.
    pub public_key: PublicKeyJwk,
    /// The previous root's signature over this root, after a rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endorsement: Option<RootEndorsement>,
}

/// A previous root key's signature over its successor (spec 3.5 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RootEndorsement {
    /// The previous root's public key.
    pub previous: PublicKeyJwk,
    /// Detached JWS by the previous root over the JCS form of the new root without its
    /// endorsement; its `kid` names the previous root.
    pub proof: Proof,
}

/// A root-signed delegation to a short-lived key (spec 3.5 §7.2).
///
/// # Example
///
/// ```
/// use awp_types::{Delegation, KeyScope, Proof, PublicKeyJwk};
/// use chrono::{TimeZone, Utc};
///
/// let delegation = Delegation {
///     kid: "release-2026-10".into(),
///     public_key: PublicKeyJwk::ed25519("11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"),
///     scopes: vec![KeyScope::Release],
///     not_before: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
///     not_after: Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap(),
///     proof: Proof::eddsa("site-root-2026", "..."),
/// };
/// assert!(delegation.allows(KeyScope::Release));
/// assert!(!delegation.allows(KeyScope::Receipt));
/// assert!(delegation.is_valid_at(Utc.with_ymd_and_hms(2026, 10, 15, 0, 0, 0).unwrap()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Delegation {
    /// Key id of the delegated key.
    pub kid: String,
    /// The delegated public key.
    pub public_key: PublicKeyJwk,
    /// What the delegated key may sign.
    pub scopes: Vec<KeyScope>,
    /// Start of the validity window.
    pub not_before: DateTime<Utc>,
    /// End of the validity window.
    pub not_after: DateTime<Utc>,
    /// The root key's detached JWS over the JCS form of the delegation without `proof`.
    pub proof: Proof,
}

impl Delegation {
    /// Returns `true` when the delegation covers `scope`.
    pub fn allows(&self, scope: KeyScope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Returns `true` when `at` falls within `notBefore..=notAfter`.
    pub fn is_valid_at(&self, at: DateTime<Utc>) -> bool {
        self.not_before <= at && at <= self.not_after
    }
}

/// What a delegated key may sign (spec 3.5 §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum KeyScope {
    /// Site log checkpoints, and through them release manifests.
    Release,
    /// A2A Agent Cards.
    Card,
    /// Documents the edge issues: commerce receipts, consent receipts, site status, stream
    /// entries and quotes.
    Receipt,
    /// Quotes and payment mandates presented to a rail.
    Payments,
}

/// The identities a site accepts and where its trust rules are published (spec 3.5 §7.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct IdentityPolicy {
    /// Accepted requester identity methods.
    #[serde(default)]
    pub accepts: Vec<IdentityMethod>,
    /// URL of the published trust rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_rules: Option<String>,
}

/// A way a requester proves its identity (spec 3.5 §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum IdentityMethod {
    /// Web Bot Auth over HTTP Message Signatures (RFC 9421).
    WebBotAuth,
    /// An OAuth 2.0 client-credentials token.
    #[serde(rename = "oauth2-client-credentials")]
    Oauth2ClientCredentials,
    /// Mutual TLS.
    Mtls,
    /// A decentralized identifier.
    Did,
    /// An API key the owner issued.
    ApiKey,
}

/// A requester's trust tier, ordered `Public < Known < Verified < Partner` (spec 3.5 §7.3).
///
/// Trust above `Public` requires a verified credential (A-I6); claims never raise it.
///
/// # Example
///
/// ```
/// use awp_types::TrustTier;
///
/// assert!(TrustTier::Public < TrustTier::Known);
/// assert!(TrustTier::Known < TrustTier::Verified);
/// assert!(TrustTier::Verified < TrustTier::Partner);
/// assert_eq!(TrustTier::Verified.to_string(), "verified");
/// ```
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrustTier {
    /// Earned by nothing.
    #[default]
    Public,
    /// A valid Web Bot Auth signature, an OAuth client-credentials token, or an owner-issued
    /// API key.
    Known,
    /// A known identity from a key directory or DID issuer on the owner's verified list.
    Verified,
    /// An owner allowlist entry.
    Partner,
}

impl TrustTier {
    /// Every tier, lowest first.
    pub const ALL: [TrustTier; 4] =
        [TrustTier::Public, TrustTier::Known, TrustTier::Verified, TrustTier::Partner];

    /// Returns the wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            TrustTier::Public => "public",
            TrustTier::Known => "known",
            TrustTier::Verified => "verified",
            TrustTier::Partner => "partner",
        }
    }
}

impl fmt::Display for TrustTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An owner rule granting a tier to verified identities (spec 3.5 §7.3).
///
/// `subject` is a [`RequesterId`], or a prefix ending in `*` that matches every id it
/// starts — for example `wba:https://bots.example.com#*` for every key of one directory.
///
/// # Example
///
/// ```
/// use awp_types::{RequesterId, TrustRule, TrustTier};
///
/// let rule = TrustRule {
///     subject: "wba:https://bots.example.com#*".into(),
///     tier: TrustTier::Partner,
///     rate_class: None,
///     note: None,
/// };
/// assert!(rule.matches(&RequesterId::web_bot_auth("https://bots.example.com", "abc")));
/// assert!(!rule.matches(&RequesterId::web_bot_auth("https://other.example", "abc")));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TrustRule {
    /// A requester id, or a prefix ending in `*`.
    pub subject: String,
    /// The tier granted to matching identities.
    pub tier: TrustTier,
    /// Rate-limit class for matching identities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_class: Option<String>,
    /// Owner's note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl TrustRule {
    /// Returns `true` when `requester` matches the rule's subject.
    pub fn matches(&self, requester: &RequesterId) -> bool {
        match self.subject.strip_suffix('*') {
            Some(prefix) => requester.as_str().starts_with(prefix),
            None => requester.as_str() == self.subject,
        }
    }
}

/// The trust rules the edge publishes at the contract's `identity.trustRules` (spec 3.5 §7.3).
///
/// # Example
///
/// ```
/// use awp_types::{RequesterId, TrustRule, TrustRules, TrustTier};
///
/// let rules = TrustRules {
///     accepts: vec![],
///     rules: vec![TrustRule {
///         subject: "wba:https://bots.example.com#*".into(),
///         tier: TrustTier::Verified,
///         rate_class: None,
///         note: None,
///     }],
///     rate_classes: Default::default(),
/// };
/// assert_eq!(rules.tier_for(&RequesterId::web_bot_auth("https://bots.example.com", "k")), TrustTier::Verified);
/// assert_eq!(rules.tier_for(&RequesterId::api_key("k1")), TrustTier::Known);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TrustRules {
    /// The identity methods the edge verifies.
    #[serde(default)]
    pub accepts: Vec<IdentityMethod>,
    /// Trust rules.
    #[serde(default)]
    pub rules: Vec<TrustRule>,
    /// Limits per rate class.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rate_classes: BTreeMap<String, RateClass>,
}

impl TrustRules {
    /// Returns the tier of a requester whose credential verified: the highest tier among the
    /// rules its id matches, or `Known` when none matches.
    pub fn tier_for(&self, requester: &RequesterId) -> TrustTier {
        self.rules
            .iter()
            .filter(|rule| rule.matches(requester))
            .map(|rule| rule.tier)
            .max()
            .unwrap_or(TrustTier::Known)
    }
}

/// The limits of one rate class (spec 3.5 §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RateClass {
    /// Requests per minute.
    pub requests_per_minute: u32,
    /// Capability calls per day.
    pub calls_per_day: u32,
}

/// A verified requester's identifier (spec 3.5 §7.3).
///
/// | Method | Form |
/// |---|---|
/// | Web Bot Auth | `wba:{directory origin}#{key thumbprint}` |
/// | OAuth client credentials | `oauth:{issuer}#{client id}` |
/// | mTLS | `mtls:{base64url SHA-256 of the DER certificate}` |
/// | DID | the DID itself, `did:{method}:{id}` |
/// | API key | `key:{key id}` |
///
/// # Example
///
/// ```
/// use awp_types::{IdentityMethod, RequesterId};
///
/// let id = RequesterId::web_bot_auth("https://bots.example.com", "NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs");
/// assert_eq!(id.as_str(), "wba:https://bots.example.com#NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs");
/// assert_eq!(id.method(), Some(IdentityMethod::WebBotAuth));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct RequesterId(String);

impl RequesterId {
    /// Wraps an identifier as received.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// `wba:{directory origin}#{key thumbprint}`.
    pub fn web_bot_auth(directory_origin: &str, thumbprint: &str) -> Self {
        Self(format!("wba:{directory_origin}#{thumbprint}"))
    }

    /// `oauth:{issuer}#{client id}`.
    pub fn oauth_client(issuer: &str, client_id: &str) -> Self {
        Self(format!("oauth:{issuer}#{client_id}"))
    }

    /// `mtls:{certificate thumbprint}`.
    pub fn mtls(thumbprint: &str) -> Self {
        Self(format!("mtls:{thumbprint}"))
    }

    /// The DID itself.
    pub fn did(did: &str) -> Self {
        Self(did.to_owned())
    }

    /// `key:{key id}`.
    pub fn api_key(key_id: &str) -> Self {
        Self(format!("key:{key_id}"))
    }

    /// Returns the identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the identity method the identifier's form names, if any.
    pub fn method(&self) -> Option<IdentityMethod> {
        let (scheme, _) = self.0.split_once(':')?;
        match scheme {
            "wba" => Some(IdentityMethod::WebBotAuth),
            "oauth" => Some(IdentityMethod::Oauth2ClientCredentials),
            "mtls" => Some(IdentityMethod::Mtls),
            "did" => Some(IdentityMethod::Did),
            "key" => Some(IdentityMethod::ApiKey),
            _ => None,
        }
    }
}

impl fmt::Display for RequesterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
