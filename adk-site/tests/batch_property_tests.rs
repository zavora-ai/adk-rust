//! Property tests for batching and delta chains (spec 3.5 §7.8.3 "Batching", "Releases at
//! scale"): no latest version is lost, no item appears twice in a batch, every version is
//! released or superseded exactly once, and a snapshot plus its deltas reproduce every
//! release's full file set.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use adk_core::ManualClock;
use adk_site::batch::{BatchPolicies, Batcher, FlushReason, Update};
use adk_site::pipeline::EntryPoint;
use adk_site::release::{PreviousRelease, ReleaseBuilder, ReleaseOptions};
use awp_types::{
    BatchPolicy, ContentId, ContentItem, ContentKind, ContentState, FileEntry, GeneratorRef,
    ReleaseSource, Trigger,
};
use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::*;

fn kind(index: u8) -> ContentKind {
    [ContentKind::Product, ContentKind::Article, ContentKind::Page][usize::from(index % 3)]
}

#[derive(Debug, Clone)]
struct Step {
    item: u8,
    gap_secs: u16,
    trigger: bool,
    poll: bool,
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        (0u8..12, 0u16..90, prop::bool::weighted(0.05), prop::bool::weighted(0.3))
            .prop_map(|(item, gap_secs, trigger, poll)| Step { item, gap_secs, trigger, poll }),
        1..120,
    )
}

fn update(item: u8, version: usize, at: DateTime<Utc>, trigger: bool) -> Update {
    Update {
        item: ContentItem {
            id: ContentId(format!("item-{item}")),
            kind: kind(item),
            slug: format!("item-{item}"),
            language: "en".parse().unwrap(),
            fields: BTreeMap::from([("version".to_string(), version.into())]),
            volatile: Vec::new(),
            body: None,
            media: Vec::new(),
            sources: Vec::new(),
            schedule: None,
            state: ContentState::Published,
        },
        at,
        entry: EntryPoint::Feed { feed: "f".into() },
        proposer: "feed:f".into(),
        trigger: trigger.then_some(Trigger::Correction),
        model_calls: 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Every pushed version is released in exactly one batch or superseded in exactly one;
    /// the last version of each item is the one its last batch releases; no batch holds an
    /// item twice or more than `max_items` items of a kind; every change in a window arrived
    /// within it, however rarely the batcher is polled.
    #[test]
    fn batching_loses_no_latest_version_and_never_duplicates(steps in steps(), max_items in 1usize..6) {
        let mut policies = BatchPolicies::default();
        for kind in [ContentKind::Product, ContentKind::Article, ContentKind::Page] {
            let window = policies.policy(kind).window;
            policies
                .set(kind, BatchPolicy { window, max_items, ..BatchPolicy::default() })
                .unwrap();
        }
        let mut batcher = Batcher::new(policies.clone());
        let mut now = Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap();
        let mut pushed: Vec<(ContentId, usize)> = Vec::new();
        let mut batches = Vec::new();
        for (version, step) in steps.iter().enumerate() {
            now += chrono::Duration::seconds(i64::from(step.gap_secs));
            if step.poll {
                batches.extend(batcher.poll(now));
            }
            let update = update(step.item, version, now, step.trigger);
            pushed.push((update.item.id.clone(), version));
            batches.extend(batcher.push(update));
        }
        batches.extend(batcher.drain(now));
        prop_assert_eq!(batcher.pending(), 0);

        let mut accounted: Vec<(ContentId, usize)> = Vec::new();
        let mut last_released: BTreeMap<ContentId, usize> = BTreeMap::new();
        for batch in &batches {
            let ids: BTreeSet<&ContentId> = batch.updates.iter().map(|u| &u.item.id).collect();
            prop_assert_eq!(ids.len(), batch.updates.len(), "an item twice in one batch");
            prop_assert!(batch.updates.len() <= max_items * 3, "max_items per kind");
            for kind in [ContentKind::Product, ContentKind::Article, ContentKind::Page] {
                prop_assert!(batch.updates.iter().filter(|u| u.item.kind == kind).count() <= max_items);
            }
            if matches!(batch.reason, FlushReason::Window | FlushReason::MaxItems | FlushReason::MaxBytes) {
                // One kind's window: every change arrived within it.
                for update in batch.updates.iter().chain(&batch.superseded) {
                    let window = chrono::Duration::from_std(policies.policy(update.item.kind).window).unwrap();
                    prop_assert!(update.at >= batch.opened_at && update.at < batch.opened_at + window);
                }
            }
            for update in batch.updates.iter().chain(&batch.superseded) {
                let version = update.item.fields["version"].as_u64().unwrap() as usize;
                accounted.push((update.item.id.clone(), version));
            }
            for update in &batch.updates {
                let version = update.item.fields["version"].as_u64().unwrap() as usize;
                last_released.insert(update.item.id.clone(), version);
            }
        }
        accounted.sort();
        let mut expected = pushed.clone();
        expected.sort();
        prop_assert_eq!(accounted, expected, "each version accounted once");
        let mut latest: BTreeMap<ContentId, usize> = BTreeMap::new();
        for (id, version) in &pushed {
            latest.insert(id.clone(), *version);
        }
        prop_assert_eq!(last_released, latest, "the latest version is released");
    }
}

fn build(dir: &std::path::Path, files: &BTreeMap<String, u8>) {
    let _ = std::fs::remove_dir_all(dir);
    for (path, version) in files {
        let target = dir.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, format!("<p>{path} v{version}</p>")).unwrap();
    }
}

/// One release step: set, change or remove some of twelve pages.
fn release_steps() -> impl Strategy<Value = Vec<Vec<(u8, Option<u8>)>>> {
    prop::collection::vec(prop::collection::vec((0u8..12, prop::option::of(0u8..4)), 0..6), 1..25)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// The nearest snapshot and the deltas after it reproduce every release's full file set,
    /// whatever the cadence; at most `snapshot_every − 1` deltas follow a snapshot.
    #[test]
    fn a_snapshot_and_its_deltas_reproduce_the_full_manifest(
        steps in release_steps(),
        every in 1u64..6,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("out");
            let clock = Arc::new(ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap()));
            let builder = ReleaseBuilder::new().with_clock(clock.clone()).snapshot_every(every);
            let mut files: BTreeMap<String, u8> = BTreeMap::from([("index.html".to_string(), 0)]);
            let mut previous: Option<PreviousRelease> = None;
            let mut manifests = Vec::new();
            let mut expected: Vec<BTreeMap<String, FileEntry>> = Vec::new();
            for step in &steps {
                for (page, version) in step {
                    let path = format!("p{page}/index.html");
                    match version {
                        Some(v) => { files.insert(path, *v); }
                        None => { files.remove(&path); }
                    }
                }
                build(&out, &files);
                let options = ReleaseOptions {
                    source: ReleaseSource::Edit { job: "prop".into(), decision: None },
                    generator: GeneratorRef { name: "prop".into(), version: "1".into() },
                    charter_version: 1,
                    redirects: Vec::new(),
                    written: None,
                    force_snapshot: false,
                };
                let staged = builder.stage(&out, previous.as_ref(), options).await.unwrap();
                expected.push(staged.files.clone());
                manifests.push(staged.manifest.clone());
                previous = Some(PreviousRelease::from_staged(&staged));
                clock.advance(Duration::from_secs(60));
            }
            for (index, manifest) in manifests.iter().enumerate() {
                let snapshot = manifests[..=index].iter().rposition(|m| m.is_snapshot()).unwrap();
                prop_assert!((index - snapshot) as u64 <= every.saturating_sub(1));
                let mut full = BTreeMap::new();
                for step in &manifests[snapshot..=index] {
                    full = step.apply_to(&full);
                }
                prop_assert_eq!(&full, &expected[index], "release {}", manifest.seq);
                if let Some(base) = manifest.base {
                    prop_assert_eq!(base + 1, manifest.seq);
                }
            }
            Ok(())
        })?;
    }
}
