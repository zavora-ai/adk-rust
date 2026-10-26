//! `awp verify` against the preview as a launch-gate check (spec 3.5 §7.8.1 "Machine views"):
//! the site's first release verifies with no log, the next verifies over the host's log, and
//! a preview a verifier would refuse fails the gate under the failure's AWP 2.0 §15.2 name.
#![cfg(feature = "verify")]

use std::path::Path;
use std::sync::Arc;

use adk_awp::conformance::fixture::{FixtureSite, RELEASE_SEED, fixture_time};
use adk_core::audit_log::InMemoryAuditLogStore;
use adk_site::content::SiteProject;
use adk_site::gate::{
    CheckStatus, GateCheck, GateInput, GateMode, GateReport, LaunchGate, PreviewVerification,
    Quarantine,
};
use adk_site::generator::{BuildReport, BuiltinGenerator, SiteGenerator};
use adk_site::host::{DirectoryHost, HostTarget, SiteHost};
use adk_site::log::SiteLog;
use adk_site::release::{ReleaseBuilder, ReleaseOptions};
use adk_site::signer::Ed25519ReleaseSigner;
use awp_types::{Profile, PublicTerms, ReleaseSource};
use bytes::Bytes;
use chrono::{TimeZone, Utc};
use tempfile::TempDir;

/// The fixture project signed by `site`'s root: its contract, key set, Agent Card and public
/// terms are the project's documents.
async fn signed_project(site: &FixtureSite) -> SiteProject {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    let mut project =
        SiteProject::load(&root, Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap())
            .await
            .unwrap();
    for path in
        ["/.well-known/awp.json", "/.well-known/awp/keys.json", "/.well-known/agent-card.json"]
    {
        project.documents.insert(
            path.trim_start_matches('/').to_string(),
            Bytes::from(site.files[path].body.clone()),
        );
    }
    let terms: PublicTerms =
        serde_json::from_value(site.json("/.well-known/awp/charter.json").unwrap()).unwrap();
    project.public_terms = Some(terms);
    project
}

async fn build(project: &SiteProject) -> (TempDir, BuildReport) {
    let out = tempfile::tempdir().unwrap();
    let report = BuiltinGenerator.build(project, out.path()).await.unwrap();
    (out, report)
}

fn gate(site: &FixtureSite, host: Arc<DirectoryHost>) -> LaunchGate {
    let check = PreviewVerification::new("example.com", host, Arc::new(site.resolver.clone()))
        .at(fixture_time());
    LaunchGate::new().with_extension(Arc::new(check))
}

async fn run(
    gate: &LaunchGate,
    mode: GateMode,
    project: &SiteProject,
    out: &Path,
    report: &BuildReport,
) -> GateReport {
    let quarantine = Quarantine::default();
    gate.run(mode, &GateInput { project, out, report, changed: None, quarantine: &quarantine })
        .await
        .unwrap()
}

fn options(report: &BuildReport) -> ReleaseOptions {
    ReleaseOptions {
        source: ReleaseSource::Onboarding,
        generator: report.generator.clone(),
        charter_version: 1,
        redirects: report.redirects.clone(),
        written: None,
        force_snapshot: false,
    }
}

#[tokio::test]
async fn awp_verify_passes_against_the_preview_of_each_release() {
    let site = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(DirectoryHost::new(dir.path()));
    let gate = gate(&site, host.clone());
    let mut project = signed_project(&site).await;

    // Release 1: nothing is logged yet, so the preview is the whole site.
    let (out, report) = build(&project).await;
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    let check = verdict.check(GateCheck::AwpVerify).unwrap();
    assert_eq!(check.status, CheckStatus::Passed, "{check:#?}");
    assert!(check.note.as_deref().unwrap().contains("release 1"), "{check:#?}");
    assert!(
        run(&gate, GateMode::Fast, &project, out.path(), &report)
            .await
            .check(GateCheck::AwpVerify)
            .is_none(),
        "the per-change tier leaves it to the full tier"
    );

    // Release 1 goes live, logged and signed by the delegated release key.
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed("example.com", RELEASE_SEED).unwrap());
    let log =
        SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), "example.com", signer).await.unwrap();
    let released = ReleaseBuilder::new().release(out.path(), None, options(&report), &log).await;
    host.publish(&released.unwrap(), HostTarget::Production).await.unwrap();

    // Release 2 verifies over the host's log of release 1.
    let about = project.items.iter_mut().find(|item| item.body.is_some()).unwrap();
    about.body = Some(awp_types::Markdown("A page that changed for release 2.".into()));
    let (out, report) = build(&project).await;
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    let check = verdict.check(GateCheck::AwpVerify).unwrap();
    assert_eq!(check.status, CheckStatus::Passed, "{check:#?}");
    assert!(check.note.as_deref().unwrap().contains("release 2"), "{check:#?}");
}

#[tokio::test]
async fn a_preview_a_verifier_refuses_fails_the_gate_by_name() {
    let site = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let gate = gate(&site, Arc::new(DirectoryHost::new(dir.path())));

    // A contract changed after the root signed it.
    let mut project = signed_project(&site).await;
    let mut contract = site.contract.clone();
    contract.site.name = "Someone Else".into();
    project
        .documents
        .insert(".well-known/awp.json".into(), Bytes::from(serde_json::to_vec(&contract).unwrap()));
    let (out, report) = build(&project).await;
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    assert!(!verdict.passed());
    let check = verdict.check(GateCheck::AwpVerify).unwrap();
    assert_eq!(check.status, CheckStatus::Failed);
    assert_eq!(check.findings[0].rule, "contract.signature", "{check:#?}");

    // No signed contract at all.
    let mut project = signed_project(&site).await;
    project.documents.remove(".well-known/awp.json");
    let (out, report) = build(&project).await;
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    let check = verdict.check(GateCheck::AwpVerify).unwrap();
    assert_eq!(check.findings[0].rule, "contract.unavailable", "{check:#?}");

    // A DNS anchor that names another root.
    let gate = LaunchGate::new().with_extension(Arc::new(
        PreviewVerification::new(
            "example.com",
            Arc::new(DirectoryHost::new(dir.path())),
            Arc::new(adk_awp::signing::anchor::FixedTxtResolver::default()),
        )
        .at(fixture_time()),
    ));
    let project = signed_project(&site).await;
    let (out, report) = build(&project).await;
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    let check = verdict.check(GateCheck::AwpVerify).unwrap();
    assert_eq!(check.findings[0].rule, "root.unanchored", "{check:#?}");
}

/// An Agent Card the edge serves, not the release, comes from the edge: without it the card
/// is unavailable and the check fails at `card.signature`.
#[tokio::test]
async fn documents_the_release_does_not_hold_come_from_the_edge() {
    let site = FixtureSite::build("example.com", Profile::StaticEdge).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut project = signed_project(&site).await;
    project.documents.remove(".well-known/agent-card.json");
    let (out, report) = build(&project).await;

    let verdict = run(
        &gate(&site, Arc::new(DirectoryHost::new(dir.path()))),
        GateMode::Full,
        &project,
        out.path(),
        &report,
    )
    .await;
    let check = verdict.check(GateCheck::AwpVerify).unwrap();
    assert_eq!(check.findings[0].rule, "card.signature", "{check:#?}");

    let edge = adk_awp::verify::fetch::RouterFetcher::new(site.router());
    let check = PreviewVerification::new(
        "example.com",
        Arc::new(DirectoryHost::new(dir.path())),
        Arc::new(site.resolver.clone()),
    )
    .with_fetcher(Arc::new(edge))
    .at(fixture_time());
    let gate = LaunchGate::new().with_extension(Arc::new(check));
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    let check = verdict.check(GateCheck::AwpVerify).unwrap();
    assert_eq!(check.status, CheckStatus::Passed, "{check:#?}");
}
