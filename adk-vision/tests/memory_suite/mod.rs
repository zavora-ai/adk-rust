//! The visual memory suite, run against every vector store backend a test can reach: index,
//! text and example-image queries, the space record, captions and graph memory, an
//! `adk-rag` pipeline over the collection, and erasure by source.
//!
//! The embedder is a real, deterministic one: an image's vector is its mean colour, and a
//! text naming a colour embeds to that colour, so texts and images share one space.

#![allow(dead_code)]

use std::future::Future;
use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService};
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_memory::text::ArtifactReference;
use adk_memory::{GraphMemoryService, InMemoryMemoryService, MemoryService, SearchRequest};
use adk_rag::{EmbeddingProvider, Field, Filter, RagConfig, RagPipeline, Scope, VectorStore};
use adk_vision::memory::{
    SPACE_SCOPE, VISION_MEMORY_ERASED, VISION_MEMORY_INDEXED, VisualItem, VisualKind, VisualMemory,
    VisualQuery,
};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodedImage, Embedder, EmbeddingSpace, ImageRef,
    Locality, MediaScope, MediaStore, Metric, Provenance, SentImage, SourceImage, VisionCx,
    VisionError, VisionRuntime, artifact_uri,
};
use chrono::{DateTime, NaiveDate, Utc};

pub const COLLECTION: &str = "inspector-frames";

/// Mean colour as the image vector; colour words as text vectors.
pub struct Palette {
    descriptor: BackendDescriptor,
    space: EmbeddingSpace,
}

impl Palette {
    pub fn new(revision: &str, dimensions: usize) -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "palette",
                Locality::InProcess,
                CoordFrame::SentPixels,
            ),
            space: EmbeddingSpace {
                model: "palette".into(),
                revision: revision.into(),
                dimensions,
                metric: Metric::Cosine,
            },
        }
    }

    fn pad(&self, mut v: Vec<f32>) -> Vec<f32> {
        v.resize(self.space.dimensions, 0.0);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }
}

#[async_trait::async_trait]
impl Embedder for Palette {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    async fn embed_images(
        &self,
        _: &VisionCx,
        images: &[SentImage],
    ) -> adk_vision::Result<Vec<Vec<f32>>> {
        Ok(images
            .iter()
            .map(|image| {
                let rgba = image.pixels().rgba();
                let n = (rgba.len() / 4).max(1) as f32;
                let mut sum = [0.0f32; 3];
                for px in rgba.chunks_exact(4) {
                    for c in 0..3 {
                        sum[c] += f32::from(px[c]);
                    }
                }
                self.pad(sum.iter().map(|s| s / n / 255.0).collect())
            })
            .collect())
    }

    async fn embed_texts(&self, _: &VisionCx, texts: &[&str]) -> adk_vision::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let has = |w: &str| if t.contains(w) { 1.0 } else { 0.0 };
                self.pad(vec![has("red"), has("green"), has("blue")])
            })
            .collect())
    }
}

pub fn runtime(revision: &str, dimensions: usize) -> Arc<VisionRuntime> {
    Arc::new(
        VisionRuntime::builder()
            .embedder(Arc::new(Palette::new(revision, dimensions)))
            .build()
            .unwrap(),
    )
}

pub fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
}

pub fn tuesday() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()
}

pub struct Fixture {
    pub artifacts: Arc<InMemoryArtifactService>,
    pub media: MediaStore,
    pub captions: Arc<InMemoryMemoryService>,
    pub graph: Arc<GraphMemoryService>,
    pub recorder: InMemoryAuditRecorder,
    pub cx: VisionCx,
}

impl Fixture {
    pub async fn new() -> Self {
        let artifacts = Arc::new(InMemoryArtifactService::new());
        let graph = GraphMemoryService::new("sqlite::memory:").await.unwrap();
        graph.migrate().await.unwrap();
        let recorder = InMemoryAuditRecorder::new();
        let cx = VisionCx::new()
            .with_audit(AuditTrail::new(Arc::new(recorder.clone())), AuditActor::default());
        Self {
            media: MediaStore::new(artifacts.clone()),
            artifacts,
            captions: Arc::new(InMemoryMemoryService::new()),
            graph: Arc::new(graph),
            recorder,
            cx,
        }
    }

    /// Stores a solid-colour picture with a mark that makes each one a distinct image.
    pub async fn image(&self, colour: [u8; 3], mark: u32) -> (ImageRef, SourceImage) {
        let mut rgba = Vec::new();
        for i in 0..(16 * 16) {
            let mut px = [colour[0], colour[1], colour[2], 255];
            if i == mark % 256 {
                px[0] = px[0].wrapping_add(1);
            }
            rgba.extend(px);
        }
        let pixels = DecodedImage::from_rgba(16, 16, rgba).unwrap();
        self.media
            .put_pixels(&MediaScope::new("plant", "u1", "s1"), pixels, Provenance::Upload)
            .await
            .unwrap()
    }

    pub fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.recorder.records().into_iter().filter(|r| r.action == action).collect()
    }

    pub async fn memory(
        &self,
        store: Arc<dyn VectorStore>,
        runtime: Arc<VisionRuntime>,
    ) -> VisualMemory {
        VisualMemory::builder(runtime, store, COLLECTION)
            .captions(self.captions.clone())
            .graph(self.graph.clone())
            .artifacts(self.artifacts.clone())
            .open()
            .await
            .unwrap()
    }

    pub async fn captions_for(&self, query: &str) -> Vec<adk_memory::MemoryEntry> {
        self.captions
            .search(SearchRequest {
                query: query.into(),
                user_id: "u1".into(),
                app_name: "plant".into(),
                limit: Some(20),
                min_score: None,
                project_id: None,
            })
            .await
            .unwrap()
            .memories
    }
}

const RED: [u8; 3] = [220, 30, 30];
const GREEN: [u8; 3] = [30, 200, 40];
const BLUE: [u8; 3] = [30, 40, 220];

/// The whole suite. `reopen` returns a store over the same data — the same object for an
/// in-memory store, a new connection for a persistent one.
pub async fn run<F, Fut>(reopen: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn VectorStore>>,
{
    let fx = Fixture::new().await;
    let store = reopen().await;
    let memory = fx.memory(store.clone(), runtime("1", 8)).await;
    let scope = Scope::from_parts(["plant", "line-a"]).unwrap();
    let other = Scope::from_parts(["plant", "line-b"]).unwrap();

    // A week's worth in miniature: red seals on Tuesday from camera-1, blue cartons on
    // Tuesday and Wednesday from camera-2, a green delivery note, and one image camera-3
    // shares with camera-1.
    let mut items = Vec::new();
    for n in 0..3 {
        let (image, pixels) = fx.image(RED, n).await;
        items.push(
            VisualItem::new(
                image,
                pixels,
                format!("camera-1 carton {n}: seal torn"),
                "camera-1",
                at(&format!("2026-11-17T0{}:00:00Z", 8 + n)),
            )
            .with_labels(["torn seal"])
            .with_run("run-tue"),
        );
    }
    for (n, day) in [(10, "17"), (11, "18"), (12, "18")] {
        let (image, pixels) = fx.image(BLUE, n).await;
        items.push(
            VisualItem::new(
                image,
                pixels,
                format!("camera-2 carton {n}: sealed"),
                "camera-2",
                at(&format!("2026-11-{day}T10:00:00Z")),
            )
            .with_run(format!("run-{day}")),
        );
    }
    let (note, note_pixels) = fx.image(GREEN, 20).await;
    items.push(
        VisualItem::new(
            note,
            note_pixels,
            "delivery note DN-4201",
            "DN-4201",
            at("2026-11-17T12:00:00Z"),
        )
        .with_kind(VisualKind::Page)
        .with_run("run-tue"),
    );
    let shared = items[0].clone();
    let mut camera3 = shared.clone().with_region(BBox::new(0.0, 0.0, 8.0, 8.0));
    camera3.source = "camera-3".into();
    camera3.caption = "camera-3 crop of the same carton".into();
    items.push(camera3);
    let indexed = memory.index(&fx.cx, &scope, &items).await.unwrap();
    assert_eq!(indexed.value.len(), items.len());
    assert_eq!(indexed.calls.len(), 2, "whole images in one call, the region in its own");
    let record = &fx.records(VISION_MEMORY_INDEXED)[0];
    assert_eq!(record.ext["chunks"], items.len());
    assert_eq!(record.ext["runs"], serde_json::json!(["run-17", "run-18", "run-tue"]));
    // Indexing again replaces, never duplicates.
    memory.index(&fx.cx, &scope, &items[..1]).await.unwrap();
    assert_eq!(store.count(COLLECTION, &scope, None).await.unwrap(), items.len() as u64);
    memory.index(&fx.cx, &other, &items[3..4]).await.unwrap();

    // By text: the red seals, each linked to its artifact and the run that stored it.
    let hits = memory
        .search_text(
            &fx.cx,
            &scope,
            "a red torn seal",
            &VisualQuery::top(3).captured_on([tuesday()]),
        )
        .await
        .unwrap()
        .value;
    assert_eq!(hits.len(), 3);
    for hit in &hits {
        assert!(hit.caption.contains("seal torn") || hit.source == "camera-3", "{hit:?}");
        assert_eq!(hit.run.as_deref(), Some("run-tue"));
        assert_eq!(hit.captured_at.unwrap().date_naive(), tuesday());
        let image =
            items.iter().find(|i| artifact_uri(&i.image.artifact) == hit.artifact_uri).unwrap();
        assert_eq!(hit.digest, Some(image.image.digest));
        let loaded = fx.media.load(&image.image).await.unwrap();
        assert_eq!(loaded.digest(), image.image.digest);
    }
    // A day filter and a kind filter.
    let wednesday = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
    let blue = memory
        .search_text(&fx.cx, &scope, "blue", &VisualQuery::top(10).captured_on([wednesday]))
        .await
        .unwrap()
        .value;
    assert_eq!(blue.len(), 2);
    assert!(blue.iter().all(|h| h.source == "camera-2"));
    let pages = memory
        .search_text(&fx.cx, &scope, "anything", &VisualQuery::top(10).kind(VisualKind::Page))
        .await
        .unwrap()
        .value;
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].source, "DN-4201");

    // By example image.
    let (_, example) = fx.image(RED, 200).await;
    let hits =
        memory.search_image(&fx.cx, &scope, &example, &VisualQuery::top(2)).await.unwrap().value;
    assert!(hits.iter().all(|h| h.labels == ["torn seal"] || h.source == "camera-3"), "{hits:?}");
    let region_hit = memory
        .search_image(&fx.cx, &scope, &example, &VisualQuery::top(1).source("camera-3"))
        .await
        .unwrap()
        .value;
    assert_eq!(region_hit[0].region, Some(BBox::new(0.0, 0.0, 8.0, 8.0)));

    // Scopes stay apart.
    let elsewhere =
        memory.search_text(&fx.cx, &other, "red", &VisualQuery::top(10)).await.unwrap().value;
    assert_eq!(elsewhere.len(), 1);
    assert_eq!(elsewhere[0].source, "camera-2");

    // Captions: keyword recall in adk-memory returns the artifact reference with the text.
    let captions = fx.captions_for("seal torn").await;
    assert_eq!(captions.len(), 3);
    for caption in &captions {
        let hit = items.iter().find(|i| Some(i.chunk_id()) == caption.id).unwrap();
        assert_eq!(
            caption.artifact_references(),
            vec![ArtifactReference::new(artifact_uri(&hit.image.artifact), "image/png")]
        );
    }
    // Graph: the source, its observations, and the label it was seen with.
    let nodes = fx.graph.search_nodes("plant", "u1", "camera-1", 5).await.unwrap();
    let camera = nodes.iter().find(|n| n.entity.name == "camera-1").unwrap();
    assert_eq!(camera.entity.entity_type, "vision.source");
    assert_eq!(camera.entity.observations.len(), 4, "three frames and the re-index");
    assert!(
        camera.relations.iter().any(|r| r.relation_type == "observed" && r.target == "torn seal")
    );

    // An adk-rag pipeline over the same collection answers text with images.
    let provider = Arc::new(memory.text_embedding());
    assert_eq!(provider.dimensions(), 8);
    let pipeline = RagPipeline::builder()
        .config(RagConfig::builder().top_k(1).similarity_threshold(-1.0).build().unwrap())
        .embedding_provider(provider)
        .vector_store(store.clone())
        .build()
        .unwrap();
    let found = pipeline.scoped(scope.clone()).query(COLLECTION, "green").await.unwrap();
    assert_eq!(
        found[0].chunk.source_uri.as_deref(),
        Some(artifact_uri(&items[6].image.artifact).as_str())
    );

    // One space per collection: another revision or size is refused, on this connection
    // and on a new one.
    for (revision, dims) in [("2", 8), ("1", 12)] {
        let err = VisualMemory::builder(runtime(revision, dims), reopen().await, COLLECTION)
            .open()
            .await
            .unwrap_err();
        let VisionError::SpaceMismatch { offered, recorded, .. } = &err else { panic!("{err}") };
        assert_eq!(offered, &format!("palette@{revision}/{dims}/cosine"));
        if dims == 8 {
            assert_eq!(recorded, "palette@1/8/cosine");
        }
        assert_eq!(err.code(), "vision.memory.space_mismatch");
    }
    let again = fx.memory(reopen().await, runtime("1", 8)).await;
    assert_eq!(again.space().revision, "1");
    assert_eq!(store.count(COLLECTION, &Scope::new(SPACE_SCOPE).unwrap(), None).await.unwrap(), 1);

    // Erasure: camera-1's chunks, captions, graph entity and the artifacts nothing else uses.
    let erased = memory.erase_source(&fx.cx, &scope, "camera-1").await.unwrap();
    assert_eq!(erased.chunks, 3);
    assert_eq!(erased.captions, 3);
    assert_eq!(erased.entities, 1);
    assert_eq!(erased.artifacts.len(), 2);
    assert_eq!(
        erased.artifacts_kept,
        vec![shared.image.artifact.clone()],
        "camera-3 still uses it"
    );
    let by_source = Filter::eq(Field::DocumentId, "camera-1");
    assert_eq!(store.count(COLLECTION, &scope, Some(&by_source)).await.unwrap(), 0);
    assert!(fx.captions_for("seal torn").await.is_empty());
    assert!(fx.graph.open_nodes("plant", "u1", vec!["camera-1".into()]).await.unwrap().is_empty());
    for gone in &erased.artifacts {
        let image = items.iter().find(|i| &i.image.artifact == gone).unwrap();
        let err = fx.media.load(&image.image).await.unwrap_err();
        assert_eq!(adk_core::AdkError::from(err).category, adk_core::ErrorCategory::NotFound);
    }
    fx.media.load(&shared.image).await.unwrap();
    let hits =
        memory.search_text(&fx.cx, &scope, "red", &VisualQuery::top(10)).await.unwrap().value;
    assert!(hits.iter().all(|h| h.source != "camera-1"));
    let record = &fx.records(VISION_MEMORY_ERASED)[0];
    assert_eq!(record.ext["source"], "camera-1");
    assert_eq!(record.ext["chunks"], 3);
    assert_eq!(record.ext["captions"], 3);
    assert_eq!(record.ext["digests"].as_array().unwrap().len(), 3);
    assert_eq!(record.payload_ref.as_deref(), Some("camera-1"));
    // The other scope is untouched; erasing camera-3 then frees the shared artifact.
    assert_eq!(store.count(COLLECTION, &other, None).await.unwrap(), 1);
    let erased = memory.erase_source(&fx.cx, &scope, "camera-3").await.unwrap();
    assert_eq!(erased.artifacts, vec![shared.image.artifact.clone()]);
    let listed = fx
        .artifacts
        .list(adk_artifact::ListRequest {
            app_name: "plant".into(),
            user_id: "u1".into(),
            session_id: "s1".into(),
        })
        .await
        .unwrap()
        .file_names;
    assert!(!listed.iter().any(|f| f == &shared.image.artifact.file_name));
}
