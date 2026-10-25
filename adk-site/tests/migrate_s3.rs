//! S3, "migrate and leave the infection behind" (spec 3.5 §5, §7.8, §7.8.1, A-I1): the
//! WordPress fixture with its `infected` overlay is replayed in isolation, every inert marker
//! is quarantined with evidence, forms and the shop map to capabilities, and the migrated
//! build passes the parity, server-code and quarantine checks with zero executables released.
#![cfg(feature = "migrate")]

mod support;

use std::sync::Arc;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_site::content::{SiteConfig, SiteProject};
use adk_site::gate::{CheckStatus, GateCheck, GateInput, GateMode, LaunchGate};
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_site::log::SiteLog;
use adk_site::migrate::convert::{ConvertOptions, to_content};
use adk_site::migrate::detect::{Detectors, FindingKind, Subject};
use adk_site::migrate::mapping::map_features;
use adk_site::migrate::parity::MigrationParity;
use adk_site::migrate::quarantine::{QuarantineBook, QuarantineState};
use adk_site::migrate::sources::{IsolatedReplay, ReplaySpec};
use adk_site::migrate::{FeatureKind, SiteSource};
use adk_site::pipeline::{ApprovalDecision, ApprovalGate, ApprovalRequest, EntryPoint};
use adk_site::release::{ReleaseBuilder, ReleaseOptions};
use adk_site::signer::Ed25519ReleaseSigner;
use async_trait::async_trait;
use awp_types::{ApprovalHint, CapabilityEffect, ContentId, ReleaseSource};
use chrono::{TimeZone, Utc};
use support::*;
use url::Url;

/// Approves as one person and remembers what it was asked.
#[derive(Default)]
struct Owner {
    asked: std::sync::Mutex<Vec<ApprovalRequest>>,
}

#[async_trait]
impl ApprovalGate for Owner {
    async fn decide(&self, request: &ApprovalRequest) -> adk_site::Result<ApprovalDecision> {
        self.asked.lock().unwrap().push(request.clone());
        Ok(ApprovalDecision::Approved {
            approvers: vec!["owner".into()],
            decision: Some("decision-1".into()),
        })
    }
}

#[tokio::test]
async fn s3_migrates_and_leaves_the_infection_behind() {
    // Replay the infected site in isolation and inventory it.
    let driver = Arc::new(FakeDriver::new(true));
    let dump_dir = tempfile::tempdir().unwrap();
    let dump = dump_dir.path().join("dump.sql");
    std::fs::write(&dump, "-- recorded dump\n").unwrap();
    let uploads = uploads(true);
    let live = FixtureServer::start(true).await;
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let ctx = proven_context(&live).await.with_audit(recorder.clone());
    let replay = IsolatedReplay::new(driver, ReplaySpec::wordpress(ORIGIN, &dump, uploads.path()));
    let inventory = replay.inventory(&ctx).await.unwrap();
    assert!(replay.last_report().unwrap().outbound_connections.is_empty());

    // Every inert marker is found, with evidence.
    let detectors = Detectors::new();
    let mut findings = detectors.scan_inventory(&inventory).await;
    let home = Url::parse(&format!("{ORIGIN}/")).unwrap();
    let cloaking =
        detectors.check_cloaking(&ctx, &Subject::Url("/".into()), &home).await.unwrap().unwrap();
    assert!(
        cloaking.evidence[0]
            .detail
            .contains("scripts served only to the crawler: src https://cdn-stats.test/c.js"),
        "{:?}",
        cloaking.evidence
    );
    findings.push(cloaking);
    let found = |kind: FindingKind, subject: &Subject| {
        findings
            .iter()
            .find(|f| f.kind == kind && &f.subject == subject)
            .unwrap_or_else(|| panic!("{kind:?} on {subject:?}: {findings:#?}"))
    };
    let hello = Subject::Item("crawl:/2026/10/01/hello-sun/".into());
    assert!(
        found(FindingKind::HiddenLinks, &hello).evidence[0]
            .detail
            .contains("https://cheap-casino.test/")
    );
    assert!(found(FindingKind::SpamLinks, &hello).evidence.len() >= 2);
    let about = Subject::Item("crawl:/about/".into());
    assert!(
        found(FindingKind::ObfuscatedScript, &about).evidence[0]
            .detail
            .contains("eval over decoded text")
    );
    let planted = Subject::Media("/wp-content/uploads/2026/10/cache.php".into());
    assert!(found(FindingKind::Executable, &planted).evidence[0].detail.contains("server code"));
    let fake = Subject::Media("/wp-content/uploads/2026/10/photo.jpg".into());
    assert!(found(FindingKind::MimeMismatch, &fake).evidence[0].detail.contains("image/jpeg"));
    for finding in &findings {
        assert!(
            finding.evidence.iter().all(|e| !e.detail.is_empty() && e.digest.is_some()),
            "{finding:?}"
        );
    }
    let clean = Subject::Item("crawl:/contact/".into());
    assert!(findings.iter().all(|f| f.subject != clean), "clean pages raise nothing");

    // Convert, quarantine against the conversion, convert again without the held subjects.
    let options = ConvertOptions::new("en".parse().unwrap());
    let first = to_content(&inventory, &options).unwrap();
    let mut book = QuarantineBook::new(findings, &first);
    book.record_findings(&(recorder.clone() as Arc<_>), &ctx.acting).await.unwrap();
    let converted = to_content(&inventory, &options.clone().excluding(book.excluded())).unwrap();
    let ids: Vec<&str> = converted.items.iter().map(|i| i.id.0.as_str()).collect();
    assert!(!ids.contains(&"posts/hello-sun/en") && !ids.contains(&"pages/about/en"), "{ids:?}");
    assert!(ids.contains(&"pages/contact/en") && ids.contains(&"posts/panel-care/en"), "{ids:?}");
    assert!(!converted.documents.contains_key("/wp-content/uploads/2026/10/cache.php"));
    assert!(!converted.documents.contains_key("/wp-content/uploads/2026/10/photo.jpg"));
    assert!(converted.documents.contains_key("/wp-content/uploads/2026/10/panel.jpg"));

    // Forms and the shop map to capabilities; comments and logins to owner decisions.
    let map = map_features(&inventory);
    let inquiry = map.capability("submit_inquiry").unwrap();
    assert_eq!(inquiry.effect, CapabilityEffect::NonIdempotent);
    assert_eq!(inquiry.consent.as_ref().unwrap().scope.as_str(), "contact");
    assert!(inquiry.input_schema.as_ref().unwrap()["properties"].get("email").is_some());
    assert!(map.capability("search_site").is_some() && map.static_search_index);
    assert!(
        map.capability("get_quote").is_some()
            && map.capability("place_order").unwrap().requires_idempotency()
    );
    assert_eq!(map.decision(FeatureKind::Comments).unwrap().default, "off");
    assert!(map.decisions.iter().any(|d| d.id == "migration.accounts"));

    // Build the migrated site and run the gate's migration checks.
    let mut config = SiteConfig::new("Example Solar", DOMAIN, vec!["en".parse().unwrap()]);
    config.redirects = converted.redirects.clone();
    let mut project = SiteProject::new(
        config,
        converted.items.clone(),
        Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap(),
    );
    project.documents = converted.documents.clone();
    project.capabilities = map.capabilities.iter().map(|d| d.capability.clone()).collect();
    let out = tempfile::tempdir().unwrap();
    let report = BuiltinGenerator.build(&project, out.path()).await.unwrap();
    let parity = MigrationParity::new(Arc::new(inventory.clone())).excluding(book.excluded());
    let gate = LaunchGate::new().with_extension(Arc::new(parity));
    let quarantine = book.quarantine();
    let verdict = gate
        .run(
            GateMode::Full,
            &GateInput {
                project: &project,
                out: out.path(),
                report: &report,
                changed: None,
                quarantine: &quarantine,
            },
        )
        .await
        .unwrap();
    for check in [GateCheck::ServerCode, GateCheck::Quarantine] {
        assert_eq!(
            verdict.check(check).unwrap().status,
            CheckStatus::Passed,
            "{check:?}: {:?}",
            verdict.check(check)
        );
    }
    let parity = verdict.check(GateCheck::MigrationParity).unwrap();
    assert!(parity.findings.is_empty(), "{:#?}", parity.findings);
    assert_eq!(
        parity.status,
        CheckStatus::NotMeasured,
        "no screenshots without Chrome: {:?}",
        parity.note
    );
    let links = verdict.check(GateCheck::Links).unwrap();
    assert_eq!(
        links.status,
        CheckStatus::Passed,
        "zero broken internal links: {:#?}",
        links.findings
    );

    // Release: zero server-side executables (A-I1).
    let signer = Arc::new(Ed25519ReleaseSigner::generate(DOMAIN).unwrap());
    let log =
        SiteLog::open(Arc::new(adk_core::audit_log::InMemoryAuditLogStore::new()), DOMAIN, signer)
            .await
            .unwrap();
    let options = ReleaseOptions {
        source: ReleaseSource::Migration,
        generator: report.generator.clone(),
        charter_version: 1,
        redirects: report.redirects.clone(),
        written: None,
        force_snapshot: false,
    };
    let release = ReleaseBuilder::new().release(out.path(), None, options, &log).await.unwrap();
    assert!(release.files.keys().all(|p| !p.ends_with(".php")));
    assert!(release.files.contains_key("/wp-content/uploads/2026/10/panel.jpg"));

    // A per-item decision releases a held item; the planted executable stays refused.
    let owner = Owner::default();
    let hello_id = ContentId("posts/hello-sun/en".into());
    let state = book
        .decide(
            &hello_id,
            &owner,
            "migrator",
            EntryPoint::SiteAgent { agent: "steward".into() },
            Some((&(recorder.clone() as Arc<_>), &ctx.acting)),
        )
        .await
        .unwrap();
    assert!(matches!(state, QuarantineState::Released { .. }));
    let asked = owner.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].items, std::slice::from_ref(&hello_id));
    assert_eq!(asked[0].action_class, "site.migration.release_quarantined");
    let executable = ContentId("media:/wp-content/uploads/2026/10/cache.php".into());
    let state = book
        .decide(
            &executable,
            &owner,
            "migrator",
            EntryPoint::SiteAgent { agent: "steward".into() },
            None,
        )
        .await
        .unwrap();
    assert_eq!(state, QuarantineState::Refused);
    assert_eq!(owner.asked.lock().unwrap().len(), 1, "an executable is never put to a decision");
    let self_approved = book
        .decide(&ContentId("pages/about/en".into()), &owner, "owner", EntryPoint::WebEditor, None)
        .await
        .unwrap();
    assert_eq!(self_approved, QuarantineState::Held, "the proposer cannot approve its own item");
    assert!(!book.quarantine().items.contains(&hello_id));
    assert!(book.quarantine().paths.contains("wp-content/uploads/2026/10/cache.php"));

    let actions: Vec<String> = recorder.records().iter().map(|r| r.action.clone()).collect();
    assert!(actions.iter().filter(|a| *a == "site.migration.finding").count() >= 5, "{actions:?}");
    assert_eq!(actions.iter().filter(|a| *a == "site.migration.decided").count(), 1);
}

#[tokio::test]
async fn a_held_comment_capability_is_added_only_by_the_owners_answer() {
    let mut inventory =
        adk_site::migrate::SiteInventory::new(adk_site::migrate::SourceKind::Crawl, Utc::now());
    inventory.add_feature(adk_site::migrate::DetectedFeature {
        kind: FeatureKind::Comments,
        evidence: vec![],
        fields: vec![],
    });
    inventory.add_feature(adk_site::migrate::DetectedFeature {
        kind: FeatureKind::Bookings,
        evidence: vec![],
        fields: vec![],
    });
    inventory.add_feature(adk_site::migrate::DetectedFeature {
        kind: FeatureKind::Newsletter,
        evidence: vec![],
        fields: vec![],
    });
    let mut map = map_features(&inventory);
    assert!(map.capability("submit_comment").is_none(), "comments are off until the owner decides");
    assert_eq!(map.capability("check_availability").unwrap().effect, CapabilityEffect::ReadOnly);
    assert!(map.capability("book").unwrap().requires_idempotency());
    assert_eq!(
        map.capability("subscribe_newsletter").unwrap().consent.as_ref().unwrap().scope.as_str(),
        "newsletter"
    );
    map.answer(FeatureKind::Comments, "held");
    let comment = map.capability("submit_comment").unwrap();
    assert_eq!(comment.approval, ApprovalHint::Always, "every comment is held");
    assert_eq!(map.decision(FeatureKind::Comments).unwrap().default, "held");
}
