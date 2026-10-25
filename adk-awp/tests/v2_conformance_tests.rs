//! The AWP 2.0 conformance kit against in-process sites for each profile, and the document
//! rules against the protocol repository's examples (spec 3.5 §7.7; AWP 2.0 §16).

#![cfg(feature = "verify")]

use std::sync::Arc;

use adk_awp::conformance::fixture::{FixtureSite, fixture_time};
use adk_awp::conformance::rules::{DocumentKind, check_document};
use adk_awp::conformance::{CheckStatus, ConformanceOptions, ConformanceReport, run};
use adk_awp::signing::jws::sign;
use adk_awp::v2::capability::{
    CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry,
};
use adk_awp::v2::routes::{EdgeState, awp_v2_routes};
use adk_awp::v2::store::{MemoryEdgeStores, StatusStore};
use adk_awp::verify::fetch::RouterFetcher;
use awp_types::{CapabilityEffect, CapabilityV2, Profile, SiteState, SiteStatus, TrustTier};
use serde_json::{Value, json};

struct Echo;

#[async_trait::async_trait]
impl CapabilityHandler for Echo {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        Ok(call.input)
    }
}

async fn edge_site(profile: Profile) -> (FixtureSite, axum::Router) {
    let site = FixtureSite::build("example.com", profile).await.unwrap();
    let mut registry = CapabilityRegistry::new("example.com");
    registry
        .declare(
            CapabilityV2::new("search_site", "Search", CapabilityEffect::ReadOnly),
            Arc::new(Echo),
        )
        .unwrap();
    registry
        .declare(
            CapabilityV2::new("book", "Book", CapabilityEffect::NonIdempotent)
                .with_min_trust(TrustTier::Known),
            Arc::new(Echo),
        )
        .unwrap();
    let stores = MemoryEdgeStores::default();
    let mut status =
        SiteStatus::new(SiteState::Operational, fixture_time(), awp_types::Proof::eddsa("", ""));
    status.release_seq = Some(2);
    status.proof = sign(&status, site.edge_key.as_ref()).await.unwrap();
    stores.status.set(status).await.unwrap();
    let edge = awp_v2_routes(EdgeState::builder(registry).memory_stores(stores).build());
    let router = site.router().merge(edge);
    (site, router)
}

async fn conformance(site: &FixtureSite, router: axum::Router, active: bool) -> ConformanceReport {
    let options = ConformanceOptions {
        active_edge_checks: active,
        now: Some(fixture_time()),
        ..Default::default()
    };
    run(
        Arc::new(RouterFetcher::new(router)),
        Arc::new(site.resolver.clone()),
        "example.com",
        &options,
    )
    .await
}

fn statuses(report: &ConformanceReport) -> Vec<(&str, CheckStatus)> {
    report.results.iter().map(|result| (result.id, result.status)).collect()
}

#[tokio::test]
async fn the_static_profile_conforms() {
    let site = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    let report = conformance(&site, site.router(), false).await;
    assert!(report.passed(), "{report:#?}");
    assert_eq!(report.profile, Some(Profile::Static));
    let ids: Vec<&str> = report.results.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec!["S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9"]);
    assert!(report.results.iter().all(|r| r.status == CheckStatus::Pass));
}

#[tokio::test]
async fn the_static_edge_profile_conforms_with_active_checks() {
    let (site, router) = edge_site(Profile::StaticEdge).await;
    let report = conformance(&site, router.clone(), true).await;
    assert!(report.passed(), "{report:#?}");
    for id in ["E1", "E3", "E8", "E11", "E12"] {
        assert_eq!(report.result(id).unwrap().status, CheckStatus::Pass, "{id}: {report:#?}");
    }
    for id in ["E2", "E4", "E5", "E6", "E7", "E9", "E10", "E13"] {
        assert_eq!(report.result(id).unwrap().status, CheckStatus::Skipped, "{id}");
    }
    let passive = conformance(&site, router, false).await;
    assert!(passive.passed());
    assert_eq!(
        passive.result("E3").unwrap().status,
        CheckStatus::Skipped,
        "active checks need consent (A-I9)"
    );
    assert_eq!(passive.result("E1").unwrap().status, CheckStatus::Pass);
}

#[tokio::test]
async fn the_dynamic_profile_takes_the_edge_rows() {
    let (site, router) = edge_site(Profile::Dynamic).await;
    let report = conformance(&site, router, true).await;
    assert!(report.passed(), "{report:#?}");
    let ids: Vec<&str> = report.results.iter().map(|r| r.id).collect();
    assert!(
        !ids.contains(&"S4") && !ids.contains(&"S5") && ids.contains(&"S6") && ids.contains(&"E1")
    );
}

#[tokio::test]
async fn failures_name_their_requirement_and_skip_what_was_not_reached() {
    let mut site = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    let mut card = site.json("/.well-known/agent-card.json").unwrap();
    card["skills"][0]["id"] = json!("place_order");
    site.set_json("/.well-known/agent-card.json", &card);
    let report = conformance(&site, site.router(), false).await;
    assert!(!report.passed());
    let by_id = statuses(&report);
    assert!(by_id.contains(&("S8", CheckStatus::Fail)));
    assert!(by_id.contains(&("S1", CheckStatus::Pass)));
    assert!(by_id.contains(&("S5", CheckStatus::Skipped)));

    // A static+edge contract that drops its identity policy fails E1.
    let (mut site, _) = edge_site(Profile::StaticEdge).await;
    let mut contract = site.contract.clone();
    contract.identity = None;
    contract.proof = None;
    contract.proof = Some(sign(&contract, site.root.as_ref()).await.unwrap());
    site.set_json("/.well-known/awp.json", &serde_json::to_value(&contract).unwrap());
    let report = conformance(&site, site.router(), false).await;
    assert_eq!(report.result("E1").unwrap().status, CheckStatus::Fail);
}

fn example(path: &str) -> Value {
    let path = format!("{}/tests/fixtures/awp-2.0/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Every rule the types accept is enforced: each of the protocol repository's invalid
/// examples breaks the rule it was written for, and every valid example breaks none.
#[test]
fn protocol_examples_meet_the_rules() {
    let invalid = [
        ("capability-manifest.bad-name.json", DocumentKind::CapabilityManifest, "capability.name"),
        (
            "capability-manifest.unknown-approval.json",
            DocumentKind::CapabilityManifest,
            "document.schema",
        ),
        ("charter.target-without-baseline.json", DocumentKind::Charter, "charter.validate"),
        (
            "consent-receipt.outreach-without-explicit-consent.json",
            DocumentKind::ConsentReceipt,
            "consent.explicit",
        ),
        ("event-envelope.catalog-without-total.json", DocumentKind::EventEnvelope, "event.data"),
        ("held-call.completed-without-result.json", DocumentKind::HeldCall, "held_call.outcome"),
        ("log-entry.extra-member.json", DocumentKind::LogEntry, "document.schema"),
        (
            "release-manifest.missing-previous.json",
            DocumentKind::ReleaseManifest,
            "release.previous",
        ),
        (
            "release-manifest.redirect-status-200.json",
            DocumentKind::ReleaseManifest,
            "release.redirect_status",
        ),
        (
            "release-manifest.snapshot-with-removed.json",
            DocumentKind::ReleaseManifest,
            "release.removed",
        ),
        (
            "site-contract.edge-without-identity.json",
            DocumentKind::SiteContract,
            "contract.identity",
        ),
        ("site-contract.missing-proof.json", DocumentKind::SiteContract, "contract.proof"),
        ("site-status.unsigned.json", DocumentKind::SiteStatus, "document.schema"),
    ];
    for (file, kind, rule) in invalid {
        let violations = check_document(kind, &example(&format!("invalid/{file}")));
        assert!(violations.iter().any(|v| v.rule == rule), "{file}: {violations:?}");
    }
    let valid = [
        ("site-contract.static.json", DocumentKind::SiteContract),
        ("site-contract.static-edge.json", DocumentKind::SiteContract),
        ("key-set.json", DocumentKind::KeySet),
        ("trust-rules.json", DocumentKind::TrustRules),
        ("capability-manifest.json", DocumentKind::CapabilityManifest),
        ("held-call.json", DocumentKind::HeldCall),
        ("machine-view.json", DocumentKind::MachineView),
        ("event-envelope.release-published.json", DocumentKind::EventEnvelope),
        ("event-envelope.catalog-changed.json", DocumentKind::EventEnvelope),
        ("event-envelope.status-changed.json", DocumentKind::EventEnvelope),
        ("subscription.json", DocumentKind::Subscription),
        ("site-status.json", DocumentKind::SiteStatus),
        ("consent-receipt.json", DocumentKind::ConsentReceipt),
        ("quote.json", DocumentKind::Quote),
        ("receipt.json", DocumentKind::Receipt),
        ("charter-public-terms.json", DocumentKind::PublicTerms),
        ("charter.json", DocumentKind::Charter),
        ("release-manifest.snapshot.json", DocumentKind::ReleaseManifest),
        ("release-manifest.delta.json", DocumentKind::ReleaseManifest),
        ("log-entry.json", DocumentKind::LogEntry),
        ("stream-entry.json", DocumentKind::StreamEntry),
    ];
    for (file, kind) in valid {
        let violations = check_document(kind, &example(file));
        assert!(violations.is_empty(), "{file}: {violations:?}");
    }
}
