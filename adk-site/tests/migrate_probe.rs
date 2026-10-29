//! Probes, the detection half of S5 (spec 3.5 §7.8 "Probes", §7.8.3, A-I4, A-I9): a healthy
//! site shows no drift; a replaced file on the host shows drift at the host and the origin; a
//! page one release ahead of the checkpoint read is accepted; a planted legacy path is drift;
//! certificate expiry, DNS records and security headers are reported.
#![cfg(feature = "migrate")]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_site::host::conformance::Fixture;
use adk_site::host::{DirectoryHost, HostTarget, PublishReport, SiteHost};
use adk_site::log::CHECKPOINT_PATH;
use adk_site::migrate::MigrateError;
use adk_site::migrate::claim::{RecordKind, StaticDnsResolver};
use adk_site::migrate::http::{EgressScope, HttpClient};
use adk_site::migrate::probe::{
    CertificateProbe, DnsExpectation, DriftKind, DriftLocation, ProbeConfig, Prober,
};
use adk_site::migrate::serve::StaticServer;
use adk_site::redirects::RedirectsFormat;
use adk_site::release::{StagedDelta, StagedRelease};
use async_trait::async_trait;
use awp_types::{HostLimits, ReleaseManifest};
use bytes::Bytes;
use chrono::{TimeZone, Utc};
use url::Url;

const SITE: &str = "conformance.example";

fn files(about: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("/index.html", b"<!doctype html><title>Home</title><p>home</p>".to_vec()),
        (
            "/about/index.html",
            format!("<!doctype html><title>About</title><p>{about}</p>").into_bytes(),
        ),
        ("/style.css", b"body{margin:0}".to_vec()),
    ]
}

/// A host that answers the first checkpoint read with an older checkpoint: a prober that read
/// the checkpoint just before the next release went live.
struct Lagging {
    inner: DirectoryHost,
    stale: Mutex<Option<Bytes>>,
}

#[async_trait]
impl SiteHost for Lagging {
    async fn publish(
        &self,
        release: &StagedRelease,
        target: HostTarget,
    ) -> adk_site::Result<PublishReport> {
        self.inner.publish(release, target).await
    }
    async fn rollback(&self, to: &ReleaseManifest) -> adk_site::Result<PublishReport> {
        self.inner.rollback(to).await
    }
    async fn fetch(&self, path: &str) -> adk_site::Result<Option<Bytes>> {
        if path == CHECKPOINT_PATH
            && let Some(stale) = self.stale.lock().unwrap().take()
        {
            return Ok(Some(stale));
        }
        self.inner.fetch(path).await
    }
    fn redirects_format(&self) -> RedirectsFormat {
        self.inner.redirects_format()
    }
    fn limits(&self) -> HostLimits {
        self.inner.limits()
    }
    async fn publish_delta(
        &self,
        delta: &StagedDelta,
        target: HostTarget,
    ) -> adk_site::Result<PublishReport> {
        self.inner.publish_delta(delta, target).await
    }
}

struct Site {
    root: tempfile::TempDir,
    fixture: Fixture,
    origin: StaticServer,
    host: Arc<Lagging>,
}

async fn site() -> Site {
    let root = tempfile::tempdir().unwrap();
    let host =
        Arc::new(Lagging { inner: DirectoryHost::new(root.path()), stale: Mutex::new(None) });
    let mut fixture = Fixture::new().await;
    let release = fixture.release(&files("about v1"), Vec::new(), false).await;
    host.publish(&release, HostTarget::Production).await.unwrap();
    // A static file server follows the `current` link, which DirectoryHost makes on Unix;
    // elsewhere it serves the directory the pointer names.
    let served = if cfg!(unix) {
        root.path().join("current")
    } else {
        host.inner.serving_dir(HostTarget::Production).unwrap().unwrap()
    };
    let origin = StaticServer::serve(&served).await.unwrap();
    Site { root, fixture, origin, host }
}

fn prober(
    site: &Site,
    proven: &adk_site::migrate::ProvenDomain,
    dns: Arc<StaticDnsResolver>,
) -> Result<Prober, MigrateError> {
    let public = Url::parse(&format!("https://{SITE}")).unwrap();
    let http = HttpClient::builder()
        .egress(EgressScope::Only(BTreeSet::from([site.origin.origin()])))
        .map_origin(&public, Url::parse(&site.origin.origin()).unwrap())
        .min_interval(Duration::ZERO)
        .build()
        .unwrap();
    let mut config = ProbeConfig::new(SITE, public);
    config.sample = 100;
    config.seed = Some(7);
    config.dns = vec![
        DnsExpectation {
            name: SITE.into(),
            kind: RecordKind::A,
            values: BTreeSet::from(["203.0.113.7".to_string()]),
        },
        DnsExpectation {
            name: SITE.into(),
            kind: RecordKind::Mx,
            values: BTreeSet::from(["10 mail.conformance.example".to_string()]),
        },
    ];
    Prober::new(
        site.host.clone() as Arc<dyn SiteHost>,
        Arc::new(http),
        dns,
        vec![site.fixture.verifier().clone()],
        proven,
        config,
    )
}

async fn dns() -> Arc<StaticDnsResolver> {
    let dns = Arc::new(StaticDnsResolver::new());
    dns.set(SITE, RecordKind::A, ["203.0.113.7"]).await;
    dns.set(SITE, RecordKind::Mx, ["10 mail.elsewhere.example"]).await;
    dns
}

#[tokio::test]
async fn probes_find_drift_and_tolerate_one_release_of_lag() {
    let mut site = site().await;
    let proven = support::proven(SITE).await;
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let prober = prober(&site, &proven, dns().await)
        .unwrap()
        .with_audit(recorder.clone(), support::acting());

    let healthy = prober.probe().await.unwrap();
    assert!(!healthy.drift_detected(), "{:#?}", healthy.drift);
    assert_eq!(healthy.checkpoint, Some(1));
    assert_eq!(healthy.window, [1]);
    assert_eq!(healthy.files_checked, 6, "three files on the host and at the origin");
    assert_eq!(healthy.status[0].status, Some(200));
    assert!(healthy.missing_headers.contains(&"content-security-policy".to_string()));
    let records: BTreeMap<RecordKind, bool> =
        healthy.dns.iter().map(|d| (d.kind, d.matches)).collect();
    assert_eq!(records, BTreeMap::from([(RecordKind::A, true), (RecordKind::Mx, false)]));
    assert!(recorder.records().is_empty(), "no drift, no record");

    // A file on the host is replaced.
    let served = site.host.inner.serving_dir(HostTarget::Production).unwrap().unwrap();
    // Replaced as an intruder replaces a file: a new file renamed over the served one.
    std::fs::write(served.join("about/index.tmp"), "<p>defaced</p>").unwrap();
    std::fs::rename(served.join("about/index.tmp"), served.join("about/index.html")).unwrap();
    let tampered = prober.probe().await.unwrap();
    let drift: BTreeSet<(DriftLocation, DriftKind, String)> =
        tampered.drift.iter().map(|d| (d.location, d.kind, d.path.clone())).collect();
    assert_eq!(
        drift,
        BTreeSet::from([
            (DriftLocation::Host, DriftKind::Changed, "/about/index.html".to_string()),
            (DriftLocation::Origin, DriftKind::Changed, "/about/index.html".to_string()),
        ])
    );
    assert_eq!(tampered.drift[0].expected.len(), 1);
    assert!(recorder.records().iter().any(|r| r.action == "site.drift.detected"));

    // A planted legacy runtime path is drift too.
    std::fs::write(served.join("about/index.html"), files("about v1")[1].1.clone()).unwrap();
    std::fs::write(served.join("wp-login.php"), "<?php // inert test marker ?>").unwrap();
    let planted = prober.probe().await.unwrap();
    let unexpected: Vec<(DriftLocation, &str)> = planted
        .drift
        .iter()
        .filter(|d| d.kind == DriftKind::Unexpected)
        .map(|d| (d.location, d.path.as_str()))
        .collect();
    assert_eq!(
        unexpected,
        [(DriftLocation::Host, "/wp-login.php"), (DriftLocation::Origin, "/wp-login.php")]
    );
    std::fs::remove_file(served.join("wp-login.php")).unwrap();

    // Release 2 goes live just after the prober read checkpoint 1: accepted, no alarm.
    let stale = site.host.fetch(CHECKPOINT_PATH).await.unwrap().unwrap();
    let second = site.fixture.release(&files("about v2"), Vec::new(), false).await;
    site.host.publish(&second, HostTarget::Production).await.unwrap();
    *site.host.stale.lock().unwrap() = Some(stale);
    let lagging = prober.probe().await.unwrap();
    assert_eq!(lagging.checkpoint, Some(1));
    assert_eq!(lagging.window, [1, 2]);
    assert!(
        !lagging.drift_detected(),
        "a page of the next release is not drift: {:#?}",
        lagging.drift
    );
    drop(site.root);
}

#[tokio::test]
async fn probing_needs_a_proven_domain() {
    let site = site().await;
    let other = support::proven("elsewhere.example").await;
    let refused = prober(&site, &other, dns().await).unwrap_err();
    assert!(matches!(refused, MigrateError::NotProven { .. }), "{refused}");
}

#[test]
fn certificate_expiry_is_read() {
    let der = std::fs::read(support::fixtures().join("probe-cert.der")).unwrap();
    let issued = Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap();
    let fresh = CertificateProbe::from_der(&der, issued, 14).unwrap();
    assert!((29..=30).contains(&fresh.days_left), "{fresh:?}");
    assert!(!fresh.expiring);
    let later = CertificateProbe::from_der(&der, issued + chrono::Duration::days(20), 14).unwrap();
    assert!(later.expiring, "{later:?}");
    assert!(CertificateProbe::from_der(b"not a certificate", issued, 14).is_err());
}
