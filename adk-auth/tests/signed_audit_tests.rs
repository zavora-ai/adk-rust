//! Signed audit log: tamper detection, key rotation, restarts, anchors, and interop
//! with Go's `golang.org/x/mod/sumdb/note`.

#![cfg(feature = "signed-audit")]

use std::sync::Arc;
use std::time::Duration;

use adk_auth::signed_audit::merkle::{leaf_hash, root, verify_consistency, verify_inclusion};
use adk_auth::signed_audit::{
    AuditExport, AuditLog, AuditSigner, CheckpointBody, Ed25519Signer, FileCheckpointAnchor, Note,
    NoteVerifier, VerifyFailureKind, VerifyReport, sign_note,
};
use adk_auth::{AuditEvent, AuditOutcome, AuditSink};
use adk_core::ManualClock;
use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditLogStore, AuditRecordDraft, AuditRecordKind,
    AuditRecordOutcome, InMemoryAuditLogStore,
};
use adk_core::principal::{ActingContext, AgentId, AgentPrincipal, SubjectRef};
use chrono::{TimeZone, Utc};
use serde_json::json;

const LOG_ID: &str = "replica-1";

fn signer(name: &str, seed: u8) -> Arc<Ed25519Signer> {
    Arc::new(Ed25519Signer::from_seed(name, [seed; 32]).unwrap())
}

fn trusted(signer: &Ed25519Signer) -> NoteVerifier {
    signer.verifier_key().parse().unwrap()
}

fn clock() -> ManualClock {
    ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 11, 9, 0, 0).unwrap())
}

fn acting() -> ActingContext {
    let agent = AgentPrincipal::new(AgentId::new("urn:adk:agent:billing").unwrap())
        .with_tenant("acme")
        .with_instance(LOG_ID);
    ActingContext::new(agent).on_behalf_of(SubjectRef::human("alice"))
}

fn draft(n: u64) -> AuditRecordDraft {
    AuditRecordDraft::new(AuditRecordKind::ToolCall, "transfer")
        .actor(AuditActor::new(&acting(), "inv-1"))
        .args_digest(
            adk_auth::signed_audit::json_sha256(&json!({ "amount": n, "to": "acct-1" })).unwrap(),
        )
        .decision(AuditDecision::Allowed)
        .outcome(AuditRecordOutcome::Succeeded)
        .policy_ref("policy:v3#transfer")
        .ext("note", json!(format!("call {n} — ünïcode")))
}

async fn open(
    store: Arc<dyn AuditLogStore>,
    signer: Arc<Ed25519Signer>,
    clock: &ManualClock,
) -> AuditLog {
    AuditLog::builder(store, LOG_ID, signer)
        .origin("example.com/adk-audit/replica-1")
        .clock(Arc::new(clock.clone()))
        .checkpoint_every(4)
        .open()
        .await
        .unwrap()
}

/// Ten records with checkpoints at 4, 8, and 10.
async fn ten_record_log() -> (AuditExport, Arc<Ed25519Signer>) {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let key = signer("audit.example.com", 1);
    let clock = clock();
    let log = open(store.clone(), key.clone(), &clock).await;
    for n in 0..10 {
        log.append(draft(n)).await.unwrap();
        clock.advance(Duration::from_secs(1));
    }
    log.checkpoint().await.unwrap();
    (AuditExport::from_store(store.as_ref(), LOG_ID).await.unwrap(), key)
}

fn kinds(report: &VerifyReport) -> Vec<VerifyFailureKind> {
    report.failures.iter().map(|failure| failure.kind).collect()
}

#[tokio::test]
async fn an_untouched_log_verifies() {
    let (export, key) = ten_record_log().await;
    assert_eq!(export.checkpoints.iter().map(|c| c.size).collect::<Vec<_>>(), vec![4, 8, 10]);
    let report = export.verify(&[trusted(&key)]);
    assert!(report.is_valid(), "{report}");
    assert_eq!(report.records, 10);
    assert_eq!(report.checkpoints_verified, 3);
    assert_eq!(report.verified_size, 10);
    assert_eq!(report.unsigned_tail, 0);
    assert_eq!(report.log_id.as_deref(), Some(LOG_ID));
    assert_eq!(report.origin.as_deref(), Some("example.com/adk-audit/replica-1"));

    let reparsed = AuditExport::from_jsonl(&export.to_jsonl()).unwrap();
    assert_eq!(reparsed, export);
}

#[tokio::test]
async fn a_flipped_byte_fails() {
    let (mut export, key) = ten_record_log().await;
    let record = export.entries[5].record.as_mut().unwrap();
    *record = record.replacen("transfer", "transfeR", 1);
    let report = export.verify(&[trusted(&key)]);
    let kinds = kinds(&report);
    assert!(kinds.contains(&VerifyFailureKind::LeafHashMismatch), "{report}");
    assert!(kinds.contains(&VerifyFailureKind::RootMismatch), "{report}");
}

#[tokio::test]
async fn a_rehashed_edit_breaks_the_chain_and_the_root() {
    let (mut export, key) = ten_record_log().await;
    let entry = &mut export.entries[5];
    let edited = entry.record.as_ref().unwrap().replacen("alice", "mallory", 1);
    entry.leaf_hash = leaf_hash(edited.as_bytes());
    entry.record = Some(edited);
    let report = export.verify(&[trusted(&key)]);
    let kinds = kinds(&report);
    assert!(kinds.contains(&VerifyFailureKind::ChainBroken), "{report}");
    assert!(kinds.contains(&VerifyFailureKind::RootMismatch), "{report}");
    assert!(!kinds.contains(&VerifyFailureKind::LeafHashMismatch), "{report}");
}

#[tokio::test]
async fn a_deleted_record_fails() {
    let (mut export, key) = ten_record_log().await;
    export.entries.remove(3);
    let kinds = kinds(&export.verify(&[trusted(&key)]));
    assert!(kinds.contains(&VerifyFailureKind::SequenceBroken));
    assert!(kinds.contains(&VerifyFailureKind::ChainBroken));
    assert!(kinds.contains(&VerifyFailureKind::RootMismatch));
}

#[tokio::test]
async fn reordered_records_fail() {
    let (mut export, key) = ten_record_log().await;
    export.entries.swap(2, 3);
    let kinds = kinds(&export.verify(&[trusted(&key)]));
    assert!(kinds.contains(&VerifyFailureKind::SequenceBroken));
    assert!(kinds.contains(&VerifyFailureKind::ChainBroken));
    assert!(kinds.contains(&VerifyFailureKind::RootMismatch));
}

#[tokio::test]
async fn truncation_below_a_checkpoint_fails() {
    let (mut export, key) = ten_record_log().await;
    export.entries.truncate(7);
    let report = export.verify(&[trusted(&key)]);
    assert_eq!(kinds(&report), vec![VerifyFailureKind::Truncated, VerifyFailureKind::Truncated]);
    assert_eq!(report.verified_size, 4);
}

#[tokio::test]
async fn records_after_the_last_checkpoint_are_reported_unsigned() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let key = signer("audit.example.com", 1);
    let log = open(store.clone(), key.clone(), &clock()).await;
    for n in 0..6 {
        log.append(draft(n)).await.unwrap();
    }
    let report =
        AuditExport::from_store(store.as_ref(), LOG_ID).await.unwrap().verify(&[trusted(&key)]);
    assert!(report.is_valid(), "{report}");
    assert_eq!((report.verified_size, report.unsigned_tail), (4, 2));
}

#[tokio::test]
async fn a_log_without_checkpoints_is_unanchored() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let key = signer("audit.example.com", 1);
    let log = open(store.clone(), key.clone(), &clock()).await;
    log.append(draft(0)).await.unwrap();
    let report =
        AuditExport::from_store(store.as_ref(), LOG_ID).await.unwrap().verify(&[trusted(&key)]);
    assert_eq!(kinds(&report), vec![VerifyFailureKind::Unanchored]);
}

#[tokio::test]
async fn a_fork_under_the_same_key_fails() {
    let key = signer("audit.example.com", 1);
    let clock = clock();
    let honest_store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let forked_store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let honest = open(honest_store.clone(), key.clone(), &clock).await;
    let forked = open(forked_store.clone(), key.clone(), &clock).await;
    for n in 0..8 {
        honest.append(draft(n)).await.unwrap();
        forked.append(draft(if n == 5 { 500 } else { n })).await.unwrap();
    }
    let honest = AuditExport::from_store(honest_store.as_ref(), LOG_ID).await.unwrap();
    let forked = AuditExport::from_store(forked_store.as_ref(), LOG_ID).await.unwrap();

    let mut both = honest.clone();
    both.checkpoints.extend(forked.checkpoints.clone());
    let kinds_both = kinds(&both.verify(&[trusted(&key)]));
    assert!(kinds_both.contains(&VerifyFailureKind::Fork), "{kinds_both:?}");

    let mut swapped = forked.clone();
    swapped.checkpoints = honest.checkpoints;
    assert!(kinds(&swapped.verify(&[trusted(&key)])).contains(&VerifyFailureKind::RootMismatch));
}

#[tokio::test]
async fn untrusted_or_damaged_signatures_fail() {
    let (mut export, _) = ten_record_log().await;
    let stranger = signer("audit.example.com", 2);
    let report = export.verify(&[trusted(&stranger)]);
    assert!(kinds(&report).iter().all(|kind| *kind == VerifyFailureKind::UntrustedCheckpoint));
    assert_eq!(report.checkpoints_verified, 0);

    let key = signer("audit.example.com", 1);
    let note = &mut export.checkpoints[1].note;
    let at = note.rfind(' ').unwrap() + 10;
    let flipped = if &note[at..=at] == "A" { "B" } else { "A" };
    note.replace_range(at..=at, flipped);
    let report = export.verify(&[trusted(&key)]);
    assert_eq!(kinds(&report), vec![VerifyFailureKind::UntrustedCheckpoint]);
}

#[tokio::test]
async fn pruned_records_keep_the_log_verifiable() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let key = signer("audit.example.com", 1);
    let log = open(store.clone(), key.clone(), &clock()).await;
    for n in 0..8 {
        log.append(draft(n)).await.unwrap();
    }
    assert_eq!(store.prune(LOG_ID, 5).await.unwrap(), 5);
    let report =
        AuditExport::from_store(store.as_ref(), LOG_ID).await.unwrap().verify(&[trusted(&key)]);
    assert!(report.is_valid(), "{report}");
    assert_eq!((report.pruned, report.verified_size), (5, 8));
}

#[tokio::test]
async fn key_rotation_is_dual_signed_and_learned() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let old = signer("audit.example.com", 1);
    let new = signer("audit.example.com", 3);
    let log = open(store.clone(), old.clone(), &clock()).await;
    for n in 0..5 {
        log.append(draft(n)).await.unwrap();
    }
    let rotation = log
        .rotate_signer(new.clone(), AuditActor::subject(SubjectRef::human("secops")))
        .await
        .unwrap();
    assert_eq!(rotation.size, 6);
    assert_eq!(Note::parse(&rotation.note).unwrap().signatures.len(), 2);
    assert_eq!(log.verifier_key().await, new.verifier_key());
    for n in 5..9 {
        log.append(draft(n)).await.unwrap();
    }
    log.checkpoint().await.unwrap();

    let export = AuditExport::from_store(store.as_ref(), LOG_ID).await.unwrap();
    let report = export.verify(&[trusted(&old)]);
    assert!(report.is_valid(), "{report}");
    assert_eq!(report.learned_keys, vec![new.verifier_key()]);
    assert_eq!(report.verified_size, 10);

    // A checkpoint past the rotation signed only by the retired key is refused.
    let mut retired = export.clone();
    let body = CheckpointBody {
        origin: "example.com/adk-audit/replica-1".into(),
        size: 10,
        root_hash: export.checkpoints.last().unwrap().root_hash,
    };
    let signers: Vec<Arc<dyn AuditSigner>> = vec![old.clone()];
    retired.checkpoints.last_mut().unwrap().note =
        sign_note(&body.to_text(), &signers).await.unwrap();
    assert!(
        kinds(&retired.verify(&[trusted(&old)])).contains(&VerifyFailureKind::UntrustedCheckpoint)
    );

    // The rotation checkpoint signed by the new key alone does not hand over trust.
    let mut single = export.clone();
    let rotation_body = CheckpointBody::parse(&Note::parse(&rotation.note).unwrap().text).unwrap();
    let signers: Vec<Arc<dyn AuditSigner>> = vec![new.clone()];
    let at = single.checkpoints.iter().position(|c| c.size == 6).unwrap();
    single.checkpoints[at].note = sign_note(&rotation_body.to_text(), &signers).await.unwrap();
    let kinds = kinds(&single.verify(&[trusted(&old)]));
    assert!(kinds.contains(&VerifyFailureKind::UntrustedCheckpoint), "{kinds:?}");
}

#[tokio::test]
async fn a_reopened_log_continues_the_chain() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let key = signer("audit.example.com", 1);
    let clock = clock();
    {
        let log = open(store.clone(), key.clone(), &clock).await;
        for n in 0..6 {
            log.append(draft(n)).await.unwrap();
        }
    }
    let log = open(store.clone(), key.clone(), &clock).await;
    assert_eq!(log.size().await, 6);
    let record = log.append(draft(6)).await.unwrap();
    assert_eq!(record.seq, 6);
    log.checkpoint().await.unwrap();
    let report =
        AuditExport::from_store(store.as_ref(), LOG_ID).await.unwrap().verify(&[trusted(&key)]);
    assert!(report.is_valid(), "{report}");
    assert_eq!(report.verified_size, 7);
}

#[tokio::test]
async fn a_second_writer_on_one_log_id_is_refused() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let key = signer("audit.example.com", 1);
    let clock = clock();
    let first = open(store.clone(), key.clone(), &clock).await;
    let second = open(store.clone(), key.clone(), &clock).await;
    first.append(draft(0)).await.unwrap();
    let error = second.append(draft(1)).await.unwrap_err();
    assert_eq!(error.code, "audit_log.seq_conflict");
}

#[tokio::test]
async fn store_backed_proofs_verify() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let key = signer("audit.example.com", 1);
    let log = open(store.clone(), key, &clock()).await;
    for n in 0..13 {
        log.append(draft(n)).await.unwrap();
    }
    let leaves: Vec<_> =
        store.read(LOG_ID, 0..13).await.unwrap().iter().map(|e| e.leaf_hash).collect();
    for size in 1..=13 {
        let size_root = log.root_at(size).await.unwrap();
        assert_eq!(size_root, root(&leaves[..size as usize]));
        for seq in 0..size {
            let proof = log.inclusion_proof(seq, size).await.unwrap();
            verify_inclusion(seq, size, &leaves[seq as usize], &proof, &size_root).unwrap();
        }
        for old in 0..=size {
            let proof = log.consistency_proof(old, size).await.unwrap();
            verify_consistency(old, size, &log.root_at(old).await.unwrap(), &size_root, &proof)
                .unwrap();
        }
    }
    assert_eq!(log.inclusion_proof(3, 14).await.unwrap_err().code, "audit_log.out_of_range");
}

#[tokio::test]
async fn checkpoints_follow_the_interval() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let clock = clock();
    let log = AuditLog::builder(store.clone(), LOG_ID, signer("audit.example.com", 1))
        .clock(Arc::new(clock.clone()))
        .checkpoint_every(1_000)
        .checkpoint_interval(Duration::from_secs(60))
        .open()
        .await
        .unwrap();
    log.append(draft(0)).await.unwrap();
    assert_eq!(log.checkpoint_if_due().await.unwrap(), None);
    clock.advance(Duration::from_secs(61));
    log.append(draft(1)).await.unwrap();
    assert_eq!(store.latest_checkpoint(LOG_ID).await.unwrap().map(|c| c.size), Some(2));
    log.append(draft(2)).await.unwrap();
    clock.advance(Duration::from_secs(61));
    assert_eq!(log.checkpoint_if_due().await.unwrap().map(|c| c.size), Some(3));
    assert_eq!(log.checkpoint_if_due().await.unwrap(), None);
}

#[tokio::test]
async fn audit_sink_events_land_in_the_signed_log() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let key = signer("audit.example.com", 1);
    let log = open(store.clone(), key.clone(), &clock()).await;
    let sink: &dyn AuditSink = &log;
    sink.log(AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed).with_session("s-1"))
        .await
        .unwrap();
    sink.log_batch(vec![
        AuditEvent::tool_access("bob", "exec", AuditOutcome::Denied),
        AuditEvent::config_changed("carol", "limits", AuditOutcome::Updated),
    ])
    .await
    .unwrap();
    sink.flush().await.unwrap();

    let export = AuditExport::from_store(store.as_ref(), LOG_ID).await.unwrap();
    assert!(export.verify(&[trusted(&key)]).is_valid());
    let first: serde_json::Value =
        serde_json::from_str(export.entries[0].record.as_deref().unwrap()).unwrap();
    assert_eq!(first["kind"], "access");
    assert_eq!(first["decision"], "allowed");
    assert_eq!(first["actor"]["onBehalfOf"], json!({ "kind": "human", "id": "alice" }));
    assert_eq!(first["ext"]["sessionId"], "s-1");
}

#[tokio::test]
async fn an_anchored_checkpoint_exposes_a_rewritten_log() {
    let dir = std::env::temp_dir().join(format!("adk-audit-anchor-{}", uuid_like()));
    std::fs::create_dir_all(&dir).unwrap();
    let anchor_path = dir.join("checkpoints.jsonl");
    let key = signer("audit.example.com", 1);
    let clock = clock();
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let log = AuditLog::builder(store.clone(), LOG_ID, key.clone())
        .origin("example.com/adk-audit/replica-1")
        .clock(Arc::new(clock.clone()))
        .checkpoint_every(4)
        .anchor(Arc::new(FileCheckpointAnchor::new(&anchor_path)))
        .open()
        .await
        .unwrap();
    for n in 0..8 {
        log.append(draft(n)).await.unwrap();
    }
    let anchored = FileCheckpointAnchor::read(&anchor_path).unwrap();
    assert_eq!(anchored.iter().map(|c| c.size).collect::<Vec<_>>(), vec![4, 8]);

    // The operator rewrites history in a fresh store and re-signs it.
    let rewritten: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let rewrite = open(rewritten.clone(), key.clone(), &clock).await;
    for n in 0..8 {
        rewrite.append(draft(n + 100)).await.unwrap();
    }
    let mut export = AuditExport::from_store(rewritten.as_ref(), LOG_ID).await.unwrap();
    assert!(export.verify(&[trusted(&key)]).is_valid());
    export.checkpoints.extend(anchored);
    let kinds = kinds(&export.verify(&[trusted(&key)]));
    assert!(kinds.contains(&VerifyFailureKind::Fork), "{kinds:?}");
    std::fs::remove_dir_all(dir).ok();
}

fn uuid_like() -> String {
    format!("{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap_or_default())
}

/// A checkpoint over the transparency-dev reference tree, signed by Go's
/// `golang.org/x/mod/sumdb/note` with the `TestSign` key. Ed25519 is deterministic, so
/// signing the same text here must give the same bytes.
#[tokio::test]
async fn checkpoint_matches_go_sumdb_note() {
    const GO_NOTE: &str = "example.com/adk-audit/replica-1\n8\nXcnaeacGWamtVZy3Ad7ZoqudgjqtL0lgz+Nw7/RgQyg=\n\n\
         — PeterNeumann x08go5mWMFHOIKtNRFhcY0iEBRakYc9IJniKZ36GfQTo6XjEjElhHyZpOI3Kga7I3EBikW9zc/eLXCHRHin6aj2cGwk=\n";
    let peter = Ed25519Signer::from_note_key(
        "PRIVATE+KEY+PeterNeumann+c74f20a3+AYEKFALVFGyNhPJEMzD1QIDr+Y7hfZx09iUvxdXHKDFz",
    )
    .unwrap();
    let verifier: NoteVerifier =
        "PeterNeumann+c74f20a3+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW".parse().unwrap();

    let leaves: Vec<_> = [
        &b""[..],
        &[0x00],
        &[0x10],
        &[0x20, 0x21],
        &[0x30, 0x31],
        &[0x40, 0x41, 0x42, 0x43],
        &[0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57],
        &[
            0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d,
            0x6e, 0x6f,
        ],
    ]
    .iter()
    .map(|data| leaf_hash(data))
    .collect();
    let body = CheckpointBody {
        origin: "example.com/adk-audit/replica-1".into(),
        size: 8,
        root_hash: root(&leaves),
    };
    let signers: Vec<Arc<dyn AuditSigner>> = vec![Arc::new(peter)];
    assert_eq!(sign_note(&body.to_text(), &signers).await.unwrap(), GO_NOTE);

    let note = Note::parse(GO_NOTE).unwrap();
    assert_eq!(note.verified_by(std::slice::from_ref(&verifier)).len(), 1);
    assert_eq!(CheckpointBody::parse(&note.text).unwrap(), body);
}
