//! `awp verify` over a fixture site: a pass, and a named failure for each S13 tampering
//! (spec 3.5 §5 S13, §7.7; AWP 2.0 §15).

#![cfg(feature = "verify")]

use std::sync::Arc;

use adk_awp::conformance::fixture::{FixtureSite, Served, fixture_time};
use adk_awp::signing::anchor::{FixedTxtResolver, root_record, root_record_name};
use adk_awp::signing::bundle::StaticBundle;
use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner, sign};
use adk_awp::verify::fetch::RouterFetcher;
use adk_awp::verify::pins::{InMemoryPinStore, PinStore};
use adk_awp::verify::{Verifier, VerifyOptions, VerifyReport};
use awp_types::{KeySet, Profile, PublicTerms, RootKey};
use serde_json::json;

async fn fixture() -> FixtureSite {
    FixtureSite::build("example.com", Profile::Static).await.unwrap()
}

async fn verify_with(
    site: &FixtureSite,
    resolver: FixedTxtResolver,
    pins: Arc<InMemoryPinStore>,
) -> VerifyReport {
    Verifier::new(Arc::new(RouterFetcher::new(site.router())), Arc::new(resolver), pins)
        .with_options(VerifyOptions { now: Some(fixture_time()), ..Default::default() })
        .verify("example.com")
        .await
}

async fn verify_site(site: &FixtureSite) -> VerifyReport {
    verify_with(site, site.resolver.clone(), Arc::new(InMemoryPinStore::default())).await
}

fn failure(report: &VerifyReport) -> &str {
    report.failure.as_ref().map_or("pass", |failure| failure.name)
}

/// The first run passes, samples every page and reports posture and readiness (S13).
#[tokio::test]
async fn a_signed_site_passes_and_is_pinned() {
    let site = fixture().await;
    let pins = Arc::new(InMemoryPinStore::default());
    let report = verify_with(&site, site.resolver.clone(), Arc::clone(&pins)).await;
    assert!(report.passed(), "{report:#?}");
    assert_eq!(report.release_seq, Some(2));
    assert!(report.sampled.contains(&"/about/index.html".to_owned()));
    assert!(!report.sampled.iter().any(|path| path.starts_with("/.well-known/awp/releases/")));
    let posture = &report.posture;
    assert_eq!(posture.profile, Some(Profile::Static));
    assert!(posture.server_code.is_empty());
    assert!(
        posture.a2a
            && posture.hsts
            && posture.csp
            && posture.security_txt
            && posture.robots_content_usage
    );
    assert_eq!(posture.usage.as_deref(), Some("train-ai=n, search=y"));
    let pin = pins.get("example.com").await.unwrap().unwrap();
    assert_eq!(pin.tree_size, 2);
    assert_eq!(pin.root_kid, "site-root-2026");
    assert!(
        verify_with(&site, site.resolver.clone(), pins).await.passed(),
        "a second run extends its pin"
    );
}

/// A contract signed by a foreign key fails at `contract.signature`.
#[tokio::test]
async fn a_foreign_signature_fails_the_contract() {
    let mut site = fixture().await;
    let foreign = Ed25519DocumentSigner::from_seed("site-root-2026", [0x99; 32]).unwrap();
    let mut contract = site.contract.clone();
    contract.proof = None;
    contract.proof = Some(sign(&contract, &foreign).await.unwrap());
    site.set_json("/.well-known/awp.json", &serde_json::to_value(&contract).unwrap());
    assert_eq!(failure(&verify_site(&site).await), "contract.signature");
}

/// A host attacker who replaces keys.json and re-signs everything is caught by the DNS
/// anchor (AWP 2.0 §5.6).
#[tokio::test]
async fn a_replaced_root_is_unanchored() {
    let mut site = fixture().await;
    let foreign = Ed25519DocumentSigner::from_seed("site-root-2026", [0x99; 32]).unwrap();
    let keys = KeySet {
        root: RootKey {
            kid: foreign.kid().into(),
            public_key: foreign.public_key(),
            endorsement: None,
        },
        ..site.keys.clone()
    };
    let bundle =
        StaticBundle::build(site.contract.clone(), &keys, &PublicTerms::default(), &foreign)
            .await
            .unwrap();
    for (path, bytes) in bundle.files() {
        site.files.insert(
            path.clone(),
            Served { content_type: "application/awp+json".into(), body: bytes.clone() },
        );
    }
    assert_eq!(failure(&verify_site(&site).await), "root.unanchored");
}

/// An altered A2A card fails at `card.signature`.
#[tokio::test]
async fn an_altered_card_fails() {
    let mut site = fixture().await;
    let mut card = site.json("/.well-known/agent-card.json").unwrap();
    card["skills"][0]["description"] = json!("Search the site and place orders");
    site.set_json("/.well-known/agent-card.json", &card);
    assert_eq!(failure(&verify_site(&site).await), "card.signature");
}

/// A changed page fails at `page.digest`.
#[tokio::test]
async fn a_changed_page_fails() {
    let mut site = fixture().await;
    site.files.get_mut("/about/index.html").unwrap().body = b"<html>defaced</html>".to_vec();
    let report = verify_site(&site).await;
    assert_eq!(failure(&report), "page.digest");
    assert!(report.failure.unwrap().detail.contains("/about/index.html"));
}

/// A checkpoint that does not extend the pinned one fails at `log.consistency`.
#[tokio::test]
async fn a_non_extending_checkpoint_fails() {
    let mut site = fixture().await;
    let pins = Arc::new(InMemoryPinStore::default());
    assert!(verify_with(&site, site.resolver.clone(), Arc::clone(&pins)).await.passed());
    // The host rewrites release 2 and republishes a log of the same size.
    site.manifests[1].created_at = fixture_time() + chrono::Duration::hours(5);
    site.publish_log().await.unwrap();
    assert!(verify_site(&site).await.passed(), "a fresh verifier has nothing pinned");
    let report = verify_with(&site, site.resolver.clone(), pins).await;
    assert_eq!(failure(&report), "log.consistency");
}

#[tokio::test]
async fn the_rest_of_the_chain_names_its_failures() {
    // The public terms changed without re-signing the contract.
    let mut site = fixture().await;
    let mut terms = site.json("/.well-known/awp/charter.json").unwrap();
    terms["usage"] = json!("train-ai=y");
    site.set_json("/.well-known/awp/charter.json", &terms);
    assert_eq!(failure(&verify_site(&site).await), "charter.digest");

    // A release carrying a PHP file.
    let mut site = fixture().await;
    let php =
        Served { content_type: "text/plain".into(), body: b"<?php system($_GET['c']);".to_vec() };
    site.manifests[1].files.insert(
        "/wp-content/uploads/shell.php".into(),
        awp_types::FileEntry {
            sha256: awp_types::Digest::from_sha256_bytes(sha2_digest(&php.body)),
            size: php.body.len() as u64,
            content_type: php.content_type.clone(),
        },
    );
    site.files.insert("/wp-content/uploads/shell.php".into(), php);
    site.publish_log().await.unwrap();
    let report = verify_site(&site).await;
    assert_eq!(failure(&report), "release.executable");
    assert!(report.posture.server_code.iter().any(|note| note.contains("shell.php")));

    // A tile that does not hash to the checkpoint.
    let mut site = fixture().await;
    site.files.get_mut("/.well-known/awp/log/tile/0/000.p/2").unwrap().body[0] ^= 1;
    assert_eq!(failure(&verify_site(&site).await), "log.consistency");

    // A checkpoint signed by a key with no release delegation.
    let mut site = fixture().await;
    let checkpoint = site.files["/.well-known/awp/log/checkpoint"].body.clone();
    let text = String::from_utf8(checkpoint).unwrap();
    let body_end = text.find("\n\n").unwrap() + 1;
    let edge = Arc::new(site.edge_key.note_signer("example.com/.well-known/awp/log").unwrap());
    let resigned = adk_auth::signed_audit::sign_note(&text[..body_end], &[edge]).await.unwrap();
    site.files.get_mut("/.well-known/awp/log/checkpoint").unwrap().body = resigned.into_bytes();
    assert_eq!(failure(&verify_site(&site).await), "checkpoint.signature");

    // The DNS record is missing.
    let site = fixture().await;
    let report =
        verify_with(&site, FixedTxtResolver::default(), Arc::new(InMemoryPinStore::default()))
            .await;
    assert_eq!(failure(&report), "root.unanchored");

    // The contract is not served as application/awp+json.
    let mut site = fixture().await;
    site.files.get_mut("/.well-known/awp.json").unwrap().content_type = "application/json".into();
    assert_eq!(failure(&verify_site(&site).await), "contract.invalid");
}

/// A new root endorsed by the pinned one is a rotation; one only DNS anchors is a recovery
/// (AWP 2.0 §5.7, §15.3).
#[tokio::test]
async fn root_changes_are_rotations_or_recoveries() {
    let site = fixture().await;
    let pins = Arc::new(InMemoryPinStore::default());
    assert!(verify_with(&site, site.resolver.clone(), Arc::clone(&pins)).await.passed());

    let mut recovered = fixture().await;
    let new_root = Ed25519DocumentSigner::from_seed("site-root-2027", [0x44; 32]).unwrap();
    let mut keys = recovered.keys.clone();
    keys.root = RootKey {
        kid: new_root.kid().into(),
        public_key: new_root.public_key(),
        endorsement: None,
    };
    for delegation in &mut keys.delegations {
        *delegation = adk_awp::signing::keys::delegate(
            &new_root,
            delegation.kid.clone(),
            delegation.public_key.clone(),
            delegation.scopes.clone(),
            delegation.not_before,
            delegation.not_after,
        )
        .await
        .unwrap();
    }
    let terms: PublicTerms =
        serde_json::from_value(recovered.json("/.well-known/awp/charter.json").unwrap()).unwrap();
    let bundle =
        StaticBundle::build(recovered.contract.clone(), &keys, &terms, &new_root).await.unwrap();
    for (path, bytes) in bundle.files() {
        recovered.files.insert(
            path.clone(),
            Served { content_type: "application/awp+json".into(), body: bytes.clone() },
        );
    }
    republish(&mut recovered, BUNDLE).await;
    // The card was signed under the old delegation, which the edge key still holds.
    let resolver = FixedTxtResolver::default().with(
        &root_record_name("example.com"),
        &root_record(new_root.kid(), &new_root.public_key()),
    );
    let report = verify_with(&recovered, resolver.clone(), Arc::clone(&pins)).await;
    assert_eq!(failure(&report), "pass", "{report:#?}");
    assert!(report.recovery, "no endorsement by the pinned root: a recovery");

    let mut rotated = recovered;
    let old_root = Ed25519DocumentSigner::from_seed(
        "site-root-2026",
        adk_awp::conformance::fixture::ROOT_SEED,
    )
    .unwrap();
    keys.root = adk_awp::signing::keys::endorse(&old_root, &new_root).await.unwrap();
    let bundle =
        StaticBundle::build(rotated.contract.clone(), &keys, &terms, &new_root).await.unwrap();
    for (path, bytes) in bundle.files() {
        rotated.files.insert(
            path.clone(),
            Served { content_type: "application/awp+json".into(), body: bytes.clone() },
        );
    }
    republish(&mut rotated, BUNDLE).await;
    let pins = Arc::new(InMemoryPinStore::default());
    assert!(verify_with(&site, site.resolver.clone(), Arc::clone(&pins)).await.passed());
    let report = verify_with(&rotated, resolver, pins).await;
    assert!(report.passed(), "{report:#?}");
    assert!(!report.recovery, "endorsed by the pinned root: a rotation");
}

/// Publishes a delta release carrying the current bytes of `paths`.
async fn republish(site: &mut FixtureSite, paths: &[&str]) {
    let last = site.manifests.last().unwrap().clone();
    let files = paths
        .iter()
        .map(|path| {
            let served = &site.files[*path];
            (
                (*path).to_owned(),
                awp_types::FileEntry {
                    sha256: awp_types::Digest::from_sha256_bytes(sha2_digest(&served.body)),
                    size: served.body.len() as u64,
                    content_type: served.content_type.clone(),
                },
            )
        })
        .collect();
    site.manifests.push(awp_types::ReleaseManifest {
        seq: last.seq + 1,
        previous: Some(adk_awp::signing::jws::document_digest(&last).unwrap()),
        base: Some(last.seq),
        files,
        removed: Default::default(),
        ..last
    });
    site.publish_log().await.unwrap();
}

const BUNDLE: &[&str] =
    &["/.well-known/awp.json", "/.well-known/awp/keys.json", "/.well-known/awp/charter.json"];

fn sha2_digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

/// Verifies a preview of `site` whose staged manifest has `candidate` as its digest.
async fn verify_preview(site: &FixtureSite, candidate: &awp_types::Digest) -> VerifyReport {
    Verifier::new(
        Arc::new(RouterFetcher::new(site.router())),
        Arc::new(site.resolver.clone()),
        Arc::new(InMemoryPinStore::default()),
    )
    .with_options(VerifyOptions { now: Some(fixture_time()), ..Default::default() })
    .verify_preview("example.com", candidate)
    .await
}

/// Serves `manifest` at its path, as a preview's host does for a release it stages but does
/// not log.
fn serve_manifest(site: &mut FixtureSite, manifest: &awp_types::ReleaseManifest) {
    site.files.insert(
        awp_types::v2::contract::release_manifest_path(manifest.seq),
        Served {
            content_type: "application/awp+json".to_owned(),
            body: serde_json::to_vec_pretty(manifest).unwrap(),
        },
    );
}

/// A preview of the next release passes `awp verify` against the digest its staging
/// recorded, on top of the logged releases; another digest, or a page that differs from the
/// preview's manifest, fails (spec 3.5 §7.8.1).
#[tokio::test]
async fn a_preview_verifies_against_its_staged_digest() {
    let mut site = fixture().await;
    let next = site.manifests.pop().unwrap();
    site.publish_log().await.unwrap();
    serve_manifest(&mut site, &next);
    let staged = adk_awp::signing::jws::document_digest(&next).unwrap();

    let report = verify_preview(&site, &staged).await;
    assert!(report.passed(), "{report:#?}");
    assert_eq!(report.release_seq, Some(2));
    assert!(report.sampled.contains(&"/about/index.html".to_owned()));

    let logged = adk_awp::signing::jws::document_digest(&site.manifests[0]).unwrap();
    assert_eq!(failure(&verify_preview(&site, &logged).await), "release.inclusion");

    // The published procedure does not take an unlogged release as the site's.
    let published = verify_site(&site).await;
    assert_ne!(published.release_seq, Some(2), "{published:#?}");

    site.files.get_mut("/about/index.html").unwrap().body = b"<p>defaced</p>".to_vec();
    assert_eq!(failure(&verify_preview(&site, &staged).await), "page.digest");
}

/// A preview of a site's first release verifies with no log at all; a preview never stands
/// in for a published site, so a verification of the same files fails at the checkpoint.
#[tokio::test]
async fn a_first_release_preview_verifies_without_a_log() {
    let mut site = fixture().await;
    site.files.retain(|path, _| {
        !path.starts_with("/.well-known/awp/releases/")
            && !path.starts_with("/.well-known/awp/log/")
    });
    let files = site
        .files
        .iter()
        .map(|(path, served)| {
            (
                path.clone(),
                awp_types::FileEntry {
                    sha256: awp_types::Digest::from_sha256_bytes(sha2_digest(&served.body)),
                    size: served.body.len() as u64,
                    content_type: served.content_type.clone(),
                },
            )
        })
        .collect();
    let first = awp_types::ReleaseManifest { files, ..site.manifests[0].clone() };
    serve_manifest(&mut site, &first);
    let staged = adk_awp::signing::jws::document_digest(&first).unwrap();

    let report = verify_preview(&site, &staged).await;
    assert!(report.passed(), "{report:#?}");
    assert_eq!(report.release_seq, Some(1));
    assert_eq!(failure(&verify_site(&site).await), "checkpoint.signature");

    let linked = awp_types::ReleaseManifest { previous: Some(staged.clone()), ..first };
    serve_manifest(&mut site, &linked);
    let digest = adk_awp::signing::jws::document_digest(&linked).unwrap();
    assert_eq!(failure(&verify_preview(&site, &digest).await), "release.manifest");
}
