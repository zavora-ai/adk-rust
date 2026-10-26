//! The pipeline's audit records and span (spec 3.5 §7.24, §9.3): a launch, a correction, a
//! withdrawal, a scheduled item and a rollback each leave the records §9.3 names, read back
//! from the recorder, and every release runs in a `site.release` span carrying its sequence.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{
    AuditRecordDraft, AuditTrail, InMemoryAuditLogStore, InMemoryAuditRecorder,
};
use adk_core::{Clock, ManualClock};
use adk_site::content::{ContentStore, FsContentStore, InMemoryContentStore, SiteProject};
use adk_site::gate::{FactStatus, FactTable, LaunchGate};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::DirectoryHost;
use adk_site::log::SiteLog;
use adk_site::pipeline::audit::{
    CONTENT_APPROVED, CONTENT_CORRECTED, CONTENT_DRAFTED, CONTENT_PUBLISHED, CONTENT_SCHEDULED,
    CONTENT_WITHDRAWN, RELEASE_BUILT, RELEASE_PUBLISHED, RELEASE_SIGNED,
};
use adk_site::pipeline::{
    AutoApprove, Change, EntryPoint, PipelineOutcome, PublishingPipeline, RELEASE_SPAN, Withdrawal,
};
use adk_site::signer::Ed25519ReleaseSigner;
use awp_types::{ContentItem, ContentKind, ContentState, FactRef, Markdown, Schedule};
use chrono::{TimeZone, Utc};
use serde_json::json;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

const EDITOR: &str = "urn:adk:agent:example/steward";

struct Site {
    pipeline: PublishingPipeline,
    recorder: InMemoryAuditRecorder,
    clock: Arc<ManualClock>,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

async fn site() -> Site {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    let clock = Arc::new(ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 25, 8, 0, 0).unwrap()));
    let mut base = SiteProject::load(&fixture, clock.now()).await.unwrap();
    base.items.clear();
    let store = Arc::new(InMemoryContentStore::default());
    for item in FsContentStore::new(&fixture).list().await.unwrap() {
        for media in &item.media {
            let bytes = std::fs::read(fixture.join(&media.path)).unwrap();
            store.write_media(&media.path, bytes.into()).await.unwrap();
        }
    }
    let (host_dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed("example.com", [9; 32]).unwrap());
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
    let recorder = InMemoryAuditRecorder::new();
    let pipeline = PublishingPipeline::builder(
        base,
        store,
        Arc::new(BuiltinGenerator),
        Arc::new(DirectoryHost::new(host_dir.path())),
        log,
        work.path(),
    )
    .gate(LaunchGate::new().with_claim_check(Arc::new(FactTable::new([(
        FactRef("fact-founded".into()),
        FactStatus::Known,
    )]))))
    .approvals(Arc::new(AutoApprove("urn:person:editor".into())))
    .clock(clock.clone())
    .audit(AuditTrail::new(Arc::new(recorder.clone())))
    .build();
    Site { pipeline, recorder, clock, _dirs: (host_dir, work) }
}

fn change(items: Vec<ContentItem>) -> Change {
    Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: EDITOR.into() },
        proposer: EDITOR.into(),
        job: "content".into(),
        action_class: "publish".into(),
    }
}

fn product(slug: &str, at: Option<chrono::DateTime<Utc>>) -> ContentItem {
    ContentItem {
        id: format!("products/{slug}/en").as_str().into(),
        kind: ContentKind::Product,
        slug: slug.into(),
        language: "en".parse().unwrap(),
        fields: [
            ("title".to_string(), json!(slug)),
            ("price".to_string(), json!(31000)),
            ("currency".to_string(), json!("KES")),
        ]
        .into_iter()
        .collect(),
        volatile: vec![],
        body: Some(Markdown("A panel.".into())),
        media: vec![],
        sources: vec![],
        schedule: at.map(|at| Schedule { publish_at: Some(at), ..Schedule::default() }),
        state: ContentState::InReview,
    }
}

fn actions(records: &[AuditRecordDraft]) -> Vec<&str> {
    records.iter().map(|record| record.action.as_str()).collect()
}

fn find<'a>(records: &'a [AuditRecordDraft], action: &str) -> Vec<&'a AuditRecordDraft> {
    records.iter().filter(|record| record.action == action).collect()
}

#[tokio::test]
async fn every_step_of_a_release_leaves_its_record() {
    let site = site().await;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    let mut launch = FsContentStore::new(&fixture).list().await.unwrap();
    launch.retain(|item| item.state == ContentState::Published);
    let launched = site.pipeline.run(change(launch)).await.unwrap();
    assert!(matches!(launched, PipelineOutcome::Published(_)), "{launched:?}");

    let records = site.recorder.records();
    assert_eq!(
        actions(&records),
        vec![
            CONTENT_DRAFTED,
            CONTENT_APPROVED,
            RELEASE_BUILT,
            RELEASE_SIGNED,
            RELEASE_PUBLISHED,
            CONTENT_PUBLISHED,
        ],
        "the launch's records, in order"
    );
    for record in &records {
        assert_eq!(record.kind.as_str(), "site");
        assert_eq!(record.ext["site"], "example.com");
        assert_eq!(record.ext["principal"], EDITOR);
        assert_eq!(record.actor.agent.as_ref().map(|agent| agent.as_str()), Some(EDITOR));
        assert!(record.ext.contains_key("charterVersion"));
        assert!(record.args_digest.is_some());
    }
    let approved = find(&records, CONTENT_APPROVED)[0];
    assert_eq!(approved.ext["approvers"], json!(["urn:person:editor"]));
    let built = find(&records, RELEASE_BUILT)[0];
    assert_eq!(
        (built.ext["releaseSeq"].clone(), built.ext["snapshot"].clone()),
        (json!(1), json!(true))
    );
    let signed = find(&records, RELEASE_SIGNED)[0];
    assert_eq!(signed.ext["logSize"], 1);
    assert_eq!(signed.ext["manifest"], built.ext["manifest"], "the signed digest is the built one");
    assert!(signed.ext["verifierKey"].as_str().is_some_and(|key| key.contains('+')), "{signed:?}");
    let published = find(&records, CONTENT_PUBLISHED)[0];
    assert_eq!(published.ext["releaseSeq"], 1);
    assert!(published.ext["total"].as_u64().unwrap() >= 1);

    // A correction and a withdrawal: each its own release and content record.
    let first = awp_types::ContentId::from("posts/hello/en");
    site.pipeline
        .correct(&first, |_| {}, "A date was wrong.", EDITOR, EntryPoint::WebEditor)
        .await
        .unwrap();
    let translation = awp_types::ContentId::from("posts/hello/sw");
    let gone = site
        .pipeline
        .withdraw(&translation, Withdrawal::Gone, EDITOR, EntryPoint::WebEditor)
        .await
        .unwrap();
    assert!(matches!(gone, PipelineOutcome::Published(_)), "{gone:?}");
    let records = site.recorder.records();
    let corrected = find(&records, CONTENT_CORRECTED);
    assert_eq!(corrected.len(), 1);
    assert_eq!(
        (corrected[0].ext["releaseSeq"].clone(), corrected[0].ext["items"].clone()),
        (json!(2), json!([first.0]))
    );
    let withdrawn = find(&records, CONTENT_WITHDRAWN);
    assert_eq!(
        (withdrawn[0].ext["releaseSeq"].clone(), withdrawn[0].ext["items"].clone()),
        (json!(3), json!([translation.0]))
    );

    // A scheduled item waits with its record, then publishes with its own.
    let at = Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap();
    let scheduled = site.pipeline.run(change(vec![product("panel-500", Some(at))])).await.unwrap();
    assert!(matches!(scheduled, PipelineOutcome::Scheduled { .. }));
    let records = site.recorder.records();
    let waiting = find(&records, CONTENT_SCHEDULED);
    assert_eq!(waiting[0].ext["items"], json!(["products/panel-500/en"]));
    assert_eq!(waiting[0].ext["at"], json!(at));
    site.clock.advance(std::time::Duration::from_secs(3600));
    site.pipeline.run_due(None).await.unwrap();
    let records = site.recorder.records();
    let live = find(&records, CONTENT_PUBLISHED);
    let items = live.last().unwrap().ext["items"].as_array().unwrap();
    assert!(items.contains(&json!("products/panel-500/en")), "{items:?}");
    assert_eq!(live.last().unwrap().ext["principal"], "schedule");

    // A rollback is a release like any other: built, signed and published.
    let before = site.recorder.records().len();
    let rollback = site.pipeline.rollback(1).await.unwrap();
    let records = site.recorder.records();
    assert_eq!(actions(&records[before..]), vec![RELEASE_BUILT, RELEASE_SIGNED, RELEASE_PUBLISHED]);
    assert_eq!(records[before].ext["releaseSeq"], rollback.staged.manifest.seq);
    assert_eq!(records[before].ext["source"], json!({ "kind": "rollback", "toSeq": 1 }));
}

/// A span's name and its fields as recorded.
type Opened = (String, BTreeMap<String, String>);

/// Every span opened, by id.
#[derive(Default)]
struct Spans(Mutex<BTreeMap<u64, Opened>>);

struct Fields<'a>(&'a mut BTreeMap<String, String>);

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), format!("{value:?}"));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
}

struct Capture(Arc<Spans>);

impl<S: Subscriber> Layer<S> for Capture {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _: Context<'_, S>) {
        let mut fields = BTreeMap::new();
        attrs.record(&mut Fields(&mut fields));
        self.0
            .0
            .lock()
            .unwrap()
            .insert(id.into_u64(), (attrs.metadata().name().to_string(), fields));
    }
    fn on_record(&self, id: &Id, values: &Record<'_>, _: Context<'_, S>) {
        if let Some((_, fields)) = self.0.0.lock().unwrap().get_mut(&id.into_u64()) {
            values.record(&mut Fields(fields));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_release_runs_in_a_site_release_span() {
    let spans = Arc::new(Spans::default());
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(Capture(spans.clone())),
    );
    let site = site().await;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    let mut launch = FsContentStore::new(&fixture).list().await.unwrap();
    launch.retain(|item| item.state == ContentState::Published);
    site.pipeline.run(change(launch)).await.unwrap();
    site.pipeline.rollback(1).await.unwrap();

    let releases: Vec<BTreeMap<String, String>> = spans
        .0
        .lock()
        .unwrap()
        .values()
        .filter(|(name, _)| name == RELEASE_SPAN)
        .map(|(_, fields)| fields.clone())
        .collect();
    assert_eq!(releases.len(), 2, "{releases:?}");
    let field = |n: usize, key: &str| releases[n].get(key).cloned().unwrap_or_default();
    assert_eq!(
        (field(0, "site.release.seq"), field(0, "site.release.source")),
        ("1".into(), "content".into())
    );
    assert_eq!(
        (field(1, "site.release.seq"), field(1, "site.release.source")),
        ("2".into(), "rollback".into())
    );
    assert!(field(0, "site.release.items").parse::<u64>().unwrap() >= 1);
}
