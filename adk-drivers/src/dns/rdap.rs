//! Domain expiry over RDAP, RFC 9083 (spec 3.5 §7.13; feature `rdap`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{DomainStatus, Money, RegistrarDriver, Renewal, bare_domain};
use crate::error::{DriverError, Result};
use crate::http::{checked_base, http_client, read_json};

const SERVICE: &str = "rdap";
/// IANA's RDAP bootstrap registry for domains (RFC 9224).
pub const IANA_BOOTSTRAP: &str = "https://data.iana.org/rdap/dns.json";

/// A domain's registration read over RDAP (spec 3.5 §7.13): public, keyless, every gTLD and
/// many ccTLDs. RDAP reads; it does not renew, so [`can_renew`](RegistrarDriver::can_renew)
/// is false and renewals go through the owner's registrar's driver.
///
/// The RDAP server for a domain is found through IANA's bootstrap registry (RFC 9224),
/// or set with [`with_base`](Self::with_base).
///
/// # Example
///
/// ```rust,no_run
/// use adk_drivers::dns::{RdapRegistrar, RegistrarDriver};
///
/// # async fn example() -> adk_drivers::Result<()> {
/// let rdap = RdapRegistrar::new()?;
/// let status = rdap.domain("example.com").await?;
/// println!("expires {:?}", status.expires_at);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct RdapRegistrar {
    http: reqwest::Client,
    base: Option<String>,
    bootstrap: String,
    bases: Mutex<HashMap<String, String>>,
}

impl RdapRegistrar {
    /// A reader that finds each domain's RDAP server through IANA's bootstrap registry.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Unavailable`] when no HTTP client can be built.
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: http_client(SERVICE, Duration::from_secs(15))?,
            base: None,
            bootstrap: IANA_BOOTSTRAP.to_string(),
            bases: Mutex::default(),
        })
    }

    /// Reads every domain from the RDAP server at `base`, such as
    /// `https://rdap.verisign.com/com/v1/`. Plain `http` is accepted on loopback only.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an address that is not `https` (or `http` on
    /// loopback).
    pub fn with_base(mut self, base: &str) -> Result<Self> {
        self.base = Some(checked_base(base)?);
        Ok(self)
    }

    /// Reads the bootstrap registry from `url` instead of IANA's.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an address that is not `https` (or `http` on
    /// loopback).
    pub fn with_bootstrap(mut self, url: &str) -> Result<Self> {
        self.bootstrap = checked_base(url)?.trim_end_matches('/').to_string();
        Ok(self)
    }

    async fn base_for(&self, domain: &str) -> Result<String> {
        if let Some(base) = &self.base {
            return Ok(base.clone());
        }
        let tld = domain.rsplit('.').next().unwrap_or(domain).to_string();
        if let Some(base) =
            self.bases.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&tld)
        {
            return Ok(base.clone());
        }
        let response = self
            .http
            .get(&self.bootstrap)
            .send()
            .await
            .map_err(|error| DriverError::unavailable(SERVICE, error))?;
        let registry = read_json(SERVICE, response).await?;
        let base = bootstrap_base(&registry, &tld)
            .ok_or_else(|| DriverError::not_found(format!("an RDAP server for .{tld}")))?;
        self.bases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(tld, base.clone());
        Ok(base)
    }
}

/// The first RDAP base the bootstrap registry lists for `tld`, ending in `/`.
fn bootstrap_base(registry: &Value, tld: &str) -> Option<String> {
    registry.get("services")?.as_array()?.iter().find_map(|service| {
        let tlds = service.get(0)?.as_array()?;
        if !tlds
            .iter()
            .any(|entry| entry.as_str().is_some_and(|entry| entry.eq_ignore_ascii_case(tld)))
        {
            return None;
        }
        let url = service
            .get(1)?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .find(|url| url.starts_with("https://"))?;
        Some(if url.ends_with('/') { url.to_string() } else { format!("{url}/") })
    })
}

/// The registration an RDAP domain object describes (RFC 9083 §5.3).
pub(crate) fn status_of(domain: &str, object: &Value) -> DomainStatus {
    let mut status = DomainStatus::new(domain);
    status.expires_at = object.get("events").and_then(Value::as_array).and_then(|events| {
        events.iter().find_map(|event| {
            (event.get("eventAction")?.as_str()? == "expiration")
                .then(|| event.get("eventDate")?.as_str())?
                .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.with_timezone(&Utc))
        })
    });
    status.statuses = object
        .get("status")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    status.registrar = object.get("entities").and_then(Value::as_array).and_then(|entities| {
        entities.iter().find_map(|entity| {
            let roles = entity.get("roles")?.as_array()?;
            if !roles.iter().any(|role| role.as_str() == Some("registrar")) {
                return None;
            }
            let card = entity.get("vcardArray")?.get(1)?.as_array()?;
            card.iter().find_map(|property| {
                let property = property.as_array()?;
                (property.first()?.as_str()? == "fn")
                    .then(|| property.get(3)?.as_str().map(str::to_string))?
            })
        })
    });
    status
}

#[async_trait]
impl RegistrarDriver for RdapRegistrar {
    fn name(&self) -> &str {
        "rdap"
    }

    async fn domain(&self, domain: &str) -> Result<DomainStatus> {
        let domain = bare_domain(domain)?;
        let base = self.base_for(&domain).await?;
        let response = self
            .http
            .get(format!("{base}domain/{domain}"))
            .header("accept", "application/rdap+json")
            .send()
            .await
            .map_err(|error| DriverError::unavailable(SERVICE, error))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(DriverError::not_found(format!("domain `{domain}` in RDAP")));
        }
        let object = read_json(SERVICE, response).await?;
        Ok(status_of(&domain, &object))
    }

    fn can_renew(&self) -> bool {
        false
    }

    async fn renew(&self, _domain: &str, _years: u32, _max_price: &Money) -> Result<Renewal> {
        Err(DriverError::unsupported("rdap", "renewals; connect the registrar's own driver"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_bootstrap_registry_names_the_server_of_a_tld() {
        let registry = json!({"services": [
            [["net", "com"], ["http://rdap.example/", "https://rdap.verisign.com/com/v1/"]],
            [["org"], ["https://rdap.publicinterestregistry.org/rdap"]]
        ]});
        assert_eq!(bootstrap_base(&registry, "com").unwrap(), "https://rdap.verisign.com/com/v1/");
        assert_eq!(
            bootstrap_base(&registry, "org").unwrap(),
            "https://rdap.publicinterestregistry.org/rdap/"
        );
        assert!(bootstrap_base(&registry, "ke").is_none());
    }
}
