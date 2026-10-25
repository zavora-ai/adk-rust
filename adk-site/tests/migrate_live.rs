//! Live runs against the public internet: a polite, signed, robots-respecting crawl of
//! `example.com`, its TLS certificate's expiry and its DNS records through the system
//! resolver. Run with `--ignored` where the network is reachable.
#![cfg(feature = "migrate")]

mod support;

use std::sync::Arc;
use std::time::Duration;

use adk_site::migrate::claim::{DnsResolver, RecordKind, SystemDnsResolver};
use adk_site::migrate::http::{FetchOptions, HttpClient};
use adk_site::migrate::probe::CertificateProbe;
use adk_site::migrate::sources::Crawl;
use adk_site::migrate::{SiteSource, SourceContext, SourceLimits};
use url::Url;

#[tokio::test]
#[ignore = "reaches the public internet"]
async fn a_polite_signed_crawl_of_a_public_site() {
    let http = Arc::new(
        HttpClient::builder()
            .signer(support::signer())
            .min_interval(Duration::from_secs(1))
            .build()
            .unwrap(),
    );
    let ctx = SourceContext::new(support::acting(), Arc::clone(&http)).with_limits(SourceLimits {
        max_pages: 2,
        max_media: 0,
        ..SourceLimits::default()
    });
    let start = Url::parse("https://example.com/").unwrap();
    let inventory = Crawl::new(start.clone()).without_sitemaps().inventory(&ctx).await.unwrap();
    let home = inventory.url("/").expect("the home page");
    println!(
        "crawled {} URLs; home answered {} with title {:?}",
        inventory.urls.len(),
        home.status,
        home.page.as_ref().and_then(|p| p.title.clone())
    );
    assert_eq!(home.status, 200);
    assert!(http.connections().iter().all(|c| c.allowed && !c.loopback));

    let fetched = http.fetch(&start, &FetchOptions::crawler()).await.unwrap();
    let der = fetched.peer_certificate.expect("a TLS certificate");
    let certificate = CertificateProbe::from_der(&der, chrono::Utc::now(), 14).unwrap();
    println!("certificate expires {} ({} days)", certificate.not_after, certificate.days_left);
    assert!(certificate.days_left > 0);

    let addresses = SystemDnsResolver.lookup("example.com", RecordKind::A).await.unwrap();
    println!("example.com A {addresses:?}");
    assert!(!addresses.is_empty());
}
