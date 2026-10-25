//! The publishing pipeline end to end on `DirectoryHost` (spec 3.5 §7.8.2, §7.8.3, A-I1,
//! A-I23, A-I24): launch, a story through an editor-and-publisher chain, a correction, a
//! withdrawal, a scheduled launch, a per-item revert and a whole-site rollback, each a
//! signed release verified from what the host serves.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use adk_auth::signed_audit::NoteVerifier;
use adk_core::audit_log::InMemoryAuditLogStore;
use adk_core::{Clock, ManualClock};
use adk_site::content::{
    ContentStore, FsContentStore, InMemoryContentHistory, InMemoryContentStore, SiteProject,
};
use adk_site::gate::{FactStatus, FactTable, LaunchGate};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::{DirectoryHost, HostTiles, SiteHost};
use adk_site::log::{
    CHECKPOINT_PATH, SiteLog, TileProofs, verify_checkpoint, verify_release_inclusion,
};
use adk_site::pipeline::{
    Announcement, Announcer, ApprovalDecision, ApprovalGate, ApprovalRequest, Change, EntryPoint,
    PipelineOutcome, PublishingPipeline, Withdrawal,
};
use adk_site::release::{file_digest, manifest_path};
use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
use adk_site::{Result, SiteError};
use async_trait::async_trait;
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, FactRef, Markdown, ReleaseSource, Schedule,
};
use chrono::{TimeZone, Utc};

/// An editor-and-publisher chain: approves with whatever the test sets.
struct Chain(Mutex<Option<ApprovalDecision>>);

#[async_trait]
impl ApprovalGate for Chain {
    async fn decide(&self, _request: &ApprovalRequest) -> Result<ApprovalDecision> {
        Ok(self.0.lock().unwrap().clone().unwrap_or(ApprovalDecision::Approved {
            approvers: vec!["urn:person:editor".into(), "urn:person:publisher".into()],
            decision: Some("decision-1".into()),
        }))
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Announcement>>);

#[async_trait]
impl Announcer for Recorder {
    async fn announce(&self, announcement: &Announcement) -> Result<()> {
        self.0.lock().unwrap().push(announcement.clone());
        Ok(())
    }
}

struct Site {
    pipeline: PublishingPipeline,
    host: Arc<DirectoryHost>,
    store: Arc<InMemoryContentStore>,
    clock: Arc<ManualClock>,
    chain: Arc<Chain>,
    announcements: Arc<Recorder>,
    verifier: NoteVerifier,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

async fn site() -> Site {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    let clock = Arc::new(ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 25, 8, 0, 0).unwrap()));
    let base = SiteProject::load(&fixture, clock.now()).await.unwrap();
    let store = Arc::new(InMemoryContentStore::default());
    for item in FsContentStore::new(&fixture).list().await.unwrap() {
        for media in &item.media {
            let bytes = std::fs::read(fixture.join(&media.path)).unwrap();
            store.write_media(&media.path, bytes.into()).await.unwrap();
        }
    }
    let host_dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let host = Arc::new(DirectoryHost::new(host_dir.path()));
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed("example.com", [9; 32]).unwrap());
    let verifier = signer.verifier_key().parse().unwrap();
    let log = Arc::new(
        SiteLog::open_with_clock(
            Arc::new(InMemoryAuditLogStore::new()),
            "example.com",
            signer,
            clock.clone(),
        )
        .await
        .unwrap(),
    );
    let chain = Arc::new(Chain(Mutex::new(None)));
    let announcements = Arc::new(Recorder::default());
    let mut base = base;
    base.items.clear();
    let pipeline = PublishingPipeline::builder(
        base,
        store.clone(),
        Arc::new(BuiltinGenerator),
        host.clone(),
        log,
        work.path(),
    )
    .history(Arc::new(InMemoryContentHistory::default()))
    .gate(LaunchGate::new().with_claim_check(Arc::new(FactTable::new([(
        FactRef("fact-founded".into()),
        FactStatus::Known,
    )]))))
    .approvals(chain.clone())
    .announcer(announcements.clone())
    .clock(clock.clone())
    .build();
    Site { pipeline, host, store, clock, chain, announcements, verifier, _dirs: (host_dir, work) }
}

fn change(items: Vec<ContentItem>, proposer: &str) -> Change {
    Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::WebEditor,
        proposer: proposer.into(),
        job: "newsroom".into(),
        action_class: "publish".into(),
    }
}

fn story(body: &str) -> ContentItem {
    ContentItem {
        id: "articles/flood/en".into(),
        kind: ContentKind::Article,
        slug: "flood".into(),
        language: "en".parse().unwrap(),
        fields: [
            ("title".to_string(), serde_json::json!("River bursts its banks")),
            ("byline".to_string(), serde_json::json!("A. Reporter")),
            ("publishedAt".to_string(), serde_json::json!("2026-10-25T08:00:00Z")),
        ]
        .into_iter()
        .collect(),
        volatile: vec![],
        body: Some(Markdown(body.into())),
        media: vec![],
        sources: vec![],
        schedule: None,
        state: ContentState::InReview,
    }
}

/// Checks the release the host serves against its signed log, from published files only.
async fn verify_served(site: &Site, seq: u64) {
    let host: &dyn SiteHost = site.host.as_ref();
    let note = host.fetch(CHECKPOINT_PATH).await.unwrap().unwrap();
    let checkpoint = verify_checkpoint(
        &String::from_utf8_lossy(&note),
        "example.com",
        std::slice::from_ref(&site.verifier),
    )
    .unwrap();
    assert_eq!(checkpoint.size, seq, "the tree size is the latest release");
    let manifest = host.fetch(&manifest_path(seq)).await.unwrap().unwrap();
    let tiles = HostTiles(host);
    let proofs = TileProofs::new(&tiles, checkpoint.size);
    let entry = proofs.entry(seq - 1).await.unwrap();
    let proof = proofs.inclusion_proof(seq - 1).await.unwrap();
    verify_release_inclusion(&checkpoint, &entry, &proof, seq, &file_digest(&manifest)).unwrap();
}

fn published(outcome: PipelineOutcome) -> adk_site::pipeline::Published {
    match outcome {
        PipelineOutcome::Published(p) => p,
        other => panic!("expected a published release, got {other:?}"),
    }
}

#[tokio::test]
async fn a_site_publishes_corrects_withdraws_schedules_reverts_and_rolls_back() {
    let site = site().await;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    let mut launch_items = FsContentStore::new(&fixture).list().await.unwrap();
    launch_items.retain(|item| item.state != ContentState::Draft);

    // Launch: release 1, every page new.
    let launch =
        published(site.pipeline.run(change(launch_items, "urn:agent:steward")).await.unwrap());
    assert_eq!(launch.staged.manifest.seq, 1);
    assert!(launch.staged.manifest.is_snapshot());
    verify_served(&site, 1).await;
    assert!(site.host.fetch("/posts/hello/").await.unwrap().is_some());

    // A reporter's story through the editor and publisher: a delta that uploads only
    // what changed, announced once with its item.
    let start = std::time::Instant::now();
    let story_release = published(
        site.pipeline
            .run(change(vec![story("Water rose overnight.")], "urn:person:reporter"))
            .await
            .unwrap(),
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(60), "approval to live under 60 s");
    assert_eq!(story_release.staged.manifest.base, Some(1), "a delta release");
    assert_eq!(
        story_release.staged.manifest.source,
        ReleaseSource::Edit { job: "newsroom".into(), decision: Some("decision-1".into()) }
    );
    let uploaded: BTreeSet<&str> = story_release
        .report
        .uploaded
        .iter()
        .map(String::as_str)
        .filter(|p| !p.starts_with("/.well-known/awp/"))
        .collect();
    for path in [
        "/articles/flood/index.html",
        "/articles/flood/index.md",
        "/articles/flood/index.awp.json",
        "/articles/index.html",
        "/sitemap.xml",
        "/llms.txt",
    ] {
        assert!(uploaded.contains(path), "{path} uploaded");
    }
    verify_served(&site, 2).await;
    let announced = site.announcements.0.lock().unwrap().last().cloned().unwrap();
    assert_eq!(announced.catalog.unwrap().items, vec![ContentId::from("articles/flood/en")]);

    // A correction: a new release with a visible note; the log keeps both digests.
    let id = ContentId::from("articles/flood/en");
    let corrected = published(
        site.pipeline
            .correct(
                &id,
                |item| item.body = Some(Markdown("Water rose overnight in two districts.".into())),
                "The first version named one district.",
                "urn:person:reporter",
                EntryPoint::WebEditor,
            )
            .await
            .unwrap(),
    );
    assert_eq!(corrected.staged.manifest.seq, 3);
    let uploaded: BTreeSet<&str> = corrected.report.uploaded.iter().map(String::as_str).collect();
    assert!(uploaded.contains("/articles/flood/index.html"));
    assert!(
        !uploaded.contains("/products/panel-400/index.html"),
        "unchanged pages are not uploaded"
    );
    assert!(!uploaded.contains("/about/index.html"));
    let page = site.host.fetch("/articles/flood/").await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&page).contains("The first version named one district."));
    verify_served(&site, 3).await;

    // A withdrawal as 410 Gone.
    published(
        site.pipeline
            .withdraw(
                &"posts/hello/sw".into(),
                Withdrawal::Gone,
                "urn:person:editor-2",
                EntryPoint::WebEditor,
            )
            .await
            .unwrap(),
    );
    assert!(site.host.fetch("/sw/posts/hello/").await.unwrap().is_none());
    let redirects = site.host.fetch("/_redirects.json").await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&redirects).contains("\"from\": \"/sw/posts/hello/\""));

    // A product scheduled for 09:00 waits, then publishes when its time comes.
    let mut product = ContentItem {
        id: "products/panel-500/en".into(),
        kind: ContentKind::Product,
        slug: "panel-500".into(),
        language: "en".parse().unwrap(),
        fields: [
            ("title".to_string(), serde_json::json!("Panel 500 W")),
            ("price".to_string(), serde_json::json!(31000)),
            ("currency".to_string(), serde_json::json!("KES")),
        ]
        .into_iter()
        .collect(),
        volatile: vec![],
        body: None,
        media: vec![],
        sources: vec![],
        schedule: Some(Schedule {
            publish_at: Some(Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap()),
            ..Schedule::default()
        }),
        state: ContentState::InReview,
    };
    match site.pipeline.run(change(vec![product.clone()], "urn:person:owner")).await.unwrap() {
        PipelineOutcome::Scheduled { at } => {
            assert_eq!(at, product.schedule.as_ref().unwrap().publish_at)
        }
        other => panic!("expected a schedule, got {other:?}"),
    }
    assert!(site.host.fetch("/products/panel-500/").await.unwrap().is_none());
    site.clock.advance(std::time::Duration::from_secs(3600));
    let due = site.pipeline.run_due(None).await.unwrap();
    assert_eq!(due.len(), 1);
    assert!(site.host.fetch("/products/panel-500/").await.unwrap().is_some(), "live at 09:00");
    product.state = ContentState::Published;

    // Undo one item: the story returns to its first version; the product stays.
    let reverted = published(
        site.pipeline
            .revert_item(&id, Some(2), "urn:person:night-editor", EntryPoint::WebEditor)
            .await
            .unwrap(),
    );
    let page =
        String::from_utf8_lossy(&site.host.fetch("/articles/flood/").await.unwrap().unwrap())
            .into_owned();
    assert!(page.contains("Water rose overnight.") && !page.contains("two districts"));
    assert!(
        site.host.fetch("/products/panel-500/").await.unwrap().is_some(),
        "every other change stays"
    );
    verify_served(&site, reverted.staged.manifest.seq).await;

    // Whole-site rollback to release 1: its files again, recorded as a new release.
    let rollback = site.pipeline.rollback(1).await.unwrap();
    assert_eq!(rollback.staged.manifest.source, ReleaseSource::Rollback { to_seq: 1 });
    assert_eq!(rollback.staged.files, launch.staged.files, "exact files of release 1");
    assert!(site.host.fetch("/articles/flood/").await.unwrap().is_none());
    verify_served(&site, rollback.staged.manifest.seq).await;
    assert_eq!(
        site.store.get(&id).await.unwrap().unwrap().state,
        ContentState::Published,
        "the working copy is not rolled back"
    );
}

#[tokio::test]
async fn approvals_follow_the_chain_rules() {
    let site = site().await;
    // The proposer cannot approve their own change (A-I23).
    *site.chain.0.lock().unwrap() = Some(ApprovalDecision::Approved {
        approvers: vec!["urn:person:reporter".into()],
        decision: None,
    });
    let error =
        site.pipeline.run(change(vec![story("Text.")], "urn:person:reporter")).await.unwrap_err();
    assert!(error.to_string().contains("A-I23"), "{error}");

    // A pending decision completes later; a rejection publishes nothing.
    *site.chain.0.lock().unwrap() = Some(ApprovalDecision::Pending { ticket: "card-7".into() });
    let PipelineOutcome::Pending { ticket, draft, preview } =
        site.pipeline.run(change(vec![story("Text.")], "urn:person:reporter")).await.unwrap()
    else {
        panic!("expected a pending decision");
    };
    assert_eq!(ticket, "card-7");
    assert!(preview.diff.iter().any(|d| d.path == "/articles/flood/index.html"));
    assert!(
        site.host.fetch("/articles/flood/").await.unwrap().is_none(),
        "nothing is live before approval"
    );
    published(site.pipeline.complete(&draft, Some("card-7".into())).await.unwrap());
    assert!(site.host.fetch("/articles/flood/").await.unwrap().is_some());

    *site.chain.0.lock().unwrap() = Some(ApprovalDecision::Rejected {
        by: "urn:person:publisher".into(),
        reason: "not verified".into(),
    });
    let outcome = site
        .pipeline
        .run(change(vec![story("Unverified claim.")], "urn:person:reporter"))
        .await
        .unwrap();
    assert!(matches!(outcome, PipelineOutcome::Rejected { .. }));
}

#[tokio::test]
async fn server_code_and_gate_failures_never_publish() {
    let site = site().await;
    let mut item = story("Download the [form](form.php).");
    item.media.push(awp_types::MediaRef {
        path: "content/articles/flood/form.php".into(),
        content_type: "text/plain".into(),
        sha256: None,
        alt: None,
        alt_confirmed: false,
        generated: false,
    });
    let mut upload = change(vec![item], "urn:person:reporter");
    upload.media.push((
        "content/articles/flood/form.php".into(),
        bytes::Bytes::from_static(b"<?php echo 1;"),
    ));
    let error = site.pipeline.run(upload).await.unwrap_err();
    assert!(
        matches!(error, SiteError::ServerCode { ref findings } if findings[0].path == "articles/flood/form.php"),
        "{error}"
    );
    assert!(site.host.fetch("/articles/flood/").await.unwrap().is_none());

    let mut bad = story("Text.");
    bad.fields.remove("title");
    let outcome = site.pipeline.run(change(vec![bad], "urn:person:reporter")).await.unwrap();
    let PipelineOutcome::Held(gate) = outcome else {
        panic!("expected the gate to hold the change")
    };
    assert!(gate.held_items().contains(&ContentId::from("articles/flood/en")));
}
