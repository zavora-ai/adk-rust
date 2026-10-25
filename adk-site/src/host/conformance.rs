//! The host conformance suite every [`SiteHost`] runs (spec 3.5 §7.8 "Hosts", §7.8.3,
//! §11 "Conformance"; AWP 2.0 §14.6).
//!
//! | Check | Requirement |
//! |-------|-------------|
//! | [`checkpoint_last`] | Files, then the manifest, then tiles, then the checkpoint; the published log verifies from the host alone |
//! | [`atomic_under_concurrent_reads`] | Readers during a publish never miss a file both releases hold, never see a third version, and see the new release's files once its checkpoint is visible; on an atomic host they never see a mixture |
//! | [`exact_rollback`] | A rollback serves the earlier release's files with their exact digests, and the log stays as it was |
//! | [`redirect_round_trip`] | Redirects written in the host's format read back unchanged |
//! | [`delta_publication`] | A delta uploads only changed files and serves the full file set |
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use std::sync::atomic::{AtomicUsize, Ordering};
//! use adk_site::host::{DirectoryHost, SiteHost, conformance};
//!
//! # async fn demo() {
//! let root = std::env::temp_dir().join("host-under-test");
//! let next = AtomicUsize::new(0);
//! conformance::run_all(|| {
//!     let n = next.fetch_add(1, Ordering::Relaxed);
//!     Arc::new(DirectoryHost::new(root.join(n.to_string()))) as Arc<dyn SiteHost>
//! })
//! .await;
//! # }
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use adk_auth::signed_audit::NoteVerifier;
use adk_core::audit_log::InMemoryAuditLogStore;
use awp_types::{Atomicity, GeneratorRef, Redirect, ReleaseSource};
use bytes::Bytes;

use super::{HostTarget, HostTiles, SiteHost};
use crate::log::{
    CHECKPOINT_PATH, SiteLog, TileProofs, verify_checkpoint, verify_release_inclusion,
};
use crate::release::{
    PreviousRelease, ReleaseBuilder, ReleaseOptions, StagedDelta, StagedRelease, file_digest,
    is_log_path, manifest_path,
};
use crate::signer::{Ed25519ReleaseSigner, ReleaseSigner};

const DOMAIN: &str = "conformance.example";

/// Builds and logs releases for the suite in a temporary directory, removed on drop.
pub struct Fixture {
    dir: PathBuf,
    log: SiteLog,
    verifier: NoteVerifier,
    builder: ReleaseBuilder,
    previous: Option<PreviousRelease>,
    count: usize,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    /// A fixture with an empty log.
    ///
    /// # Panics
    ///
    /// Panics when the temporary directory or the log cannot be created.
    pub async fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir()
            .join(format!("adk-site-conformance-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temporary directory");
        let signer = Arc::new(Ed25519ReleaseSigner::from_seed(DOMAIN, [42; 32]).expect("signer"));
        let verifier = signer.verifier_key().parse().expect("verifier");
        let log = SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), DOMAIN, signer)
            .await
            .expect("log");
        Self { dir, log, verifier, builder: ReleaseBuilder::new(), previous: None, count: 0 }
    }

    /// Writes `files` as the next build and releases it, as a delta when `delta` is set.
    ///
    /// # Panics
    ///
    /// Panics when the release cannot be built.
    pub async fn release(
        &mut self,
        files: &[(&str, Vec<u8>)],
        redirects: Vec<Redirect>,
        delta: bool,
    ) -> StagedRelease {
        self.count += 1;
        let out = self.dir.join(format!("build-{}", self.count));
        for (path, bytes) in files {
            let target = out.join(path.trim_start_matches('/'));
            std::fs::create_dir_all(target.parent().expect("parent")).expect("dir");
            std::fs::write(target, bytes).expect("write");
        }
        let written = delta
            .then(|| files.iter().map(|(p, _)| p.trim_start_matches('/').to_string()).collect());
        let options = ReleaseOptions {
            source: ReleaseSource::Edit { job: "conformance".into(), decision: None },
            generator: GeneratorRef { name: "conformance".into(), version: "1".into() },
            charter_version: 1,
            redirects,
            written,
            force_snapshot: !delta,
        };
        let mut builder = self.builder.clone();
        if delta {
            builder = builder.snapshot_every(1_000);
        }
        let staged = builder
            .release(&out, self.previous.as_ref(), options, &self.log)
            .await
            .expect("release");
        self.previous = Some(PreviousRelease::from_staged(&staged));
        staged
    }

    /// The trusted checkpoint key.
    pub fn verifier(&self) -> &NoteVerifier {
        &self.verifier
    }
}

fn pages(version: &str, count: usize, extra: &str) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = (0..count)
        .map(|i| {
            (
                format!("/p{i}/index.html"),
                format!("<p>{version}-{i}</p>{}", "x".repeat(4096)).into_bytes(),
            )
        })
        .collect();
    files.push((format!("/{extra}.html"), format!("<p>{extra}</p>").into_bytes()));
    files
}

fn borrowed(files: &[(String, Vec<u8>)]) -> Vec<(&str, Vec<u8>)> {
    files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect()
}

async fn fetch_text(host: &dyn SiteHost, path: &str) -> Option<Bytes> {
    host.fetch(path).await.expect("fetch")
}

/// Files, then the manifest, then tiles, then the checkpoint; and the release verifies from
/// what the host serves.
///
/// # Panics
///
/// Panics with the failed expectation.
pub async fn checkpoint_last(host: &dyn SiteHost) {
    let mut fixture = Fixture::new().await;
    let files = pages("a", 3, "only-a");
    let staged = fixture.release(&borrowed(&files), Vec::new(), false).await;
    let report = host.publish(&staged, HostTarget::Production).await.expect("publish");
    let position = |path: &str| report.uploaded.iter().position(|p| p == path);
    let manifest = position(&format!("/{}", manifest_path(1))).expect("the manifest is uploaded");
    for (path, _) in &files {
        assert!(
            position(path).expect("every file is uploaded") < manifest,
            "files precede the manifest"
        );
    }
    let checkpoint = position(&format!("/{CHECKPOINT_PATH}")).expect("the checkpoint is uploaded");
    assert_eq!(checkpoint, report.uploaded.len() - 1, "the checkpoint is last");
    for (i, path) in report.uploaded.iter().enumerate() {
        if path.contains("/log/tile/") {
            assert!(
                i > manifest && i < checkpoint,
                "tiles sit between the manifest and the checkpoint"
            );
        }
    }
    let note = fetch_text(host, CHECKPOINT_PATH).await.expect("checkpoint served");
    let verified = verify_checkpoint(
        &String::from_utf8_lossy(&note),
        DOMAIN,
        std::slice::from_ref(fixture.verifier()),
    )
    .expect("checkpoint verifies");
    let tiles = HostTiles(host);
    let proofs = TileProofs::new(&tiles, verified.size);
    let entry = proofs.entry(0).await.expect("entry bundle served");
    let proof = proofs.inclusion_proof(0).await.expect("tiles served");
    verify_release_inclusion(&verified, &entry, &proof, 1, &staged.digest)
        .expect("release 1 is in the log");
    let served = fetch_text(host, &manifest_path(1)).await.expect("manifest served");
    assert_eq!(file_digest(&served), staged.digest, "the served manifest is the logged one");
}

/// Readers during a publish see whole releases.
///
/// # Panics
///
/// Panics with the failed expectation.
pub async fn atomic_under_concurrent_reads(host: Arc<dyn SiteHost>) {
    let mut fixture = Fixture::new().await;
    let a_files = pages("a", 40, "only-a");
    let b_files = pages("b", 40, "only-b");
    let a = fixture.release(&borrowed(&a_files), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await.expect("publish a");
    let b = fixture.release(&borrowed(&b_files), Vec::new(), false).await;
    let checkpoint_of = |staged: &StagedRelease| {
        staged.log.as_ref().and_then(|l| l.checkpoint.clone()).expect("logged").1
    };
    let (cp_a, cp_b) = (checkpoint_of(&a), checkpoint_of(&b));
    let atomic = host.limits().atomic != Atomicity::None;
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let a_map: Arc<BTreeMap<String, Vec<u8>>> = Arc::new(a_files.into_iter().collect());
    let b_map: Arc<BTreeMap<String, Vec<u8>>> = Arc::new(b_files.into_iter().collect());
    let mut readers = Vec::new();
    for reader in 0..4 {
        let (host, stop, reads) = (host.clone(), stop.clone(), reads.clone());
        let (a_map, b_map, cp_a, cp_b) = (a_map.clone(), b_map.clone(), cp_a.clone(), cp_b.clone());
        readers.push(tokio::spawn(async move {
            let mut violations = Vec::new();
            let mut round = 0usize;
            while !stop.load(Ordering::Relaxed) || round < 3 {
                round += 1;
                let before = host.fetch(CHECKPOINT_PATH).await.expect("fetch");
                let mut seen = Vec::new();
                for i in (reader..40).step_by(7) {
                    let path = format!("/p{i}/index.html");
                    let body = host.fetch(&path).await.expect("fetch");
                    reads.fetch_add(1, Ordering::Relaxed);
                    match body {
                        None => violations.push(format!("{path} missing during the publish")),
                        Some(b) if b.as_ref() == a_map[&path].as_slice() => seen.push('a'),
                        Some(b) if b.as_ref() == b_map[&path].as_slice() => seen.push('b'),
                        Some(_) => violations.push(format!("{path} is neither release")),
                    }
                }
                let after = host.fetch(CHECKPOINT_PATH).await.expect("fetch");
                if before.as_ref() == Some(&cp_b) && seen.contains(&'a') {
                    violations.push("old files served after the new checkpoint was visible".into());
                }
                if atomic
                    && before.as_ref() == Some(&cp_a)
                    && after.as_ref() == Some(&cp_a)
                    && seen.contains(&'b')
                {
                    violations.push("new files served before the switch on an atomic host".into());
                }
                tokio::task::yield_now().await;
            }
            violations
        }));
    }
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    host.publish(&b, HostTarget::Production).await.expect("publish b");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    stop.store(true, Ordering::Relaxed);
    let mut violations = Vec::new();
    for reader in readers {
        violations.extend(reader.await.expect("reader"));
    }
    assert!(violations.is_empty(), "{violations:?}");
    assert!(reads.load(Ordering::Relaxed) > 0, "readers ran");
    assert_eq!(fetch_text(host.as_ref(), CHECKPOINT_PATH).await, Some(cp_b));
    assert_eq!(fetch_text(host.as_ref(), "/only-a.html").await, None, "removed files are gone");
}

/// A rollback serves the earlier release's exact files.
///
/// # Panics
///
/// Panics with the failed expectation.
pub async fn exact_rollback(host: &dyn SiteHost) {
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&borrowed(&pages("a", 5, "only-a")), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await.expect("publish a");
    let b = fixture.release(&borrowed(&pages("b", 5, "only-b")), Vec::new(), false).await;
    host.publish(&b, HostTarget::Production).await.expect("publish b");
    let checkpoint = fetch_text(host, CHECKPOINT_PATH).await;
    host.rollback(&a.manifest).await.expect("rollback");
    for (path, entry) in &a.files {
        let body = fetch_text(host, path)
            .await
            .unwrap_or_else(|| panic!("{path} is served after the rollback"));
        assert_eq!(file_digest(&body), entry.sha256, "{path} has its exact digest");
    }
    for path in b.files.keys().filter(|p| !a.files.contains_key(*p) && !is_log_path(p)) {
        assert_eq!(fetch_text(host, path).await, None, "{path} left with the rollback");
    }
    assert_eq!(fetch_text(host, CHECKPOINT_PATH).await, checkpoint, "the log is not rolled back");
}

/// Redirects in the host's format read back unchanged.
///
/// # Panics
///
/// Panics with the failed expectation.
pub async fn redirect_round_trip(host: &dyn SiteHost) {
    let mut fixture = Fixture::new().await;
    let redirects = vec![
        Redirect { from: "/old/".into(), to: "/new/".into(), status: 301 },
        Redirect {
            from: "/withdrawn/".into(),
            to: crate::redirects::GONE_PAGE.into(),
            status: 410,
        },
    ];
    let format = host.redirects_format();
    let rendered = format.render(&redirects);
    let mut files = pages("r", 1, "new/index");
    files.extend(rendered.iter().map(|(p, b)| (format!("/{p}"), b.clone())));
    let staged = fixture.release(&borrowed(&files), redirects.clone(), false).await;
    host.publish(&staged, HostTarget::Production).await.expect("publish");
    let mut read = Vec::new();
    for (path, _) in &rendered {
        let body = fetch_text(host, path).await.unwrap_or_else(|| panic!("{path} is served"));
        read.push((path.clone(), body.to_vec()));
    }
    let mut parsed = format.parse(&read).expect("parse");
    parsed.sort_by(|x, y| x.from.cmp(&y.from));
    let mut expected = redirects;
    expected.sort_by(|x, y| x.from.cmp(&y.from));
    assert_eq!(parsed, expected);
}

/// A delta uploads only its changed files and serves the full set.
///
/// # Panics
///
/// Panics with the failed expectation.
pub async fn delta_publication(host: &dyn SiteHost) {
    let mut fixture = Fixture::new().await;
    let mut files = pages("d", 6, "keep");
    let first = fixture.release(&borrowed(&files), Vec::new(), false).await;
    host.publish(&first, HostTarget::Production).await.expect("publish snapshot");
    files[0].1 = b"<p>changed</p>".to_vec();
    files.retain(|(p, _)| p != "/p5/index.html");
    let staged = fixture.release(&borrowed(&files), Vec::new(), true).await;
    assert_eq!(staged.manifest.base, Some(1), "a delta against release 1");
    assert_eq!(staged.manifest.files.len(), 1);
    assert!(staged.manifest.removed.contains("/p5/index.html"));
    let report = host
        .publish_delta(&StagedDelta::new(staged.clone()).expect("delta"), HostTarget::Production)
        .await
        .expect("publish delta");
    let content: Vec<&String> = report.uploaded.iter().filter(|p| !is_log_path(p)).collect();
    if host.limits().delta_upload {
        assert_eq!(content, vec!["/p0/index.html"], "only the changed file is uploaded");
    }
    for (path, bytes) in &files {
        assert_eq!(fetch_text(host, path).await.as_deref(), Some(bytes.as_slice()), "{path}");
    }
    assert_eq!(fetch_text(host, "/p5/index.html").await, None);
}

/// Runs every check, each on a fresh host from `make_host`.
///
/// # Panics
///
/// Panics with the first failed expectation.
pub async fn run_all(make_host: impl Fn() -> Arc<dyn SiteHost>) {
    checkpoint_last(make_host().as_ref()).await;
    atomic_under_concurrent_reads(make_host()).await;
    exact_rollback(make_host().as_ref()).await;
    redirect_round_trip(make_host().as_ref()).await;
    delta_publication(make_host().as_ref()).await;
}
