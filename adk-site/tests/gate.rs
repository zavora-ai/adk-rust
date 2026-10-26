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

/// A page quotes a review only with the reviewer's public name and the owner's approval
/// (spec 3.5 S2): the claims check finds a quote by its words and holds the page.
#[tokio::test]
async fn review_quotes_need_attribution_and_approval() {
    use adk_site::gate::QuotableReview;

    let mut project = project().await;
    let wanjiru =
        QuotableReview::new("The panels went up in a day and the crew left nothing behind.")
            .by("Wanjiru K.");
    let anonymous = QuotableReview::new("Fair prices, but the quote took a week to arrive.")
        .approved("decision-2");
    item(&mut project, "pages/about/en").body = Some(Markdown(
        "What clients say:\n\n> The panels went up in a day and the crew left nothing behind.\n>\n> — Wanjiru K.\n\n\
         > Fair prices, but the quote took a week to arrive.\n"
            .into(),
    ));
    let (out, report) = build(&project).await;
    let gate = facts(FactStatus::Known).with_reviews(vec![wanjiru.clone(), anonymous.clone()]);
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    let mut rules = failed(&verdict, GateCheck::Claims);
    rules.sort();
    assert_eq!(rules, ["review-quote-anonymous", "review-quote-unapproved"]);
    assert!(verdict.held_items().contains(&ContentId("pages/about/en".into())));

    // Approved and attributed, the quote is published.
    item(&mut project, "pages/about/en").body = Some(Markdown(
        "What clients say:\n\n> The panels went up in a day and the crew left nothing behind.\n>\n> — Wanjiru K.\n"
            .into(),
    ));
    let (out, report) = build(&project).await;
    let gate =
        facts(FactStatus::Known).with_reviews(vec![wanjiru.approved("decision-1"), anonymous]);
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    assert_eq!(verdict.check(GateCheck::Claims).unwrap().status, CheckStatus::Passed);
}

/// A page or stylesheet that loads anything from another origin fails the third-party
/// resource check in both tiers (spec 3.5 §7.16.1, S16).
#[tokio::test]
async fn third_party_resources_fail_their_own_check() {
    use adk_site::gate::ThirdPartyResources;

    let project = project().await;
    let (out, report) = build(&project).await;
    let gate = facts(FactStatus::Known)
        .with_extension(Arc::new(ThirdPartyResources::new("https://example.com")));
    let clean = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    assert_eq!(clean.check(GateCheck::ThirdPartyResources).unwrap().status, CheckStatus::Passed);

    let index = out.path().join("index.html");
    let html = std::fs::read_to_string(&index).unwrap().replace(
        "</head>",
        "<link rel=\"icon\" href=\"https://cdn.example/favicon.ico\">\
         <style>body{background:url(https://pixel.example/p.gif)}</style></head>",
    );
    let html = html.replacen(
        "<main",
        "<img alt=\"\" src=\"/logo.png\" srcset=\"/logo.png 1x, https://img.example/logo@2x.png 2x\"><main",
        1,
    );
    std::fs::write(&index, html).unwrap();
    let css = out.path().join("assets/site.css");
    let mut text = std::fs::read_to_string(&css).unwrap();
    text.insert_str(0, "@import \"https://fonts.example/inter.css\";\n");
    std::fs::write(&css, text).unwrap();
    let verdict = run(&gate, GateMode::Full, &project, out.path(), &report).await;
    let result = verdict.check(GateCheck::ThirdPartyResources).unwrap();
    assert_eq!(result.status, CheckStatus::Failed);
    let loads: Vec<&str> = result.findings.iter().map(|f| f.message.as_str()).collect();
    for origin in ["cdn.example", "pixel.example", "img.example", "fonts.example"] {
        assert!(loads.iter().any(|m| m.contains(origin)), "{origin} in {loads:#?}");
    }
    assert!(result.findings.iter().all(|f| f.rule == "third-party-resource"));
}

/// The release key the site log signs with must be delegated by the owner's root; the check
/// reports as `GateCheck::Signing` (spec 3.5 §7.2, D-11, A-I5).
#[tokio::test]
async fn an_undelegated_release_key_fails_the_signing_check() {
    use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner, public_key_jwk};
    use adk_awp::signing::keys::delegate;
    use adk_site::gate::ReleaseDelegation;
    use awp_types::{KeyScope, KeySet, RootKey};

    let root = Ed25519DocumentSigner::from_seed("root-1", [1; 32]).unwrap();
    let release = Ed25519DocumentSigner::from_seed("release-1", [2; 32]).unwrap();
    let release_key = public_key_jwk(release.public_key_bytes());
    let check = ReleaseDelegation::new("release-1", release_key.clone(), "data/keys/release.jwk");
    let gate = LaunchGate::new().with_extension(Arc::new(check));

    let mut project = project().await;
    let (out, report) = build(&project).await;
    let verdict = run(&gate, GateMode::Fast, &project, out.path(), &report).await;
    assert_eq!(failed(&verdict, GateCheck::Signing), ["release-delegation"]);

    let now = Utc::now();
    let delegation = delegate(
        &root,
        "release-1",
        release_key,
        vec![KeyScope::Release],
        now - chrono::Duration::days(1),
        now + chrono::Duration::days(30),
    )
    .await
    .unwrap();
    let keys = KeySet {
        root: RootKey {
            kid: root.kid().to_string(),
            public_key: public_key_jwk(root.public_key_bytes()),
            endorsement: None,
        },
        delegations: vec![delegation],
        retired_roots: Vec::new(),
        revocations: Vec::new(),
    };
    project
        .documents
        .insert(".well-known/awp/keys.json".into(), serde_json::to_vec(&keys).unwrap().into());
    let (out, report) = build(&project).await;
    let verdict = run(&gate, GateMode::Fast, &project, out.path(), &report).await;
    assert_eq!(verdict.check(GateCheck::Signing).unwrap().status, CheckStatus::Passed);
}
