//! The DNS and registrar suites (spec 3.5 §7.13, §11).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Months;

use crate::dns::{DnsDriver, DnsRecord, Money, RecordChange, RegistrarDriver, fqdn};
use crate::error::DriverError;

/// Makes fresh DNS adapters for [`dns_driver_contract`].
#[async_trait]
pub trait DnsHarness: Send + Sync {
    /// The zone the adapters serve, such as `example.test`.
    fn zone(&self) -> String;

    /// A fresh adapter whose zone holds `www.<zone> 300 IN A 192.0.2.1` among its records,
    /// and nothing else at `www`, `mail` or `mixed`.
    async fn driver(&self) -> Arc<dyn DnsDriver>;
}

fn record(name: &str, rtype: &str, value: &str) -> DnsRecord {
    DnsRecord::new(name, rtype, 300, value).normalized().expect("a valid record")
}

async fn at(driver: &dyn DnsDriver, zone: &str, name: &str, rtype: &str) -> Vec<String> {
    let records = driver.records(zone).await.expect("the zone reads");
    let name = fqdn(name).expect("a name");
    records.into_iter().filter(|r| r.name == name && r.rtype == rtype).map(|r| r.value).collect()
}

/// Runs every DNS case (spec 3.5 §7.13): reads are canonical and in the zone; adds,
/// removals and set replacements apply; a change set with one bad change applies nothing;
/// types a driver never changes are refused; names and values are normalised; an unknown
/// zone is an error.
///
/// # Panics
///
/// When the adapter breaks the contract, naming the case.
pub async fn dns_driver_contract(harness: &dyn DnsHarness) {
    let zone = fqdn(&harness.zone()).expect("a zone name");
    let www = format!("www.{zone}");

    // Reads are canonical, sorted and in the zone.
    let dns = harness.driver().await;
    let records = dns.records(&zone).await.expect("reads: the zone reads");
    assert!(
        records.contains(&record(&www, "A", "192.0.2.1")),
        "reads: www A is listed: {records:?}"
    );
    let mut sorted = records.clone();
    sorted.sort();
    assert_eq!(records, sorted, "reads: records are sorted");
    for read in &records {
        assert!(crate::dns::in_zone(&read.name, &zone), "reads: {} is in the zone", read.name);
        if crate::dns::CHANGEABLE_TYPES.contains(&read.rtype.as_str()) {
            assert_eq!(read.normalized().as_ref(), Ok(read), "reads: records are canonical");
        }
    }

    // Adds apply.
    let dns = harness.driver().await;
    let mail = format!("mail.{zone}");
    let changes = [
        RecordChange::Add { record: DnsRecord::new(mail.as_str(), "A", 300, "192.0.2.25") },
        RecordChange::Add {
            record: DnsRecord::new(zone.as_str(), "TXT", 300, "v=spf1 a:mail.example.test -all"),
        },
        RecordChange::Add {
            record: DnsRecord::new(zone.as_str(), "MX", 300, format!("10 {mail}")),
        },
    ];
    let receipt = dns.change(&zone, &changes).await.expect("adds: the change applies");
    assert_eq!(receipt.applied, 3, "adds: three changes applied");
    assert_eq!(at(dns.as_ref(), &zone, &mail, "A").await, ["192.0.2.25"], "adds: mail A");
    assert!(
        at(dns.as_ref(), &zone, &zone, "TXT")
            .await
            .contains(&"v=spf1 a:mail.example.test -all".to_string()),
        "adds: apex TXT"
    );
    assert_eq!(at(dns.as_ref(), &zone, &zone, "MX").await, [format!("10 {mail}")], "adds: apex MX");

    // A removal takes one record out of its set.
    let dns = harness.driver().await;
    dns.change(
        &zone,
        &[RecordChange::Add { record: DnsRecord::new(www.as_str(), "A", 300, "192.0.2.2") }],
    )
    .await
    .expect("removes: the add applies");
    dns.change(
        &zone,
        &[RecordChange::Remove { record: DnsRecord::new(www.as_str(), "A", 300, "192.0.2.1") }],
    )
    .await
    .expect("removes: the removal applies");
    assert_eq!(
        at(dns.as_ref(), &zone, &www, "A").await,
        ["192.0.2.2"],
        "removes: only the other record is left"
    );

    // A replacement swaps the whole set.
    let dns = harness.driver().await;
    let replace = RecordChange::Replace {
        name: www.clone(),
        rtype: "A".into(),
        ttl: 600,
        values: vec!["192.0.2.7".into(), "192.0.2.8".into()],
    };
    dns.change(&zone, &[replace]).await.expect("replaces: the replacement applies");
    let mut values = at(dns.as_ref(), &zone, &www, "A").await;
    values.sort();
    assert_eq!(values, ["192.0.2.7", "192.0.2.8"], "replaces: the set is the new values");

    // A change set with a bad change applies nothing.
    let dns = harness.driver().await;
    let before = dns.records(&zone).await.expect("atomic: reads");
    let outside = RecordChange::Add {
        record: DnsRecord::new("www.elsewhere.invalid", "A", 300, "192.0.2.9"),
    };
    let good = RecordChange::Add {
        record: DnsRecord::new(format!("mixed.{zone}"), "A", 300, "192.0.2.9"),
    };
    let error = dns
        .change(&zone, &[good, outside])
        .await
        .expect_err("atomic: a name outside the zone is refused");
    assert!(matches!(error, DriverError::Invalid { .. }), "atomic: refused as invalid: {error:?}");
    assert_eq!(dns.records(&zone).await.expect("atomic: reads"), before, "atomic: nothing applied");

    // Types a driver never changes are refused.
    let soa = RecordChange::Add {
        record: DnsRecord::new(zone.as_str(), "SOA", 300, "ns. host. 1 2 3 4 5"),
    };
    let error = dns.change(&zone, &[soa]).await.expect_err("types: SOA changes are refused");
    assert!(matches!(error, DriverError::Invalid { .. }), "types: refused as invalid: {error:?}");
    assert!(dns.change(&zone, &[]).await.is_err(), "types: an empty change set is refused");

    // Names and values are normalised.
    let dns = harness.driver().await;
    let mixed = format!("MIXED.{}", zone.to_ascii_uppercase());
    dns.change(
        &zone,
        &[RecordChange::Add {
            record: DnsRecord::new(mixed.as_str(), "cname", 300, "Target.Example.NET"),
        }],
    )
    .await
    .expect("normalises: the change applies");
    assert_eq!(
        at(dns.as_ref(), &zone, &format!("mixed.{zone}"), "CNAME").await,
        ["target.example.net."],
        "normalises: stored in canonical form"
    );

    // An unknown zone is an error.
    assert!(dns.records("no-such-zone.invalid").await.is_err(), "unknown zone: reading it fails");
}

/// Makes fresh registrar adapters for [`registrar_driver_contract`].
#[async_trait]
pub trait RegistrarHarness: Send + Sync {
    /// A domain the registrar knows, with an expiry.
    fn domain(&self) -> String;

    /// What renewing it costs a year.
    fn price_per_year(&self) -> Money;

    /// A fresh adapter.
    async fn driver(&self) -> Arc<dyn RegistrarDriver>;
}

/// Runs every registrar case (spec 3.5 §7.13): expiry is read; an unknown domain is not
/// found; a renewal within its price limit extends the expiry by its years, one above it is
/// refused and charges nothing, and years outside 1 to 10 are refused — or, for a reader
/// that does not renew, every renewal is unsupported.
///
/// # Panics
///
/// When the adapter breaks the contract, naming the case.
pub async fn registrar_driver_contract(harness: &dyn RegistrarHarness) {
    let domain = harness.domain();
    let registrar = harness.driver().await;
    let status = registrar.domain(&domain).await.expect("expiry: the domain reads");
    assert_eq!(
        status.domain,
        crate::dns::bare_domain(&domain).unwrap(),
        "expiry: the domain is named"
    );
    let expires = status.expires_at.expect("expiry: the domain has an expiry");
    let error = registrar
        .domain("no-such-domain-zz.invalid")
        .await
        .expect_err("unknown: an unknown domain fails");
    assert!(matches!(error, DriverError::NotFound { .. }), "unknown: not found: {error:?}");

    let price = harness.price_per_year();
    if !registrar.can_renew() {
        let error = registrar.renew(&domain, 1, &price).await.expect_err("renew: unsupported");
        assert!(matches!(error, DriverError::Unsupported { .. }), "renew: unsupported: {error:?}");
        return;
    }
    let below = Money::new(price.amount_minor.saturating_sub(1), price.currency.clone());
    let error =
        registrar.renew(&domain, 1, &below).await.expect_err("renew: above the limit is refused");
    assert!(matches!(error, DriverError::Refused { .. }), "renew: refused: {error:?}");
    assert_eq!(
        registrar.domain(&domain).await.unwrap().expires_at,
        Some(expires),
        "renew: nothing changed"
    );
    assert!(registrar.renew(&domain, 0, &price).await.is_err(), "renew: zero years refused");
    let limit = Money::new(price.amount_minor * 2, price.currency.clone());
    let renewal = registrar.renew(&domain, 2, &limit).await.expect("renew: within the limit");
    assert_eq!(renewal.years, 2, "renew: two years");
    assert!(renewal.price.amount_minor <= limit.amount_minor, "renew: within the limit");
    assert_eq!(
        renewal.expires_at,
        expires.checked_add_months(Months::new(24)).unwrap(),
        "renew: two years added"
    );
    assert_eq!(
        registrar.domain(&domain).await.unwrap().expires_at,
        Some(renewal.expires_at),
        "renew: the new expiry reads back"
    );
}
