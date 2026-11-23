//! S16 (visual memory): a week of the Inspector's frames and document pages, one run a day,
//! indexed with SigLIP 2 on the device into an `adk-rag` vector store, with each caption in
//! `adk-memory` naming its artifact and each camera in graph memory. "Torn seals from Tuesday"
//! is asked by text and by an example image; Recall@5 of each is held to the baseline recorded
//! in `fixtures/baselines.json`, and every hit is followed back to its artifact and to the
//! run whose audit record stored it.
//!
//! The week is `adk_vision::memory::fixtures::WeekCorpus`: cartons on two conveyor cameras,
//! sealed with packing tape, some torn or dented on known days, and generated delivery
//! notes, inspection reports and invoices whose data is invented.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use adk_artifact::InMemoryArtifactService;
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_memory::text::artifact_references;
use adk_memory::{GraphMemoryService, InMemoryMemoryService, MemoryService};
use adk_rag::{InMemoryVectorStore, Scope, VectorStore};
use adk_vision::memory::fixtures::{Defect, WeekCorpus, WeekSpec};
use adk_vision::memory::{
    VISION_MEMORY_INDEXED, VisualHit, VisualItem, VisualKind, VisualMemory, VisualQuery, space_key,
};
use adk_vision::onnx::{OnnxOptions, SigLip2};
use adk_vision::{
    ImageRef, ManifestSet, MediaScope, MediaStore, Provenance, SourceImage, VisionCx,
    VisionRuntime, artifact_uri,
};
use anyhow::{Context, Result};
use chrono::{NaiveDate, Weekday};
use serde::Deserialize;
use serde_json::Value;

use crate::Checks;
use crate::fixtures::{fetcher, fixtures, read_json};

/// SigLIP 2 base, quantized: 381 MB of weights (spec 3.7 Q14).
pub const MODEL: &str = "siglip2-base-patch16-256-quantized";
const COLLECTION: &str = "inspector-week";
const APP: &str = "inspector";
const PLANT: &str = "plant-7";

/// What the scenario holds a run to, from `fixtures/baselines.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Baseline {
    model: String,
    text_query: String,
    /// Tuesday's torn seals: the denominator of Recall@5.
    relevant: usize,
    /// How many of them the first five hits hold, by text and by the example image.
    text_hits_at5: usize,
    image_hits_at5: usize,
}

fn run_of(day: NaiveDate) -> String {
    format!("inspector-run-{day}")
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    let baseline: Baseline = serde_json::from_value(
        read_json(&fixtures().join("baselines.json"))?
            .get("s16")
            .cloned()
            .context("baselines.json has s16")?,
    )?;
    let manifest = ManifestSet::builtin()?
        .get(MODEL)
        .cloned()
        .context("SigLIP 2 is in the built-in manifest")?;
    let started = Instant::now();
    let verified = fetcher()
        .fetch(&manifest)
        .await
        .context("fetching SigLIP 2 (set HF_HUB_CACHE to keep it)")?;
    let siglip = SigLip2::new(&verified, &OnnxOptions::default())?;
    checks.note(format!(
        "SigLIP 2 base quantized on the device, fetched by revision and SHA-256 and loaded in {:.1} s",
        started.elapsed().as_secs_f32()
    ));
    let runtime = Arc::new(VisionRuntime::builder().embedder(Arc::new(siglip)).build()?);

    let artifacts = Arc::new(InMemoryArtifactService::new());
    let media = MediaStore::new(artifacts.clone());
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    let captions = Arc::new(InMemoryMemoryService::new());
    let graph = Arc::new(GraphMemoryService::new("sqlite::memory:").await?);
    graph.migrate().await?;
    let memory = VisualMemory::builder(runtime, store.clone(), COLLECTION)
        .captions(captions.clone())
        .graph(graph.clone())
        .artifacts(artifacts.clone())
        .open()
        .await?;
    let scope = Scope::from_parts([APP, PLANT])?;
    let audit = InMemoryAuditRecorder::new();

    // One Inspector run a day stores that day's frames and pages.
    let corpus = WeekCorpus::new(WeekSpec::default());
    let mut days: BTreeMap<NaiveDate, Vec<VisualItem>> = BTreeMap::new();
    // The corpus item each chunk stands for.
    let mut chunk_of: BTreeMap<String, String> = BTreeMap::new();
    for item in corpus.items() {
        let day = item.captured_at.date_naive();
        let at = MediaScope::new(APP, PLANT, format!("week-{day}"));
        let (image, pixels) =
            media.put_pixels(&at, corpus.render(item), Provenance::Upload).await?;
        let visual = VisualItem::new(
            image,
            pixels,
            item.caption.clone(),
            item.source.clone(),
            item.captured_at,
        )
        .with_kind(item.kind)
        .with_labels(item.defect.map(|d| format!("{d:?}")))
        .with_run(run_of(day));
        chunk_of.insert(item.id.clone(), visual.chunk_id());
        days.entry(day).or_default().push(visual);
    }
    let started = Instant::now();
    for (day, items) in &days {
        let actor = AuditActor { invocation_id: Some(run_of(*day)), ..AuditActor::default() };
        let cx = VisionCx::new().with_audit(AuditTrail::new(Arc::new(audit.clone())), actor);
        for batch in items.chunks(16) {
            memory.index(&cx, &scope, batch).await?;
        }
    }
    let items: Vec<&VisualItem> = days.values().flatten().collect();
    let frames = items.iter().filter(|i| i.kind == VisualKind::Frame).count();
    let indexed = store.count(COLLECTION, &scope, None).await?;
    checks.check(
        &format!(
            "{indexed} images indexed by {} daily runs in {:.1} s: {frames} frames and {} pages, one chunk each, in the space {}",
            days.len(),
            started.elapsed().as_secs_f32(),
            items.len() - frames,
            space_key(memory.space())
        ),
        indexed == items.len() as u64 && items.len() == 105 && memory.space().model.contains("siglip2"),
    );

    // "Torn seals from Tuesday", by text and by an example image.
    let tuesday = corpus.day(Weekday::Tue);
    let relevant: BTreeSet<String> = corpus
        .relevant(Defect::TornSeal, tuesday)
        .into_iter()
        .filter_map(|id| chunk_of.get(id).cloned())
        .collect();
    let from_tuesday = VisualQuery::top(5).captured_on([tuesday]);
    let cx = VisionCx::new();
    let by_text = memory.search_text(&cx, &scope, &baseline.text_query, &from_tuesday).await?.value;
    let example = SourceImage::from_pixels(corpus.example(Some(Defect::TornSeal)))?;
    let by_image = memory.search_image(&cx, &scope, &example, &from_tuesday).await?.value;
    let hits =
        |found: &[VisualHit]| found.iter().take(5).filter(|h| relevant.contains(&h.id)).count();
    let (text_hits, image_hits) = (hits(&by_text), hits(&by_image));
    checks.check(
        &format!(
            "by text, \"{}\" on Tuesday: Recall@5 {text_hits}/{} against the recorded {}/{}",
            baseline.text_query, baseline.relevant, baseline.text_hits_at5, baseline.relevant
        ),
        baseline.model == MODEL
            && relevant.len() == baseline.relevant
            && text_hits >= baseline.text_hits_at5,
    );
    checks.check(
        &format!(
            "by an example image of a torn seal, on Tuesday: Recall@5 {image_hits}/{} against the recorded {}/{}",
            baseline.relevant, baseline.image_hits_at5, baseline.relevant
        ),
        image_hits >= baseline.image_hits_at5,
    );

    // Every hit leads back to its artifact, its caption and the run that stored it.
    let stored = captions.export_user(APP, PLANT).await?;
    let indexed_by: Vec<(Option<String>, Value)> = audit
        .records()
        .into_iter()
        .filter(|r| r.action == VISION_MEMORY_INDEXED)
        .map(|r| (r.actor.invocation_id, r.ext.get("digests").cloned().unwrap_or_default()))
        .collect();
    let mut linked = 0;
    let all: Vec<&VisualHit> = by_text.iter().chain(&by_image).collect();
    for hit in &all {
        let (Some(digest), Some(item)) =
            (hit.digest, items.iter().find(|i| i.chunk_id() == hit.id))
        else {
            continue;
        };
        // The hit's own locator, not the item's, must find the bytes.
        let image = ImageRef { artifact: hit.artifact.clone(), ..item.image.clone() };
        let loads = media.load(&image).await.is_ok_and(|pixels| pixels.digest() == digest);
        let captioned = stored.iter().any(|m| {
            hit.caption_id.is_some()
                && m.entry.id == hit.caption_id
                && artifact_references(&m.entry.content)
                    .iter()
                    .any(|r| r.uri == artifact_uri(&hit.artifact))
        });
        let day = hit.captured_at.map(|at| at.date_naive());
        let by_its_run = indexed_by.iter().any(|(run, digests)| {
            run.is_some()
                && *run == hit.run
                && digests.as_array().is_some_and(|d| d.iter().any(|d| d == &digest.to_string()))
        });
        if loads && captioned && by_its_run && day == Some(tuesday) && hit.run == day.map(run_of) {
            linked += 1;
        }
    }
    checks.check(
        &format!(
            "each of the {} hits loads from its artifact with its digest, its caption in adk-memory names that artifact, and the `{VISION_MEMORY_INDEXED}` record of the run it names holds its digest ({linked} of {})",
            all.len(),
            all.len()
        ),
        !all.is_empty() && linked == all.len(),
    );
    let cameras =
        graph.open_nodes(APP, PLANT, vec!["conveyor-1".into(), "conveyor-2".into()]).await?.len();
    checks.note(format!(
        "Recall@5 text {text_hits}/{0}, example image {image_hits}/{0}; {cameras} cameras in graph memory",
        relevant.len()
    ));
    Ok(checks)
}
