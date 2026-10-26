//! Constantly updated sites end to end on `DirectoryHost` (spec 3.5 §7.8.3, A-I1, A-I23,
//! A-I25, A-I26, A-I27): lanes, model-free feeds, batching, feed bounds, exceptions held
//! alone, tiered checks, delta releases, coalesced events, per-item revert and replay.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::{InMemoryAuditLogStore, InMemoryAuditRecorder};
use adk_core::{ActingContext, Clock, ManualClock, SubjectKind};
use adk_site::batch::{BatchPolicies, Update};
use adk_site::content::{
    ContentHistory, ContentStore, InMemoryContentHistory, InMemoryContentStore, SiteConfig,
    SiteProject,
};
use adk_site::feed::{FeedMapping, FeedRecord, FieldMapping, FieldType};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::{DirectoryHost, HostTarget, PublishReport, SiteHost};
use adk_site::lanes::KindLanes;
use adk_site::live::{InMemoryLiveStore, LiveStore};
use adk_site::log::{CHECKPOINT_PATH, SiteLog, verify_checkpoint};
use adk_site::pipeline::{Announcement, Announcer, EntryPoint, PublishingPipeline};
use adk_site::redirects::RedirectsFormat;
use adk_site::release::{StagedDelta, StagedRelease, manifest_path};
use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
use adk_site::updates::{
    Decision, FileHeldStore, HeldStore, HoldReason, InMemoryAlarms, InMemoryHeldStore,
    UpdateEngine, held_store_conformance,
};
use adk_site::{Result, SiteError};
use async_trait::async_trait;
use awp_types::{
    Atomicity, Bound, ContentId, ContentItem, ContentKind, ContentState, FeedBounds, HostLimits,
    PriceRule, Rate, ReleaseManifest, ReleaseSource, Trigger, Urgency,
};
use bytes::Bytes;
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};

const PRODUCTS: usize = 50;

#[derive(Default)]
struct Events(Mutex<Vec<Announcement>>);

#[async_trait]
impl Announcer for Events {
    async fn announce(&self, announcement: &Announcement) -> Result<()> {
        self.0.lock().unwrap().push(announcement.clone());
        Ok(())
    }
}

struct Shop {
    engine: UpdateEngine,
    host: Arc<DirectoryHost>,
    store: Arc<InMemoryContentStore>,
    history: Arc<InMemoryContentHistory>,
    live: Arc<InMemoryLiveStore>,
    clock: Arc<ManualClock>,
    alarms: Arc<InMemoryAlarms>,
    events: Arc<Events>,
    audit: Arc<InMemoryAuditRecorder>,
    verifier: adk_auth::signed_audit::NoteVerifier,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn config() -> SiteConfig {
    let mut config = SiteConfig::new("Solar Shop", "shop.example", vec!["en".parse().unwrap()]);
    config.lanes.insert(
        ContentKind::Product,
        KindLanes { volatile: vec!["stock".into()], ..KindLanes::default() },
    );
    config.live_capabilities.insert("stock".into(), "check_availability".into());
    config.page_size = 200;
    config
}

fn product(i: usize, price: f64) -> ContentItem {
    ContentItem {
        id: ContentId(format!("SKU-{i:03}")),
        kind: ContentKind::Product,
        slug: format!("sku-{i:03}"),
        language: "en".parse().unwrap(),
        fields: BTreeMap::from([
            ("title".to_string(), json!(format!("Panel {i}"))),
            ("price".to_string(), number(price)),
            ("currency".to_string(), json!("KES")),
            ("stock".to_string(), json!(10)),
        ]),
        volatile: Vec::new(),
        body: None,
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Published,
    }
}

fn legal_page(kind: &str) -> ContentItem {
    ContentItem {
        id: ContentId(format!("pages/{kind}")),
        kind: ContentKind::Page,
        slug: kind.into(),
        language: "en".parse().unwrap(),
        fields: BTreeMap::from([
            ("title".to_string(), json!(format!("{kind} policy"))),
            ("legal".to_string(), json!(kind)),
        ]),
        volatile: Vec::new(),
        body: Some(awp_types::Markdown(format!("Our {kind} policy."))),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Published,
    }
}

fn number(value: f64) -> Value {
    if value.fract() == 0.0 { json!(value as i64) } else { json!(value) }
}

fn mapping() -> FeedMapping {
    FeedMapping::new("supplier", ContentKind::Product, "en".parse().unwrap(), "sku")
        .field(
            "price",
            FieldMapping::new("price", FieldType::Number).bound(Bound::Range {
                minimum: None,
                exclusive_minimum: Some(0.0),
                maximum: None,
                exclusive_maximum: None,
            }),
        )
        .field("stock", FieldMapping::new("qty", FieldType::Integer))
}

fn bounds() -> FeedBounds {
    FeedBounds {
        max_change_fraction: 0.2,
        price_rule: PriceRule::default(),
        field_bounds: BTreeMap::from([(
            "stock".to_string(),
            Bound::Range {
                minimum: Some(0.0),
                exclusive_minimum: None,
                maximum: Some(100_000.0),
                exclusive_maximum: None,
            },
        )]),
        max_rate: Rate { per_minute: 1_000 },
    }
}

fn record(sku: usize, pairs: &[(&str, Value)]) -> FeedRecord {
    let mut fields = BTreeMap::from([("sku".to_string(), json!(format!("SKU-{sku:03}")))]);
    fields.extend(pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())));
    FeedRecord(fields)
}

async fn shop_with(products: usize, host: Option<Arc<dyn SiteHost>>) -> Result<Shop> {
    let clock = Arc::new(ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 25, 8, 0, 0).unwrap()));
    let host_dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let directory = Arc::new(DirectoryHost::new(host_dir.path()).materialise(false));
    let site_host = host.unwrap_or_else(|| directory.clone() as Arc<dyn SiteHost>);
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed("shop.example", [5; 32])?);
    let verifier = signer.verifier_key().parse().unwrap();
    let log = Arc::new(
        SiteLog::open_with_clock(
            Arc::new(InMemoryAuditLogStore::new()),
            "shop.example",
            signer,
            clock.clone(),
        )
        .await?,
    );
    let store = Arc::new(InMemoryContentStore::default());
    let history = Arc::new(InMemoryContentHistory::default());
    let events = Arc::new(Events::default());
    let base = SiteProject::new(config(), Vec::new(), clock.now());
    let pipeline = Arc::new(
        PublishingPipeline::builder(
            base,
            store.clone(),
            Arc::new(BuiltinGenerator),
            site_host,
            log,
            work.path(),
        )
        .history(history.clone())
        .announcer(events.clone())
        .changes_url("https://edge.shop.example/awp/v2/events/changes")
        .clock(clock.clone())
        .build(),
    );
    let live = Arc::new(InMemoryLiveStore::default());
    let alarms = Arc::new(InMemoryAlarms::default());
    let audit = Arc::new(InMemoryAuditRecorder::new());
    let engine = UpdateEngine::builder(pipeline.clone(), live.clone())
        .alarms(alarms.clone())
        .audit(
            audit.clone(),
            ActingContext::derived("shop.example", "steward", "owner", SubjectKind::Human),
        )
        .build()?;
    let mut launch: Vec<ContentItem> =
        (0..products).map(|i| product(i, 1_000.0 + i as f64)).collect();
    launch.extend(["privacy", "consent", "terms"].map(legal_page));
    let launched = pipeline
        .publish_batch(launch, Vec::new(), ReleaseSource::Onboarding, "urn:person:owner")
        .await?;
    assert_eq!(launched.published.unwrap().staged.manifest.seq, 1);
    engine.add_feed(mapping(), bounds()).await?;
    Ok(Shop {
        engine,
        host: directory,
        store,
        history,
        live,
        clock,
        alarms,
        events,
        audit,
        verifier,
        _dirs: (host_dir, work),
    })
}

async fn shop() -> Shop {
    shop_with(PRODUCTS, None).await.unwrap()
}

impl Shop {
    fn advance(&self, secs: u64) {
        self.clock.advance(Duration::from_secs(secs));
    }

    async fn page(&self, i: usize) -> String {
        let bytes = self.host.fetch(&format!("/products/sku-{i:03}/")).await.unwrap().unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    async fn checkpoint_size(&self) -> u64 {
        let note = self.host.fetch(CHECKPOINT_PATH).await.unwrap().unwrap();
        verify_checkpoint(
            &String::from_utf8_lossy(&note),
            "shop.example",
            std::slice::from_ref(&self.verifier),
        )
        .unwrap()
        .size
    }

    async fn manifest(&self, seq: u64) -> ReleaseManifest {
        let bytes = self.host.fetch(&manifest_path(seq)).await.unwrap().unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn update(&self, item: ContentItem, proposer: &str) -> Update {
        Update {
            item,
            at: self.clock.now(),
            entry: EntryPoint::WebEditor,
            proposer: proposer.into(),
            trigger: None,
            model_calls: 0,
        }
    }
}

#[tokio::test]
async fn stock_changes_never_release_and_prices_publish_once_per_window() {
    let shop = shop().await;
    let before = shop.page(1).await;

    // A stock-only delivery goes to the live lane: no batch, no release.
    let stock: Vec<FeedRecord> = (0..5).map(|i| record(i, &[("qty", json!(3 + i))])).collect();
    let outcome = shop.engine.ingest("supplier", stock).await.unwrap();
    assert_eq!((outcome.live, outcome.queued, outcome.held), (5, 0, false));
    shop.advance(120);
    assert!(shop.engine.tick().await.unwrap().is_empty());
    assert_eq!(shop.checkpoint_size().await, 1, "no release for stock");
    assert_eq!(shop.page(1).await, before, "the released page is unchanged");
    assert_eq!(shop.live.get(&"SKU-001".into()).await.unwrap()["stock"].value, 4);

    // Price changes coalesce for the shop's 60 s window; the latest version wins.
    let t0 = shop.clock.now();
    let prices: Vec<FeedRecord> =
        (1..4).map(|i| record(i, &[("price", json!(1_000.0 + i as f64 + 20.0))])).collect();
    shop.engine.ingest("supplier", prices).await.unwrap();
    shop.advance(20);
    shop.engine.ingest("supplier", vec![record(2, &[("price", json!(1_050.0))])]).await.unwrap();
    shop.advance(39);
    assert!(shop.engine.tick().await.unwrap().is_empty(), "the window is still open");
    shop.advance(1);
    let batches = shop.engine.tick().await.unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.release_seq, Some(2));
    assert_eq!(batch.published.len(), 3);
    assert_eq!(batch.superseded, 1);
    assert_eq!(batch.opened_at, t0);
    assert_eq!(batch.model_calls, 0, "feed updates call no model");
    assert!(batch.latencies_ms.iter().all(|ms| *ms <= 60_000), "{:?}", batch.latencies_ms);

    // One delta release, one log entry, one checkpoint, one catalog.changed.
    let manifest = shop.manifest(2).await;
    assert_eq!(manifest.base, Some(1));
    assert!(manifest.files.contains_key("/products/sku-002/index.html"));
    assert!(!manifest.files.contains_key("/products/sku-004/index.html"));
    assert_eq!(shop.checkpoint_size().await, 2);
    let events = shop.events.0.lock().unwrap().clone();
    assert_eq!(events.len(), 2, "the launch and the batch");
    let catalog = events[1].catalog.clone().unwrap();
    assert_eq!((catalog.release_seq, catalog.total, catalog.next), (2, 3, None));

    // The page shows the new price and the last released stock with its time.
    let page = shop.page(2).await;
    assert!(page.contains("1050 KES"), "{page}");
    assert!(page.contains(r#"data-awp-live="stock""#));
    assert!(page.contains("as of"), "live values carry their time with script off");
    let view = shop.host.fetch("/products/sku-002/index.awp.json").await.unwrap().unwrap();
    let view: Value = serde_json::from_slice(&view).unwrap();
    assert_eq!(view["resource"]["stock"]["value"], 5, "the live value at release time");
    assert_eq!(view["resource"]["stock"]["capability"], "check_availability");

    // The superseded version is in the content history only.
    let versions = shop.history.versions(&"SKU-002".into()).await.unwrap();
    let superseded: Vec<_> = versions.iter().filter(|v| v.meta.release_seq.is_none()).collect();
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].item.fields["price"], 1_022);

    let review = shop.engine.review().await;
    assert_eq!((review.live_updates, review.release_updates), (5, 4));
    assert_eq!((review.model_calls, review.model_share), (0, 0.0));
    let feeds = shop.engine.feeds().await;
    assert_eq!((feeds[0].deliveries, feeds[0].records, feeds[0].model_calls), (3, 9, 0));
    assert!(shop.audit.records().iter().any(|r| r.action == "site.batch.published"));
}

#[tokio::test]
async fn a_delivery_that_breaks_its_bounds_is_held_whole() {
    let shop = shop().await;
    // A corrupted delivery changes 60% of prices and some stock.
    let corrupted: Vec<FeedRecord> = (0..30)
        .map(|i| record(i, &[("price", json!(1.0 + i as f64)), ("qty", json!(0))]))
        .collect();
    let outcome = shop.engine.ingest("supplier", corrupted).await.unwrap();
    assert!(outcome.held);
    shop.advance(300);
    assert!(shop.engine.tick().await.unwrap().is_empty());
    assert_eq!(shop.checkpoint_size().await, 1, "nothing from it publishes");
    assert!(shop.live.get(&"SKU-000".into()).await.unwrap().is_empty(), "not even live values");
    let alarms = shop.alarms.all().await;
    assert_eq!(alarms.len(), 1);
    assert_eq!((alarms[0].urgency, alarms[0].code.as_str()), (Urgency::P2, "site.feed.held"));
    let summary = &alarms[0].detail["summary"];
    assert_eq!(
        (summary["itemsChanged"].as_u64(), summary["collection"].as_u64()),
        (Some(30), Some(50))
    );
    assert_eq!(summary["prices"]["outsideRule"], 30);
    let held = shop.engine.held_deliveries().await;
    assert_eq!(held.len(), 1);
    shop.engine
        .decide_delivery(held[0].delivery, Decision::Discard, "urn:person:owner")
        .await
        .unwrap();
    assert!(shop.engine.held_deliveries().await.is_empty());

    // A field bound and the rate hold a delivery too.
    let zero =
        shop.engine.ingest("supplier", vec![record(1, &[("price", json!(0))])]).await.unwrap();
    assert!(zero.held, "price > 0");
    let stock =
        shop.engine.ingest("supplier", vec![record(1, &[("qty", json!(200_000))])]).await.unwrap();
    assert!(stock.held, "stock ≤ 100,000");
    let flood: Vec<FeedRecord> = (0..1_001).map(|i| record(i % PRODUCTS, &[])).collect();
    assert!(shop.engine.ingest("supplier", flood).await.unwrap().held, "records per minute");
    assert_eq!(shop.engine.review().await.held_deliveries, 4);

    // Released by the owner, a held delivery applies whole.
    shop.advance(120);
    let fine =
        shop.engine.ingest("supplier", vec![record(7, &[("qty", json!(-1))])]).await.unwrap();
    assert!(fine.held);
    let applied = shop
        .engine
        .decide_delivery(fine.delivery, Decision::Release, "urn:person:owner")
        .await
        .unwrap();
    assert_eq!(applied.live, 1);
    assert_eq!(shop.live.get(&"SKU-007".into()).await.unwrap()["stock"].value, -1);
}

#[tokio::test]
async fn exceptions_are_held_alone_and_the_rest_publishes() {
    let shop = shop().await;
    let records = vec![
        record(1, &[("price", json!(1_151.15))]),
        record(2, &[("price", json!(1_052.1))]),
        record(3, &[("price", json!(952.85))]),
    ];
    shop.engine.ingest("supplier", records).await.unwrap();
    shop.advance(60);
    let batch = shop.engine.tick().await.unwrap().remove(0);
    assert_eq!(batch.release_seq, Some(2));
    assert_eq!(batch.published, vec![ContentId::from("SKU-002"), ContentId::from("SKU-003")]);
    assert!(matches!(batch.held[0], (ref id, HoldReason::PriceRule { .. }) if id.0 == "SKU-001"));
    assert!(shop.page(1).await.contains("1001 KES"), "the out-of-rule price is not live");

    // The proposer cannot approve its own change (A-I23); the owner can.
    let own = shop.engine.decide_item(&"SKU-001".into(), Decision::Release, "feed:supplier").await;
    assert!(matches!(own, Err(SiteError::Pipeline(ref m)) if m.contains("A-I23")));
    let approved = shop
        .engine
        .decide_item(&"SKU-001".into(), Decision::Release, "urn:person:owner")
        .await
        .unwrap();
    assert_eq!(approved[0].release_seq, Some(3), "an approved exception publishes at once");
    assert!(shop.page(1).await.contains("1151.15 KES"));

    // A new item waits for its approval chain.
    let newcomer = shop.update(product(99, 10.0), "urn:person:editor");
    assert!(shop.engine.submit(newcomer).await.unwrap().is_empty());
    shop.advance(60);
    let batch = shop.engine.tick().await.unwrap().remove(0);
    assert_eq!(batch.release_seq, None);
    assert!(matches!(batch.held[0].1, HoldReason::NewItem));
    assert_eq!(shop.engine.held_items().await.len(), 1);
}

#[tokio::test]
async fn the_fast_tier_holds_items_and_server_code_refuses_the_release() {
    let shop = shop().await;
    let mut untitled = product(4, 1_004.0);
    untitled.fields.remove("title");
    let mut renamed = product(5, 1_005.0);
    renamed.fields.insert("title".into(), json!("Panel five, renamed"));
    shop.engine.submit(shop.update(untitled, "urn:person:editor")).await.unwrap();
    shop.engine.submit(shop.update(renamed, "urn:person:editor")).await.unwrap();
    shop.advance(60);
    let batch = shop.engine.tick().await.unwrap().remove(0);
    assert_eq!(batch.published, vec![ContentId::from("SKU-005")]);
    let (id, reason) = &batch.held[0];
    assert_eq!(id.0, "SKU-004");
    assert!(
        matches!(reason, HoldReason::FastTier { findings } if findings[0].rule == "title-missing")
    );
    assert_eq!(
        shop.store.get(&"SKU-004".into()).await.unwrap().unwrap().fields["title"],
        "Panel 4",
        "the held item's working copy is restored"
    );
    assert!(shop.page(5).await.contains("renamed"));

    // Server code anywhere in a batch refuses the whole release (A-I1).
    let mut leaflet = product(6, 1_006.0);
    leaflet.media.push(awp_types::MediaRef {
        path: "content/products/sku-006/order.php".into(),
        content_type: "text/plain".into(),
        sha256: None,
        alt: None,
        alt_confirmed: false,
        generated: false,
    });
    shop.store
        .write_media("content/products/sku-006/order.php", Bytes::from_static(b"<?php echo 1;"))
        .await
        .unwrap();
    let mut fine = product(7, 1_007.0);
    fine.fields.insert("title".into(), json!("Panel seven, renamed"));
    shop.engine.submit(shop.update(leaflet, "urn:person:editor")).await.unwrap();
    shop.engine.submit(shop.update(fine, "urn:person:editor")).await.unwrap();
    shop.advance(60);
    let refused = shop.engine.tick().await.unwrap().remove(0);
    assert!(refused.refused.is_some());
    assert_eq!(refused.release_seq, None);
    assert!(!shop.page(7).await.contains("renamed"), "nothing from the batch publishes");
    assert!(shop.store.get(&"SKU-006".into()).await.unwrap().unwrap().media.is_empty());
    assert!(shop.alarms.all().await.iter().any(|a| a.code == "site.release.refused"));

    // The next batch publishes normally from the restored working copy.
    let mut later = product(8, 1_008.0);
    later.fields.insert("title".into(), json!("Panel eight, renamed"));
    shop.engine.submit(shop.update(later, "urn:person:editor")).await.unwrap();
    shop.advance(60);
    let next = shop.engine.tick().await.unwrap().remove(0);
    assert_eq!(next.release_seq, Some(3));
    assert!(shop.page(8).await.contains("renamed"));
    assert!(!shop.page(7).await.contains("renamed"));
}

#[tokio::test]
async fn a_revert_restores_one_item_and_keeps_every_other_change() {
    let shop = shop().await;
    let edit = |i: usize, title: &str| {
        let mut item = product(i, 1_000.0 + i as f64);
        item.fields.insert("title".into(), json!(title));
        shop.update(item, "urn:person:editor")
    };
    shop.engine.submit(edit(1, "Panel one, good")).await.unwrap();
    shop.engine.submit(edit(2, "Panel two, good")).await.unwrap();
    shop.advance(60);
    assert_eq!(shop.engine.tick().await.unwrap()[0].release_seq, Some(2));
    shop.engine.submit(edit(1, "Panel one, BAD EDIT")).await.unwrap();
    shop.engine.submit(edit(3, "Panel three, good")).await.unwrap();
    shop.advance(60);
    assert_eq!(shop.engine.tick().await.unwrap()[0].release_seq, Some(3));
    assert!(shop.page(1).await.contains("BAD EDIT"));

    let reverted = shop
        .engine
        .revert(&"SKU-001".into(), None, "urn:person:owner", EntryPoint::WebEditor)
        .await
        .unwrap();
    assert_eq!(reverted[0].release_seq, Some(4), "a revert publishes at once");
    assert_eq!(reverted[0].published, vec![ContentId::from("SKU-001")]);
    assert!(shop.page(1).await.contains("Panel one, good"));
    assert!(shop.page(2).await.contains("Panel two, good"), "other changes stay");
    assert!(shop.page(3).await.contains("Panel three, good"), "other changes stay");
    assert!(shop.audit.records().iter().any(|r| r.action == "site.content.reverted"));

    // After an incident rollback to release 1, the changes since are listed for replay.
    let (rollback, candidates) = shop.engine.rollback(1).await.unwrap();
    assert_eq!(rollback.staged.manifest.source, ReleaseSource::Rollback { to_seq: 1 });
    let listed: Vec<(String, u64)> =
        candidates.iter().map(|c| (c.id.0.clone(), c.release_seq)).collect();
    assert_eq!(
        listed,
        vec![("SKU-002".to_string(), 2), ("SKU-003".to_string(), 3), ("SKU-001".to_string(), 4)]
    );
    assert!(shop.page(3).await.contains("Panel 3"), "the rollback serves release 1");
    assert_eq!(
        shop.store.get(&"SKU-003".into()).await.unwrap().unwrap().fields["title"],
        "Panel 3",
        "the working copy follows the rollback"
    );
    let replayed = shop
        .engine
        .replay(&candidates[1], "urn:person:owner", EntryPoint::WebEditor)
        .await
        .unwrap();
    assert_eq!(replayed[0].published, vec![ContentId::from("SKU-003")]);
    assert!(shop.page(3).await.contains("Panel three, good"));
    assert!(!shop.page(2).await.contains("good"), "replayed one by one");
}

#[tokio::test]
async fn full_checks_run_on_schedule_and_events_page_beyond_a_thousand() {
    let shop = shop_with(1_100, None).await.unwrap();
    shop.advance(6 * 3_600);
    shop.engine.tick().await.unwrap();
    assert_eq!(shop.engine.review().await.full_checks, 1);
    assert!(
        shop.alarms.all().await.is_empty(),
        "a clean site passes its full checks: {:?}",
        shop.alarms.all().await
    );

    // One catalog.changed for a batch of 1,050 items, its ids paginated.
    for i in 0..1_050 {
        let mut item = product(i, 1_000.0 + i as f64);
        item.fields.insert("summary".into(), json!("Restocked."));
        shop.engine.submit(shop.update(item, "urn:person:owner")).await.unwrap();
    }
    shop.advance(60);
    let batch = shop.engine.tick().await.unwrap().remove(0);
    assert_eq!(batch.published.len(), 1_050);
    assert_eq!(batch.catalog_pages, 1);
    let events = shop.events.0.lock().unwrap().clone();
    let last = events.last().unwrap();
    let catalog = last.catalog.clone().unwrap();
    assert_eq!((catalog.items.len(), catalog.total), (1_000, 1_050));
    assert_eq!(
        catalog.next.as_deref(),
        Some(
            format!("https://edge.shop.example/awp/v2/events/changes/{}/2", catalog.release_seq)
                .as_str()
        )
    );
    assert_eq!(last.catalog_pages[0].items.len(), 50);
}

/// A host that publishes in place without an atomic switch.
struct InPlace(DirectoryHost);

#[async_trait]
impl SiteHost for InPlace {
    async fn publish(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport> {
        self.0.publish(release, target).await
    }
    async fn rollback(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        self.0.rollback(to).await
    }
    async fn fetch(&self, path: &str) -> Result<Option<Bytes>> {
        self.0.fetch(path).await
    }
    fn redirects_format(&self) -> RedirectsFormat {
        RedirectsFormat::Json
    }
    fn limits(&self) -> HostLimits {
        HostLimits { max_publishes_per_hour: None, delta_upload: true, atomic: Atomicity::None }
    }
    async fn publish_delta(
        &self,
        delta: &StagedDelta,
        target: HostTarget,
    ) -> Result<PublishReport> {
        self.0.publish_delta(delta, target).await
    }
}

#[tokio::test]
async fn hosts_and_lanes_that_cannot_keep_the_rules_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let in_place: Arc<dyn SiteHost> = Arc::new(InPlace(DirectoryHost::new(dir.path())));
    let error = shop_with(2, Some(in_place)).await.err().expect("refused");
    assert!(error.to_string().contains("a minute or more"), "{error}");

    // A claim in the live lane is refused at the configuration and at the build (A-I26).
    let mut config = config();
    config.lanes.get_mut(&ContentKind::Product).unwrap().volatile.push("price".into());
    assert!(config.validate().unwrap_err().to_string().contains("A-I26"));
    let shop = shop().await;
    let mut item = product(9, 1_009.0);
    item.volatile = vec!["price".into()];
    let error = shop
        .engine
        .pipeline()
        .publish_batch(vec![item], Vec::new(), ReleaseSource::Onboarding, "urn:person:owner")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("A-I26"), "{error}");

    let mut policies = BatchPolicies::default();
    policies.default.window = Duration::from_secs(1);
    assert!(policies.validate().is_err());
    let flush = Update {
        trigger: Some(Trigger::EmergencyNotice),
        ..shop.update(product(10, 1_010.0), "urn:person:owner")
    };
    let records = shop.engine.submit(flush).await.unwrap();
    assert_eq!(records.len(), 1, "an emergency notice flushes at once");
}

/// What the engine holds for the owner survives a restart (spec 3.5 §7.8.3, A-I25, A-I27):
/// a delivery held whole and an item held alone are still there for the owner's decision on
/// a new engine over the same store, and delivery numbers are never reused.
#[tokio::test]
async fn held_items_and_deliveries_survive_a_restart() {
    let shop = shop().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates/held.json");
    let engine = || {
        UpdateEngine::builder(shop.engine.pipeline().clone(), shop.live.clone())
            .held_store(Arc::new(FileHeldStore::new(&path)))
            .build()
            .unwrap()
    };

    let first = engine();
    first.add_feed(mapping(), bounds()).await.unwrap();
    let corrupted: Vec<FeedRecord> =
        (0..30).map(|i| record(i, &[("price", json!(1.0 + i as f64))])).collect();
    let held = first.ingest("supplier", corrupted).await.unwrap();
    assert!(held.held, "60% of the collection breaks the bounds");
    let mut item = shop.store.get(&"SKU-007".into()).await.unwrap().unwrap();
    item.fields.insert("price".into(), json!(5_000));
    first.submit(shop.update(item, "urn:person:editor")).await.unwrap();
    shop.advance(120);
    first.tick().await.unwrap();
    assert_eq!(first.held_items().await.len(), 1, "a price outside the rule is held alone");
    drop(first);

    let second = engine();
    second.add_feed(mapping(), bounds()).await.unwrap();
    let deliveries: Vec<u64> = second.held_deliveries().await.iter().map(|d| d.delivery).collect();
    assert_eq!(deliveries, [held.delivery]);
    let items = second.held_items().await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].update.id(), &ContentId::from("SKU-007"));
    assert!(matches!(items[0].reason, HoldReason::PriceRule { .. }));
    let next = second.ingest("supplier", vec![record(1, &[("qty", json!(4))])]).await.unwrap();
    assert!(next.delivery > held.delivery, "{} after {}", next.delivery, held.delivery);

    second.decide_item(&"SKU-007".into(), Decision::Release, "urn:person:owner").await.unwrap();
    shop.advance(120);
    second.tick().await.unwrap();
    assert!(shop.page(7).await.contains("5000 KES"), "the owner's release publishes");
    second.decide_delivery(held.delivery, Decision::Discard, "urn:person:owner").await.unwrap();
    drop(second);

    let third = engine();
    assert!(third.held_items().await.is_empty() && third.held_deliveries().await.is_empty());
}

#[tokio::test]
async fn held_stores_pass_the_conformance_suite() {
    let shop = shop().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("held.json");
    let engine = UpdateEngine::builder(shop.engine.pipeline().clone(), shop.live.clone())
        .held_store(Arc::new(FileHeldStore::new(&path)))
        .build()
        .unwrap();
    engine.add_feed(mapping(), bounds()).await.unwrap();
    let corrupted: Vec<FeedRecord> =
        (0..30).map(|i| record(i, &[("price", json!(2.0 + i as f64))])).collect();
    assert!(engine.ingest("supplier", corrupted).await.unwrap().held);
    let mut item = shop.store.get(&"SKU-003".into()).await.unwrap().unwrap();
    item.fields.insert("price".into(), json!(9_000));
    engine.submit(shop.update(item, "urn:person:editor")).await.unwrap();
    shop.advance(120);
    engine.tick().await.unwrap();
    let sample = FileHeldStore::new(&path).load().await.unwrap();
    assert!(!sample.items.is_empty() && !sample.deliveries.is_empty(), "{sample:?}");

    held_store_conformance(&InMemoryHeldStore::default(), &sample).await;
    held_store_conformance(&FileHeldStore::new(dir.path().join("other/held.json")), &sample).await;
}

/// A live store that refuses every write.
struct Unwritable;

#[async_trait]
impl LiveStore for Unwritable {
    async fn set(
        &self,
        _id: &ContentId,
        _values: BTreeMap<String, adk_site::live::LiveValue>,
    ) -> Result<()> {
        Err(SiteError::Pipeline("the live store is down".into()))
    }
    async fn get(&self, _id: &ContentId) -> Result<BTreeMap<String, adk_site::live::LiveValue>> {
        Ok(BTreeMap::new())
    }
}

/// An owner's release that fails to take effect leaves the delivery held, in memory and in
/// the store, so the decision can be made again (spec 3.5 A-I27).
#[tokio::test]
async fn a_release_that_fails_keeps_the_delivery_held() {
    let shop = shop().await;
    let held_store = Arc::new(InMemoryHeldStore::default());
    let engine = UpdateEngine::builder(shop.engine.pipeline().clone(), Arc::new(Unwritable))
        .held_store(held_store.clone())
        .build()
        .unwrap();
    engine.add_feed(mapping(), bounds()).await.unwrap();
    let records: Vec<FeedRecord> = (0..30)
        .map(|i| record(i, &[("price", json!(3.0 + i as f64)), ("qty", json!(1))]))
        .collect();
    let held = engine.ingest("supplier", records).await.unwrap();
    assert!(held.held);
    let error = engine.decide_delivery(held.delivery, Decision::Release, "urn:person:owner").await;
    assert!(error.is_err(), "the live store refuses the delivery's stock values");
    let still: Vec<u64> = engine.held_deliveries().await.iter().map(|d| d.delivery).collect();
    assert_eq!(still, [held.delivery]);
    assert_eq!(held_store.load().await.unwrap().deliveries.len(), 1);
}
