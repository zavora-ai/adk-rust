//! Migration parity (spec 3.5 §7.8.1): a faithful migration passes URL coverage, main-text
//! similarity and metadata; a missing page, changed text, a lost title or a changed look fails
//! with the rule that broke. The visual half runs real headless Chrome when it is installed.
#![cfg(feature = "migrate")]

mod support;

use std::path::Path;
use std::sync::Arc;

use adk_site::content::{SiteConfig, SiteProject};
use adk_site::gate::{CheckResult, CheckStatus, GateCheck, GateExtension, GateInput, Quarantine};
use adk_site::generator::{BuildReport, BuiltinGenerator, SiteGenerator};
use adk_site::migrate::convert::{ConvertOptions, to_content};
use adk_site::migrate::parity::{
    ChromeScreenshotter, MigrationParity, Screenshotter, perceptual_difference, text_similarity,
};
use adk_site::migrate::serve::StaticServer;
use adk_site::migrate::sources::Crawl;
use adk_site::migrate::{SiteInventory, SiteSource};
use chrono::{TimeZone, Utc};
use support::*;
use url::Url;

async fn migrate() -> (SiteInventory, SiteProject, tempfile::TempDir, BuildReport) {
    let server = FixtureServer::start(false).await;
    let inventory =
        Crawl::new(Url::parse(ORIGIN).unwrap()).inventory(&context(&server)).await.unwrap();
    let converted = to_content(&inventory, &ConvertOptions::new("en".parse().unwrap())).unwrap();
    let mut config = SiteConfig::new("Example Solar", DOMAIN, vec!["en".parse().unwrap()]);
    config.redirects = converted.redirects.clone();
    let mut project = SiteProject::new(
        config,
        converted.items,
        Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap(),
    );
    project.documents = converted.documents;
    let out = tempfile::tempdir().unwrap();
    let report = BuiltinGenerator.build(&project, out.path()).await.unwrap();
    (inventory, project, out, report)
}

async fn run(
    parity: &MigrationParity,
    project: &SiteProject,
    out: &Path,
    report: &BuildReport,
) -> CheckResult {
    let quarantine = Quarantine::default();
    parity
        .run(&GateInput { project, out, report, changed: None, quarantine: &quarantine })
        .await
        .unwrap()
}

fn rules(result: &CheckResult) -> Vec<(String, String)> {
    let mut rules: Vec<(String, String)> = result
        .findings
        .iter()
        .map(|f| (f.rule.clone(), f.path.clone().unwrap_or_default()))
        .collect();
    rules.sort();
    rules
}

#[tokio::test]
async fn a_faithful_migration_passes_and_each_break_fails() {
    let (inventory, project, out, report) = migrate().await;
    let parity = MigrationParity::new(Arc::new(inventory));
    let result = run(&parity, &project, out.path(), &report).await;
    assert_eq!(result.check, GateCheck::MigrationParity);
    assert!(result.findings.is_empty(), "{:#?}", result.findings);
    assert_eq!(result.status, CheckStatus::NotMeasured, "visual comparison needs screenshots");
    let note = result.note.unwrap();
    assert!(note.contains("pages compared") && note.contains("lowest text similarity"), "{note}");

    // A missing page, changed text, a lost title and a changed canonical each fail.
    std::fs::remove_file(out.path().join("contact/index.html")).unwrap();
    let about = out.path().join("about/index.html");
    let html = std::fs::read_to_string(&about).unwrap();
    let html = html
        .replace(
            "We are a team of eight engineers and installers.",
            "Prices start at nothing and quality is optional for every buyer today.",
        )
        .replace("How we work", "What we sell");
    std::fs::write(&about, html).unwrap();
    let care = out.path().join("2026/10/12/panel-care/index.html");
    let html = std::fs::read_to_string(&care).unwrap();
    let html = html
        .replace("<title>Caring for your panels", "<title>Untitled")
        .replace("/2026/10/12/panel-care/\">", "/elsewhere/\">");
    std::fs::write(&care, html).unwrap();
    let result = run(&parity, &project, out.path(), &report).await;
    assert_eq!(result.status, CheckStatus::Failed);
    let found = rules(&result);
    for expected in [
        ("metadata-canonical", "/2026/10/12/panel-care/"),
        ("metadata-title", "/2026/10/12/panel-care/"),
        ("text-similarity", "/about/"),
        ("url-not-covered", "/contact/"),
    ] {
        assert!(
            found.contains(&(expected.0.to_string(), expected.1.to_string())),
            "{expected:?} in {found:?}"
        );
    }

    // Refused and quarantined subjects are exempt from coverage.
    let exempt = parity.clone().excluding(["/contact/".to_string(), "crawl:/contact/".to_string()]);
    let result = run(&exempt, &project, out.path(), &report).await;
    assert!(
        !rules(&result).iter().any(|(rule, path)| rule == "url-not-covered" && path == "/contact/")
    );
}

#[test]
fn similarity_and_difference_measures() {
    assert_eq!(text_similarity("", ""), 1.0);
    assert!(text_similarity("a b c d e f g h i j", "a b c d e f g h i x") > 0.85);
    let white = image::RgbImage::from_pixel(80, 60, image::Rgb([255, 255, 255]));
    let mut marked = white.clone();
    for x in 0..80 {
        for y in 0..30 {
            marked.put_pixel(x, y, image::Rgb([200, 20, 20]));
        }
    }
    assert!(perceptual_difference(&white, &white).unwrap() < 1e-9);
    assert!(perceptual_difference(&white, &marked).unwrap() > 0.05);
    let small = image::RgbImage::from_pixel(40, 30, image::Rgb([255, 255, 255]));
    assert!(perceptual_difference(&white, &small).unwrap() < 1e-6, "sizes are matched first");
}

/// Real headless Chrome: identical pages are under the 0.05 threshold at both widths, a page
/// with a changed look is over it, and the parity check measures the migrated build.
#[tokio::test]
#[ignore = "needs Chrome; run with --ignored"]
async fn chrome_screenshots_measure_visual_parity() {
    let chrome = ChromeScreenshotter::detect().expect("Chrome is installed");
    let legacy_root = tempfile::tempdir().unwrap();
    copy_tree(&fixtures().join("site"), legacy_root.path());
    let legacy = StaticServer::serve(legacy_root.path()).await.unwrap();
    let copy_root = tempfile::tempdir().unwrap();
    copy_tree(&fixtures().join("site"), copy_root.path());
    let about = copy_root.path().join("about/index.html");
    let changed = std::fs::read_to_string(&about)
        .unwrap()
        .replace("<body class=", "<body style=\"background:#123;color:#fff\" class=");
    std::fs::write(&about, changed).unwrap();
    let copy = StaticServer::serve(copy_root.path()).await.unwrap();
    for width in [1280, 390] {
        let same_a = chrome
            .screenshot(&Url::parse(&format!("{}/contact/", legacy.origin())).unwrap(), width, 900)
            .await
            .unwrap();
        let same_b = chrome
            .screenshot(&Url::parse(&format!("{}/contact/", copy.origin())).unwrap(), width, 900)
            .await
            .unwrap();
        let same = perceptual_difference(&same_a, &same_b).unwrap();
        let old = chrome
            .screenshot(&Url::parse(&format!("{}/about/", legacy.origin())).unwrap(), width, 900)
            .await
            .unwrap();
        let new = chrome
            .screenshot(&Url::parse(&format!("{}/about/", copy.origin())).unwrap(), width, 900)
            .await
            .unwrap();
        let different = perceptual_difference(&old, &new).unwrap();
        println!("width {width}: identical pages {same:.4}, restyled page {different:.4}");
        assert!(same < 0.05, "identical pages differ by {same}");
        assert!(different >= 0.05, "a restyled page differs by only {different}");
    }

    let (inventory, project, out, report) = migrate().await;
    let parity = MigrationParity::new(Arc::new(inventory))
        .with_legacy(Url::parse(&legacy.origin()).unwrap())
        .with_screenshotter(Arc::new(chrome))
        .with_config(adk_site::migrate::parity::ParityConfig { top_n: 3, ..Default::default() });
    let result = run(&parity, &project, out.path(), &report).await;
    println!("migrated build: {:?} {}", result.status, result.note.clone().unwrap_or_default());
    assert!(result.note.unwrap().contains("largest perceptual difference"));
    assert_ne!(result.status, CheckStatus::NotMeasured, "the visual comparison ran");
}
