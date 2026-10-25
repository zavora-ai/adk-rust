//! The launch gate (spec 3.5 §7.8.1, §7.8.2 step 2, §7.8.3 tiers): a clean site passes, every
//! check fails on its own fixture, and the fast tier checks only what a change touched.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use adk_site::content::SiteProject;
use adk_site::gate::{
    CheckStatus, FactStatus, FactTable, GateCheck, GateInput, GateMode, GateReport, LaunchGate,
    Quarantine,
};
use adk_site::generator::{BuildReport, BuiltinGenerator, ChangeSet, SiteGenerator};
use adk_site::theme::Rgb;
use awp_types::{ContentId, FactRef, Markdown, RedLine, RedLineKind};
use chrono::{TimeZone, Utc};
use tempfile::TempDir;

async fn project() -> SiteProject {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    SiteProject::load(&root, Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap()).await.unwrap()
}

fn facts(status: FactStatus) -> LaunchGate {
    LaunchGate::new()
        .with_claim_check(Arc::new(FactTable::new([(FactRef("fact-founded".into()), status)])))
}

async fn build(project: &SiteProject) -> (TempDir, BuildReport) {
    let out = tempfile::tempdir().unwrap();
    let report = BuiltinGenerator.build(project, out.path()).await.unwrap();
    (out, report)
}

async fn run(
    gate: &LaunchGate,
    mode: GateMode,
    project: &SiteProject,
    out: &Path,
    report: &BuildReport,
) -> GateReport {
    run_with(gate, mode, project, out, report, None, &Quarantine::default()).await
}

async fn run_with(
    gate: &LaunchGate,
    mode: GateMode,
    project: &SiteProject,
    out: &Path,
    report: &BuildReport,
    changed: Option<&BTreeSet<ContentId>>,
    quarantine: &Quarantine,
) -> GateReport {
    gate.run(mode, &GateInput { project, out, report, changed, quarantine }).await.unwrap()
}

fn failed(report: &GateReport, check: GateCheck) -> Vec<String> {
    let result = report.check(check).unwrap();
    assert_eq!(result.status, CheckStatus::Failed, "{check:?} should fail: {result:?}");
    result.findings.iter().filter(|f| f.blocking).map(|f| f.rule.clone()).collect()
}

fn item<'a>(project: &'a mut SiteProject, id: &str) -> &'a mut awp_types::ContentItem {
    project.items.iter_mut().find(|i| i.id.0 == id).unwrap()
}

#[tokio::test]
async fn a_clean_site_passes_the_full_gate() {
    let project = project().await;
    let (out, report) = build(&project).await;
    let verdict =
        run(&facts(FactStatus::Known), GateMode::Full, &project, out.path(), &report).await;
    assert!(
        verdict.passed(),
        "{}: {:#?}",
        verdict.summary(),
        verdict.blocking().collect::<Vec<_>>()
    );
    let performance = verdict.check(GateCheck::Performance).unwrap();
    assert_eq!(performance.status, CheckStatus::Passed);
    assert!(performance.note.as_deref().unwrap().contains("LCP not measured"));
}

#[tokio::test]
async fn server_code_refuses_the_release() {
    let project = project().await;
    let (out, report) = build(&project).await;
    std::fs::create_dir_all(out.path().join("uploads")).unwrap();
    std::fs::write(out.path().join("uploads/shell.php"), "<?php system($_GET['c']);").unwrap();
    std::fs::write(
        out.path().join("about/extra.html"),
        "<p><!--#include virtual=\"/etc/passwd\" --></p>",
    )
    .unwrap();
    let verdict =
        run(&facts(FactStatus::Known), GateMode::Full, &project, out.path(), &report).await;
    assert_eq!(
        failed(&verdict, GateCheck::ServerCode).len(),
        3,
        "extension, PHP tag, SSI directive"
    );
    assert!(verdict.refuses_release());
}

#[tokio::test]
async fn claims_need_known_facts() {
    let project = project().await;
    let (out, report) = build(&project).await;
    let verdict =
        run(&facts(FactStatus::Assumed), GateMode::Full, &project, out.path(), &report).await;
    assert_eq!(failed(&verdict, GateCheck::Claims), vec!["claim-not-known"]);
    assert!(verdict.held_items().contains(&ContentId::from("pages/about/en")));
    let unchecked = run(&LaunchGate::new(), GateMode::Full, &project, out.path(), &report).await;
    assert_eq!(failed(&unchecked, GateCheck::Claims), vec!["claims-unverified"]);
}

#[tokio::test]
async fn broken_internal_links_fail() {
    let mut project = project().await;
    item(&mut project, "pages/about/en").body =
        Some(Markdown("See [the team](/team/) and [news](https://example.com/nowhere/).".into()));
    let (out, report) = build(&project).await;
    let verdict =
        run(&facts(FactStatus::Known), GateMode::Full, &project, out.path(), &report).await;
    assert_eq!(failed(&verdict, GateCheck::Links), vec!["broken-link", "broken-link"]);
}

#[tokio::test]
async fn accessibility_failures_each_fail() {
    let mut project = project().await;
    item(&mut project, "posts/hello/en").media[0].alt_confirmed = false;
    project.theme.tokens.colors.muted = Rgb(0xbb, 0xbb, 0xbb);
    let base = project.theme.templates["base.html"]
        .replace("<html lang=\"{{ page.language }}\">", "<html>");
    project.theme.templates.insert("base.html".into(), base);
    let (out, report) = build(&project).await;
    let verdict =
        run(&facts(FactStatus::Known), GateMode::Full, &project, out.path(), &report).await;
    let rules: BTreeSet<String> = failed(&verdict, GateCheck::Accessibility).into_iter().collect();
    assert_eq!(
        rules,
        ["color-contrast", "html-has-lang", "media-alt-confirmed"]
            .into_iter()
            .map(String::from)
            .collect()
    );
}

#[tokio::test]
async fn pages_over_the_performance_budget_fail() {
    let mut project = project().await;
    project.config.performance.critical_path_bytes = 3_000;
    let (out, report) = build(&project).await;
    let verdict =
        run(&facts(FactStatus::Known), GateMode::Full, &project, out.path(), &report).await;
    assert!(failed(&verdict, GateCheck::Performance).iter().all(|r| r == "critical-path-budget"));
}

#[tokio::test]
async fn legal_pages_are_required_per_jurisdiction() {
    let mut project = project().await;
    project.items.retain(|i| i.id.0 != "pages/consent/en");
    item(&mut project, "pages/privacy/en")
        .fields
        .insert("jurisdictions".into(), serde_json::json!(["KE"]));
    project.config.jurisdictions = vec!["KE".into(), "EU".into()];
    let (out, report) = build(&project).await;
    let verdict =
        run(&facts(FactStatus::Known), GateMode::Full, &project, out.path(), &report).await;
    // Consent is missing for KE and EU; privacy covers KE only.
    assert_eq!(failed(&verdict, GateCheck::Legal).len(), 3);
}

#[tokio::test]
async fn missing_or_invalid_twins_fail() {
    let project = project().await;
    let (out, report) = build(&project).await;
    std::fs::remove_file(out.path().join("about/index.md")).unwrap();
    std::fs::write(out.path().join("posts/hello/index.awp.json"), "{}").unwrap();
    let verdict =
        run(&facts(FactStatus::Known), GateMode::Full, &project, out.path(), &report).await;
    let rules = failed(&verdict, GateCheck::MachineViews);
    assert!(
        rules.contains(&"twin-missing".to_string())
            && rules.contains(&"machine-view-invalid".to_string())
    );
}

#[tokio::test]
async fn quarantined_items_never_ship() {
    let project = project().await;
    let (out, report) = build(&project).await;
    let quarantine = Quarantine {
        items: BTreeSet::from([ContentId::from("posts/hello/en")]),
        paths: BTreeSet::from(["posts/hello/panel.png".to_string()]),
    };
    let verdict = run_with(
        &facts(FactStatus::Known),
        GateMode::Fast,
        &project,
        out.path(),
        &report,
        None,
        &quarantine,
    )
    .await;
    assert_eq!(
        failed(&verdict, GateCheck::Quarantine),
        vec!["quarantined-item", "quarantined-file"]
    );
}

#[tokio::test]
async fn required_languages_red_lines_and_structured_data_fail() {
    let mut project = project().await;
    project.config.translation.required = vec!["en".parse().unwrap(), "sw".parse().unwrap()];
    item(&mut project, "products/panel-400/en").fields.insert("currency".into(), "kes".into());
    item(&mut project, "events/open-day/en").fields.remove("startsAt");
    let gate = facts(FactStatus::Known)
        .with_red_lines(vec![RedLine { kind: RedLineKind::Claim, text: "First panel".into() }]);
    let (out, report) = build(&project).await;
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    assert!(
        failed(&verdict, GateCheck::RequiredLanguages).len() >= 5,
        "items with no Swahili version"
    );
    assert_eq!(failed(&verdict, GateCheck::RedLines), vec!["red-line"]);
    let structured: BTreeSet<String> =
        failed(&verdict, GateCheck::StructuredData).into_iter().collect();
    assert_eq!(
        structured,
        ["currency-invalid", "start-invalid"].into_iter().map(String::from).collect()
    );
}

#[tokio::test]
async fn the_fast_tier_checks_only_the_change() {
    let mut project = project().await;
    let (out, first) = build(&project).await;
    // A broken link already on an untouched page: only the full tier sees it.
    let about = out.path().join("about/index.html");
    let html = std::fs::read_to_string(&about)
        .unwrap()
        .replace("</main>", "<a href=\"/gone-page/\">Gone</a></main>");
    std::fs::write(&about, html).unwrap();

    let id = ContentId::from("products/panel-400/en");
    item(&mut project, &id.0).fields.insert("currency".into(), "kes".into());
    project.release_seq = 2;
    let changes = ChangeSet { items: BTreeSet::from([id.clone()]), previous: first.graph.clone() };
    let second = BuiltinGenerator.build_incremental(&project, out.path(), &changes).await.unwrap();
    let gate = facts(FactStatus::Known);
    let fast = run_with(
        &gate,
        GateMode::Fast,
        &project,
        out.path(),
        &second,
        Some(&changes.items),
        &Quarantine::default(),
    )
    .await;
    assert_eq!(
        fast.check(GateCheck::Links).unwrap().status,
        CheckStatus::Passed,
        "the edited about page is not in scope"
    );
    assert_eq!(fast.check(GateCheck::Performance).unwrap().status, CheckStatus::Skipped);
    assert_eq!(fast.check(GateCheck::Legal).unwrap().status, CheckStatus::Skipped);
    assert_eq!(failed(&fast, GateCheck::StructuredData), vec!["currency-invalid"]);
    assert_eq!(fast.held_items(), BTreeSet::from([id]), "the failing item is held, not the batch");
    assert!(!fast.refuses_release());

    let full = run(&gate, GateMode::Full, &project, out.path(), &second).await;
    assert_eq!(failed(&full, GateCheck::Links), vec!["broken-link"]);
}
