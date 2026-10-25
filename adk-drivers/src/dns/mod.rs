//! DNS and registrar drivers (spec 3.5 §7.13): records and their changes, domain expiry
//! and renewal.
//!
//! | Tool | Effect class | Side | Default cap |
//! |---|---|---|---|
//! | `dns_list_records` | `read` | back office | Read |
//! | `dns_change_records` | `publish_public` | back office | Always held (A-I21) |
//! | `registrar_domain_status` | `read` | back office | Read |
//! | `registrar_renew_domain` | `financial` | back office | Within budget |
//!
//! | Adapter | Contract | Feature |
//! |---|---|---|
//! | [`InMemoryDns`], [`InMemoryRegistrar`] | [`DnsDriver`], [`RegistrarDriver`] | — |
//! | `Rfc2136Dns`: RFC 2136 dynamic update with TSIG (RFC 8945), read by AXFR (RFC 5936) | [`DnsDriver`] | `rfc2136` |
//! | `RdapRegistrar`: expiry over RDAP (RFC 9083), renewals unsupported | [`RegistrarDriver`] | `rdap` |

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};

use adk_core::Tool;
use adk_kernel::drivers::EffectClass;
use async_trait::async_trait;
use chrono::{DateTime, Months, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::class::{AutonomyCap, Side, ToolSpec};
use crate::error::{DriverError, Result};
use crate::outbound::str_arg;
use crate::tools::{DriverTool, spec};

#[cfg(feature = "rdap")]
mod rdap;
#[cfg(feature = "rfc2136")]
mod rfc2136;
#[cfg(feature = "rfc2136")]
pub mod tsig;

#[cfg(feature = "rdap")]
pub use rdap::RdapRegistrar;
#[cfg(feature = "rfc2136")]
pub use rfc2136::Rfc2136Dns;

/// The record types a change may name; reads return every type the zone holds.
pub const CHANGEABLE_TYPES: [&str; 7] = ["A", "AAAA", "CNAME", "MX", "NS", "SRV", "TXT"];

/// One DNS record (spec 3.5 §7.13), in presentation form: a fully qualified lower-case name
/// with its trailing dot, the type in upper case, the TTL, and the value — an address, a
/// target name with its trailing dot, `<preference> <exchange>` for MX,
/// `<priority> <weight> <port> <target>` for SRV, and the text itself, unquoted, for TXT.
///
/// # Example
///
/// ```rust
/// use adk_drivers::dns::DnsRecord;
///
/// let record = DnsRecord::new("WWW.Example.com", "cname", 300, "Site.Example.NET").normalized().unwrap();
/// assert_eq!(record.name, "www.example.com.");
/// assert_eq!(record.value, "site.example.net.");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DnsRecord {
    /// The owner name.
    pub name: String,
    /// The type, such as `A`.
    #[serde(rename = "type")]
    pub rtype: String,
    /// The time to live, in seconds.
    pub ttl: u32,
    /// The value.
    pub value: String,
}

impl DnsRecord {
    /// A record as given; see [`normalized`](Self::normalized).
    pub fn new(
        name: impl Into<String>,
        rtype: impl Into<String>,
        ttl: u32,
        value: impl Into<String>,
    ) -> Self {
        Self { name: name.into(), rtype: rtype.into(), ttl, value: value.into() }
    }

    /// The record in canonical presentation form, so two spellings of one record compare
    /// equal.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an empty name or type, an address that does not
    /// parse for A or AAAA, or an MX or SRV value with the wrong fields.
    pub fn normalized(&self) -> Result<Self> {
        let rtype = self.rtype.trim().to_ascii_uppercase();
        if rtype.is_empty() {
            return Err(DriverError::invalid("a DNS record needs a type, such as A or TXT"));
        }
        let value = normalize_value(&rtype, self.value.trim())?;
        Ok(Self { name: fqdn(&self.name)?, rtype, ttl: self.ttl, value })
    }

    /// The record set it belongs to: its name and type.
    pub fn set_key(&self) -> (String, String) {
        (self.name.clone(), self.rtype.clone())
    }
}

/// `name` fully qualified, in lower case, with its trailing dot.
///
/// # Errors
///
/// Returns [`DriverError::Invalid`] for an empty name or one with whitespace.
pub fn fqdn(name: &str) -> Result<String> {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err(DriverError::invalid(format!("`{name}` is not a domain name")));
    }
    Ok(if name.ends_with('.') { name } else { format!("{name}.") })
}

fn normalize_value(rtype: &str, value: &str) -> Result<String> {
    let bad =
        |what: &str| DriverError::invalid(format!("`{value}` is not {what} for a {rtype} record"));
    Ok(match rtype {
        "A" => match value.parse::<IpAddr>() {
            Ok(IpAddr::V4(address)) => address.to_string(),
            _ => return Err(bad("an IPv4 address")),
        },
        "AAAA" => match value.parse::<IpAddr>() {
            Ok(IpAddr::V6(address)) => address.to_string(),
            _ => return Err(bad("an IPv6 address")),
        },
        "CNAME" | "NS" | "PTR" => fqdn(value)?,
        "MX" => {
            let mut parts = value.split_whitespace();
            let preference = parts.next().and_then(|p| p.parse::<u16>().ok());
            let (Some(preference), Some(exchange), None) = (preference, parts.next(), parts.next())
            else {
                return Err(bad("`<preference> <exchange>`"));
            };
            format!("{preference} {}", fqdn(exchange)?)
        }
        "SRV" => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            let numbers: Vec<Option<u16>> = parts.iter().take(3).map(|p| p.parse().ok()).collect();
            match (parts.len(), numbers.as_slice()) {
                (4, [Some(priority), Some(weight), Some(port)]) => {
                    format!("{priority} {weight} {port} {}", fqdn(parts[3])?)
                }
                _ => return Err(bad("`<priority> <weight> <port> <target>`")),
            }
        }
        _ => value.to_string(),
    })
}

/// One change to a zone (spec 3.5 §7.13), as RFC 2136 expresses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "op")]
#[non_exhaustive]
pub enum RecordChange {
    /// Adds one record to its set.
    Add {
        /// The record.
        record: DnsRecord,
    },
    /// Removes one record from its set.
    Remove {
        /// The record.
        record: DnsRecord,
    },
    /// Replaces the whole set of `name` and `rtype` with `values`.
    Replace {
        /// The owner name.
        name: String,
        /// The type.
        #[serde(rename = "type")]
        rtype: String,
        /// The TTL of the new records.
        ttl: u32,
        /// The new values; empty removes the set.
        values: Vec<String>,
    },
}

impl RecordChange {
    /// The change in canonical form, checked against `zone` (spec 3.5 §7.13): every name
    /// in the zone, a changeable type, values that parse.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] naming what is wrong.
    pub fn validated(&self, zone: &str) -> Result<Self> {
        let zone = fqdn(zone)?;
        let check = |name: &str, rtype: &str| -> Result<()> {
            if !in_zone(name, &zone) {
                return Err(DriverError::invalid(format!("`{name}` is not in zone `{zone}`")));
            }
            if !CHANGEABLE_TYPES.contains(&rtype) {
                return Err(DriverError::invalid(format!(
                    "{rtype} records are not changed by a driver; changeable types are {}",
                    CHANGEABLE_TYPES.join(", ")
                )));
            }
            Ok(())
        };
        Ok(match self {
            Self::Add { record } => {
                let record = record.normalized()?;
                check(&record.name, &record.rtype)?;
                Self::Add { record }
            }
            Self::Remove { record } => {
                let record = record.normalized()?;
                check(&record.name, &record.rtype)?;
                Self::Remove { record }
            }
            Self::Replace { name, rtype, ttl, values } => {
                let records = values
                    .iter()
                    .map(|value| {
                        DnsRecord::new(name.as_str(), rtype.as_str(), *ttl, value.as_str())
                            .normalized()
                    })
                    .collect::<Result<Vec<_>>>()?;
                let name = fqdn(name)?;
                let rtype = rtype.trim().to_ascii_uppercase();
                check(&name, &rtype)?;
                Self::Replace {
                    name,
                    rtype,
                    ttl: *ttl,
                    values: records.into_iter().map(|r| r.value).collect(),
                }
            }
        })
    }
}

/// Whether `name` is `zone` or below it; both fully qualified.
pub fn in_zone(name: &str, zone: &str) -> bool {
    name == zone || name.ends_with(&format!(".{zone}")) || zone == "."
}

/// Checks and canonicalises a change set for `zone`.
///
/// # Errors
///
/// Returns [`DriverError::Invalid`] for an empty set or a change [`RecordChange::validated`]
/// refuses.
pub fn validate_changes(zone: &str, changes: &[RecordChange]) -> Result<Vec<RecordChange>> {
    if changes.is_empty() {
        return Err(DriverError::invalid("a DNS change names at least one change"));
    }
    changes.iter().map(|change| change.validated(zone)).collect()
}

/// What a DNS change did (spec 3.5 §7.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChangeReceipt {
    /// The zone.
    pub zone: String,
    /// How many changes were applied.
    pub applied: usize,
    /// The zone's serial after the change, when the driver knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u32>,
}

/// A DNS host (spec 3.5 §7.13): reads a zone's records and changes them.
///
/// Every change is `PublishPublic` and held for the owner in every mode (A-I21); the
/// governed path holds it before the driver is called, so an adapter only applies what a
/// person approved. A change set applies whole or not at all.
#[async_trait]
pub trait DnsDriver: Send + Sync {
    /// The adapter's name, for errors and origins.
    fn name(&self) -> &str;

    /// Every record of `zone`, in canonical form, sorted.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::NotFound`] for a zone the host does not serve, and the
    /// host's errors.
    async fn records(&self, zone: &str) -> Result<Vec<DnsRecord>>;

    /// Applies `changes` to `zone`, all or none.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a change [`validate_changes`] refuses, and the
    /// host's errors; nothing is applied then.
    async fn change(&self, zone: &str, changes: &[RecordChange]) -> Result<ChangeReceipt>;
}

/// An amount of money in minor units (spec 3.5 §7.13, §7.14).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Money {
    /// The amount, in minor units such as cents.
    pub amount_minor: u64,
    /// The currency, as ISO 4217.
    pub currency: String,
}

impl Money {
    /// `amount_minor` of `currency`.
    pub fn new(amount_minor: u64, currency: impl Into<String>) -> Self {
        Self { amount_minor, currency: currency.into().to_ascii_uppercase() }
    }
}

/// A domain's registration (spec 3.5 §7.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DomainStatus {
    /// The domain, fully qualified without its trailing dot.
    pub domain: String,
    /// When the registration expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Whether the registrar renews it automatically, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_renew: Option<bool>,
    /// The registry's status values, such as `client transfer prohibited`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<String>,
    /// The sponsoring registrar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registrar: Option<String>,
}

impl DomainStatus {
    /// A status of `domain` with nothing known.
    pub fn new(domain: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            expires_at: None,
            auto_renew: None,
            statuses: Vec::new(),
            registrar: None,
        }
    }

    /// Days until expiry from `now`, when the expiry is known.
    pub fn days_left(&self, now: DateTime<Utc>) -> Option<i64> {
        self.expires_at.map(|at| (at - now).num_days())
    }
}

/// A renewal that was charged (spec 3.5 §7.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Renewal {
    /// The domain.
    pub domain: String,
    /// The years added.
    pub years: u32,
    /// What it cost.
    pub price: Money,
    /// The new expiry.
    pub expires_at: DateTime<Utc>,
}

/// A domain registrar (spec 3.5 §7.13): reads expiry and renews.
///
/// A renewal is `Financial`: the governed path holds it beyond the owner's budget, and the
/// driver refuses it when the price is above `max_price`, so the site never pays more than
/// the call said.
#[async_trait]
pub trait RegistrarDriver: Send + Sync {
    /// The adapter's name.
    fn name(&self) -> &str;

    /// The registration of `domain`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::NotFound`] for a domain the registrar does not know.
    async fn domain(&self, domain: &str) -> Result<DomainStatus>;

    /// Whether [`renew`](Self::renew) is offered.
    fn can_renew(&self) -> bool;

    /// Renews `domain` for `years` (1 to 10) at no more than `max_price`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Unsupported`] when [`can_renew`](Self::can_renew) is false,
    /// [`DriverError::Refused`] when the price is above `max_price` or in another currency
    /// (nothing is charged), and [`DriverError::Invalid`] for years outside 1 to 10.
    async fn renew(&self, domain: &str, years: u32, max_price: &Money) -> Result<Renewal>;
}

/// `domain` without its trailing dot, in lower case.
pub(crate) fn bare_domain(domain: &str) -> Result<String> {
    Ok(fqdn(domain)?.trim_end_matches('.').to_string())
}

/// A DNS host in process memory (spec 3.5 §7.13): zones and their records, for tests and
/// as the hermetic reference of the [`DnsDriver`] conformance suite.
///
/// # Example
///
/// ```rust
/// use adk_drivers::dns::{DnsDriver, DnsRecord, InMemoryDns, RecordChange};
///
/// # futures::executor::block_on(async {
/// let dns = InMemoryDns::new().with_zone("example.com", []);
/// let www = DnsRecord::new("www.example.com", "A", 300, "192.0.2.10");
/// dns.change("example.com", &[RecordChange::Add { record: www.clone() }]).await.unwrap();
/// assert_eq!(dns.records("example.com").await.unwrap(), [www.normalized().unwrap()]);
/// # });
/// ```
#[derive(Debug, Default)]
pub struct InMemoryDns {
    zones: Mutex<BTreeMap<String, (u32, Vec<DnsRecord>)>>,
}

impl InMemoryDns {
    /// No zones.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `zone` with `records`.
    ///
    /// # Panics
    ///
    /// When a record does not normalise; the zone is the test's own.
    #[must_use]
    pub fn with_zone(self, zone: &str, records: impl IntoIterator<Item = DnsRecord>) -> Self {
        let zone = fqdn(zone).expect("a zone name");
        let mut records: Vec<DnsRecord> = records
            .into_iter()
            .map(|record| record.normalized().expect("a valid record"))
            .collect();
        records.sort();
        self.zones.lock().unwrap_or_else(PoisonError::into_inner).insert(zone, (1, records));
        self
    }
}

/// Applies validated `changes` to `records`.
pub(crate) fn apply_changes(records: &mut Vec<DnsRecord>, changes: &[RecordChange]) {
    for change in changes {
        match change {
            RecordChange::Add { record } => {
                let same = |known: &DnsRecord| {
                    known.set_key() == record.set_key() && known.value == record.value
                };
                if !records.iter().any(same) {
                    records.push(record.clone());
                }
            }
            RecordChange::Remove { record } => {
                records.retain(|known| {
                    !(known.set_key() == record.set_key() && known.value == record.value)
                });
            }
            RecordChange::Replace { name, rtype, ttl, values } => {
                records.retain(|known| !(known.name == *name && known.rtype == *rtype));
                for value in values {
                    records.push(DnsRecord::new(
                        name.as_str(),
                        rtype.as_str(),
                        *ttl,
                        value.as_str(),
                    ));
                }
            }
        }
    }
    records.sort();
    records.dedup();
}

#[async_trait]
impl DnsDriver for InMemoryDns {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn records(&self, zone: &str) -> Result<Vec<DnsRecord>> {
        let zone = fqdn(zone)?;
        let zones = self.zones.lock().unwrap_or_else(PoisonError::into_inner);
        zones
            .get(&zone)
            .map(|(_, records)| records.clone())
            .ok_or_else(|| DriverError::not_found(format!("zone `{zone}`")))
    }

    async fn change(&self, zone: &str, changes: &[RecordChange]) -> Result<ChangeReceipt> {
        let changes = validate_changes(zone, changes)?;
        let zone = fqdn(zone)?;
        let mut zones = self.zones.lock().unwrap_or_else(PoisonError::into_inner);
        let (serial, records) =
            zones.get_mut(&zone).ok_or_else(|| DriverError::not_found(format!("zone `{zone}`")))?;
        apply_changes(records, &changes);
        *serial += 1;
        Ok(ChangeReceipt { zone, applied: changes.len(), serial: Some(*serial) })
    }
}

/// A registrar in process memory (spec 3.5 §7.13), for tests and as the hermetic reference
/// of the [`RegistrarDriver`] conformance suite.
///
/// # Example
///
/// ```rust
/// use adk_drivers::dns::{InMemoryRegistrar, Money, RegistrarDriver};
///
/// # futures::executor::block_on(async {
/// let expires = chrono::Utc::now() + chrono::Duration::days(20);
/// let registrar = InMemoryRegistrar::new().with_domain("example.com", expires, Money::new(1_200, "USD"));
/// let renewal = registrar.renew("example.com", 1, &Money::new(1_500, "USD")).await.unwrap();
/// assert!(renewal.expires_at > expires);
/// assert!(registrar.renew("example.com", 1, &Money::new(1_000, "USD")).await.is_err());
/// # });
/// ```
#[derive(Debug, Default)]
pub struct InMemoryRegistrar {
    domains: Mutex<BTreeMap<String, (DomainStatus, Money)>>,
}

impl InMemoryRegistrar {
    /// No domains.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `domain` until `expires_at`, renewing at `price_per_year`.
    ///
    /// # Panics
    ///
    /// When `domain` is not a domain name.
    #[must_use]
    pub fn with_domain(
        self,
        domain: &str,
        expires_at: DateTime<Utc>,
        price_per_year: Money,
    ) -> Self {
        let domain = bare_domain(domain).expect("a domain name");
        let mut status = DomainStatus::new(domain.clone());
        status.expires_at = Some(expires_at);
        status.auto_renew = Some(false);
        status.registrar = Some("in-memory".into());
        self.domains
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(domain, (status, price_per_year));
        self
    }
}

#[async_trait]
impl RegistrarDriver for InMemoryRegistrar {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn domain(&self, domain: &str) -> Result<DomainStatus> {
        let domain = bare_domain(domain)?;
        let domains = self.domains.lock().unwrap_or_else(PoisonError::into_inner);
        domains
            .get(&domain)
            .map(|(status, _)| status.clone())
            .ok_or_else(|| DriverError::not_found(format!("domain `{domain}`")))
    }

    fn can_renew(&self) -> bool {
        true
    }

    async fn renew(&self, domain: &str, years: u32, max_price: &Money) -> Result<Renewal> {
        check_years(years)?;
        let domain = bare_domain(domain)?;
        let mut domains = self.domains.lock().unwrap_or_else(PoisonError::into_inner);
        let (status, per_year) = domains
            .get_mut(&domain)
            .ok_or_else(|| DriverError::not_found(format!("domain `{domain}`")))?;
        let price = Money::new(per_year.amount_minor * u64::from(years), per_year.currency.clone());
        check_price(&price, max_price)?;
        let from = status.expires_at.unwrap_or_else(Utc::now);
        let expires_at = from
            .checked_add_months(Months::new(12 * years))
            .ok_or_else(|| DriverError::invalid("the new expiry is out of range"))?;
        status.expires_at = Some(expires_at);
        Ok(Renewal { domain, years, price, expires_at })
    }
}

/// Refuses years outside 1 to 10.
pub(crate) fn check_years(years: u32) -> Result<()> {
    if (1..=10).contains(&years) {
        Ok(())
    } else {
        Err(DriverError::invalid("a renewal is for 1 to 10 years"))
    }
}

/// Refuses a `price` above `max` or in another currency.
pub(crate) fn check_price(price: &Money, max: &Money) -> Result<()> {
    if !price.currency.eq_ignore_ascii_case(&max.currency) || price.amount_minor > max.amount_minor
    {
        return Err(DriverError::refused(format!(
            "the renewal costs {} {} (minor units), above the call's limit of {} {}; nothing was charged",
            price.amount_minor, price.currency, max.amount_minor, max.currency
        )));
    }
    Ok(())
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn list_records_schema() -> Value {
    object_schema(
        json!({ "zone": {"type": "string", "description": "The zone, such as example.com"} }),
        &["zone"],
    )
}

fn change_records_schema() -> Value {
    object_schema(
        json!({
            "zone": {"type": "string", "description": "The zone, such as example.com"},
            "changes": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "op": {"type": "string", "enum": ["add", "remove", "replace"]},
                        "record": {"type": "object", "description": "For add and remove: name, type, ttl, value"},
                        "name": {"type": "string"},
                        "type": {"type": "string"},
                        "ttl": {"type": "integer", "minimum": 0},
                        "values": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["op"]
                }
            }
        }),
        &["zone", "changes"],
    )
}

fn domain_status_schema() -> Value {
    object_schema(json!({ "domain": {"type": "string"} }), &["domain"])
}

fn renew_schema() -> Value {
    object_schema(
        json!({
            "domain": {"type": "string"},
            "years": {"type": "integer", "minimum": 1, "maximum": 10},
            "max_price_minor": {"type": "integer", "minimum": 0, "description": "The most the renewal may cost, in minor units"},
            "currency": {"type": "string", "description": "ISO 4217"}
        }),
        &["domain", "years", "max_price_minor", "currency"],
    )
}

/// The DNS class's tools (spec 3.5 §7.13).
pub const DNS_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "dns_list_records",
        description: "List every record of a DNS zone.",
        class: EffectClass::Read,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::Read,
        budget_arg: None,
        schema: list_records_schema,
    },
    ToolSpec {
        name: "dns_change_records",
        description: "Change records of a DNS zone: add, remove or replace. Always held for the owner.",
        class: EffectClass::PublishPublic,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::AlwaysHeld,
        budget_arg: None,
        schema: change_records_schema,
    },
];

/// The registrar class's tools (spec 3.5 §7.13).
pub const REGISTRAR_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "registrar_domain_status",
        description: "Read a domain's registration: expiry, auto-renewal, status and registrar.",
        class: EffectClass::Read,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::Read,
        budget_arg: None,
        schema: domain_status_schema,
    },
    ToolSpec {
        name: "registrar_renew_domain",
        description: "Renew a domain for some years at no more than a given price.",
        class: EffectClass::Financial,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::WithinBudget,
        budget_arg: Some("/max_price_minor"),
        schema: renew_schema,
    },
];

/// The changes of a `dns_change_records` call.
fn changes_arg(args: &Value) -> Result<Vec<RecordChange>> {
    let changes = args
        .get("changes")
        .and_then(Value::as_array)
        .ok_or_else(|| DriverError::invalid("`changes` is a list of changes"))?;
    changes
        .iter()
        .map(|change| {
            serde_json::from_value::<RecordChange>(change.clone()).map_err(|error| {
                DriverError::invalid(format!(
                    "a change is {{\"op\": \"add\"|\"remove\", \"record\": {{name, type, ttl, value}}}} or \
                     {{\"op\": \"replace\", name, type, ttl, values}}: {error}"
                ))
            })
        })
        .collect()
}

/// The DNS tools over `driver` (spec 3.5 §7.13).
pub fn dns_tools(driver: Arc<dyn DnsDriver>) -> Vec<Arc<dyn Tool>> {
    let name = format!("dns/{}", driver.name());
    let list = Arc::clone(&driver);
    let change = driver;
    vec![
        DriverTool::shared(spec(DNS_TOOLS, "dns_list_records"), &name, move |args| {
            let driver = Arc::clone(&list);
            Box::pin(async move {
                let zone = str_arg(&args, "zone")?;
                Ok(json!({ "zone": fqdn(&zone)?, "records": driver.records(&zone).await? }))
            })
        }),
        DriverTool::shared(spec(DNS_TOOLS, "dns_change_records"), &name, move |args| {
            let driver = Arc::clone(&change);
            Box::pin(async move {
                let zone = str_arg(&args, "zone")?;
                let changes = changes_arg(&args)?;
                Ok(serde_json::to_value(driver.change(&zone, &changes).await?).unwrap_or_default())
            })
        }),
    ]
}

/// The registrar tools over `driver` (spec 3.5 §7.13).
pub fn registrar_tools(driver: Arc<dyn RegistrarDriver>) -> Vec<Arc<dyn Tool>> {
    let name = format!("registrar/{}", driver.name());
    let status = Arc::clone(&driver);
    let renew = driver;
    vec![
        DriverTool::shared(spec(REGISTRAR_TOOLS, "registrar_domain_status"), &name, move |args| {
            let driver = Arc::clone(&status);
            Box::pin(async move {
                let domain = str_arg(&args, "domain")?;
                Ok(serde_json::to_value(driver.domain(&domain).await?).unwrap_or_default())
            })
        }),
        DriverTool::shared(spec(REGISTRAR_TOOLS, "registrar_renew_domain"), &name, move |args| {
            let driver = Arc::clone(&renew);
            Box::pin(async move {
                let domain = str_arg(&args, "domain")?;
                let years = args
                    .get("years")
                    .and_then(Value::as_u64)
                    .and_then(|years| u32::try_from(years).ok())
                    .ok_or_else(|| {
                        DriverError::invalid("`years` is a whole number from 1 to 10")
                    })?;
                let max = args
                    .get("max_price_minor")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| DriverError::invalid("`max_price_minor` is a whole number"))?;
                let currency = str_arg(&args, "currency")?;
                let renewal = driver.renew(&domain, years, &Money::new(max, currency)).await?;
                Ok(serde_json::to_value(renewal).unwrap_or_default())
            })
        }),
    ]
}
