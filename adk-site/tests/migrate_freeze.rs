//! S4, "freeze in hours" (spec 3.5 §5, §7.8 "Freeze", A-I1, A-I9): the legacy site is crawled
//! from a private vantage point and published as a signed release; while it is served, no
//! request reaches the old server; forms post to edge capabilities.
#![cfg(feature = "migrate")]

mod support;

use std::sync::Arc;

use adk_core::audit_log::{InMemoryAuditLogStore, InMemoryAuditRecorder};
use adk_site::host::{DirectoryHost, HostTarget};
use adk_site::log::SiteLog;
use adk_site::migrate::freeze::{Freeze, FreezeConfig};
use adk_site::migrate::serve::StaticServer;
use adk_site::migrate::{FeatureKind, MigrateError};
use adk_site::signer::Ed25519ReleaseSigner;
use awp_types::ReleaseSource;
use support::*;
use url::Url;

fn config(server: &FixtureServer) -> FreezeConfig {
    FreezeConfig {
        public_origin: Url::parse(ORIGIN).unwrap(),
        vantage: server.url(),
        edge_origin: Url::parse("https://agent.solar.test").unwrap(),
    }
}

#[tokio::test]
async fn s4_a_frozen_site_serves_without_its_upstream() {
    let upstream = FixtureServer::start(false).await;
    let host_dir = tempfile::tempdir().unwrap();
    let host = DirectoryHost::new(host_dir.path());
    let signer = Arc::new(Ed25519ReleaseSigner::generate(DOMAIN).unwrap());
    let log = SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), DOMAIN, signer).await.unwrap();
    let freeze = Freeze::new(config(&upstream))
        .with_min_interval(std::time::Duration::ZERO)
        .with_signer(signer_for_tests());
    let out = tempfile::tempdir().unwrap();

    let refused = freeze.run(&context(&upstream), out.path(), None, &log, &host).await.unwrap_err();
    assert!(matches!(refused, MigrateError::NotProven { .. }), "{refused}");
    assert_eq!(upstream.requests(), 0, "nothing is read before the domain is proven");

    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let ctx = proven_context(&upstream).await.with_audit(recorder.clone());
    let report = freeze.run(&ctx, out.path(), None, &log, &host).await.unwrap();
    assert!(
        report.connections.iter().all(|c| c.origin == upstream.origin && c.allowed),
        "only the vantage point is reached"
    );
    assert!(upstream.seen().iter().all(|s| s.signed), "the crawl is signed");
    assert_eq!(report.release.manifest.seq, 1);
    assert_eq!(report.release.manifest.source, ReleaseSource::Migration);
    for path in [
        "/index.html",
        "/about/index.html",
        "/wp-content/themes/solar/style.css",
        "/wp-content/uploads/2026/10/team.png",
        "/wp-content/themes/solar/logo.png",
    ] {
        assert!(report.release.files.contains_key(path), "{path} in the snapshot");
    }
    assert!(report.release.files.keys().all(|p| !p.ends_with(".php")), "no server code (A-I1)");
    assert!(
        report.excluded.iter().any(|(path, why)| path == "/wp-login.php" && why.contains("A-I1")),
        "{:?}",
        report.excluded
    );
    assert!(
        report.redirects.iter().any(|r| r.from == "/old-offers/" && r.to == "/products/panel-400/")
    );
    let contact = report
        .forms
        .iter()
        .find(|f| f.page == "/contact/" && f.feature == Some(FeatureKind::ContactForm))
        .unwrap();
    assert_eq!(contact.capability.as_deref(), Some("submit_inquiry"));
    let comments = report.forms.iter().find(|f| f.feature == Some(FeatureKind::Comments)).unwrap();
    assert_eq!(comments.capability, None, "comments are off until the owner decides");
    let search = report
        .forms
        .iter()
        .find(|f| f.page == "/" && f.feature == Some(FeatureKind::Search))
        .unwrap();
    assert_eq!(search.capability.as_deref(), Some("search_site"));
    let login = report.forms.iter().find(|f| f.feature == Some(FeatureKind::Login));
    assert!(login.is_none(), "the login page itself is left out (A-I1)");
    assert!(report.capabilities.iter().any(|c| c.name == "submit_inquiry"));
    let frozen_contact = std::fs::read_to_string(out.path().join("contact/index.html")).unwrap();
    assert!(
        frozen_contact
            .contains("action=\"https://agent.solar.test/awp/v2/capabilities/submit_inquiry\"")
    );
    assert!(recorder.records().iter().any(|r| r.action == "site.freeze"));

    // Serve the release; the upstream sees nothing more.
    let served = StaticServer::serve(&host.serving_dir(HostTarget::Production).unwrap().unwrap())
        .await
        .unwrap();
    let before = upstream.requests();
    let client =
        reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let mut pages = 0;
    for path in report.release.files.keys().filter(|p| p.ends_with("index.html")) {
        let url = format!("{}{}", served.origin(), path.trim_end_matches("index.html"));
        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status(), 200, "{url}");
        pages += 1;
    }
    let redirect = client.get(format!("{}/old-offers/", served.origin())).send().await.unwrap();
    assert_eq!(redirect.status(), 301);
    assert!(pages >= 7, "{pages} pages served");
    assert_eq!(upstream.requests(), before, "zero requests reach the upstream at request time");
    assert!(served.requests() > pages);
}

fn signer_for_tests() -> Arc<dyn adk_site::migrate::http::CrawlSigner> {
    signer()
}
