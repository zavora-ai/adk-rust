//! SPF, DKIM and DMARC checks before the first send (spec 3.5 §7.13).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// The most DNS lookups an SPF evaluation makes (RFC 7208 §4.6.4).
const SPF_LOOKUP_LIMIT: usize = 10;

/// Reads TXT records (spec 3.5 §7.13): the system's resolver in production, a fixed table
/// in tests.
#[async_trait]
pub trait TxtLookup: Send + Sync {
    /// The TXT records of `name`, each with its character-strings joined; empty when the
    /// name has none or does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Unavailable`](crate::DriverError::Unavailable) when the resolver fails.
    async fn txt(&self, name: &str) -> Result<Vec<String>>;
}

/// TXT records from a fixed table, for tests.
///
/// # Example
///
/// ```rust
/// use adk_drivers::email::{StaticTxt, TxtLookup};
///
/// # futures::executor::block_on(async {
/// let txt = StaticTxt::new().with("example.com", "v=spf1 include:_spf.mail.example ~all");
/// assert_eq!(txt.txt("Example.com.").await.unwrap().len(), 1);
/// assert!(txt.txt("other.example").await.unwrap().is_empty());
/// # });
/// ```
#[derive(Debug, Default)]
pub struct StaticTxt {
    records: Mutex<BTreeMap<String, Vec<String>>>,
}

impl StaticTxt {
    /// No records.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a TXT record `text` at `name`.
    #[must_use]
    pub fn with(self, name: &str, text: &str) -> Self {
        self.insert(name, text);
        self
    }

    /// Adds a TXT record `text` at `name`.
    pub fn insert(&self, name: &str, text: &str) {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(lookup_key(name))
            .or_default()
            .push(text.to_string());
    }

    /// Removes every record at `name`.
    pub fn clear(&self, name: &str) {
        self.records.lock().unwrap_or_else(PoisonError::into_inner).remove(&lookup_key(name));
    }
}

fn lookup_key(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[async_trait]
impl TxtLookup for StaticTxt {
    async fn txt(&self, name: &str) -> Result<Vec<String>> {
        Ok(self
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&lookup_key(name))
            .cloned()
            .unwrap_or_default())
    }
}

/// TXT records through the system's resolver (feature `system-dns`).
///
/// The system's resolver configuration is read at the first lookup, and again after a lookup
/// that could not read it, so a site configures its email whatever the system's DNS settings
/// are at that moment.
#[cfg(feature = "system-dns")]
#[derive(Debug, Clone, Default)]
pub struct SystemTxtLookup {
    resolver: Arc<tokio::sync::OnceCell<hickory_resolver::TokioResolver>>,
}

#[cfg(feature = "system-dns")]
impl SystemTxtLookup {
    /// A lookup through the resolvers the system is configured with.
    ///
    /// # Errors
    ///
    /// None: the configuration is read at the first lookup, which returns
    /// [`DriverError::Unavailable`](crate::DriverError::Unavailable) when it cannot be read.
    pub fn new() -> Result<Self> {
        Ok(Self::default())
    }
}

#[cfg(feature = "system-dns")]
#[async_trait]
impl TxtLookup for SystemTxtLookup {
    async fn txt(&self, name: &str) -> Result<Vec<String>> {
        let resolver = self
            .resolver
            .get_or_try_init(|| async {
                hickory_resolver::TokioResolver::builder_tokio()
                    .map_err(|error| crate::DriverError::unavailable("dns", error))?
                    .build()
                    .map_err(|error| crate::DriverError::unavailable("dns", error))
            })
            .await?;
        match resolver.txt_lookup(name).await {
            Ok(lookup) => Ok(lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    hickory_resolver::proto::rr::RData::TXT(txt) => Some(
                        txt.txt_data
                            .iter()
                            .map(|part| String::from_utf8_lossy(part))
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .collect()),
            Err(error) if error.is_no_records_found() || error.is_nx_domain() => Ok(Vec::new()),
            Err(error) => Err(crate::DriverError::unavailable("dns", error)),
        }
    }
}

/// What the sending domain must publish (spec 3.5 §7.13).
///
/// # Example
///
/// ```rust
/// use adk_drivers::email::SenderPolicy;
///
/// let policy = SenderPolicy::new("example.com", "mail", "include:_spf.mail.example");
/// assert_eq!(policy.dkim_name(), "mail._domainkey.example.com");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SenderPolicy {
    /// The domain mail is sent from.
    pub domain: String,
    /// The DKIM selector the sending service signs with.
    pub dkim_selector: String,
    /// The SPF mechanism that authorises the sending service, such as
    /// `include:_spf.mail.example` or `ip4:203.0.113.5`.
    pub spf_mechanism: String,
}

impl SenderPolicy {
    /// The policy for `domain`, signed under `dkim_selector`, sent by the service SPF
    /// `spf_mechanism` names.
    pub fn new(
        domain: impl Into<String>,
        dkim_selector: impl Into<String>,
        spf_mechanism: impl Into<String>,
    ) -> Self {
        Self {
            domain: lookup_key(&domain.into()),
            dkim_selector: dkim_selector.into().trim().to_string(),
            spf_mechanism: spf_mechanism.into().trim().to_ascii_lowercase(),
        }
    }

    /// The name of the DKIM key record.
    pub fn dkim_name(&self) -> String {
        format!("{}._domainkey.{}", self.dkim_selector, self.domain)
    }

    /// The name of the DMARC record.
    pub fn dmarc_name(&self) -> String {
        format!("_dmarc.{}", self.domain)
    }
}

/// One of the three checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AuthCheck {
    /// Whether it passed.
    pub pass: bool,
    /// What was found, or what to publish.
    pub detail: String,
}

impl AuthCheck {
    fn pass(detail: impl Into<String>) -> Self {
        Self { pass: true, detail: detail.into() }
    }

    fn fail(detail: impl Into<String>) -> Self {
        Self { pass: false, detail: detail.into() }
    }
}

/// What a domain publishes for mail authentication (spec 3.5 §7.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DomainAuthReport {
    /// The domain.
    pub domain: String,
    /// SPF: one `v=spf1` record that authorises the sending service and not everyone.
    pub spf: AuthCheck,
    /// DKIM: a public key at the selector.
    pub dkim: AuthCheck,
    /// DMARC: one `v=DMARC1` record with a policy.
    pub dmarc: AuthCheck,
    /// When it was checked.
    pub checked_at: DateTime<Utc>,
}

impl DomainAuthReport {
    /// Whether all three pass, so bulk sending may start.
    pub fn passes(&self) -> bool {
        self.spf.pass && self.dkim.pass && self.dmarc.pass
    }

    /// The checks that failed, in plain words.
    pub fn failures(&self) -> Vec<String> {
        [("SPF", &self.spf), ("DKIM", &self.dkim), ("DMARC", &self.dmarc)]
            .into_iter()
            .filter(|(_, check)| !check.pass)
            .map(|(name, check)| format!("{name}: {}", check.detail))
            .collect()
    }
}

/// Checks a sending domain's SPF, DKIM and DMARC records (spec 3.5 §7.13).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_drivers::email::{DomainAuthCheck, SenderPolicy, StaticTxt};
///
/// # futures::executor::block_on(async {
/// let txt = StaticTxt::new()
///     .with("example.com", "v=spf1 include:_spf.mail.example ~all")
///     .with("mail._domainkey.example.com", "v=DKIM1; k=rsa; p=MIIBIjANBgkq")
///     .with("_dmarc.example.com", "v=DMARC1; p=quarantine; rua=mailto:dmarc@example.com");
/// let check = DomainAuthCheck::new(Arc::new(txt));
/// let report = check.run(&SenderPolicy::new("example.com", "mail", "include:_spf.mail.example")).await.unwrap();
/// assert!(report.passes(), "{:?}", report.failures());
/// # });
/// ```
#[derive(Clone)]
pub struct DomainAuthCheck {
    lookup: Arc<dyn TxtLookup>,
}

impl std::fmt::Debug for DomainAuthCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomainAuthCheck").finish_non_exhaustive()
    }
}

impl DomainAuthCheck {
    /// A check reading TXT records through `lookup`.
    pub fn new(lookup: Arc<dyn TxtLookup>) -> Self {
        Self { lookup }
    }

    /// Checks `policy`'s domain.
    ///
    /// # Errors
    ///
    /// Returns the resolver's error; a missing or wrong record is a failed check, not an
    /// error.
    pub async fn run(&self, policy: &SenderPolicy) -> Result<DomainAuthReport> {
        Ok(DomainAuthReport {
            domain: policy.domain.clone(),
            spf: self.spf(policy).await?,
            dkim: self.dkim(policy).await?,
            dmarc: self.dmarc(policy).await?,
            checked_at: Utc::now(),
        })
    }

    async fn spf_records(&self, domain: &str) -> Result<Vec<String>> {
        Ok(self
            .lookup
            .txt(domain)
            .await?
            .into_iter()
            .filter(|text| {
                let lower = text.trim().to_ascii_lowercase();
                lower == "v=spf1" || lower.starts_with("v=spf1 ")
            })
            .collect())
    }

    async fn spf(&self, policy: &SenderPolicy) -> Result<AuthCheck> {
        let records = self.spf_records(&policy.domain).await?;
        let record = match records.as_slice() {
            [] => {
                return Ok(AuthCheck::fail(format!(
                    "publish a TXT record at {} reading `v=spf1 {} ~all`",
                    policy.domain, policy.spf_mechanism
                )));
            }
            [record] => record.clone(),
            _ => {
                return Ok(AuthCheck::fail(
                    "the domain publishes more than one SPF record; keep one",
                ));
            }
        };
        let mut lookups = 0;
        let mut pending = vec![record.clone()];
        let mut authorised = false;
        while let Some(record) = pending.pop() {
            let mut redirect = None;
            for term in record.split_whitespace().skip(1) {
                let term = term.to_ascii_lowercase();
                let (qualifier, mechanism) = match term.chars().next() {
                    Some(c @ ('+' | '-' | '~' | '?')) => (c, term[1..].to_string()),
                    _ => ('+', term.clone()),
                };
                if mechanism == "all" && qualifier == '+' {
                    return Ok(AuthCheck::fail(format!(
                        "`{term}` lets anyone send as {}; end the record with `~all` or `-all`",
                        policy.domain
                    )));
                }
                if mechanism == policy.spf_mechanism && qualifier == '+' {
                    authorised = true;
                }
                if let Some(domain) = mechanism.strip_prefix("include:") {
                    lookups += 1;
                    if lookups > SPF_LOOKUP_LIMIT {
                        return Ok(AuthCheck::fail(
                            "the SPF record needs more than 10 lookups (RFC 7208 §4.6.4)",
                        ));
                    }
                    if qualifier == '+' {
                        pending.extend(self.spf_records(domain).await?);
                    }
                } else if let Some(domain) = mechanism.strip_prefix("redirect=") {
                    redirect = Some(domain.to_string());
                }
            }
            if let Some(domain) = redirect {
                lookups += 1;
                if lookups > SPF_LOOKUP_LIMIT {
                    return Ok(AuthCheck::fail(
                        "the SPF record needs more than 10 lookups (RFC 7208 §4.6.4)",
                    ));
                }
                pending.extend(self.spf_records(&domain).await?);
            }
        }
        Ok(if authorised {
            AuthCheck::pass(format!("`{record}` authorises {}", policy.spf_mechanism))
        } else {
            AuthCheck::fail(format!(
                "`{record}` does not authorise {}; add it",
                policy.spf_mechanism
            ))
        })
    }

    async fn dkim(&self, policy: &SenderPolicy) -> Result<AuthCheck> {
        let name = policy.dkim_name();
        let records = self.lookup.txt(&name).await?;
        let key = records.iter().find(|text| {
            let tags = tags(text);
            tags.get("v").is_none_or(|version| version.eq_ignore_ascii_case("DKIM1"))
                && tags.get("p").is_some_and(|key| !key.is_empty())
        });
        Ok(match key {
            Some(_) => AuthCheck::pass(format!("a DKIM key is published at {name}")),
            None => AuthCheck::fail(format!(
                "publish the sending service's DKIM key as a TXT record at {name} (`v=DKIM1; k=rsa; p=...`)"
            )),
        })
    }

    async fn dmarc(&self, policy: &SenderPolicy) -> Result<AuthCheck> {
        let name = policy.dmarc_name();
        let records: Vec<String> = self
            .lookup
            .txt(&name)
            .await?
            .into_iter()
            .filter(|text| text.trim().to_ascii_lowercase().starts_with("v=dmarc1"))
            .collect();
        Ok(match records.as_slice() {
            [] => AuthCheck::fail(format!(
                "publish a TXT record at {name} such as `v=DMARC1; p=quarantine`"
            )),
            [record] => match tags(record).get("p").map(|p| p.to_ascii_lowercase()) {
                Some(p) if matches!(p.as_str(), "none" | "quarantine" | "reject") => {
                    AuthCheck::pass(format!("DMARC policy `{p}` at {name}"))
                }
                _ => {
                    AuthCheck::fail(format!("the DMARC record at {name} has no valid `p=` policy"))
                }
            },
            _ => AuthCheck::fail(format!("{name} has more than one DMARC record; keep one")),
        })
    }
}

/// The `tag=value` pairs of a DKIM or DMARC record.
fn tags(text: &str) -> BTreeMap<String, String> {
    text.split(';')
        .filter_map(|part| {
            let (tag, value) = part.split_once('=')?;
            Some((tag.trim().to_ascii_lowercase(), value.split_whitespace().collect::<String>()))
        })
        .collect()
}
