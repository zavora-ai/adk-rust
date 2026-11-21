//! Scenario S16 (spec 3.7 §5, §7.15) on SigLIP 2: a week of generated Inspector frames and
//! document pages indexed in a visual memory, then "torn seals from Tuesday" asked by text
//! and by an example image.
//!
//! | Check | Pass condition |
//! |-------|----------------|
//! | Recall@5, text and example image | at or above the baseline in `fixtures/memory/s16-baseline.json` |
//! | Links | each hit's artifact loads with the hit's digest; its run is the run that stored it |
//! | One space | the SigLIP 2 collection refuses an embedder of another space |
//! | Erasure | erasing a camera leaves none of its chunks, captions or artifacts |
//!
//! With `--features memory-lancedb` the same run repeats on LanceDB, which must reach the
//! same recall. Weights come through the fetcher (`HF_HUB_CACHE` chooses the cache).
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-vision --features memory,onnx \
//!     --run-ignored only -E 'binary(memory_live_tests)' --no-capture
//! ```

#![cfg(feature = "onnx")]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use adk_artifact::{ArtifactService, InMemoryArtifactService, ListRequest};
use adk_memory::{GraphMemoryService, InMemoryMemoryService, MemoryService, SearchRequest};
use adk_rag::{Field, Filter, InMemoryVectorStore, Scope, VectorStore};
use adk_vision::memory::fixtures::{Defect, WeekCorpus, WeekSpec};
use adk_vision::memory::{VisualHit, VisualItem, VisualMemory, VisualQuery, space_key};
use adk_vision::onnx::{OnnxOptions, SigLip2};
use adk_vision::{
    BackendDescriptor, CoordFrame, Embedder, EmbeddingSpace, FetchConfig, ImageRef,
    InMemoryAcceptanceStore, Locality, ManifestSet, MediaScope, MediaStore, Metric, ModelFetcher,
    Provenance, SentImage, SourceImage, VisionCx, VisionError, VisionRuntime,
};
use chrono::Weekday;
use serde::Deserialize;

const MODEL: &str = "siglip2-base-patch16-256-quantized";
const COLLECTION: &str = "inspector-week";

/// The query texts: the canonical one the baseline is recorded for, then variants that are
/// reported but not gated.
const TEXT_QUERY: &str = "a cardboard box with a torn tape seal";
const TEXT_VARIANTS: [&str; 3] =
    ["torn seal", "a photo of a carton whose packing tape is ripped open", "damaged packaging"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Baseline {
    model: String,
    text_query: String,
    text_recall_at_5: f32,
    image_recall_at_5: f32,
}

fn baseline() -> Baseline {
    serde_json::from_str(include_str!("fixtures/memory/s16-baseline.json")).unwrap()
}

fn recall_at(
    k: usize,
    hits: &[VisualHit],
    relevant: &BTreeSet<String>,
    ids: &dyn Fn(&VisualHit) -> String,
) -> f32 {
    let found = hits.iter().take(k).filter(|h| relevant.contains(&ids(h))).count();
    found as f32 / relevant.len() as f32
}

struct Week {
    corpus: WeekCorpus,
    items: Vec<VisualItem>,
    artifacts: Arc<InMemoryArtifactService>,
    media: MediaStore,
}

async fn week() -> Week {
    let corpus = WeekCorpus::new(WeekSpec::default());
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let media = MediaStore::new(artifacts.clone());
    let mut items = Vec::new();
    for item in corpus.items() {
        let day = item.captured_at.date_naive();
        let scope = MediaScope::new("inspector", "plant-7", format!("week-{day}"));
        let (image, pixels) =
            media.put_pixels(&scope, corpus.render(item), Provenance::Upload).await.unwrap();
        items.push(
            VisualItem::new(
                image,
                pixels,
                item.caption.clone(),
                item.source.clone(),
                item.captured_at,
            )
            .with_kind(item.kind)
            .with_labels(item.defect.map(|d| format!("{d:?}")))
            .with_run(format!("inspector-run-{day}")),
        );
    }
    Week { corpus, items, artifacts, media }
}

fn siglip() -> Arc<SigLip2> {
    let manifest = ManifestSet::builtin().unwrap().get(MODEL).cloned().unwrap();
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let started = Instant::now();
    let model = fetcher.fetch_blocking(&manifest).unwrap();
    let embedder = SigLip2::new(&model, &OnnxOptions::default()).unwrap();
    eprintln!("{MODEL}: fetched, verified and loaded in {:?}", started.elapsed());
    Arc::new(embedder)
}

/// Two dimensions, another model: the embedder a SigLIP 2 collection must refuse.
struct Other(BackendDescriptor, EmbeddingSpace);

#[async_trait::async_trait]
impl Embedder for Other {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.0
    }
    fn space(&self) -> &EmbeddingSpace {
        &self.1
    }
    async fn embed_images(
        &self,
        _: &VisionCx,
        i: &[SentImage],
    ) -> adk_vision::Result<Vec<Vec<f32>>> {
        Ok(i.iter().map(|_| vec![1.0; 768]).collect())
    }
    async fn embed_texts(&self, _: &VisionCx, t: &[&str]) -> adk_vision::Result<Vec<Vec<f32>>> {
        Ok(t.iter().map(|_| vec![1.0; 768]).collect())
    }
}

/// Indexes the week, asks both queries, checks links, the space refusal and erasure, and
/// returns (text, image) Recall@5.
async fn s16(
    label: &str,
    store: Arc<dyn VectorStore>,
    runtime: Arc<VisionRuntime>,
    week: &Week,
) -> (f32, f32) {
    let captions = Arc::new(InMemoryMemoryService::new());
    let graph = Arc::new(GraphMemoryService::new("sqlite::memory:").await.unwrap());
    graph.migrate().await.unwrap();
    let memory = VisualMemory::builder(runtime, store.clone(), COLLECTION)
        .captions(captions.clone())
        .graph(graph.clone())
        .artifacts(week.artifacts.clone())
        .open()
        .await
        .unwrap();
    let scope = Scope::from_parts(["inspector", "plant-7"]).unwrap();
    let cx = VisionCx::new();

    let started = Instant::now();
    for batch in week.items.chunks(16) {
        memory.index(&cx, &scope, batch).await.unwrap();
    }
    eprintln!(
        "{label}: indexed {} images ({} frames, {} pages) in {:?}",
        week.items.len(),
        week.corpus.items().iter().filter(|i| i.page.is_none()).count(),
        week.corpus.items().iter().filter(|i| i.page.is_some()).count(),
        started.elapsed()
    );

    let tuesday = week.corpus.day(Weekday::Tue);
    let relevant: BTreeSet<String> = week
        .corpus
        .relevant(Defect::TornSeal, tuesday)
        .into_iter()
        .map(|id| {
            let n = week.corpus.items().iter().position(|i| i.id == id).unwrap();
            week.items[n].chunk_id()
        })
        .collect();
    let by_id = |h: &VisualHit| h.id.clone();
    let from_tuesday = VisualQuery::top(5).captured_on([tuesday]);

    let text = memory.search_text(&cx, &scope, TEXT_QUERY, &from_tuesday).await.unwrap().value;
    let text_recall = recall_at(5, &text, &relevant, &by_id);
    let example = SourceImage::from_pixels(week.corpus.example(Some(Defect::TornSeal))).unwrap();
    let image = memory.search_image(&cx, &scope, &example, &from_tuesday).await.unwrap().value;
    let image_recall = recall_at(5, &image, &relevant, &by_id);
    for (query, hits) in [(TEXT_QUERY, &text), ("example image", &image)] {
        eprintln!("{label} | torn seals from Tuesday by {query:?}:");
        for hit in hits.iter() {
            eprintln!(
                "  {:+.4} {} {} {} | {} | run {}",
                hit.score,
                if relevant.contains(&hit.id) { "RELEVANT" } else { "        " },
                hit.captured_at.unwrap().format("%a %H:%M"),
                hit.source,
                hit.caption,
                hit.run.as_deref().unwrap_or("-"),
            );
        }
    }
    eprintln!("{label}: Recall@5 text {text_recall:.3}, example image {image_recall:.3}");
    for variant in TEXT_VARIANTS {
        let hits = memory.search_text(&cx, &scope, variant, &from_tuesday).await.unwrap().value;
        eprintln!(
            "{label}: Recall@5 for {variant:?} (not gated): {:.3}",
            recall_at(5, &hits, &relevant, &by_id)
        );
    }
    let week_relevant: BTreeSet<String> = week
        .corpus
        .items()
        .iter()
        .zip(&week.items)
        .filter(|(i, _)| i.defect == Some(Defect::TornSeal))
        .map(|(_, v)| v.chunk_id())
        .collect();
    let whole =
        memory.search_image(&cx, &scope, &example, &VisualQuery::top(10)).await.unwrap().value;
    eprintln!(
        "{label}: whole week by example image, Recall@10 of the {} torn seals: {:.3}",
        week_relevant.len(),
        recall_at(10, &whole, &week_relevant, &by_id)
    );

    // Each hit links to its artifact (the bytes load and match the digest) and its run.
    for hit in text.iter().chain(&image) {
        let item = week.items.iter().find(|i| i.chunk_id() == hit.id).unwrap();
        let reference = ImageRef { artifact: hit.artifact.clone(), ..item.image.clone() };
        assert_eq!(week.media.load(&reference).await.unwrap().digest(), hit.digest.unwrap());
        let day = hit.captured_at.unwrap().date_naive();
        assert_eq!(hit.run.as_deref(), Some(&*format!("inspector-run-{day}")));
        assert_eq!(day, tuesday);
    }

    // One space per collection.
    let other = Other(
        BackendDescriptor::new("other", Locality::InProcess, CoordFrame::SentPixels),
        EmbeddingSpace {
            model: "other".into(),
            revision: "1".into(),
            dimensions: 768,
            metric: Metric::Cosine,
        },
    );
    let other = Arc::new(VisionRuntime::builder().embedder(Arc::new(other)).build().unwrap());
    let err = VisualMemory::builder(other, store.clone(), COLLECTION).open().await.unwrap_err();
    let VisionError::SpaceMismatch { recorded, offered, .. } = &err else { panic!("{err}") };
    assert_eq!(recorded, &space_key(memory.space()));
    eprintln!("{label}: refused another space: {err}");
    assert_eq!(offered, "other@1/768/cosine");

    // Erasure: conveyor-2 leaves no chunk, caption or artifact.
    let camera: Vec<&VisualItem> = week.items.iter().filter(|i| i.source == "conveyor-2").collect();
    let erased = memory.erase_source(&cx, &scope, "conveyor-2").await.unwrap();
    assert_eq!(erased.chunks, camera.len() as u64);
    assert_eq!(erased.captions, camera.len() as u64);
    assert_eq!(erased.artifacts.len(), camera.len());
    let left = Filter::eq(Field::DocumentId, "conveyor-2");
    assert_eq!(store.count(COLLECTION, &scope, Some(&left)).await.unwrap(), 0);
    let remaining = captions
        .search(SearchRequest {
            query: "conveyor".into(),
            user_id: "plant-7".into(),
            app_name: "inspector".into(),
            limit: Some(500),
            min_score: None,
            project_id: None,
        })
        .await
        .unwrap()
        .memories;
    assert!(
        remaining
            .iter()
            .all(|m| !adk_memory::text::extract_text(&m.content).starts_with("conveyor-2"))
    );
    assert!(
        remaining
            .iter()
            .any(|m| adk_memory::text::extract_text(&m.content).starts_with("conveyor-1"))
    );
    for item in &camera {
        let at = &item.image.artifact;
        let names = week
            .artifacts
            .list(ListRequest {
                app_name: at.app_name.clone(),
                user_id: at.user_id.clone(),
                session_id: at.session_id.clone(),
            })
            .await
            .unwrap()
            .file_names;
        assert!(!names.contains(&at.file_name), "{} survived", at.file_name);
    }
    assert!(
        graph
            .open_nodes("inspector", "plant-7", vec!["conveyor-2".into()])
            .await
            .unwrap()
            .is_empty()
    );
    eprintln!(
        "{label}: erased conveyor-2: {} chunks, {} captions, {} artifacts, {} graph entity",
        erased.chunks,
        erased.captions,
        erased.artifacts.len(),
        erased.entities
    );
    (text_recall, image_recall)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs SigLIP 2 base quantized (381 MB)"]
async fn live_s16_torn_seals_from_tuesday() {
    let embedder = siglip();
    let runtime = Arc::new(VisionRuntime::builder().embedder(embedder).build().unwrap());
    let week = week().await;
    let baseline = baseline();
    assert_eq!(baseline.model, MODEL);
    assert_eq!(baseline.text_query, TEXT_QUERY);

    let (text, image) =
        s16("in-memory", Arc::new(InMemoryVectorStore::new()), runtime.clone(), &week).await;
    assert!(
        text >= baseline.text_recall_at_5,
        "text Recall@5 {text} under the baseline {}",
        baseline.text_recall_at_5
    );
    assert!(
        image >= baseline.image_recall_at_5,
        "image Recall@5 {image} under the baseline {}",
        baseline.image_recall_at_5
    );

    #[cfg(feature = "memory-lancedb")]
    {
        let dir = tempfile::TempDir::new().unwrap();
        let store =
            adk_rag::lancedb::LanceDBVectorStore::new(dir.path().to_str().unwrap()).await.unwrap();
        // The artifacts conveyor-2 lost in the first run are stored again.
        let week = self::week().await;
        let (lance_text, lance_image) = s16("lancedb", Arc::new(store), runtime, &week).await;
        assert_eq!(
            (lance_text, lance_image),
            (text, image),
            "the backend does not change the answer"
        );
    }
}
