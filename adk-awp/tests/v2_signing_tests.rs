//! Detached JWS, keys, delegations, the DNS anchor, the site log and `StaticBundle` (spec 3.5
//! §7.2, §7.7, D-11, A-I5).

#![cfg(feature = "signing")]

use std::sync::Arc;

use adk_auth::signed_audit::merkle::root;
use adk_auth::signed_audit::{CheckpointBody, Note};
use adk_awp::signing::anchor::{
    AnchorError, FixedTxtResolver, RootRecord, check_root_anchor, root_record, root_record_name,
};
use adk_awp::signing::bundle::StaticBundle;
use adk_awp::signing::card::{sign_agent_card, verify_agent_card};
use adk_awp::signing::jws::{
    DocumentSigner, Ed25519DocumentSigner, ProofError, document_digest, jwk_thumbprint, sign,
    verify, verify_value,
};
use adk_awp::signing::keys::{
    Dated, KeyError, delegate, delegated_key, endorse, revoke, verify_key_set,
};
use adk_awp::signing::log::{
    checkpoint_verifier, entry_leaf, hash_tile_path, log_origin, signed_checkpoint, tiles,
};
use awp_types::{
    ConsentReceipt, Digest, KeyScope, KeySet, LogEntry, Profile, Proof, PublicTerms, Receipt,
    RetiredRoot, RootKey, SiteContract, SiteRef, SiteStatus, StreamEntry,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Value, json};

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, 9, 0, 0).unwrap()
}

fn signer(kid: &str, byte: u8) -> Ed25519DocumentSigner {
    Ed25519DocumentSigner::from_seed(kid, [byte; 32]).unwrap()
}

fn contract() -> SiteContract {
    let mut contract = SiteContract::new(SiteRef {
        id: uuid::Uuid::nil(),
        name: "Example Solar".into(),
        domain: "example.com".into(),
        languages: vec!["en".parse().unwrap()],
    });
    contract.profile = Some(Profile::Static);
    contract
}

fn key_set(root: &Ed25519DocumentSigner) -> KeySet {
    KeySet {
        root: RootKey {
            kid: root.kid().to_owned(),
            public_key: root.public_key(),
            endorsement: None,
        },
        delegations: vec![],
        retired_roots: vec![],
        revocations: vec![],
    }
}

/// Tampering with any member, the header or the key breaks the proof (AWP 2.0 §5.4).
#[tokio::test]
async fn proofs_detect_every_change() {
    let root = signer("site-root-2026", 1);
    let mut document = contract();
    let proof = sign(&document, &root).await.unwrap();
    document.proof = Some(proof.clone());
    assert!(verify(&document, &proof, &root.public_key()).is_ok());
    assert!(verify_value(&serde_json::to_value(&document).unwrap(), &root.public_key()).is_ok());

    // Member order and whitespace do not matter: JCS.
    let pretty = serde_json::to_string_pretty(&document).unwrap();
    let reparsed: Value = serde_json::from_str(&pretty).unwrap();
    assert!(verify_value(&reparsed, &root.public_key()).is_ok());

    let mut changed = document.clone();
    changed.site.domain = "example.org".into();
    assert_eq!(verify(&changed, &proof, &root.public_key()), Err(ProofError::Signature));
    let mut extra = serde_json::to_value(&document).unwrap();
    extra["futureMember"] = json!(true);
    assert_eq!(
        verify_value(&extra, &root.public_key()),
        Err(ProofError::Signature),
        "unknown members are signed too"
    );

    let foreign = signer("site-root-2026", 9);
    assert_eq!(verify(&document, &proof, &foreign.public_key()), Err(ProofError::Signature));

    let (header, signature) = proof.jws.split_once("..").unwrap();
    let with_payload = Proof::eddsa(&proof.kid, format!("{header}.e30.{signature}"));
    assert_eq!(verify(&document, &with_payload, &root.public_key()), Err(ProofError::NotDetached));
    let other_kid = Proof::eddsa("other", proof.jws.clone());
    assert!(matches!(
        verify(&document, &other_kid, &root.public_key()),
        Err(ProofError::Header(_))
    ));
    let crit = URL_SAFE_NO_PAD
        .encode(br#"{"alg":"EdDSA","kid":"site-root-2026","crit":["b64"],"b64":false}"#);
    let with_crit = Proof::eddsa(&proof.kid, format!("{crit}..{signature}"));
    assert!(matches!(
        verify(&document, &with_crit, &root.public_key()),
        Err(ProofError::Header(_))
    ));
    let wrong_alg = Proof { alg: "ES256".into(), ..proof.clone() };
    assert!(matches!(
        verify(&document, &wrong_alg, &root.public_key()),
        Err(ProofError::Header(_))
    ));
    assert_eq!(
        verify(&document, &Proof::eddsa(&proof.kid, "garbage"), &root.public_key()),
        Err(ProofError::Malformed)
    );
}

/// Keys never appear in `Debug` (A-I5).
#[test]
fn signers_do_not_print_their_keys() {
    let root = signer("site-root-2026", 0x5a);
    let debug = format!("{root:?}");
    assert!(debug.contains("site-root-2026"));
    assert!(!debug.contains("5a5a") && !debug.contains("90, 90"), "{debug}");
    let generated = Ed25519DocumentSigner::generate("fresh").unwrap();
    assert_ne!(generated.public_key(), signer("fresh", 0).public_key());
}

/// Delegations carry scopes and windows; revocations and windows are checked by date (AWP
/// 2.0 §5.3, §5.5).
#[tokio::test]
async fn delegations_scopes_windows_and_revocations() {
    let root = signer("root", 1);
    let release = signer("release", 2);
    let edge = signer("edge", 3);
    let mut keys = key_set(&root);
    keys.delegations = vec![
        delegate(&root, "release", release.public_key(), vec![KeyScope::Release], at(1), at(31))
            .await
            .unwrap(),
        delegate(
            &root,
            "edge",
            edge.public_key(),
            vec![KeyScope::Card, KeyScope::Receipt],
            at(1),
            at(31),
        )
        .await
        .unwrap(),
    ];
    verify_key_set(&keys).unwrap();
    assert!(delegated_key(&keys, "release", KeyScope::Release, Dated::Live(at(10))).is_ok());
    assert!(matches!(
        delegated_key(&keys, "release", KeyScope::Card, Dated::Live(at(10))),
        Err(KeyError::Scope { .. })
    ));
    assert!(matches!(
        delegated_key(&keys, "edge", KeyScope::Receipt, Dated::Durable(at(31) + Duration::days(1))),
        Err(KeyError::Window(_))
    ));
    assert!(matches!(
        delegated_key(&keys, "nobody", KeyScope::Card, Dated::Live(at(10))),
        Err(KeyError::Unknown(_))
    ));

    keys.revocations.push(revoke(&root, "edge", at(20)).await.unwrap());
    verify_key_set(&keys).unwrap();
    assert!(
        delegated_key(&keys, "edge", KeyScope::Receipt, Dated::Durable(at(19))).is_ok(),
        "durable before revokedAt"
    );
    assert!(matches!(
        delegated_key(&keys, "edge", KeyScope::Receipt, Dated::Durable(at(20))),
        Err(KeyError::Revoked(_))
    ));
    assert!(
        matches!(
            delegated_key(&keys, "edge", KeyScope::Card, Dated::Live(at(10))),
            Err(KeyError::Revoked(_))
        ),
        "every live document"
    );

    let mut widened = keys.clone();
    widened.delegations[0].scopes.push(KeyScope::Payments);
    assert!(matches!(verify_key_set(&widened), Err(KeyError::Delegation { .. })));
    let mut forged_revocation = keys.clone();
    forged_revocation.revocations[0] = revoke(&edge, "release", at(20)).await.unwrap();
    assert!(matches!(verify_key_set(&forged_revocation), Err(KeyError::Revocation { .. })));
}

/// A rotated root is endorsed by its predecessor; delegations the retired root signed still
/// verify (AWP 2.0 §5.7).
#[tokio::test]
async fn rotated_roots_keep_older_delegations_verifiable() {
    let old = signer("root-2025", 1);
    let new = signer("root-2026", 4);
    let edge = signer("edge", 3);
    let older = delegate(&old, "edge", edge.public_key(), vec![KeyScope::Receipt], at(1), at(31))
        .await
        .unwrap();
    let keys = KeySet {
        root: endorse(&old, &new).await.unwrap(),
        delegations: vec![older],
        retired_roots: vec![RetiredRoot {
            kid: "root-2025".into(),
            public_key: old.public_key(),
            retired_at: at(15),
        }],
        revocations: vec![],
    };
    verify_key_set(&keys).unwrap();
    let mut forged = keys.clone();
    forged.root.endorsement.as_mut().unwrap().previous = signer("root-2025", 7).public_key();
    assert!(matches!(verify_key_set(&forged), Err(KeyError::Endorsement(_))));
    let mut orphaned = keys.clone();
    orphaned.retired_roots.clear();
    assert!(matches!(verify_key_set(&orphaned), Err(KeyError::Delegation { .. })));
}

#[tokio::test]
async fn the_dns_record_anchors_the_root() {
    let root = signer("site-root-2026", 1);
    let record = root_record(root.kid(), &root.public_key());
    assert_eq!(RootRecord::parse(&record).unwrap().jkt, jwk_thumbprint(&root.public_key()));
    assert_eq!(
        RootRecord::parse(" jkt=x ; kid=k; v=awp2 ").unwrap(),
        RootRecord { kid: "k".into(), jkt: "x".into() }
    );
    assert!(RootRecord::parse("v=awp1; kid=k; jkt=x").is_none());
    assert_eq!(root_record_name("Example.COM."), "_awp-root.example.com");

    let resolver = FixedTxtResolver::default()
        .with("_awp-root.example.com", "v=spf1 -all")
        .with("_awp-root.example.com", &record)
        .authenticated();
    let anchored =
        check_root_anchor(&resolver, "example.com", root.kid(), &root.public_key()).await.unwrap();
    assert!(anchored.authenticated);
    let foreign = signer("site-root-2026", 9);
    assert!(matches!(
        check_root_anchor(&resolver, "example.com", foreign.kid(), &foreign.public_key()).await,
        Err(AnchorError::Unanchored { .. })
    ));
}

/// `StaticBundle` signs the contract with the root, links keys and charter on the domain,
/// carries the public terms' digest and is deterministic (spec 3.5 §7.2).
#[tokio::test]
async fn static_bundles_are_signed_linked_and_deterministic() {
    let root = signer("site-root-2026", 1);
    let keys = key_set(&root);
    let terms = PublicTerms { usage: Some("train-ai=n".into()), ..Default::default() };
    let first = StaticBundle::build(contract(), &keys, &terms, &root).await.unwrap();
    let second = StaticBundle::build(contract(), &keys, &terms, &root).await.unwrap();
    assert_eq!(first.files(), second.files());
    let contract = first.contract();
    assert_eq!(contract.keys.as_deref(), Some("https://example.com/.well-known/awp/keys.json"));
    let charter = contract.charter.as_ref().unwrap();
    assert_eq!(charter.public, "https://example.com/.well-known/awp/charter.json");
    let served_terms: Value =
        serde_json::from_slice(&first.files()["/.well-known/awp/charter.json"]).unwrap();
    assert_eq!(document_digest(&served_terms).unwrap(), charter.digest);
    let served: Value = serde_json::from_slice(&first.files()["/.well-known/awp.json"]).unwrap();
    assert!(verify_value(&served, &root.public_key()).is_ok());
    let served_keys: KeySet =
        serde_json::from_slice(&first.files()["/.well-known/awp/keys.json"]).unwrap();
    assert_eq!(served_keys, keys);

    let dir = tempfile::tempdir().unwrap();
    first.write_to(dir.path()).unwrap();
    assert!(dir.path().join(".well-known/awp/charter.json").exists());

    let impostor = signer("site-root-2026", 9);
    assert!(
        StaticBundle::build(self::contract(), &keys, &terms, &impostor).await.is_err(),
        "only the root signs"
    );
}

/// Agent Cards carry a JWS by a `card` key over the card without `signatures`.
#[tokio::test]
async fn agent_cards_are_signed_by_card_keys() {
    let root = signer("root", 1);
    let edge = signer("edge", 3);
    let mut keys = key_set(&root);
    keys.delegations.push(
        delegate(&root, "edge", edge.public_key(), vec![KeyScope::Card], at(1), at(31))
            .await
            .unwrap(),
    );
    let card =
        json!({ "name": "Example", "skills": [{ "id": "search_site", "description": "Search" }] });
    let signed = sign_agent_card(&card, &edge).await.unwrap();
    assert!(verify_agent_card(&signed, &keys, at(10)).is_ok());
    let mut altered = signed.clone();
    altered["skills"][0]["description"] = json!("Search and buy");
    assert!(verify_agent_card(&altered, &keys, at(10)).is_err());
    assert!(verify_agent_card(&card, &keys, at(10)).is_err(), "unsigned");
    assert!(
        verify_agent_card(&signed, &keys, at(31) + Duration::days(1)).is_err(),
        "expired delegation"
    );
}

/// The site log's tiles hash to the checkpoint, which a `release` key signs under the log
/// origin (AWP 2.0 §14.5).
#[tokio::test]
async fn site_log_tiles_and_checkpoint() {
    let release = signer("release", 2);
    let entries: Vec<LogEntry> = (1..=300)
        .map(|seq| LogEntry { seq, manifest: Digest::from_sha256_bytes([(seq % 251) as u8; 32]) })
        .collect();
    let files = tiles(&entries).unwrap();
    assert_eq!(files[&hash_tile_path(0, 0, 256)].len(), 256 * 32);
    assert_eq!(files[&hash_tile_path(0, 1, 44)].len(), 44 * 32);
    assert_eq!(files[&hash_tile_path(1, 0, 1)].len(), 32, "one complete subtree of 256 leaves");
    assert!(files.contains_key("tile/entries/000") && files.contains_key("tile/entries/001.p/44"));
    let leaves: Vec<_> = entries.iter().map(|e| entry_leaf(e).unwrap()).collect();
    let level0: Vec<u8> =
        [&files[&hash_tile_path(0, 0, 256)][..], &files[&hash_tile_path(0, 1, 44)][..]].concat();
    assert_eq!(level0, leaves.iter().flat_map(|h| h.0).collect::<Vec<u8>>());
    assert_eq!(files[&hash_tile_path(1, 0, 1)], root(&leaves[..256]).0.to_vec());

    let note_signer = Arc::new(release.note_signer(&log_origin("example.com")).unwrap());
    let checkpoint = signed_checkpoint("example.com", &entries, note_signer).await.unwrap();
    let note = Note::parse(&checkpoint).unwrap();
    let verifier = checkpoint_verifier("example.com", release.public_key_bytes()).unwrap();
    assert_eq!(note.verified_by(std::slice::from_ref(&verifier)).len(), 1);
    let body = CheckpointBody::parse(&note.text).unwrap();
    assert_eq!(
        (body.origin.as_str(), body.size, body.root_hash),
        ("example.com/.well-known/awp/log", 300, root(&leaves))
    );
    let wrong_name = Arc::new(release.note_signer("other.example/.well-known/awp/log").unwrap());
    assert!(signed_checkpoint("example.com", &entries, wrong_name).await.is_err());
}

fn example(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/awp-2.0/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The protocol repository's examples verify with this implementation: its key set, the
/// contract's root proof and the edge's `receipt`-key proofs (AWP 2.0 §5, examples/2.0).
#[test]
fn protocol_examples_verify() {
    let keys: KeySet = serde_json::from_value(example("key-set.json")).unwrap();
    verify_key_set(&keys).expect("the example key set verifies");
    for contract in ["site-contract.static.json", "site-contract.static-edge.json"] {
        let value = example(contract);
        verify_value(&value, &keys.root.public_key).unwrap_or_else(|e| panic!("{contract}: {e}"));
    }
    let receipt_proofs = [
        (
            "site-status.json",
            serde_json::from_value::<SiteStatus>(example("site-status.json")).unwrap().proof,
        ),
        (
            "consent-receipt.json",
            serde_json::from_value::<ConsentReceipt>(example("consent-receipt.json"))
                .unwrap()
                .proof,
        ),
        ("receipt.json", serde_json::from_value::<Receipt>(example("receipt.json")).unwrap().proof),
        (
            "stream-entry.json",
            serde_json::from_value::<StreamEntry>(example("stream-entry.json")).unwrap().proof,
        ),
    ];
    for (name, proof) in receipt_proofs {
        let key =
            delegated_key(&keys, &proof.kid, KeyScope::Receipt, Dated::Durable(at(25))).unwrap();
        verify_value(&example(name), key).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}
