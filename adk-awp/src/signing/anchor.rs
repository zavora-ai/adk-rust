//! The DNS root anchor: `_awp-root.{domain}` TXT `v=awp2; kid=…; jkt=…` (spec 3.5 §7.2;
//! AWP 2.0 §5.6).
//!
//! The record is what makes a key set authentic: an attacker who controls only the host can
//! replace `keys.json` and re-sign the contract with a key of their own, and the anchor exposes
//! it. A person changes the record through the registrar or DNS provider.

use std::collections::BTreeMap;

use async_trait::async_trait;
use awp_types::PublicKeyJwk;

use super::jws::jwk_thumbprint;

/// Why a root is not anchored (AWP 2.0 §5.6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AnchorError {
    /// The lookup failed.
    #[error("the _awp-root lookup failed: {0}")]
    Lookup(String),
    /// No record's `jkt` matches the root key.
    #[error("no _awp-root record for {domain} names the root key; publish `{expected}`")]
    Unanchored {
        /// The domain.
        domain: String,
        /// The record that would anchor the root.
        expected: String,
    },
}

/// A TXT answer and whether DNSSEC authenticated it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TxtAnswer {
    /// The TXT strings, each record's strings concatenated.
    pub records: Vec<String>,
    /// Whether the resolver validated the answer with DNSSEC.
    pub authenticated: bool,
}

/// Resolves DNS TXT records (AWP 2.0 §5.6).
#[async_trait]
pub trait DnsTxtResolver: Send + Sync {
    /// Returns the TXT records at `name`.
    ///
    /// # Errors
    ///
    /// [`AnchorError::Lookup`] when the lookup fails; an absent name is an empty answer.
    async fn txt(&self, name: &str) -> Result<TxtAnswer, AnchorError>;
}

/// A resolver answering from a fixed table, for tests and offline verification.
///
/// # Example
///
/// ```
/// use adk_awp::signing::anchor::{DnsTxtResolver, FixedTxtResolver};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let resolver = FixedTxtResolver::default().with("_awp-root.example.com", "v=awp2; kid=root; jkt=abc");
/// assert_eq!(resolver.txt("_awp-root.example.com").await.unwrap().records.len(), 1);
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct FixedTxtResolver {
    records: BTreeMap<String, Vec<String>>,
    authenticated: bool,
}

impl FixedTxtResolver {
    /// Adds a TXT record at `name`.
    pub fn with(mut self, name: &str, record: &str) -> Self {
        self.records.entry(name.to_ascii_lowercase()).or_default().push(record.to_owned());
        self
    }

    /// Marks every answer as DNSSEC-authenticated.
    pub fn authenticated(mut self) -> Self {
        self.authenticated = true;
        self
    }
}

#[async_trait]
impl DnsTxtResolver for FixedTxtResolver {
    async fn txt(&self, name: &str) -> Result<TxtAnswer, AnchorError> {
        Ok(TxtAnswer {
            records: self.records.get(&name.to_ascii_lowercase()).cloned().unwrap_or_default(),
            authenticated: self.authenticated,
        })
    }
}

/// The name of a domain's root record: `_awp-root.{domain}`.
pub fn root_record_name(domain: &str) -> String {
    format!("_awp-root.{}", domain.trim_end_matches('.').to_ascii_lowercase())
}

/// The TXT value anchoring root key `kid`: `v=awp2; kid={kid}; jkt={thumbprint}`.
///
/// # Example
///
/// ```
/// use adk_awp::signing::anchor::root_record;
/// use awp_types::PublicKeyJwk;
///
/// let jwk = PublicKeyJwk::ed25519("11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo");
/// assert_eq!(root_record("site-root-2026", &jwk), "v=awp2; kid=site-root-2026; jkt=kPrK_qmxVWaYVA9wwBF6Iuo3vVzz7TxHCTwXBygrS4k");
/// ```
pub fn root_record(kid: &str, root: &PublicKeyJwk) -> String {
    format!("v=awp2; kid={kid}; jkt={}", jwk_thumbprint(root))
}

/// A parsed root record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRecord {
    /// The root key's id.
    pub kid: String,
    /// The root key's RFC 7638 thumbprint.
    pub jkt: String,
}

impl RootRecord {
    /// Parses `v=awp2; kid=…; jkt=…`; tags may come in any order; unknown tags are ignored.
    pub fn parse(record: &str) -> Option<Self> {
        let mut version = None;
        let mut kid = None;
        let mut jkt = None;
        for tag in record.split(';') {
            let (name, value) = tag.split_once('=')?;
            match name.trim() {
                "v" => version = Some(value.trim().to_owned()),
                "kid" => kid = Some(value.trim().to_owned()),
                "jkt" => jkt = Some(value.trim().to_owned()),
                _ => {}
            }
        }
        (version.as_deref() == Some("awp2")).then_some(())?;
        Some(Self { kid: kid?, jkt: jkt? })
    }
}

/// A root key anchored in DNS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchored {
    /// The record that names the key.
    pub record: RootRecord,
    /// Whether DNSSEC authenticated the answer.
    pub authenticated: bool,
}

/// Checks that a `_awp-root.{domain}` record names `root` (AWP 2.0 §5.6).
///
/// # Errors
///
/// [`AnchorError::Unanchored`] when no record matches; [`AnchorError::Lookup`] when the
/// lookup fails.
pub async fn check_root_anchor(
    resolver: &dyn DnsTxtResolver,
    domain: &str,
    root_kid: &str,
    root: &PublicKeyJwk,
) -> Result<Anchored, AnchorError> {
    let answer = resolver.txt(&root_record_name(domain)).await?;
    let thumbprint = jwk_thumbprint(root);
    answer
        .records
        .iter()
        .filter_map(|record| RootRecord::parse(record))
        .find(|record| record.jkt == thumbprint)
        .map(|record| Anchored { record, authenticated: answer.authenticated })
        .ok_or_else(|| AnchorError::Unanchored {
            domain: domain.to_owned(),
            expected: root_record(root_kid, root),
        })
}
