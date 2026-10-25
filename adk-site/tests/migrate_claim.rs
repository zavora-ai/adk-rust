//! Domain claims (spec 3.5 §7.18 stage 1, A-I9): control is proven by a DNS TXT record or a
//! well-known token; until then only polite public reads run, and every active step refuses.
#![cfg(feature = "migrate")]

mod support;

use adk_site::migrate::claim::{
    CLAIM_WELL_KNOWN_PREFIX, ClaimMethod, DomainClaim, RecordKind, StaticDnsResolver,
};
use adk_site::migrate::detect::{Detectors, Subject};
use adk_site::migrate::{ClaimScope, MigrateError};
use chrono::Utc;
use support::*;
use url::Url;

#[tokio::test]
async fn a_dns_record_proves_control() {
    let claim = DomainClaim::new("Solar.Test.", Utc::now()).unwrap();
    assert_eq!(claim.domain, "solar.test");
    assert_eq!(claim.token.len(), 64);
    let (name, value) = claim.dns_record();
    assert_eq!(name, "_awp-claim.solar.test");
    let dns = StaticDnsResolver::new();
    let missing = claim.verify_dns(&dns, Utc::now()).await.unwrap_err();
    assert!(matches!(missing, MigrateError::ClaimFailed { .. }), "{missing}");
    dns.set(&name, RecordKind::Txt, ["awp-claim=0000", "v=spf1 -all"]).await;
    assert!(claim.verify_dns(&dns, Utc::now()).await.is_err(), "another token proves nothing");
    dns.set(&name, RecordKind::Txt, [value.clone(), "v=spf1 -all".into()]).await;
    let proven = claim.verify_dns(&dns, Utc::now()).await.unwrap();
    assert_eq!(proven.method(), ClaimMethod::DnsTxt);
    assert!(proven.covers("solar.test") && proven.covers("www.solar.test"));
    assert!(!proven.covers("notsolar.test") && !proven.covers("solar.test.evil.example"));
    assert!(!format!("{claim:?}").contains(&claim.token), "the token is not printed");
    assert!(DomainClaim::new("localhost", Utc::now()).is_err());
    assert!(DomainClaim::new("a b.example", Utc::now()).is_err());
}

#[tokio::test]
async fn a_well_known_token_proves_control() {
    let claim = DomainClaim::new(DOMAIN, Utc::now()).unwrap();
    let (path, body) = claim.well_known();
    assert!(path.starts_with(CLAIM_WELL_KNOWN_PREFIX));
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("site").join(path.trim_start_matches('/'));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let server = FixtureServer::start_at(root.path().to_path_buf(), false).await;
    let missing = claim.verify_well_known(&client(&server), Utc::now()).await.unwrap_err();
    assert!(matches!(missing, MigrateError::ClaimFailed { .. }), "{missing}");
    std::fs::write(&file, format!("{body}\n")).unwrap();
    let proven = claim.verify_well_known(&client(&server), Utc::now()).await.unwrap();
    assert_eq!(proven.method(), ClaimMethod::WellKnown);
    assert_eq!(proven.domain(), DOMAIN);
}

#[tokio::test]
async fn until_proven_only_polite_reads_run() {
    let server = FixtureServer::start(true).await;
    let polite = context(&server);
    assert!(matches!(polite.scope, ClaimScope::Polite));
    let home = Url::parse(&format!("{ORIGIN}/")).unwrap();
    let refused = Detectors::new()
        .check_cloaking(&polite, &Subject::Url("/".into()), &home)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, MigrateError::NotProven { ref action, .. } if action == "a cloaking check"),
        "{refused}"
    );
    assert_eq!(server.requests(), 0, "a refused step sends nothing");
    let elsewhere =
        polite.clone().with_scope(ClaimScope::Proven(proven("elsewhere.example").await));
    let refused = Detectors::new()
        .check_cloaking(&elsewhere, &Subject::Url("/".into()), &home)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, MigrateError::NotProven { .. }),
        "a proof covers its own domain only"
    );
    let proven_ctx = proven_context(&server).await;
    let finding = Detectors::new()
        .check_cloaking(&proven_ctx, &Subject::Url("/".into()), &home)
        .await
        .unwrap();
    assert!(finding.is_some(), "the infected home page cloaks");
    let browser = server.seen().into_iter().filter(|s| !s.signed).count();
    assert_eq!(browser, 1, "the browser half of the dual fetch is the only unsigned request");
}
