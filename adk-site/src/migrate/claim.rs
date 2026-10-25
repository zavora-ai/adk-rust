//! Domain claims (spec 3.5 §7.18 stage 1, A-I9): proof that the owner controls a domain, by
//! a DNS TXT record or a token served at a well-known path. Until a claim is proven only
//! polite public reads run; cloaking checks, freezes and probes need a [`ProvenDomain`].

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::RwLock;
use url::Url;

use super::error::{MigrateError, Result};
use super::http::{FetchOptions, HttpClient};

/// The DNS label a claim's TXT record goes under: `_awp-claim.<domain>`.
pub const CLAIM_DNS_LABEL: &str = "_awp-claim";

/// The path prefix a claim's token is served under: `/.well-known/awp/claim/<token>`.
pub const CLAIM_WELL_KNOWN_PREFIX: &str = "/.well-known/awp/claim/";

/// A DNS record type a [`DnsResolver`] looks up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
#[non_exhaustive]
pub enum RecordKind {
    /// IPv4 address.
    A,
    /// IPv6 address.
    Aaaa,
    /// Canonical name.
    Cname,
    /// Mail exchanger.
    Mx,
    /// Name server.
    Ns,
    /// Text.
    Txt,
    /// Certification authority authorisation.
    Caa,
}

/// Looks up DNS records as text: addresses, names, `priority exchange` for MX, the joined
/// strings of a TXT record.
#[async_trait]
pub trait DnsResolver: Send + Sync + fmt::Debug {
    /// The records of `kind` at `name`; empty when there are none.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Dns`] when the lookup fails for another reason than an empty answer.
    async fn lookup(&self, name: &str, kind: RecordKind) -> Result<Vec<String>>;
}

/// The system's resolver configuration, through hickory.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDnsResolver;

#[async_trait]
impl DnsResolver for SystemDnsResolver {
    async fn lookup(&self, name: &str, kind: RecordKind) -> Result<Vec<String>> {
        use hickory_resolver::proto::rr::{RData, RecordType};
        let resolver = hickory_resolver::TokioResolver::builder_tokio()
            .map_err(|e| MigrateError::Dns(e.to_string()))?
            .build()
            .map_err(|e| MigrateError::Dns(e.to_string()))?;
        let record_type = match kind {
            RecordKind::A => RecordType::A,
            RecordKind::Aaaa => RecordType::AAAA,
            RecordKind::Cname => RecordType::CNAME,
            RecordKind::Mx => RecordType::MX,
            RecordKind::Ns => RecordType::NS,
            RecordKind::Txt => RecordType::TXT,
            RecordKind::Caa => RecordType::CAA,
        };
        let fqdn = format!("{}.", name.trim_end_matches('.'));
        let lookup = match resolver.lookup(fqdn.as_str(), record_type).await {
            Ok(lookup) => lookup,
            Err(error) if error.is_no_records_found() || error.is_nx_domain() => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(MigrateError::Dns(format!("{name} {kind:?}: {error}"))),
        };
        let mut values: Vec<String> = lookup
            .answers()
            .iter()
            .filter(|record| record.record_type() == record_type)
            .map(|record| match &record.data {
                RData::TXT(txt) => txt
                    .txt_data
                    .iter()
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect::<String>(),
                other => other.to_string().trim_end_matches('.').to_string(),
            })
            .collect();
        values.sort();
        values.dedup();
        Ok(values)
    }
}

/// A resolver over fixed records, for tests and recorded zones.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::claim::{DnsResolver, RecordKind, StaticDnsResolver};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let dns = StaticDnsResolver::new();
/// dns.set("example.com", RecordKind::A, ["203.0.113.7"]).await;
/// assert_eq!(dns.lookup("example.com", RecordKind::A).await.unwrap(), ["203.0.113.7"]);
/// # }
/// ```
#[derive(Debug, Default)]
pub struct StaticDnsResolver {
    records: RwLock<BTreeMap<(String, RecordKind), Vec<String>>>,
}

impl StaticDnsResolver {
    /// An empty zone.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the records of `kind` at `name`.
    pub async fn set<I, S>(&self, name: &str, kind: RecordKind, values: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut values: Vec<String> = values.into_iter().map(Into::into).collect();
        values.sort();
        self.records.write().await.insert((normalise(name), kind), values);
    }
}

#[async_trait]
impl DnsResolver for StaticDnsResolver {
    async fn lookup(&self, name: &str, kind: RecordKind) -> Result<Vec<String>> {
        Ok(self.records.read().await.get(&(normalise(name), kind)).cloned().unwrap_or_default())
    }
}

fn normalise(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// How a claim was proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimMethod {
    /// A TXT record at `_awp-claim.<domain>`.
    DnsTxt,
    /// The token served at `/.well-known/awp/claim/<token>`.
    WellKnown,
}

/// A pending claim on a domain: a random token the owner publishes by DNS or at a well-known
/// path (spec 3.5 §7.18 stage 1, A-I9).
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::claim::{DomainClaim, RecordKind, StaticDnsResolver};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let claim = DomainClaim::new("example.com", chrono::Utc::now()).unwrap();
/// let (name, value) = claim.dns_record();
/// let dns = StaticDnsResolver::new();
/// dns.set(&name, RecordKind::Txt, [value]).await;
/// let proven = claim.verify_dns(&dns, chrono::Utc::now()).await.unwrap();
/// assert_eq!(proven.domain(), "example.com");
/// # }
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainClaim {
    /// The domain.
    pub domain: String,
    /// The token, 64 lowercase hex digits.
    pub token: String,
    /// When the claim was made.
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for DomainClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DomainClaim")
            .field("domain", &self.domain)
            .field("token", &"[redacted]")
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl DomainClaim {
    /// A claim on `domain` with a fresh random token.
    ///
    /// # Errors
    ///
    /// [`MigrateError::ClaimFailed`] for a domain that is not a host name, or when no
    /// randomness is available.
    pub fn new(domain: &str, now: DateTime<Utc>) -> Result<Self> {
        let domain = normalise(domain);
        let valid = !domain.is_empty()
            && domain.contains('.')
            && domain.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if !valid {
            return Err(MigrateError::ClaimFailed {
                domain,
                reason: "not a host name such as example.com".into(),
            });
        }
        let mut bytes = [0u8; 32];
        aws_lc_rs::rand::fill(&mut bytes).map_err(|_| MigrateError::ClaimFailed {
            domain: domain.clone(),
            reason: "no randomness available".into(),
        })?;
        Ok(Self { domain, token: hex::encode(bytes), created_at: now })
    }

    /// The TXT record to publish: its name and value.
    pub fn dns_record(&self) -> (String, String) {
        (format!("{CLAIM_DNS_LABEL}.{}", self.domain), format!("awp-claim={}", self.token))
    }

    /// The well-known path to serve and the body to serve there.
    pub fn well_known(&self) -> (String, String) {
        (format!("{CLAIM_WELL_KNOWN_PREFIX}{}", self.token), self.token.clone())
    }

    fn proven(&self, method: ClaimMethod, now: DateTime<Utc>) -> ProvenDomain {
        tracing::info!(domain = %self.domain, method = ?method, "domain claim proven");
        ProvenDomain {
            domain: self.domain.clone(),
            method,
            proven_at: now,
            token_digest: hex::encode(Sha256::digest(self.token.as_bytes())),
        }
    }

    /// Proves the claim by its TXT record.
    ///
    /// # Errors
    ///
    /// [`MigrateError::ClaimFailed`] when no TXT record carries the token; DNS errors.
    pub async fn verify_dns(
        &self,
        resolver: &dyn DnsResolver,
        now: DateTime<Utc>,
    ) -> Result<ProvenDomain> {
        let (name, expected) = self.dns_record();
        let found = resolver.lookup(&name, RecordKind::Txt).await?;
        if found.iter().any(|value| value.trim() == expected) {
            Ok(self.proven(ClaimMethod::DnsTxt, now))
        } else {
            Err(MigrateError::ClaimFailed {
                domain: self.domain.clone(),
                reason: format!("no TXT record at {name} carries the claim token"),
            })
        }
    }

    /// Proves the claim by fetching the token from `https://<domain>` (redirects followed
    /// within the domain).
    ///
    /// # Errors
    ///
    /// [`MigrateError::ClaimFailed`] when the token is not served; HTTP errors.
    pub async fn verify_well_known(
        &self,
        http: &HttpClient,
        now: DateTime<Utc>,
    ) -> Result<ProvenDomain> {
        let (path, body) = self.well_known();
        let url = Url::parse(&format!("https://{}{path}", self.domain))
            .map_err(|e| MigrateError::parse("claim URL", e))?;
        let fetched = http.fetch(&url, &FetchOptions::crawler().following(3)).await?;
        let same_site = fetched.hops.iter().all(|(hop, _)| {
            hop.host_str().is_some_and(|host| {
                let host = normalise(host);
                host == self.domain || host.ends_with(&format!(".{}", self.domain))
            })
        });
        if fetched.status == 200 && same_site && fetched.text().trim() == body {
            Ok(self.proven(ClaimMethod::WellKnown, now))
        } else {
            Err(MigrateError::ClaimFailed {
                domain: self.domain.clone(),
                reason: format!("{url} did not serve the claim token (HTTP {})", fetched.status),
            })
        }
    }
}

/// Proof that the owner controls a domain, from a verified [`DomainClaim`] (A-I9).
///
/// It can only be made by verifying a claim, and it is not deserialisable: a process that
/// needs it again verifies the claim again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenDomain {
    domain: String,
    method: ClaimMethod,
    proven_at: DateTime<Utc>,
    token_digest: String,
}

impl ProvenDomain {
    /// The domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// How it was proven.
    pub fn method(&self) -> ClaimMethod {
        self.method
    }

    /// When it was proven.
    pub fn proven_at(&self) -> DateTime<Utc> {
        self.proven_at
    }

    /// Whether the proof covers `host`: the domain itself or a name under it.
    pub fn covers(&self, host: &str) -> bool {
        let host = normalise(host);
        host == self.domain || host.ends_with(&format!(".{}", self.domain))
    }
}

/// What the migration stages may do on a domain (A-I9).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ClaimScope {
    /// Polite public reads only: signed, robots-respecting, rate-limited `GET`s of published
    /// pages and documents.
    #[default]
    Polite,
    /// The owner proved control: cloaking checks, freezes and probes may run.
    Proven(ProvenDomain),
}

impl ClaimScope {
    /// The proof covering `host`.
    ///
    /// # Errors
    ///
    /// [`MigrateError::NotProven`] naming `action` when the scope is polite or the proof
    /// covers another domain.
    pub fn require(&self, host: &str, action: &str) -> Result<&ProvenDomain> {
        match self {
            Self::Proven(proven) if proven.covers(host) => Ok(proven),
            _ => Err(MigrateError::NotProven { domain: host.to_string(), action: action.into() }),
        }
    }
}
