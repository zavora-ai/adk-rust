//! The site log (spec 3.5 §7.7, D-11, A-I4, A-I5; AWP 2.0 §14.5): `{seq, manifest}` entries
//! at leaf `seq − 1`, verified from published C2SP tiles alone; inclusion and consistency
//! proofs verify; a tampered manifest, a foreign key or a rewritten log fails.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use adk_auth::signed_audit::NoteVerifier;
use adk_core::audit_log::{AuditLogStore, InMemoryAuditLogStore};
use adk_site::log::{
    CHECKPOINT_PATH, LOG_PREFIX, SiteLog, TileFetch, TileProofs, entry_jcs, verify_checkpoint,
    verify_checkpoint_extends, verify_release_inclusion,
};
use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
use async_trait::async_trait;
use awp_types::{Digest, LogEntry};
use bytes::Bytes;
use proptest::prelude::*;

const DOMAIN: &str = "example.com";

/// A published site: what a verifier fetches over HTTP.
#[derive(Default)]
struct Published(Mutex<BTreeMap<String, Bytes>>);

impl Published {
    fn put(&self, path: &str, bytes: Bytes) {
        self.0.lock().unwrap().insert(path.to_string(), bytes);
    }
    fn get(&self, path: &str) -> Option<Bytes> {
        self.0.lock().unwrap().get(path).cloned()
    }
    fn checkpoint(&self) -> String {
        String::from_utf8(self.get(CHECKPOINT_PATH).unwrap().to_vec()).unwrap()
    }
}

#[async_trait]
impl TileFetch for Published {
    async fn fetch_tile(&self, path: &str) -> adk_site::Result<Option<Bytes>> {
        Ok(self.get(&format!("{LOG_PREFIX}/{path}")))
    }
}

fn digest(seq: u64) -> Digest {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seq.to_be_bytes());
    Digest::from_sha256_bytes(bytes)
}

fn signer(seed: u8) -> (Arc<Ed25519ReleaseSigner>, NoteVerifier) {
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed(DOMAIN, [seed; 32]).unwrap());
    let verifier = signer.verifier_key().parse().unwrap();
    (signer, verifier)
}

async fn open(seed: u8) -> (SiteLog, NoteVerifier) {
    let (signer, verifier) = signer(seed);
    let log = SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), DOMAIN, signer).await.unwrap();
    (log, verifier)
}

/// Appends releases `from..from + count`, publishing tiles and the checkpoint after each.
async fn grow(log: &SiteLog, published: &Published, from: u64, count: u64, salt: u64) {
    for seq in from..from + count {
        let before = log.size().await;
        log.append(&LogEntry { seq, manifest: digest(seq + salt) }).await.unwrap();
        let publication = log.publication(before).await.unwrap();
        let files: Vec<_> = publication.files().cloned().collect();
        assert_eq!(files.last().unwrap().0, CHECKPOINT_PATH, "the checkpoint is published last");
        for (path, bytes) in files {
            published.put(&path, bytes);
        }
    }
}

#[tokio::test]
async fn every_release_verifies_from_published_tiles() {
    let (log, verifier) = open(7).await;
    let published = Published::default();
    grow(&log, &published, 1, 300, 0).await;
    let note = published.checkpoint();
    assert!(note.starts_with("example.com/.well-known/awp/log\n300\n"));
    assert!(
        note.contains("\u{2014} example.com/.well-known/awp/log "),
        "the key name is the origin"
    );
    let checkpoint = verify_checkpoint(&note, DOMAIN, std::slice::from_ref(&verifier)).unwrap();
    assert_eq!(checkpoint.size, 300, "the tree size is the latest release");
    let proofs = TileProofs::new(&published, checkpoint.size);
    assert_eq!(proofs.root().await.unwrap(), checkpoint.root_hash);
    for seq in [1, 2, 128, 256, 257, 300] {
        let entry = proofs.entry(seq - 1).await.unwrap();
        assert_eq!(
            entry,
            entry_jcs(&LogEntry { seq, manifest: digest(seq) }).unwrap(),
            "leaf seq − 1 holds release seq"
        );
        let proof = proofs.inclusion_proof(seq - 1).await.unwrap();
        assert_eq!(
            proof,
            log.inclusion_proof(seq, 300).await.unwrap(),
            "tile proof equals store proof"
        );
        verify_release_inclusion(&checkpoint, &entry, &proof, seq, &digest(seq)).unwrap();
    }
    assert_eq!(log.entry(42).await.unwrap(), Some(LogEntry { seq: 42, manifest: digest(42) }));
}

#[tokio::test]
async fn releases_append_without_gaps_and_survive_reopening() {
    let store: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
    let (key, _) = signer(5);
    let log = SiteLog::open(store.clone(), DOMAIN, key.clone()).await.unwrap();
    assert!(
        log.append(&LogEntry { seq: 2, manifest: digest(2) }).await.is_err(),
        "release 1 comes first"
    );
    log.append(&LogEntry { seq: 1, manifest: digest(1) }).await.unwrap();
    assert!(log.append(&LogEntry { seq: 1, manifest: digest(1) }).await.is_err(), "no duplicates");
    let reopened = SiteLog::open(store, DOMAIN, key).await.unwrap();
    assert_eq!(reopened.size().await, 1);
    reopened.append(&LogEntry { seq: 2, manifest: digest(2) }).await.unwrap();
    let foreign = Arc::new(Ed25519ReleaseSigner::from_seed("other.org", [5; 32]).unwrap());
    assert!(SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), DOMAIN, foreign).await.is_err());
}

#[tokio::test]
async fn a_tampered_manifest_or_foreign_key_fails() {
    let (log, verifier) = open(9).await;
    let published = Published::default();
    grow(&log, &published, 1, 5, 0).await;
    let note = published.checkpoint();
    let checkpoint = verify_checkpoint(&note, DOMAIN, std::slice::from_ref(&verifier)).unwrap();
    let proofs = TileProofs::new(&published, checkpoint.size);
    let entry = proofs.entry(2).await.unwrap();
    let proof = proofs.inclusion_proof(2).await.unwrap();

    let changed =
        verify_release_inclusion(&checkpoint, &entry, &proof, 3, &digest(99)).unwrap_err();
    assert!(changed.to_string().contains("served manifest"), "{changed}");

    let forged = entry_jcs(&LogEntry { seq: 3, manifest: digest(99) }).unwrap();
    let forged =
        verify_release_inclusion(&checkpoint, &forged, &proof, 3, &digest(99)).unwrap_err();
    assert!(forged.to_string().contains("not in the log"), "{forged}");

    let (_, stranger) = signer(10);
    assert!(verify_checkpoint(&note, DOMAIN, &[stranger]).is_err());
    assert!(
        verify_checkpoint(&note, "example.org", std::slice::from_ref(&verifier)).is_err(),
        "origin is checked"
    );
    let edited = note.replacen("\n5\n", "\n6\n", 1);
    assert!(verify_checkpoint(&edited, DOMAIN, std::slice::from_ref(&verifier)).is_err());
}

#[tokio::test]
async fn checkpoints_extend_and_a_rewritten_log_does_not() {
    let (log, verifier) = open(11).await;
    let published = Published::default();
    grow(&log, &published, 1, 3, 0).await;
    let old = verify_checkpoint(&published.checkpoint(), DOMAIN, std::slice::from_ref(&verifier))
        .unwrap();
    grow(&log, &published, 4, 6, 0).await;
    let new = verify_checkpoint(&published.checkpoint(), DOMAIN, std::slice::from_ref(&verifier))
        .unwrap();
    let proof = TileProofs::new(&published, new.size).consistency_proof(old.size).await.unwrap();
    verify_checkpoint_extends(&old, &new, &proof).unwrap();

    // The same key over a different history: its checkpoint does not extend the pinned one.
    let (rewritten, _) = open(11).await;
    let other = Published::default();
    grow(&rewritten, &other, 1, 9, 1000).await;
    let forged =
        verify_checkpoint(&other.checkpoint(), DOMAIN, std::slice::from_ref(&verifier)).unwrap();
    let proof = TileProofs::new(&other, forged.size).consistency_proof(old.size).await.unwrap();
    assert!(verify_checkpoint_extends(&old, &forged, &proof).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// For any two sizes, the consistency proof built from tiles verifies, and any release's
    /// inclusion proof verifies against the larger checkpoint.
    #[test]
    fn tile_proofs_verify_for_any_sizes(old in 1u64..40, extra in 0u64..40, probe in 0u64..80) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let (log, verifier) = open(3).await;
            let published = Published::default();
            grow(&log, &published, 1, old, 0).await;
            let first = verify_checkpoint(&published.checkpoint(), DOMAIN, std::slice::from_ref(&verifier)).unwrap();
            grow(&log, &published, old + 1, extra, 0).await;
            let second = verify_checkpoint(&published.checkpoint(), DOMAIN, std::slice::from_ref(&verifier)).unwrap();
            let proofs = TileProofs::new(&published, second.size);
            let consistency = proofs.consistency_proof(first.size).await.unwrap();
            verify_checkpoint_extends(&first, &second, &consistency).unwrap();
            let seq = probe % second.size + 1;
            let entry = proofs.entry(seq - 1).await.unwrap();
            let inclusion = proofs.inclusion_proof(seq - 1).await.unwrap();
            verify_release_inclusion(&second, &entry, &inclusion, seq, &digest(seq)).unwrap();
        });
    }
}
