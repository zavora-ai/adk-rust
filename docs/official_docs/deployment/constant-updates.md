# Constantly updated sites

Some sites change all day: a shop syncing a supplier's catalogue every minute, a newsroom on
election night, a public body during an emergency. With `adk-site` they stay static and signed —
nothing runs when a page is requested. Every update is sorted into a lane, batched, checked in
tiers and published without a model unless it needs writing (spec 3.5 §7.8.3).

> **Note:** `adk-site` is **Experimental**. Its API may change while spec 3.5 lands.

```toml
[dependencies]
adk-site = "3.0.0"
```

## Lanes

A content kind declares the lane of each field in `site.toml`; an item can add live fields of its
own through its `volatile` list.

```toml
[lanes.product]
volatile = ["stock", "availability"]

[lanes.article]
stream = ["entries"]
```

| Lane | Carries | Delivered |
|------|---------|-----------|
| Release | Claims — prices, statements, final results — and every field not declared otherwise | Batched signed releases |
| Live (`volatile`) | State: stock, availability, scores, queue times | The edge's read-only capabilities through a `LiveStore`; with script off the page shows the last released value and its time |
| Stream (`stream`) | Append-only coverage: live blogs, tickers | An edge feed of signed entries, folded into signed releases |

A claim never travels outside the release lane (A-I26). `SiteConfig::validate` refuses a kind
that declares `price`, `currency`, `sku`, `title`, `summary`, `description`, `validUntil`,
`startsAt`, `endsAt`, `publishedAt`, `result`, `corrections` or `withdrawal` live or streamed —
plus any field the kind lists under `claims` — and every build refuses an item that lists one in
its own `volatile`.

## The update engine

`UpdateEngine` sits in front of a `PublishingPipeline`:

| Step | Behaviour |
|------|-----------|
| Feeds | A `ContentSource` delivers records; a `FeedMapping` turns them into item fields through declared types, units and bounds, with no model call |
| Bounds | A delivery that breaks its `FeedBounds` — the share of its collection it changes (default 0.2), a field bound, its rate — is held whole with a P2 alarm and a summary of what it would change; nothing from it publishes until the owner decides (A-I27) |
| Live values | Go to the `LiveStore` and never make a release |
| Batching | Item versions coalesce in a window per kind — 30 s by default, 10 s for articles, 60 s for products, 5 s to 5 min — and the latest version of an item wins; corrections, takedowns, emergency notices and owner requests publish at once |
| Exceptions | A price outside the price rule (A-I25) or a new item is held on its own; the rest of the batch publishes. The owner approves it later, and the proposer cannot approve its own change (A-I23) |
| Fast checks | Run on the batch's items and their dependents; a failing item is held on its own, and server code refuses the whole release (A-I1) |
| Release | One release per batch — a delta, or a snapshot every 100 releases or daily — with one site log entry, one checkpoint and one `catalog.changed` whose item ids are paginated beyond 1,000 |
| Full checks | The launch gate's full tier every six hours or 500 releases, with a P2 alarm when it fails |

```rust
use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::audit_log::InMemoryAuditLogStore;
use adk_site::content::{InMemoryContentHistory, InMemoryContentStore, SiteConfig, SiteProject};
use adk_site::feed::{FeedMapping, FeedRecord, FieldMapping, FieldType};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::DirectoryHost;
use adk_site::live::InMemoryLiveStore;
use adk_site::log::SiteLog;
use adk_site::pipeline::PublishingPipeline;
use adk_site::signer::Ed25519ReleaseSigner;
use adk_site::updates::{InMemoryAlarms, UpdateEngine};
use awp_types::{Bound, ContentKind, FeedBounds, PriceRule, Rate};

async fn shop(config: SiteConfig) -> adk_site::Result<()> {
    let domain = config.domain.clone();
    let signer = Arc::new(Ed25519ReleaseSigner::generate(&domain)?);
    let log = Arc::new(SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), &domain, signer).await?);
    // A host that publishes each release under its own prefix and stores only what changed.
    let host = Arc::new(DirectoryHost::new("/srv/shop").materialise(false).retain_releases(500));
    let pipeline = Arc::new(
        PublishingPipeline::builder(
            SiteProject::new(config, Vec::new(), chrono::Utc::now()),
            Arc::new(InMemoryContentStore::default()),
            Arc::new(BuiltinGenerator),
            host,
            log,
            "/var/lib/shop/work",
        )
        .history(Arc::new(InMemoryContentHistory::default()))
        .build(),
    );
    let engine = UpdateEngine::builder(pipeline, Arc::new(InMemoryLiveStore::default()))
        .alarms(Arc::new(InMemoryAlarms::default()))
        .build()?;

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
        field_bounds: BTreeMap::new(),
        max_rate: Rate { per_minute: 5_000 },
    };
    engine.add_feed(mapping, bounds).await?;

    let delivery = vec![FeedRecord::new([("sku", "PANEL-400".into()), ("qty", 7.into())])];
    let outcome = engine.ingest("supplier", delivery).await?;
    assert_eq!(outcome.queued, 0, "a stock change never makes a release");
    // Called by the site's scheduler: closes elapsed windows and runs due full checks.
    engine.tick().await?;
    Ok(())
}
```

## Streams

A live blog, a ticker or an emergency update is a stream on a published story. Each entry is a
`StreamEntry` signed by the edge's delegated key — a detached JWS over the entry's JCS form, the
same proof every AWP 2.0 document carries — and the edge serves it at once in a feed the page
polls. Every `fold_interval` (default five minutes) and when the stream closes, the entries fold
into the story's stream field and release through the batcher, so every entry ends up in a signed
release; a fold refuses an entry whose proof does not verify.

```rust
use std::sync::Arc;

use adk_site::stream::{Ed25519EntrySigner, StreamLane};
use adk_site::updates::UpdateEngine;
use awp_types::ContentId;

async fn election_night(engine: &UpdateEngine) -> adk_site::Result<()> {
    // The engine was built with `.streams(Arc::new(StreamLane::new(edge_key)))`.
    let story = ContentId::from("articles/election-night/en");
    engine.open_stream(&story, "entries").await?;
    let entry = engine.append_entry(&story, "Results from the first district.", None).await?;
    assert_eq!(engine.stream_since(&story, 0).await?, vec![entry]);
    engine.close_stream(&story).await?;
    engine.tick().await?; // the closing fold
    Ok(())
}

fn lane() -> adk_site::Result<StreamLane> {
    Ok(StreamLane::new(Arc::new(Ed25519EntrySigner::generate("edge-receipt-2026-11")?)))
}
```

The story must already be published, and the field must be one its kind declares under
`stream`; feeds never fill a stream field. The page renders folded entries newest first under a
`data-awp-stream` hook, with corrections marked.

## Operating

| Library call | CLI | Shows or does |
|--------------|-----|---------------|
| `UpdateEngine::batches` | `adk-rust site batches` | Recent batches: release, items, superseded versions, held items and why, approval-to-live |
| `UpdateEngine::feeds` | `adk-rust site feeds` | Feeds with their mappings, bounds, rates and held deliveries; model calls are always zero |
| `UpdateEngine::revert` | `adk-rust site revert <item> [--to <seq>]` | Publishes one item's earlier version at once and keeps every other change |
| `UpdateEngine::rollback`, `replay` | `adk-rust site rollback <seq>` | Whole-site rollback for an incident; the changes since are listed and replayed one by one |
| `UpdateEngine::decide_delivery`, `decide_item` | — | The owner's decision on a held delivery or a held item |
| `UpdateEngine::review` | — | For the weekly review: updates, the share that needed a model, cost per thousand updates |

The CLI acts on a site project ([Sites and AWP from the command line](site-cli.md)); `site
batches` reads the batches its engine journals in `data/updates/`.

## Hosts at speed

`SiteHost::limits` declares a host's publication limits. `check_host` compares them with the
batch windows: a host without atomic publication (`Atomicity::None`) cannot serve a window under
one minute, and `UpdateEngine::builder(..).build()` refuses such a pairing with a proposal of
object storage behind a CDN.

`DirectoryHost` stores every file version once under its digest and switches a pointer last
(`Atomicity::PrefixPointer`). By default it also writes a full directory per release for a static
file server; `materialise(false)` drops those, so a publish costs what changed rather than the size
of the site, and `retain_releases(n)` keeps the files of the latest `n` releases only. Every host
runs `adk_site::host::conformance::run_all`, which checks atomicity under concurrent readers for
snapshots and for deltas.

## Events

Each batch produces one `Announcement` for the edge: `site.release.published` and one
`catalog.changed` with the first 1,000 changed item ids. Beyond 1,000, `catalog.next` links the
next page at `<changes_url>/<seq>/<page>` and `Announcement::catalog_pages` holds the pages for
the edge to serve; `PublishingPipelineBuilder::changes_url` sets the base, by default
`<base_url>/awp/v2/events/changes`.

## Scenario S19

`adk-site/tests/s19_constant_updates.rs` runs spec 3.5's scenario S19 on a warped clock against
`DirectoryHost`: a generated shop fed every minute with 2% stock and 0.5% price changes, a
corrupted delivery at hour three that changes 60% of the prices, an election-night live blog, a
bad product edit that is reverted, and the Guardian's `Prober` sampling the host and the origin
while releases go out. It asserts that stock never makes a release, that each price change
publishes once with p95 approval-to-live under the window plus 60 s, that the corrupted delivery
is held whole with a P2 alarm, that every entry is folded into a signed release, that the revert
keeps every other change, that no probe reports drift, that each subscriber gets one
`catalog.changed` per batch, and that feed updates make no model call. Every pull request runs
it with 600 products over four hours; the nightly soak runs 50,000 products over 24 hours:

```bash
ADK_SITE_S19_FULL=1 cargo nextest run -p adk-site --features migrate --cargo-profile ci \
  --run-ignored only -E 'test(s19_full_day)' --no-capture
```
