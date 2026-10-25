//! `adk-rust awp` (spec 3.5 §8.1): `verify` against a local fixture passes and fails naming
//! the broken link on each S13 tampering, `conformance` runs the kit, `migrate-config` turns a
//! 1.x `business.toml` into site data and a draft charter.
//!
//! The fixture is adk-awp's signed site, served on loopback; `--origin` reads the domain
//! from it, `--anchor` stands in for its DNS record and `--at` checks it at the fixture's
//! clock.

#![cfg(feature = "website")]

use std::path::Path;
use std::process::{Command, Output};

use adk_awp::conformance::fixture::{FixtureSite, fixture_time};
use adk_awp::signing::anchor::{root_record, root_record_name};
use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner, sign};
use awp_types::Profile;
use serde_json::{Value, json};

fn adk_rust(args: &[&str], home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_adk-rust"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("RUST_LOG", "off")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("adk-rust runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Serves `site` on loopback until the test ends; returns its origin.
async fn serve(site: &FixtureSite) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = site.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    origin
}

fn anchor(site: &FixtureSite) -> String {
    root_record(site.root.kid(), &site.root.public_key())
}

/// `awp verify example.com` against `origin`, pinned in `pins`, as JSON.
fn verify(origin: &str, anchor: &str, pins: &Path, home: &Path) -> (Option<i32>, Value) {
    let at = fixture_time().to_rfc3339();
    let pins = pins.display().to_string();
    let output = adk_rust(
        &[
            "awp",
            "verify",
            "example.com",
            "--origin",
            origin,
            "--anchor",
            anchor,
            "--at",
            &at,
            "--pins",
            &pins,
            "--json",
        ],
        home,
    );
    let report = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("not a JSON report:\n{}", text(&output)));
    (output.status.code(), report)
}

fn failure(report: &Value) -> &str {
    report["failure"]["name"].as_str().unwrap_or("pass")
}

/// S13: the first run passes and prints posture and readiness; a contract signed by a
/// foreign key, an altered A2A card, a changed page and a checkpoint that does not extend
/// the pinned one each fail naming the broken link.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_passes_on_the_fixture_and_names_each_s13_tampering() {
    let home = tempfile::tempdir().unwrap();
    let pins = home.path().join("pins.json");
    let site = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    let origin = serve(&site).await;
    let anchor = anchor(&site);

    let at = fixture_time().to_rfc3339();
    let pins_arg = pins.display().to_string();
    let output = adk_rust(
        &[
            "awp",
            "verify",
            "example.com",
            "--origin",
            &origin,
            "--anchor",
            &anchor,
            "--at",
            &at,
            "--pins",
            &pins_arg,
        ],
        home.path(),
    );
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    for expected in [
        "awp verify example.com: PASS",
        "release 2",
        "posture",
        "readiness",
        "A2A card      signed",
    ] {
        assert!(printed.contains(expected), "{expected:?} in:\n{printed}");
    }
    let pinned: Value = serde_json::from_slice(&std::fs::read(&pins).unwrap()).unwrap();
    assert_eq!(pinned["example.com"]["treeSize"], 2);

    // A contract signed by a foreign key.
    let mut foreign = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    let key = Ed25519DocumentSigner::from_seed("site-root-2026", [0x99; 32]).unwrap();
    let mut contract = foreign.contract.clone();
    contract.proof = None;
    contract.proof = Some(sign(&contract, &key).await.unwrap());
    foreign.set_json("/.well-known/awp.json", &serde_json::to_value(&contract).unwrap());
    let (code, report) =
        verify(&serve(&foreign).await, &anchor, &home.path().join("p1"), home.path());
    assert_eq!((code, failure(&report)), (Some(1), "contract.signature"), "{report:#}");

    // An altered A2A card.
    let mut altered = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    let mut card = altered.json("/.well-known/agent-card.json").unwrap();
    card["skills"][0]["description"] = json!("Search the site and place orders");
    altered.set_json("/.well-known/agent-card.json", &card);
    let (code, report) =
        verify(&serve(&altered).await, &anchor, &home.path().join("p2"), home.path());
    assert_eq!((code, failure(&report)), (Some(1), "card.signature"), "{report:#}");

    // A changed page.
    let mut defaced = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    defaced.files.get_mut("/about/index.html").unwrap().body = b"<html>defaced</html>".to_vec();
    let (code, report) =
        verify(&serve(&defaced).await, &anchor, &home.path().join("p3"), home.path());
    assert_eq!((code, failure(&report)), (Some(1), "page.digest"), "{report:#}");
    assert!(report["failure"]["detail"].as_str().unwrap().contains("/about/index.html"));

    // A checkpoint that does not extend the one pinned by the first run.
    let mut forked = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    forked.manifests[1].created_at = fixture_time() + chrono::Duration::hours(5);
    forked.publish_log().await.unwrap();
    let forked_origin = serve(&forked).await;
    let (code, report) = verify(&forked_origin, &anchor, &home.path().join("p4"), home.path());
    assert_eq!((code, failure(&report)), (Some(0), "pass"), "a fresh verifier has nothing pinned");
    let (code, report) = verify(&forked_origin, &anchor, &pins, home.path());
    assert_eq!((code, failure(&report)), (Some(1), "log.consistency"), "{report:#}");

    // Without the anchor the root is checked against DNS, where the fixture has no record.
    let missing = format!("v=awp2; kid={}; jkt=not-the-root", site.root.kid());
    let (code, report) = verify(&origin, &missing, &home.path().join("p5"), home.path());
    assert_eq!((code, failure(&report)), (Some(1), "root.unanchored"), "{report:#}");
    assert_eq!(root_record_name("example.com"), "_awp-root.example.com");
}

/// The conformance kit against a local deployment reads the domain from its contract; the
/// edge's active checks run only against loopback (A-I9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conformance_runs_the_kit_and_keeps_active_checks_local() {
    let home = tempfile::tempdir().unwrap();
    let site = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    let origin = serve(&site).await;
    let anchor = anchor(&site);
    let at = fixture_time().to_rfc3339();
    let output = adk_rust(
        &["awp", "conformance", &origin, "--anchor", &anchor, "--at", &at, "--json"],
        home.path(),
    );
    assert!(output.status.success(), "{}", text(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        (report["domain"].as_str(), report["passed"].as_bool()),
        (Some("example.com"), Some(true))
    );
    let ids: Vec<&str> =
        report["results"].as_array().unwrap().iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids, ["S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9"]);

    let mut defaced = FixtureSite::build("example.com", Profile::Static).await.unwrap();
    defaced.files.get_mut("/about/index.html").unwrap().body = b"<html>defaced</html>".to_vec();
    let output = adk_rust(
        &["awp", "conformance", &serve(&defaced).await, "--anchor", &anchor, "--at", &at],
        home.path(),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output).contains("S4   FAIL"), "{}", text(&output));

    let output = adk_rust(
        &["awp", "conformance", &origin, "--anchor", &anchor, "--at", &at, "--active"],
        home.path(),
    );
    assert!(output.status.success(), "{}", text(&output));
    let output = adk_rust(&["awp", "conformance", "https://example.com", "--active"], home.path());
    assert!(!output.status.success());
    assert!(text(&output).contains("A-I9"), "{}", text(&output));
}

/// A 1.x `business.toml` becomes site data and a draft charter, and nothing is overwritten.
#[test]
fn migrate_config_writes_site_data_and_a_draft_charter() {
    let home = tempfile::tempdir().unwrap();
    let mut context =
        awp_types::BusinessContext::core("Example Solar", "Solar for Nairobi homes", "example.com");
    context.capabilities = vec![awp_types::BusinessCapability {
        name: "place-order".into(),
        description: "Place an order".into(),
        endpoint: "/api/orders".into(),
        method: "POST".into(),
        access_level: awp_types::TrustLevel::Known,
        input_schema: None,
        output_schema: None,
    }];
    let business = home.path().join("business.toml");
    std::fs::write(&business, adk_awp::business_context_to_toml(&context).unwrap()).unwrap();
    let out = home.path().join("site");
    let args =
        ["awp", "migrate-config", business.to_str().unwrap(), "--out", out.to_str().unwrap()];
    let output = adk_rust(&args, home.path());
    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("Confirm during onboarding"), "{}", text(&output));

    let config = adk_site::content::SiteConfig::from_toml(
        &std::fs::read_to_string(out.join("site.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!((config.name.as_str(), config.domain.as_str()), ("Example Solar", "example.com"));
    let capabilities: Vec<awp_types::CapabilityV2> =
        serde_json::from_slice(&std::fs::read(out.join("capabilities.json")).unwrap()).unwrap();
    assert_eq!(capabilities[0].name, "place_order");
    assert_eq!(capabilities[0].effect, awp_types::CapabilityEffect::NonIdempotent);
    let charter: awp_types::Charter =
        serde_json::from_slice(&std::fs::read(out.join("charter.draft.json")).unwrap()).unwrap();
    assert_eq!((charter.version, charter.signed.is_none()), (0, true));
    let contract: awp_types::SiteContract =
        serde_json::from_slice(&std::fs::read(out.join("contract.draft.json")).unwrap()).unwrap();
    assert!(contract.proof.is_none(), "the contract stays unsigned until onboarding");
    assert!(out.join("public-terms.json").exists());

    let again = adk_rust(&args, home.path());
    assert!(!again.status.success());
    assert!(text(&again).contains("never overwrites"), "{}", text(&again));
}
