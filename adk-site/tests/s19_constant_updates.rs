//! Scenario S19, "A site that never stops changing" (spec 3.5 §5, §7.8.3, §11 "Soak"), on a
//! warped clock against `DirectoryHost`.
//!
//! A shop fed by its supplier every minute (2% stock and 0.5% price changes per minute), an
//! election-night live blog, a corrupted feed delivery at hour three that changes 60% of the
//! prices, one bad product edit that is reverted, and the Guardian probing the host and the
//! origin while releases go out. The product fixture is generated here.
//!
//! | Test | Scale | Runs |
//! |------|-------|------|
//! | `s19_scaled` | 600 products, 4 hours, 120 entries over an hour | Every PR (`--features migrate`) |
//! | `s19_full_day` | 50,000 products, 24 hours, 600 entries over six hours | Nightly with `ADK_SITE_S19_FULL=1`, optimised build |
#![cfg(feature = "migrate")]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::Clock;
use adk_core::audit_log::InMemoryAuditLogStore;
use adk_site::batch::Update;
use adk_site::content::{
    ContentHistory, ContentStore, InMemoryContentHistory, InMemoryContentStore, SiteConfig,
    SiteProject,
};
use adk_site::feed::{FeedMapping, FeedRecord, FieldMapping, FieldType};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::{DirectoryHost, SiteHost};
use adk_site::lanes::KindLanes;
use adk_site::live::InMemoryLiveStore;
use adk_site::log::{CHECKPOINT_PATH, SiteLog, verify_checkpoint};
use adk_site::migrate::claim::{DomainClaim, RecordKind, StaticDnsResolver};
use adk_site::migrate::http::{EgressScope, HttpClient};
use adk_site::migrate::probe::{ProbeConfig, Prober};
use adk_site::pipeline::{Announcement, Announcer, EntryPoint, PublishingPipeline};
use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
use adk_site::stream::{Ed25519EntrySigner, EntrySigner, StreamLane, verify_entry};
use adk_site::updates::{BatchRecord, CostRates, InMemoryAlarms, UpdateEngine};
use async_trait::async_trait;
use awp_types::{
    Bound, ContentId, ContentItem, ContentKind, ContentState, FeedBounds, Markdown, PriceRule,
    Rate, ReleaseSource, StreamEntry, Urgency,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};

const DOMAIN: &str = "shop.example";
const STORY: &str = "articles/election-night/en";

/// How large a run is.
#[derive(Debug, Clone, Copy)]
struct Scale {
    products: usize,
    hours: u64,
    corrupt_at_hour: u64,
    stream_entries: u64,
    stream_start_minute: u64,
    stream_minutes: u64,
    bad_edit_minute: u64,
    revert_minute: u64,
    probe_every_minutes: u64,
    retain: u64,
}

/// A clock that jumps forward on the scenario's schedule and runs at real speed in between,
/// so the time a publish takes counts towards approval-to-live.
#[derive(Debug)]
struct WarpClock(Mutex<(DateTime<Utc>, Instant)>);

impl WarpClock {
    fn new(at: DateTime<Utc>) -> Self {
        Self(Mutex::new((at, Instant::now())))
    }

    fn jump(&self, to: DateTime<Utc>) {
        let now = self.now();
        *self.0.lock().unwrap() = (to.max(now), Instant::now());
    }
}

impl Clock for WarpClock {
    fn now(&self) -> DateTime<Utc> {
        let (at, since) = *self.0.lock().unwrap();
        at + chrono::Duration::from_std(since.elapsed()).unwrap_or_default()
    }
}

/// A deterministic generator for the fixture and the feed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Two subscribers behind the edge, each receiving every `catalog.changed`.
#[derive(Default)]
struct Subscribers(Mutex<[Vec<(u64, u64)>; 2]>, Mutex<Vec<Announcement>>);

#[async_trait]
impl Announcer for Subscribers {
    async fn announce(&self, announcement: &Announcement) -> adk_site::Result<()> {
        if let Some(catalog) = &announcement.catalog {
            for inbox in self.0.lock().unwrap().iter_mut() {
                inbox.push((catalog.release_seq, catalog.total));
            }
        }
        self.1.lock().unwrap().push(announcement.clone());
        Ok(())
    }
}

fn sku(i: usize) -> String {
    format!("P{i:05}")
}

fn price_value(price: f64) -> Value {
    let cents = (price * 100.0).round();
    if cents % 100.0 == 0.0 { json!((cents / 100.0) as i64) } else { json!(cents / 100.0) }
}

fn product(i: usize, price: f64) -> ContentItem {
    ContentItem {
        id: ContentId(sku(i)),
        kind: ContentKind::Product,
        slug: sku(i).to_lowercase(),
        language: "en".parse().unwrap(),
        fields: BTreeMap::from([
            ("title".to_string(), json!(format!("Panel {i}"))),
            ("price".to_string(), price_value(price)),
            ("currency".to_string(), json!("KES")),
            ("stock".to_string(), json!(50)),
        ]),
        volatile: Vec::new(),
        body: None,
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Published,
    }
}

fn page(id: &str, kind: ContentKind, slug: &str, fields: Value) -> ContentItem {
    ContentItem {
        id: id.into(),
        kind,
        slug: slug.into(),
        language: "en".parse().unwrap(),
        fields: serde_json::from_value(fields).unwrap(),
        volatile: Vec::new(),
        body: Some(Markdown("Text.".into())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Published,
    }
}

fn config(products: usize) -> SiteConfig {
    let mut config = SiteConfig::new("Solar Shop", DOMAIN, vec!["en".parse().unwrap()]);
    config.lanes.insert(
        ContentKind::Product,
        KindLanes { volatile: vec!["stock".into()], ..KindLanes::default() },
    );
    config.lanes.insert(
        ContentKind::Article,
        KindLanes { stream: vec!["entries".into()], ..KindLanes::default() },
    );
    config.live_capabilities.insert("stock".into(), "check_availability".into());
    config.page_size = (products / 100).max(50);
    config
}

/// Serves what the host serves, as an edge in front of it does. Aborted on drop.
struct Origin {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn origin(host: Arc<DirectoryHost>) -> Origin {
    use axum::response::IntoResponse;
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let host = host.clone();
        async move {
            match host.fetch(uri.path()).await {
                Ok(Some(bytes)) => bytes.to_vec().into_response(),
                _ => axum::http::StatusCode::NOT_FOUND.into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Origin { url, task }
}

async fn prober(host: Arc<DirectoryHost>, origin: &Origin, release_key: &str) -> Prober {
    let claim = DomainClaim::new(DOMAIN, Utc::now()).unwrap();
    let (name, value) = claim.dns_record();
    let dns = Arc::new(StaticDnsResolver::new());
    dns.set(&name, RecordKind::Txt, [value]).await;
    let proven = claim.verify_dns(dns.as_ref(), Utc::now()).await.unwrap();
    let public = url::Url::parse(&format!("https://{DOMAIN}")).unwrap();
    let http = HttpClient::builder()
        .egress(EgressScope::Only(BTreeSet::from([origin.url.clone()])))
        .map_origin(&public, url::Url::parse(&origin.url).unwrap())
        .min_interval(Duration::ZERO)
        .build()
        .unwrap();
    let mut config = ProbeConfig::new(DOMAIN, public);
    config.headers = Vec::new();
    Prober::new(
        host as Arc<dyn SiteHost>,
        Arc::new(http),
        dns,
        vec![release_key.parse().unwrap()],
        &proven,
        config,
    )
    .unwrap()
}

fn percentile(values: &mut [i64], p: f64) -> i64 {
    values.sort_unstable();
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    values[rank.clamp(1, values.len()) - 1]
}

fn dir_bytes(dir: &std::path::Path) -> (u64, u64) {
    let mut total = (0, 0);
    let Ok(entries) = std::fs::read_dir(dir) else { return total };
    for entry in entries.flatten() {
        let meta = entry.metadata().unwrap();
        if meta.is_dir() {
            let (bytes, files) = dir_bytes(&entry.path());
            total = (total.0 + bytes, total.1 + files);
        } else {
            total = (total.0 + meta.len(), total.1 + 1);
        }
    }
    total
}

#[allow(clippy::too_many_lines)]
async fn run(scale: Scale) {
    let started = Instant::now();
    let t0 = Utc.with_ymd_and_hms(2026, 11, 3, 0, 0, 0).unwrap();
    let clock = Arc::new(WarpClock::new(t0));
    let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let host = Arc::new(
        DirectoryHost::new(dirs.0.path()).materialise(false).retain_releases(scale.retain),
    );
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed(DOMAIN, [19; 32]).unwrap());
    let release_key = signer.verifier_key();
    let log = Arc::new(
        SiteLog::open_with_clock(
            Arc::new(InMemoryAuditLogStore::new()),
            DOMAIN,
            signer,
            clock.clone(),
        )
        .await
        .unwrap(),
    );
    let store = Arc::new(InMemoryContentStore::default());
    let history = Arc::new(InMemoryContentHistory::default());
    let subscribers = Arc::new(Subscribers::default());
    let pipeline = Arc::new(
        PublishingPipeline::builder(
            SiteProject::new(config(scale.products), Vec::new(), clock.now()),
            store.clone(),
            Arc::new(BuiltinGenerator),
            host.clone(),
            log,
            dirs.1.path(),
        )
        .history(history.clone())
        .announcer(subscribers.clone())
        .changes_url("https://edge.shop.example/awp/v2/events/changes")
        .clock(clock.clone())
        .build(),
    );
    let edge_key =
        Arc::new(Ed25519EntrySigner::from_seed("edge-receipt-2026-11", [7; 32]).unwrap());
    let alarms = Arc::new(InMemoryAlarms::default());
    let engine = UpdateEngine::builder(pipeline.clone(), Arc::new(InMemoryLiveStore::default()))
        .streams(Arc::new(StreamLane::new(edge_key.clone())))
        .alarms(alarms.clone())
        .costs(CostRates { per_model_call_micros: 2_000, per_release_micros: 50 })
        .build()
        .unwrap();

    // The generated fixture: products, legal pages and the live story, released as one.
    let mut prices: Vec<f64> = (0..scale.products).map(|i| 1_000.0 + (i % 500) as f64).collect();
    let mut launch: Vec<ContentItem> = (0..scale.products).map(|i| product(i, prices[i])).collect();
    for kind in ["privacy", "consent", "terms"] {
        launch.push(page(
            &format!("pages/{kind}"),
            ContentKind::Page,
            kind,
            json!({ "title": kind, "legal": kind }),
        ));
    }
    launch.push(page(
        STORY,
        ContentKind::Article,
        "election-night",
        json!({ "title": "Election night, live", "publishedAt": "2026-11-03T00:00:00Z" }),
    ));
    let launched = pipeline
        .publish_batch(launch, Vec::new(), ReleaseSource::Onboarding, "urn:person:owner")
        .await
        .unwrap();
    assert!(launched.published.is_some());
    let launch_time = started.elapsed();

    let mapping = FeedMapping::new("supplier", ContentKind::Product, "en".parse().unwrap(), "sku")
        .field(
            "price",
            FieldMapping::new("price", FieldType::Number).bound(Bound::Range {
                minimum: None,
                exclusive_minimum: Some(0.0),
                maximum: None,
                exclusive_maximum: None,
            }),
        )
        .field("stock", FieldMapping::new("qty", FieldType::Integer));
    let bounds = FeedBounds {
        max_change_fraction: FeedBounds::DEFAULT_MAX_CHANGE_FRACTION,
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
        // The corrupted delivery breaks the collection share, the bound S19 exercises.
        max_rate: Rate { per_minute: scale.products as u32 },
    };
    engine.add_feed(mapping, bounds).await.unwrap();

    let origin = origin(host.clone()).await;
    let guardian = prober(host.clone(), &origin, &release_key).await;

    let mut rng = Rng(0x5319_2026_1103_0001);
    let stock_per_minute = scale.products * 2 / 100;
    let prices_per_minute = (scale.products * 5 / 1_000).max(1);
    let entry_gap = scale.stream_minutes * 60 * 1_000 / scale.stream_entries;
    let story = ContentId::from(STORY);
    let mut records: Vec<BatchRecord> = Vec::new();
    // Each delivery's time → the products whose price it changed.
    let mut price_windows: BTreeMap<DateTime<Utc>, BTreeSet<ContentId>> = BTreeMap::new();
    let mut corrupted: BTreeMap<usize, f64> = BTreeMap::new();
    let mut entry_latency_ms: Vec<u128> = Vec::new();
    let mut appended = 0u64;
    let mut probes = 0u64;
    let mut stock_updates = 0u64;
    let mut price_updates = 0u64;
    let bad = 0usize;
    for minute in 0..scale.hours * 60 {
        let at = t0 + chrono::Duration::minutes(minute as i64 + 1);
        // Stream entries due before this minute's delivery, each at its own time.
        let stream_start = t0 + chrono::Duration::minutes(scale.stream_start_minute as i64);
        if minute == scale.stream_start_minute {
            clock.jump(stream_start);
            engine.open_stream(&story, "entries").await.unwrap();
        }
        while appended < scale.stream_entries && minute >= scale.stream_start_minute {
            let due = stream_start + chrono::Duration::milliseconds((appended * entry_gap) as i64);
            if due >= at {
                break;
            }
            clock.jump(due);
            let sent = Instant::now();
            let entry = engine
                .append_entry(&story, &format!("Results from district {}.", appended + 1), None)
                .await
                .unwrap();
            let feed: Vec<StreamEntry> = engine.stream_since(&story, entry.n - 1).await.unwrap();
            assert_eq!(feed[0].n, entry.n, "the edge feed serves the entry");
            entry_latency_ms.push(sent.elapsed().as_millis());
            appended += 1;
            if appended == scale.stream_entries {
                engine.close_stream(&story).await.unwrap();
            }
            records.extend(engine.tick().await.unwrap());
        }
        clock.jump(at);

        // The supplier's delivery: 2% stock and 0.5% price changes.
        let mut delivery: BTreeMap<usize, Vec<(&str, Value)>> = BTreeMap::new();
        for _ in 0..stock_per_minute {
            let i = rng.below(scale.products as u64) as usize;
            delivery.entry(i).or_default().push(("qty", json!(rng.below(200))));
        }
        let mut priced = BTreeSet::new();
        for _ in 0..prices_per_minute {
            let i = 1 + rng.below(scale.products as u64 - 1) as usize;
            let pct = 1.0 + (rng.below(9) as f64 - 4.0) / 100.0;
            let pct = if pct == 1.0 { 1.02 } else { pct };
            prices[i] = (prices[i] * pct * 100.0).round() / 100.0;
            delivery.entry(i).or_default().retain(|(k, _)| *k != "price");
            delivery.entry(i).or_default().push(("price", price_value(prices[i])));
            priced.insert(ContentId(sku(i)));
        }
        stock_updates += stock_per_minute as u64;
        price_updates += priced.len() as u64;
        let feed: Vec<FeedRecord> = delivery
            .into_iter()
            .map(|(i, pairs)| {
                let mut fields = BTreeMap::from([("sku".to_string(), json!(sku(i)))]);
                fields.extend(pairs.into_iter().map(|(k, v)| (k.to_string(), v)));
                FeedRecord(fields)
            })
            .collect();
        price_windows.insert(clock.now(), priced);
        let outcome = engine.ingest("supplier", feed).await.unwrap();
        assert!(!outcome.held, "an ordinary delivery proceeds at minute {minute}");
        records.extend(outcome.batches);

        // Hour three: a corrupted delivery changes 60% of the prices.
        if minute == scale.corrupt_at_hour * 60 {
            let count = scale.products * 6 / 10;
            let corrupt: Vec<FeedRecord> = (0..count)
                .map(|i| {
                    corrupted.insert(i, (prices[i] * 0.4 * 100.0).round() / 100.0);
                    FeedRecord(BTreeMap::from([
                        ("sku".to_string(), json!(sku(i))),
                        ("price".to_string(), price_value(corrupted[&i])),
                    ]))
                })
                .collect();
            let held = engine.ingest("supplier", corrupt).await.unwrap();
            assert!(held.held, "the corrupted delivery is held whole");
        }

        // One bad product edit, reverted later.
        if minute == scale.bad_edit_minute {
            let mut broken = store.get(&ContentId(sku(bad))).await.unwrap().unwrap();
            broken.fields.insert("title".into(), json!("Panel 0 — WRONG SPECIFICATIONS"));
            let closed = engine
                .submit(Update {
                    item: broken,
                    at: clock.now(),
                    entry: EntryPoint::WebEditor,
                    proposer: "urn:person:editor".into(),
                    trigger: None,
                    model_calls: 0,
                })
                .await
                .unwrap();
            records.extend(closed);
        }
        if minute == scale.revert_minute {
            records.extend(
                engine
                    .revert(&ContentId(sku(bad)), None, "urn:person:owner", EntryPoint::WebEditor)
                    .await
                    .unwrap(),
            );
        }

        // Releases go out; the Guardian probes while they do.
        if minute % scale.probe_every_minutes == 0 {
            let (report, published) = tokio::join!(guardian.probe(), engine.tick());
            let report = report.unwrap();
            assert!(
                !report.drift_detected(),
                "false drift at minute {minute}: {:#?}",
                report.drift
            );
            records.extend(published.unwrap());
            probes += 1;
        } else {
            records.extend(engine.tick().await.unwrap());
        }
    }
    clock.jump(t0 + chrono::Duration::minutes(scale.hours as i64 * 60 + 2));
    records.extend(engine.tick().await.unwrap());
    records.extend(engine.drain().await.unwrap());
    let last = guardian.probe().await.unwrap();
    assert!(!last.drift_detected(), "{:#?}", last.drift);
    probes += 1;

    // Stock changes never trigger a release, and price changes publish once per window:
    // every product a release carries had its price changed by a delivery inside that
    // release's window, and no product publishes more often than its price changed.
    let released: Vec<&BatchRecord> = records.iter().filter(|r| r.release_seq.is_some()).collect();
    let second = chrono::Duration::seconds(1);
    let mut changes: BTreeMap<&ContentId, u64> = BTreeMap::new();
    for priced in price_windows.values() {
        for id in priced {
            *changes.entry(id).or_default() += 1;
        }
    }
    let mut publications: BTreeMap<&ContentId, u64> = BTreeMap::new();
    for record in &released {
        let window = price_windows.range(record.opened_at - second..=record.closed_at + second);
        let priced: BTreeSet<&ContentId> = window.flat_map(|(_, ids)| ids).collect();
        for id in &record.published {
            *publications.entry(id).or_default() += 1;
            let explained = id.0 == STORY || id.0 == sku(bad) || priced.contains(id);
            assert!(
                explained,
                "{id} was released by batch {} without a price change",
                record.batch
            );
        }
    }
    for (id, changed) in &changes {
        let published = publications.get(id).copied().unwrap_or_default();
        assert!(
            (1..=*changed).contains(&published),
            "{id}: {changed} changes, {published} releases"
        );
    }
    let feed_batches = released
        .iter()
        .filter(|r| r.published.iter().any(|id| id.0.starts_with('P') && id.0 != sku(bad)))
        .count() as u64;
    assert!(
        (scale.hours * 30..=scale.hours * 60).contains(&feed_batches),
        "{feed_batches} feed releases for {} deliveries",
        scale.hours * 60
    );
    let mut latencies: Vec<i64> = released.iter().flat_map(|r| r.latencies_ms.clone()).collect();
    let p50 = percentile(&mut latencies, 50.0);
    let p95 = percentile(&mut latencies, 95.0);
    assert!(p95 < 120_000, "p95 approval-to-live {p95} ms is over the 60 s window plus 60 s");

    // The corrupted delivery was held whole with a P2 alarm; none of its prices published.
    let held_alarms: Vec<_> =
        alarms.all().await.into_iter().filter(|a| a.code == "site.feed.held").collect();
    assert_eq!(held_alarms.len(), 1);
    assert_eq!(held_alarms[0].urgency, Urgency::P2);
    for (i, bad_price) in &corrupted {
        let item = store.get(&ContentId(sku(*i))).await.unwrap().unwrap();
        assert_ne!(
            item.fields["price"],
            price_value(*bad_price),
            "{} published a corrupted price",
            sku(*i)
        );
        assert_eq!(
            item.fields["price"],
            price_value(prices[*i]),
            "{} holds its latest price",
            sku(*i)
        );
    }
    assert_eq!(engine.held_deliveries().await.len(), 1, "it waits for the owner");

    // Live-blog entries appear within 10 s; a signed fold lands every five minutes and at
    // the close, and every entry ends up in a signed release.
    assert_eq!(appended, scale.stream_entries);
    assert!(entry_latency_ms.iter().all(|ms| *ms < 10_000));
    let folds: Vec<&BatchRecord> =
        released.iter().filter(|r| r.published.iter().any(|id| id.0 == STORY)).copied().collect();
    let stream_status = engine.streams().await.remove(0);
    assert_eq!(stream_status.folded, scale.stream_entries);
    assert_eq!(folds.len() as u64, stream_status.folds);
    let expected_folds = scale.stream_minutes / 5;
    assert!(
        (expected_folds - 1..=expected_folds + 1).contains(&stream_status.folds),
        "{} folds for a {}-minute stream",
        stream_status.folds,
        scale.stream_minutes
    );
    for pair in folds.windows(2) {
        let gap = pair[1].opened_at - pair[0].opened_at;
        assert!(gap <= chrono::Duration::seconds(5 * 60 + 70), "folds {gap} apart");
    }
    let closed = stream_status.closed_at.unwrap();
    assert!(folds.last().unwrap().opened_at >= closed, "a fold lands at the close");
    let final_story = store.get(&story).await.unwrap().unwrap();
    let entries: Vec<StreamEntry> =
        serde_json::from_value(final_story.fields["entries"].clone()).unwrap();
    assert_eq!(entries.len() as u64, scale.stream_entries);
    for entry in &entries {
        verify_entry(entry, &edge_key.public_key()).unwrap();
    }
    let page = host.fetch("/articles/election-night/").await.unwrap().unwrap();
    assert!(
        String::from_utf8_lossy(&page)
            .contains(&format!("Results from district {}.", scale.stream_entries))
    );

    // The revert restored one product and kept every other change.
    let reverted = store.get(&ContentId(sku(bad))).await.unwrap().unwrap();
    assert_eq!(reverted.fields["title"], "Panel 0");
    let versions = history.versions(&ContentId(sku(bad))).await.unwrap();
    assert!(versions.iter().any(|v| v.item.fields["title"] == "Panel 0 — WRONG SPECIFICATIONS"));
    for (i, price) in prices.iter().enumerate().skip(1) {
        let item = store.get(&ContentId(sku(i))).await.unwrap().unwrap();
        assert_eq!(item.fields["price"], price_value(*price), "{} kept its change", sku(i));
    }

    // Each subscriber got one catalog.changed per batch.
    let inboxes = subscribers.0.lock().unwrap().clone();
    let with_items = released.iter().filter(|r| !r.published.is_empty()).count() + 1;
    for inbox in &inboxes {
        assert_eq!(inbox.len(), with_items, "one event per batch, the launch included");
        let seqs: BTreeSet<u64> = inbox.iter().map(|(seq, _)| *seq).collect();
        assert_eq!(seqs.len(), inbox.len(), "never two for one release");
    }

    // Feed updates made no model calls; the weekly review prices updates.
    let review = engine.review().await;
    assert_eq!(review.model_calls, 0);
    assert_eq!(review.model_share, 0.0);
    assert!(released.iter().all(|r| r.model_calls == 0));
    assert!(review.live_updates > 0 && review.live_updates <= stock_updates);
    assert!(review.cost_per_thousand_updates_micros > 0.0);

    // One log entry and checkpoint per release.
    let note = host.fetch(CHECKPOINT_PATH).await.unwrap().unwrap();
    let checkpoint =
        verify_checkpoint(&String::from_utf8_lossy(&note), DOMAIN, &[release_key.parse().unwrap()])
            .unwrap();
    assert_eq!(checkpoint.size, released.len() as u64 + 1, "the launch and one entry per batch");

    let (manifest_bytes, manifests) = dir_bytes(&dirs.0.path().join("manifests"));
    let (tile_bytes, tiles) = dir_bytes(&dirs.0.path().join("log"));
    let (checkpoint_bytes, _) = dir_bytes(&dirs.0.path().join("checkpoints"));
    let (object_bytes, objects) = dir_bytes(&dirs.0.path().join("objects"));
    let report = json!({
        "products": scale.products,
        "simulatedHours": scale.hours,
        "releases": checkpoint.size,
        "deliveries": review.deliveries,
        "heldDeliveries": review.held_deliveries,
        "stockUpdates": stock_updates,
        "priceUpdates": price_updates,
        "liveUpdates": review.live_updates,
        "releaseUpdates": review.release_updates,
        "approvalToLiveMs": { "p50": p50, "p95": p95 },
        "streamEntries": appended,
        "folds": stream_status.folds,
        "maxEntryLatencyMs": entry_latency_ms.iter().max(),
        "probes": probes,
        "falseDriftAlarms": 0,
        "modelCalls": review.model_calls,
        "costPerThousandUpdatesMicros": review.cost_per_thousand_updates_micros,
        "siteLog": { "manifests": manifests, "manifestBytes": manifest_bytes, "tiles": tiles, "tileBytes": tile_bytes, "checkpointBytes": checkpoint_bytes },
        "hostObjects": { "files": objects, "bytes": object_bytes },
        "launchSeconds": launch_time.as_secs_f64(),
        "wallSeconds": started.elapsed().as_secs_f64(),
    });
    println!("S19 {report}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s19_scaled() {
    run(Scale {
        products: 600,
        hours: 4,
        corrupt_at_hour: 3,
        stream_entries: 120,
        stream_start_minute: 30,
        stream_minutes: 60,
        bad_edit_minute: 100,
        revert_minute: 150,
        probe_every_minutes: 30,
        retain: 20,
    })
    .await;
}

/// The nightly soak (spec 3.5 §11): build optimised and set `ADK_SITE_S19_FULL=1`:
///
/// ```bash
/// ADK_SITE_S19_FULL=1 cargo nextest run -p adk-site --features migrate --cargo-profile ci \
///   --run-ignored only -E 'test(s19_full_day)' --no-capture
/// ```
///
/// `ADK_SITE_S19_PRODUCTS` runs the same day — 24 hours, the six-hour live blog, the
/// corrupted delivery and the revert — over a smaller catalogue, so the day's timings
/// reproduce locally in minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly soak: 50,000 products over 24 simulated hours; set ADK_SITE_S19_FULL=1"]
async fn s19_full_day() {
    if std::env::var("ADK_SITE_S19_FULL").is_err() {
        return;
    }
    let products = std::env::var("ADK_SITE_S19_PRODUCTS")
        .map_or(50_000, |n| n.parse().expect("ADK_SITE_S19_PRODUCTS is a number of products"));
    run(Scale {
        products,
        hours: 24,
        corrupt_at_hour: 3,
        stream_entries: 600,
        stream_start_minute: 18 * 60,
        stream_minutes: 360,
        bad_edit_minute: 4 * 60,
        revert_minute: 5 * 60,
        probe_every_minutes: 30,
        retain: 20,
    })
    .await;
}
