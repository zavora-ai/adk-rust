//! S19 — a site that never stops changing (spec 3.5 §5 S19, §7.8.3, §7.20, §7.21, §11 "Soak",
//! A-I26, A-I27), on a warped clock.
//!
//! A shop fed by its supplier every minute (2% stock and 0.5% price changes per minute), an
//! election-night live blog, a corrupted feed delivery at hour three that changes 60% of the
//! prices, one bad product edit that is reverted, and the Guardian's probe running against the
//! host and the origin while releases go out. The site is the product's: its runtime, its
//! update engine on the runtime's shared pipeline, the engine's alarms on the site's alarm
//! router to the owner's phone, the Guardian's probe and incident path, and the weekly review.
//!
//! Pass condition: stock changes never trigger a release; price changes publish once per
//! batch window with p95 approval-to-live under the window plus 60 s; the corrupted batch is
//! held whole with a P2 alarm and no price from it publishes; live-blog entries appear within
//! 10 s and a signed fold lands every five minutes and at the close; the revert restores one
//! product and keeps every other change; the Guardian raises no false drift alarm; each
//! subscriber gets one `catalog.changed` per batch; feed updates make no model calls; the
//! weekly review reports cost per thousand updates.
//!
//! [`Scale::SCALED`] runs in the pull-request tier; [`Scale::FULL_DAY`] is the nightly soak
//! (`website-scenarios soak`), which writes its report.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::Clock;
use adk_core::audit_log::InMemoryAuditLogStore;
use adk_harness::website::{SiteRef, SiteRuntime};
use adk_kernel::charter::{
    AutonomyLadder, InMemoryAutonomyStore, InMemoryCharterStore, InMemoryKpiStore, KpiTracker,
};
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
use adk_site::migrate::probe::ProbeConfig;
use adk_site::pipeline::{Announcement, Announcer, EntryPoint};
use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
use adk_site::stream::{Ed25519EntrySigner, EntrySigner, StreamLane, verify_entry};
use adk_site::updates::{BatchRecord, CostRates, UpdateEngine};
use adk_website::alarms::AlarmRouter;
use adk_website::deploy::updates::SiteAlarms;
use adk_website::incident::{GuardianProbe, InMemoryStatusBoard, IncidentPath};
use adk_website::kinds::{CharterInputs, template};
use adk_website::review::{ReviewComposer, ReviewInputs};
use adk_website::team::SiteTeam;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use awp_types::{
    Bound, Channel, ContentId, ContentItem, ContentKind, ContentState, FeedBounds, Markdown,
    PriceRule, Rate, ReleaseSource, SiteKind, StreamEntry,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};

use crate::checks::Checks;
use crate::fixture::Phone;
use crate::live::Mode;

const DOMAIN: &str = "shop.example";
const STORY: &str = "articles/election-night/en";

/// How large a run is.
#[derive(Debug, Clone, Copy)]
pub struct Scale {
    /// Products in the catalogue.
    pub products: usize,
    /// Simulated hours.
    pub hours: u64,
    /// The hour of the corrupted delivery.
    pub corrupt_at_hour: u64,
    /// Live-blog entries.
    pub stream_entries: u64,
    /// When the live blog opens, in minutes.
    pub stream_start_minute: u64,
    /// How long it runs, in minutes.
    pub stream_minutes: u64,
    /// When the bad edit lands, in minutes.
    pub bad_edit_minute: u64,
    /// When it is reverted, in minutes.
    pub revert_minute: u64,
    /// The Guardian's probe interval, in minutes.
    pub probe_every_minutes: u64,
    /// Releases the host retains.
    pub retain: u64,
}

impl Scale {
    /// The pull-request tier's run: 600 products over four simulated hours.
    pub const SCALED: Self = Self {
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
    };
    /// The nightly soak: 50,000 products over 24 simulated hours, 600 entries over six.
    pub const FULL_DAY: Self = Self {
        products: 50_000,
        hours: 24,
        corrupt_at_hour: 3,
        stream_entries: 600,
        stream_start_minute: 18 * 60,
        stream_minutes: 360,
        bad_edit_minute: 4 * 60,
        revert_minute: 5 * 60,
        probe_every_minutes: 30,
        retain: 20,
    };
}

/// A clock that jumps forward on the scenario's schedule and runs at real speed in between,
/// so the time a publish takes counts towards approval-to-live.
#[derive(Debug)]
struct WarpClock(Mutex<(DateTime<Utc>, Instant)>);

impl WarpClock {
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
struct Subscribers(Mutex<[Vec<(u64, u64)>; 2]>);

#[async_trait]
impl Announcer for Subscribers {
    async fn announce(&self, announcement: &Announcement) -> adk_site::Result<()> {
        if let Some(catalog) = &announcement.catalog {
            for inbox in self.0.lock().unwrap().iter_mut() {
                inbox.push((catalog.release_seq, catalog.total));
            }
        }
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

fn item(id: &str, kind: ContentKind, slug: &str, fields: Value, body: Option<&str>) -> ContentItem {
    ContentItem {
        id: id.into(),
        kind,
        slug: slug.into(),
        language: "en".parse().expect("a language"),
        fields: serde_json::from_value(fields).unwrap_or_default(),
        volatile: Vec::new(),
        body: body.map(|b| Markdown(b.into())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Published,
    }
}

fn product(i: usize, price: f64) -> ContentItem {
    item(
        &sku(i),
        ContentKind::Product,
        &sku(i).to_lowercase(),
        json!({ "title": format!("Panel {i}"), "price": price_value(price), "currency": "KES", "stock": 50 }),
        None,
    )
}

fn config(products: usize) -> SiteConfig {
    let mut config = SiteConfig::new("Solar Shop", DOMAIN, vec!["en".parse().expect("a language")]);
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

async fn origin(host: Arc<DirectoryHost>) -> Result<Origin> {
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Origin { url, task })
}

fn percentile(values: &mut [i64], p: f64) -> i64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    values[rank.clamp(1, values.len()) - 1]
}

fn dir_bytes(dir: &std::path::Path) -> (u64, u64) {
    let mut total = (0, 0);
    let Ok(entries) = std::fs::read_dir(dir) else { return total };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (bytes, files) = dir_bytes(&entry.path());
            total = (total.0 + bytes, total.1 + files);
        } else {
            total = (total.0 + meta.len(), total.1 + 1);
        }
    }
    total
}

/// Runs S19 at the pull-request tier's scale.
pub async fn run(_mode: Mode) -> Result<Checks> {
    Ok(soak(Scale::SCALED).await?.0)
}

/// Runs S19 at `scale`; returns the checks and the soak report.
#[allow(clippy::too_many_lines)]
pub async fn soak(scale: Scale) -> Result<(Checks, Value)> {
    let mut checks = Checks::default();
    let started = Instant::now();
    let t0 =
        Utc.with_ymd_and_hms(2026, 11, 3, 0, 0, 0).single().ok_or_else(|| anyhow!("a date"))?;
    let clock = Arc::new(WarpClock(Mutex::new((t0, Instant::now()))));
    let dirs = (tempfile::tempdir()?, tempfile::tempdir()?);
    let host = Arc::new(
        DirectoryHost::new(dirs.0.path()).materialise(false).retain_releases(scale.retain),
    );
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed(DOMAIN, [19; 32])?);
    let release_key = signer.verifier_key();
    let log = Arc::new(
        SiteLog::open_with_clock(
            Arc::new(InMemoryAuditLogStore::new()),
            DOMAIN,
            signer,
            clock.clone(),
        )
        .await?,
    );
    let store = Arc::new(InMemoryContentStore::default());
    let history = Arc::new(InMemoryContentHistory::default());
    let subscribers = Arc::new(Subscribers::default());

    // The product: the shop's runtime, its charter and alarm routing to the owner's phone.
    let site = SiteRef::new(uuid::Uuid::new_v4(), "shop", format!("https://{DOMAIN}"));
    let kind = template(SiteKind::Ecommerce);
    let team = SiteTeam::new(site.clone(), &kind);
    let mut inputs = CharterInputs::new(
        site.id,
        "Solar Shop",
        "owner",
        "Panels at fair prices.",
        vec!["en".parse()?],
        "KES",
    );
    inputs.p1_channels = vec![Channel::Sms];
    inputs.p2_channels = vec![Channel::Whatsapp];
    let charter = kind.draft_charter(&team, &inputs);
    let (sms, whatsapp) = (Phone::new("sms"), Phone::new("whatsapp"));
    let alarms = Arc::new(
        AlarmRouter::new(site.id, &charter.reporting)
            .with_sink(sms.clone(), "owner-sms")
            .with_sink(whatsapp.clone(), "owner-wa")
            .with_clock(clock.clone()),
    );
    let runtime = SiteRuntime::builder(
        site.clone(),
        SiteProject::new(config(scale.products), Vec::new(), clock.now()),
        store.clone(),
        Arc::new(BuiltinGenerator),
        host.clone(),
        log,
        dirs.1.path(),
    )
    .history(history.clone())
    .announcer(subscribers.clone())
    .clock(clock.clone())
    .build()?;
    let edge_key = Arc::new(Ed25519EntrySigner::from_seed("edge-receipt-2026-11", [7; 32])?);
    let engine = Arc::new(
        UpdateEngine::builder(runtime.shared_pipeline(), Arc::new(InMemoryLiveStore::default()))
            .streams(Arc::new(StreamLane::new(edge_key.clone())))
            .alarms(Arc::new(SiteAlarms::new(alarms.clone(), site.id)))
            .costs(CostRates { per_model_call_micros: 2_000, per_release_micros: 50 })
            .build()?,
    );

    // The generated fixture: products, legal pages and the live story, released as one.
    let mut prices: Vec<f64> = (0..scale.products).map(|i| 1_000.0 + (i % 500) as f64).collect();
    let mut launch: Vec<ContentItem> = (0..scale.products).map(|i| product(i, prices[i])).collect();
    for legal in ["privacy", "consent", "terms"] {
        launch.push(item(
            &format!("pages/{legal}"),
            ContentKind::Page,
            legal,
            json!({ "title": legal, "legal": legal }),
            Some("Text."),
        ));
    }
    launch.push(item(
        STORY,
        ContentKind::Article,
        "election-night",
        json!({ "title": "Election night, live", "publishedAt": "2026-11-03T00:00:00Z" }),
        Some("Text."),
    ));
    let launched = runtime
        .pipeline()
        .publish_batch(launch, Vec::new(), ReleaseSource::Onboarding, "urn:person:owner")
        .await?;
    checks.require(
        "release 1 publishes the catalogue",
        launched.published.is_some(),
        format!("{} products", scale.products),
    )?;
    let launch_time = started.elapsed();

    let mapping = FeedMapping::new("supplier", ContentKind::Product, "en".parse()?, "sku")
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
        max_rate: Rate { per_minute: scale.products as u32 },
    };
    engine.add_feed(mapping, bounds).await?;

    // The Guardian's probe through the product's incident path.
    let origin = origin(host.clone()).await?;
    let claim = DomainClaim::new(DOMAIN, Utc::now())?;
    let resolver = Arc::new(StaticDnsResolver::new());
    let (name, value) = claim.dns_record();
    resolver.set(&name, RecordKind::Txt, [value]).await;
    let public = url::Url::parse(&format!("https://{DOMAIN}"))?;
    let http = HttpClient::builder()
        .egress(EgressScope::Only(BTreeSet::from([origin.url.clone()])))
        .map_origin(&public, url::Url::parse(&origin.url)?)
        .min_interval(Duration::ZERO)
        .build()?;
    let mut probe_config = ProbeConfig::new(DOMAIN, public);
    probe_config.headers = Vec::new();
    let charters = Arc::new(InMemoryCharterStore::new());
    let ladder =
        AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new())).with_charters(charters);
    let incidents = Arc::new(IncidentPath::new(
        runtime.clone(),
        ladder,
        alarms.clone(),
        Arc::new(InMemoryStatusBoard::new()),
    ));
    let guardian = GuardianProbe {
        claim,
        resolver,
        host: host.clone(),
        http: Arc::new(http),
        trusted: vec![release_key.parse().map_err(|e| anyhow!("{e:?}"))?],
        config: probe_config,
        audit: None,
        incidents: incidents.clone(),
    };

    let mut rng = Rng(0x5319_2026_1103_0001);
    let stock_per_minute = scale.products * 2 / 100;
    let prices_per_minute = (scale.products * 5 / 1_000).max(1);
    let entry_gap = scale.stream_minutes * 60 * 1_000 / scale.stream_entries;
    let story = ContentId::from(STORY);
    let mut records: Vec<BatchRecord> = Vec::new();
    let mut price_windows: BTreeMap<DateTime<Utc>, BTreeSet<ContentId>> = BTreeMap::new();
    let mut corrupted: BTreeMap<usize, f64> = BTreeMap::new();
    let mut entry_latency_ms: Vec<u128> = Vec::new();
    let (mut appended, mut probes, mut false_drift, mut stock_updates, mut price_updates) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut ordinary_held = 0u64;
    let mut corrupted_held = false;
    let bad = 0usize;
    for minute in 0..scale.hours * 60 {
        let at = t0 + chrono::Duration::minutes(minute as i64 + 1);
        let stream_start = t0 + chrono::Duration::minutes(scale.stream_start_minute as i64);
        if minute == scale.stream_start_minute {
            clock.jump(stream_start);
            engine.open_stream(&story, "entries").await?;
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
                .await?;
            let feed: Vec<StreamEntry> = engine.stream_since(&story, entry.n - 1).await?;
            if feed.first().map(|e| e.n) == Some(entry.n) {
                entry_latency_ms.push(sent.elapsed().as_millis());
            }
            appended += 1;
            if appended == scale.stream_entries {
                engine.close_stream(&story).await?;
            }
            records.extend(engine.tick().await?);
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
        let outcome = engine.ingest("supplier", feed).await?;
        if outcome.held {
            ordinary_held += 1;
        }
        records.extend(outcome.batches);

        // Hour three: a corrupted delivery changes 60% of the prices.
        if minute == scale.corrupt_at_hour * 60 {
            let count = scale.products * 6 / 10;
            let corrupt: Vec<FeedRecord> = (0..count)
                .map(|i| {
                    let bad_price = (prices[i] * 0.4 * 100.0).round() / 100.0;
                    corrupted.insert(i, bad_price);
                    FeedRecord(BTreeMap::from([
                        ("sku".to_string(), json!(sku(i))),
                        ("price".to_string(), price_value(bad_price)),
                    ]))
                })
                .collect();
            corrupted_held = engine.ingest("supplier", corrupt).await?.held;
        }

        // One bad product edit, reverted later.
        if minute == scale.bad_edit_minute
            && let Some(mut broken) = store.get(&ContentId(sku(bad))).await?
        {
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
                .await?;
            records.extend(closed);
        }
        if minute == scale.revert_minute {
            records.extend(
                engine
                    .revert(&ContentId(sku(bad)), None, "urn:person:owner", EntryPoint::WebEditor)
                    .await?,
            );
        }

        // Releases go out; the Guardian probes while they do.
        if minute % scale.probe_every_minutes == 0 {
            alarms.tick().await;
            let (outcome, published) = tokio::join!(guardian.run(), engine.tick());
            let outcome = outcome?;
            if outcome.report.drift_detected() || outcome.incident.is_some() {
                false_drift += 1;
            }
            records.extend(published?);
            probes += 1;
        } else {
            records.extend(engine.tick().await?);
        }
    }
    clock.jump(t0 + chrono::Duration::minutes(scale.hours as i64 * 60 + 2));
    records.extend(engine.tick().await?);
    records.extend(engine.drain().await?);
    // The owner's quiet hours end in the morning, Nairobi time; a P2 that waited goes out
    // on the router's first tick after them.
    if scale.hours < 12 {
        clock.jump(t0 + chrono::Duration::hours(12));
    }
    alarms.tick().await;
    let last = guardian.run().await?;
    if last.report.drift_detected() || last.incident.is_some() {
        false_drift += 1;
    }
    probes += 1;

    // Stock changes never trigger a release; price changes publish once per window.
    let released: Vec<&BatchRecord> = records.iter().filter(|r| r.release_seq.is_some()).collect();
    let second = chrono::Duration::seconds(1);
    let mut changes: BTreeMap<&ContentId, u64> = BTreeMap::new();
    for priced in price_windows.values() {
        for id in priced {
            *changes.entry(id).or_default() += 1;
        }
    }
    let mut publications: BTreeMap<&ContentId, u64> = BTreeMap::new();
    let mut unexplained = Vec::new();
    for record in &released {
        let window = price_windows.range(record.opened_at - second..=record.closed_at + second);
        let priced: BTreeSet<&ContentId> = window.flat_map(|(_, ids)| ids).collect();
        for id in &record.published {
            *publications.entry(id).or_default() += 1;
            if !(id.0 == STORY || id.0 == sku(bad) || priced.contains(id)) {
                unexplained.push(id.0.clone());
            }
        }
    }
    let over = changes
        .iter()
        .filter(|(id, changed)| {
            !(1..=**changed).contains(&publications.get(*id).copied().unwrap_or_default())
        })
        .count();
    let feed_batches = released
        .iter()
        .filter(|r| r.published.iter().any(|id| id.0.starts_with('P') && id.0 != sku(bad)))
        .count() as u64;
    checks.check(
        "stock changes never trigger a release",
        unexplained.is_empty() && ordinary_held == 0,
        format!(
            "{stock_updates} stock updates; no release carried a product without a price change"
        ),
    );
    let mut latencies: Vec<i64> = released.iter().flat_map(|r| r.latencies_ms.clone()).collect();
    let (p50, p95) = (percentile(&mut latencies, 50.0), percentile(&mut latencies, 95.0));
    checks.check(
        "price changes publish once per batch window, p95 approval-to-live under the window plus 60 s",
        over == 0 && (scale.hours * 30..=scale.hours * 60).contains(&feed_batches) && p95 < 120_000,
        format!("{price_updates} price updates in {feed_batches} feed releases over {} deliveries; approval-to-live p50 {p50} ms, p95 {p95} ms", scale.hours * 60),
    );

    // The corrupted delivery: held whole, a P2 alarm, none of its prices published.
    let mut leaked = 0;
    for (i, bad_price) in &corrupted {
        if let Some(item) = store.get(&ContentId(sku(*i))).await?
            && (item.fields["price"] == price_value(*bad_price)
                || item.fields["price"] != price_value(prices[*i]))
        {
            leaked += 1;
        }
    }
    checks.check(
        "the corrupted batch is held whole with a P2 alarm and no price from it publishes",
        corrupted_held
            && leaked == 0
            && engine.held_deliveries().await.len() == 1
            && whatsapp.messages().iter().any(|m| m.to_lowercase().contains("held")),
        format!(
            "{} prices held back; the P2 waited out the owner's quiet hours, then reached WhatsApp: {:?}",
            corrupted.len(),
            whatsapp.messages().first()
        ),
    );

    // Live-blog entries within 10 s; a signed fold every five minutes and at the close.
    let folds: Vec<&BatchRecord> =
        released.iter().filter(|r| r.published.iter().any(|id| id.0 == STORY)).copied().collect();
    let stream_status =
        engine.streams().await.into_iter().next().ok_or_else(|| anyhow!("the stream"))?;
    let expected_folds = scale.stream_minutes / 5;
    let gaps_ok = folds.windows(2).all(|pair| {
        pair[1].opened_at - pair[0].opened_at <= chrono::Duration::seconds(5 * 60 + 70)
    });
    let closed_fold = stream_status
        .closed_at
        .is_some_and(|closed| folds.last().is_some_and(|f| f.opened_at >= closed));
    let final_story = store.get(&story).await?.ok_or_else(|| anyhow!("the story"))?;
    let entries: Vec<StreamEntry> =
        serde_json::from_value(final_story.fields["entries"].clone()).unwrap_or_default();
    let signed = entries.iter().all(|e| verify_entry(e, &edge_key.public_key()).is_ok());
    let page = host
        .fetch("/articles/election-night/")
        .await?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    checks.check(
        "live-blog entries appear within 10 s and a signed fold lands every five minutes and at the close",
        appended == scale.stream_entries
            && entry_latency_ms.len() as u64 == appended
            && entry_latency_ms.iter().all(|ms| *ms < 10_000)
            && stream_status.folded == scale.stream_entries
            && (expected_folds - 1..=expected_folds + 1).contains(&stream_status.folds)
            && gaps_ok
            && closed_fold
            && signed
            && entries.len() as u64 == scale.stream_entries
            && page.contains(&format!("Results from district {}.", scale.stream_entries)),
        format!(
            "{appended} entries, slowest {:?} ms; {} folds for a {}-minute stream, the last at the close",
            entry_latency_ms.iter().max(),
            stream_status.folds,
            scale.stream_minutes
        ),
    );

    // The revert restored one product and kept every other change.
    let reverted = store.get(&ContentId(sku(bad))).await?.ok_or_else(|| anyhow!("the product"))?;
    let versions = history.versions(&ContentId(sku(bad))).await?;
    let mut kept = true;
    for (i, price) in prices.iter().enumerate().skip(1) {
        if let Some(item) = store.get(&ContentId(sku(i))).await?
            && item.fields["price"] != price_value(*price)
        {
            kept = false;
        }
    }
    checks.check(
        "the revert restores one product and keeps every other change",
        reverted.fields["title"] == "Panel 0"
            && versions.iter().any(|v| v.item.fields["title"] == "Panel 0 — WRONG SPECIFICATIONS")
            && kept,
        format!("{} versions of {}", versions.len(), sku(bad)),
    );
    checks.check(
        "the Guardian raises no false drift alarm",
        false_drift == 0 && sms.messages().is_empty() && incidents.incidents().await.is_empty(),
        format!("{probes} probes while releases went out; no incident, no P1"),
    );

    // Each subscriber got one catalog.changed per batch.
    let inboxes = subscribers.0.lock().unwrap().clone();
    let with_items = released.iter().filter(|r| !r.published.is_empty()).count() + 1;
    checks.check(
        "each subscriber gets one catalog.changed per batch",
        inboxes.iter().all(|inbox| {
            inbox.len() == with_items
                && inbox.iter().map(|(seq, _)| *seq).collect::<BTreeSet<_>>().len() == inbox.len()
        }),
        format!("{with_items} events each, the launch included"),
    );

    // Feed updates made no model calls; the weekly review prices updates.
    let review = engine.review().await;
    let composer = ReviewComposer::new(
        site.id,
        KpiTracker::new(
            site.id,
            Arc::new(InMemoryCharterStore::new()),
            Arc::new(InMemoryKpiStore::new()),
        ),
        AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new())),
        Arc::new(InMemoryCharterStore::new()),
    )
    .with_updates(engine.clone());
    let weekly = composer.compose(t0.date_naive(), ReviewInputs::default()).await?;
    let document = weekly.to_markdown();
    checks.check(
        "feed updates make no model calls",
        review.model_calls == 0 && released.iter().all(|r| r.model_calls == 0),
        format!("{} updates, {} with a model", review.updates, review.updates_with_model),
    );
    checks.check(
        "the weekly review reports cost per thousand updates",
        document.contains("## Updates")
            && document.contains("per thousand updates")
            && review.cost_per_thousand_updates_micros > 0.0,
        format!(
            "{:.4} per thousand updates",
            review.cost_per_thousand_updates_micros / 1_000_000.0
        ),
    );

    // One log entry and checkpoint per release.
    let note = host.fetch(CHECKPOINT_PATH).await?.ok_or_else(|| anyhow!("a checkpoint"))?;
    let checkpoint = verify_checkpoint(
        &String::from_utf8_lossy(&note),
        DOMAIN,
        &[release_key.parse().map_err(|e| anyhow!("{e:?}"))?],
    )?;
    checks.check(
        "every release is one signed entry of the site log",
        checkpoint.size == released.len() as u64 + 1,
        format!("{} releases", checkpoint.size),
    );
    let (manifest_bytes, manifests) = dir_bytes(&dirs.0.path().join("manifests"));
    let (tile_bytes, tiles) = dir_bytes(&dirs.0.path().join("log"));
    let (checkpoint_bytes, _) = dir_bytes(&dirs.0.path().join("checkpoints"));
    let (object_bytes, objects) = dir_bytes(&dirs.0.path().join("objects"));
    let updates = review.updates.max(1) as f64;
    let report = json!({
        "products": scale.products,
        "simulatedHours": scale.hours,
        "releases": checkpoint.size,
        "deliveries": review.deliveries,
        "heldDeliveries": review.held_deliveries,
        "heldBatches": engine.held_deliveries().await.len(),
        "stockUpdates": stock_updates,
        "priceUpdates": price_updates,
        "liveUpdates": review.live_updates,
        "releaseUpdates": review.release_updates,
        "approvalToLiveMs": { "p50": p50, "p95": p95 },
        "streamEntries": appended,
        "folds": stream_status.folds,
        "maxEntryLatencyMs": entry_latency_ms.iter().max(),
        "probes": probes,
        "falseDriftAlarms": false_drift,
        "modelCalls": review.model_calls,
        "modelCallsPerThousandUpdates": review.model_calls as f64 * 1_000.0 / updates,
        "costPerThousandUpdatesMicros": review.cost_per_thousand_updates_micros,
        "siteLog": { "manifests": manifests, "manifestBytes": manifest_bytes, "tiles": tiles, "tileBytes": tile_bytes, "checkpointBytes": checkpoint_bytes },
        "logGrowthBytesPerHour": (manifest_bytes + tile_bytes + checkpoint_bytes) as f64 / scale.hours as f64,
        "hostObjects": { "files": objects, "bytes": object_bytes },
        "launchSeconds": launch_time.as_secs_f64(),
        "wallSeconds": started.elapsed().as_secs_f64(),
    });
    Ok((checks, report))
}
