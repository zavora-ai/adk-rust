//! Visual memory (spec 3.7 §7.15), feature `memory`: images indexed by their embeddings in
//! any `adk-rag` [`VectorStore`], found again by text or by an example image, captioned in
//! `adk-memory`, and erased by source.
//!
//! | Step | What happens |
//! |------|--------------|
//! | Index | [`VisualMemory::index`] embeds each image through the [`VisionRuntime`] — metered, local first — and stores one [`Chunk`] per image: `text` is the caption, `embedding` the image vector, `source_uri` the artifact's URI, and `metadata` the digest, artifact, region, source, capture time and the run that stored it |
//! | Query | [`VisualMemory::search_text`] embeds the text into the same space (SigLIP 2 has one space for texts and images); [`VisualMemory::search_image`] embeds an example image. Each [`VisualHit`] names its artifact and its run |
//! | One space | The collection records its [`EmbeddingSpace`] — model, revision, dimensions, metric — when it is first opened, and refuses an embedder of another space with [`VisionError::SpaceMismatch`]: changing the embedder means a new collection, never mixed vectors |
//! | Captions | With [`captions`](VisualMemoryBuilder::captions), each caption is an `adk-memory` entry — its own memory session, named by its chunk — whose content is the caption's text and a `Part::FileData` reference to its artifact, so keyword recall finds it and returns the image's locator |
//! | Graph | With [`graph`](VisualMemoryBuilder::graph), each source is an entity whose observations are its captions, related to the labels seen in it |
//! | Erasure | [`VisualMemory::erase_source`] deletes a source's chunks, captions, graph entity and the artifacts no other chunk of the scope still uses, and writes `vision.memory.erased` |
//!
//! The space is recorded in the collection itself, as a record in a reserved scope
//! ([`SPACE_SCOPE`]), so it holds on every backend — in memory, LanceDB, pgvector, Qdrant
//! and Agent Retrieval alike — and survives a restart on the persistent ones.
//!
//! [`SiglipTextEmbedding`] adapts the runtime's embedder to `adk_rag::EmbeddingProvider`, so
//! a [`RagPipeline`](adk_rag::RagPipeline) or [`RagTool`](adk_rag::RagTool) over the same
//! collection answers text questions with images.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use adk_rag::{InMemoryVectorStore, Scope};
//! use adk_vision::memory::{VisualItem, VisualMemory, VisualQuery};
//! use adk_vision::{
//!     ArtifactLocator, BackendDescriptor, CoordFrame, DecodedImage, Digest, Embedder,
//!     EmbeddingSpace, ImageRef, Locality, Metric, Provenance, SentImage, SourceImage, VisionCx,
//!     VisionRuntime,
//! };
//!
//! /// Embeds an image as its mean colour, and a text naming a colour as that colour.
//! struct Colours(BackendDescriptor, EmbeddingSpace);
//!
//! #[async_trait::async_trait]
//! impl Embedder for Colours {
//!     fn descriptor(&self) -> &BackendDescriptor {
//!         &self.0
//!     }
//!     fn space(&self) -> &EmbeddingSpace {
//!         &self.1
//!     }
//!     async fn embed_images(&self, _: &VisionCx, images: &[SentImage]) -> adk_vision::Result<Vec<Vec<f32>>> {
//!         Ok(images.iter().map(|i| {
//!             let p = i.pixels().pixel(0, 0);
//!             vec![f32::from(p[0]), f32::from(p[1]), f32::from(p[2])]
//!         }).collect())
//!     }
//!     async fn embed_texts(&self, _: &VisionCx, texts: &[&str]) -> adk_vision::Result<Vec<Vec<f32>>> {
//!         Ok(texts.iter().map(|t| if t.contains("red") { vec![1.0, 0.0, 0.0] } else { vec![0.0, 0.0, 1.0] }).collect())
//!     }
//! }
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let space = EmbeddingSpace { model: "colours".into(), revision: "1".into(), dimensions: 3, metric: Metric::Cosine };
//! let descriptor = BackendDescriptor::new("colours", Locality::InProcess, CoordFrame::SentPixels);
//! let runtime = VisionRuntime::builder().embedder(Arc::new(Colours(descriptor, space))).build().unwrap();
//! let memory = VisualMemory::builder(Arc::new(runtime), Arc::new(InMemoryVectorStore::new()), "frames")
//!     .open()
//!     .await
//!     .unwrap();
//!
//! let scope = Scope::new("plant-7").unwrap();
//! let cx = VisionCx::new();
//! let mut items = Vec::new();
//! for (n, colour) in [[255, 0, 0, 255], [0, 0, 255, 255]].into_iter().enumerate() {
//!     let pixels = SourceImage::from_pixels(DecodedImage::filled(8, 8, colour)).unwrap();
//!     let image = ImageRef {
//!         artifact: ArtifactLocator::new("plant", "u1", "s1", format!("frame-{n}.png"), 0),
//!         digest: pixels.digest(),
//!         mime_type: "image/png".into(),
//!         width: 8,
//!         height: 8,
//!         provenance: Provenance::Upload,
//!     };
//!     let captured = chrono::DateTime::parse_from_rfc3339("2026-11-17T10:00:00Z").unwrap().to_utc();
//!     items.push(VisualItem::new(image, pixels, format!("frame {n}"), "camera-1", captured).with_run("run-1"));
//! }
//! memory.index(&cx, &scope, &items).await.unwrap();
//!
//! let hits = memory.search_text(&cx, &scope, "something red", &VisualQuery::top(1)).await.unwrap().value;
//! assert_eq!(hits[0].caption, "frame 0");
//! assert_eq!(hits[0].run.as_deref(), Some("run-1"));
//! assert_eq!(hits[0].artifact.file_name, "frame-0.png");
//! # });
//! ```

mod embedding;
pub mod fixtures;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use adk_artifact::{ArtifactService, DeleteRequest};
use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{AdkError, Content, ErrorCategory};
use adk_memory::graph::{CreateEntityInput, CreateRelationInput};
use adk_memory::{GraphMemoryService, MemoryEntry, MemoryService};
use adk_rag::{
    Chunk, CollectionSpec, Field, Filter, RagError, Scope, SearchResult, VectorQuery, VectorStore,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use embedding::SiglipTextEmbedding;

use crate::audit::VISION_AUDIT_KIND;
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::BBox;
use crate::media::{ArtifactLocator, ImageRef, SourceImage, artifact_uri};
use crate::runtime::{CallOptions, CallRecord, VisionOutput, VisionRuntime};
use crate::traits::EmbeddingSpace;

/// Action of the record an index call writes.
pub const VISION_MEMORY_INDEXED: &str = "vision.memory.indexed";
/// Action of the record an erasure writes.
pub const VISION_MEMORY_ERASED: &str = "vision.memory.erased";

/// The reserved scope a collection's space record lives in. [`Scope::from_parts`] never
/// produces it, because it percent-encodes `:`.
pub const SPACE_SCOPE: &str = ":adk-vision-space";

/// The id of the space record in [`SPACE_SCOPE`].
const SPACE_RECORD: &str = "space";

/// The metadata keys of a visual chunk.
pub mod keys {
    /// What the image is: `frame`, `page` or `image`.
    pub const KIND: &str = "kind";
    /// `sha256:<hex>` of the image's encoded bytes.
    pub const DIGEST: &str = "digest";
    /// The image's artifact locator, as JSON.
    pub const ARTIFACT: &str = "artifact";
    /// The image's media type.
    pub const MIME_TYPE: &str = "mimeType";
    /// The image's width and height in source pixels, as `<w>x<h>`.
    pub const SIZE: &str = "size";
    /// The embedded region in source pixels, as JSON `{x0, y0, x1, y1}`; absent for the
    /// whole image.
    pub const REGION: &str = "region";
    /// The frame source or document the image came from; also the chunk's `document_id`.
    pub const SOURCE: &str = "source";
    /// When the image was captured, RFC 3339 in UTC.
    pub const CAPTURED_AT: &str = "capturedAt";
    /// The UTC date of capture, `YYYY-MM-DD`: what a day filter matches.
    pub const DAY: &str = "day";
    /// The run (invocation) that stored the image.
    pub const RUN: &str = "run";
    /// The key of the collection's embedding space.
    pub const SPACE: &str = "space";
    /// Labels seen in the image, joined with `\n`.
    pub const LABELS: &str = "labels";
    /// The `adk-memory` entry id of the caption, when captions are stored.
    pub const CAPTION: &str = "caption";
}

/// The metadata keys a visual collection declares filterable.
pub const FILTERABLE: [&str; 5] = [keys::KIND, keys::DAY, keys::RUN, keys::SPACE, keys::DIGEST];

/// What an indexed image is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VisualKind {
    /// A frame from a camera, a screen or a video.
    Frame,
    /// A document page.
    Page,
    /// Any other image.
    Image,
}

impl VisualKind {
    /// The metadata value: `frame`, `page` or `image`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Page => "page",
            Self::Image => "image",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "frame" => Self::Frame,
            "page" => Self::Page,
            _ => Self::Image,
        }
    }
}

/// The key a space is recorded and filtered by: `<model>@<revision>/<dimensions>/<metric>`.
///
/// # Example
///
/// ```
/// use adk_vision::memory::space_key;
/// use adk_vision::{EmbeddingSpace, Metric};
///
/// let space = EmbeddingSpace { model: "siglip2".into(), revision: "d111".into(), dimensions: 768, metric: Metric::Cosine };
/// assert_eq!(space_key(&space), "siglip2@d111/768/cosine");
/// ```
pub fn space_key(space: &EmbeddingSpace) -> String {
    let metric = match space.metric {
        crate::traits::Metric::Cosine => "cosine",
        crate::traits::Metric::Dot => "dot",
        crate::traits::Metric::L2 => "l2",
    };
    format!("{}@{}/{}/{metric}", space.model, space.revision, space.dimensions)
}

/// One image to index.
///
/// The pixels are embedded; the [`ImageRef`] says where the image is kept, and the chunk
/// links to it. Both must be the same image: their digests must match.
#[derive(Clone, Debug)]
pub struct VisualItem {
    /// Where the image is kept.
    pub image: ImageRef,
    /// The decoded image.
    pub pixels: SourceImage,
    /// What the image shows, in words: the chunk's text and the stored caption.
    pub caption: String,
    /// The frame source or document the image came from.
    pub source: String,
    /// When it was captured.
    pub captured_at: DateTime<Utc>,
    /// What it is.
    pub kind: VisualKind,
    /// Only this region is embedded, in source pixels; `None` embeds the whole image.
    pub region: Option<BBox>,
    /// Labels seen in it, such as a detector's classes; graph memory relates the source to
    /// each.
    pub labels: Vec<String>,
    /// The run (invocation) that stores it.
    pub run: Option<String>,
}

impl VisualItem {
    /// A whole-image frame with no labels and no run.
    pub fn new(
        image: ImageRef,
        pixels: SourceImage,
        caption: impl Into<String>,
        source: impl Into<String>,
        captured_at: DateTime<Utc>,
    ) -> Self {
        Self {
            image,
            pixels,
            caption: caption.into(),
            source: source.into(),
            captured_at,
            kind: VisualKind::Frame,
            region: None,
            labels: Vec::new(),
            run: None,
        }
    }

    /// Sets what the image is.
    pub fn with_kind(mut self, kind: VisualKind) -> Self {
        self.kind = kind;
        self
    }

    /// Embeds only `region`, in source pixels.
    pub fn with_region(mut self, region: BBox) -> Self {
        self.region = Some(region);
        self
    }

    /// Adds labels seen in the image.
    pub fn with_labels<I, S>(mut self, labels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.labels.extend(labels.into_iter().map(Into::into));
        self
    }

    /// Names the run (invocation) that stores the image.
    pub fn with_run(mut self, run: impl Into<String>) -> Self {
        self.run = Some(run.into());
        self
    }

    /// The chunk id: stable for one source, image and region, so indexing again replaces.
    pub fn chunk_id(&self) -> String {
        let region = self.region.map(|r| format!("{},{},{},{}", r.x0, r.y0, r.x1, r.y1));
        let key = format!("{}\0{}\0{}", self.source, self.image.digest, region.unwrap_or_default());
        format!("vm-{}", &Digest::of(key.as_bytes()).hex()[..32])
    }
}

/// A search over a visual collection.
///
/// # Example
///
/// ```
/// use adk_vision::memory::{VisualKind, VisualQuery};
///
/// let tuesday = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
/// let query = VisualQuery::top(5).captured_on([tuesday]).kind(VisualKind::Frame);
/// assert_eq!(query.top_k, 5);
/// ```
#[derive(Clone, Debug, Default)]
pub struct VisualQuery {
    /// The most hits to return.
    pub top_k: usize,
    /// Only images captured on these UTC dates; empty means any day.
    pub days: Vec<NaiveDate>,
    /// Only images of this kind.
    pub kind: Option<VisualKind>,
    /// Only images from this source.
    pub source: Option<String>,
    /// A further filter over declared keys ([`FILTERABLE`]), the source or the artifact URI.
    pub filter: Option<Filter>,
}

impl VisualQuery {
    /// The `top_k` nearest images.
    pub fn top(top_k: usize) -> Self {
        Self { top_k, ..Self::default() }
    }

    /// Only images captured on `days` (UTC dates).
    pub fn captured_on(mut self, days: impl IntoIterator<Item = NaiveDate>) -> Self {
        self.days.extend(days);
        self
    }

    /// Only images of `kind`.
    pub fn kind(mut self, kind: VisualKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Only images from `source`.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Adds a filter.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    fn compile(&self, space: &str) -> Filter {
        let mut all = vec![Filter::eq(Field::metadata(keys::SPACE), space)];
        if !self.days.is_empty() {
            all.push(Filter::is_in(
                Field::metadata(keys::DAY),
                self.days.iter().map(|d| d.format("%Y-%m-%d").to_string()),
            ));
        }
        if let Some(kind) = self.kind {
            all.push(Filter::eq(Field::metadata(keys::KIND), kind.as_str()));
        }
        if let Some(source) = &self.source {
            all.push(Filter::eq(Field::DocumentId, source.clone()));
        }
        if let Some(filter) = &self.filter {
            all.push(filter.clone());
        }
        Filter::and(all)
    }
}

/// One image found: its score, caption, artifact and the run that stored it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualHit {
    /// The chunk id.
    pub id: String,
    /// The store's similarity score, higher is nearer.
    pub score: f32,
    /// The caption.
    pub caption: String,
    /// Where the image is kept.
    pub artifact: ArtifactLocator,
    /// The artifact's URI, as the chunk's `source_uri`.
    pub artifact_uri: String,
    /// The image's digest.
    pub digest: Option<Digest>,
    /// The image's media type.
    pub mime_type: Option<String>,
    /// The embedded region in source pixels, when only a region was embedded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<BBox>,
    /// The frame source or document.
    pub source: String,
    /// What the image is.
    pub kind: VisualKind,
    /// When it was captured.
    pub captured_at: Option<DateTime<Utc>>,
    /// The run (invocation) that stored it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Labels seen in it.
    pub labels: Vec<String>,
    /// The caption's `adk-memory` entry id, when captions are stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption_id: Option<String>,
}

impl VisualHit {
    /// Reads a hit back from a stored chunk.
    ///
    /// # Errors
    ///
    /// [`VisionError::Parse`] when the chunk carries no artifact locator: it was not stored
    /// by a visual memory.
    pub fn from_result(result: &SearchResult) -> Result<Self> {
        let chunk = &result.chunk;
        let meta = |key: &str| chunk.metadata.get(key).cloned();
        let artifact: ArtifactLocator = meta(keys::ARTIFACT)
            .and_then(|json| serde_json::from_str(&json).ok())
            .ok_or_else(|| VisionError::Parse {
                backend: "visual-memory".into(),
                message: format!("chunk `{}` carries no artifact locator", chunk.id),
            })?;
        Ok(Self {
            id: chunk.id.clone(),
            score: result.score,
            caption: chunk.text.clone(),
            artifact_uri: chunk.source_uri.clone().unwrap_or_else(|| artifact_uri(&artifact)),
            artifact,
            digest: meta(keys::DIGEST).and_then(|d| d.parse().ok()),
            mime_type: meta(keys::MIME_TYPE),
            region: meta(keys::REGION).and_then(|r| serde_json::from_str(&r).ok()),
            source: chunk.document_id.clone(),
            kind: meta(keys::KIND).map_or(VisualKind::Image, |k| VisualKind::parse(&k)),
            captured_at: meta(keys::CAPTURED_AT)
                .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
                .map(|t| t.to_utc()),
            run: meta(keys::RUN),
            labels: meta(keys::LABELS)
                .map(|l| l.split('\n').filter(|s| !s.is_empty()).map(str::to_string).collect())
                .unwrap_or_default(),
            caption_id: meta(keys::CAPTION),
        })
    }
}

/// What an erasure removed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Erasure {
    /// The erased source.
    pub source: String,
    /// Chunks deleted from the collection.
    pub chunks: u64,
    /// Caption entries deleted from `adk-memory`.
    pub captions: u64,
    /// Graph entities deleted.
    pub entities: usize,
    /// Artifacts deleted: those no remaining chunk of the scope refers to.
    pub artifacts: Vec<ArtifactLocator>,
    /// Artifacts kept because another chunk of the scope still refers to them.
    pub artifacts_kept: Vec<ArtifactLocator>,
}

/// Images by embedding in one collection of an `adk-rag` vector store, in one embedding
/// space. See the [module documentation](self).
pub struct VisualMemory {
    runtime: Arc<VisionRuntime>,
    options: CallOptions,
    store: Arc<dyn VectorStore>,
    collection: String,
    space: EmbeddingSpace,
    space_key: String,
    captions: Option<Arc<dyn MemoryService>>,
    graph: Option<Arc<GraphMemoryService>>,
    artifacts: Option<Arc<dyn ArtifactService>>,
}

impl std::fmt::Debug for VisualMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisualMemory")
            .field("collection", &self.collection)
            .field("space", &self.space)
            .field("captions", &self.captions.is_some())
            .field("graph", &self.graph.is_some())
            .field("artifacts", &self.artifacts.is_some())
            .finish_non_exhaustive()
    }
}

/// Builds a [`VisualMemory`].
#[must_use]
pub struct VisualMemoryBuilder {
    runtime: Arc<VisionRuntime>,
    options: CallOptions,
    store: Arc<dyn VectorStore>,
    collection: String,
    captions: Option<Arc<dyn MemoryService>>,
    graph: Option<Arc<GraphMemoryService>>,
    artifacts: Option<Arc<dyn ArtifactService>>,
}

impl VisualMemoryBuilder {
    /// Embeds with these call options: a route naming the embedder, a region.
    pub fn call_options(mut self, options: CallOptions) -> Self {
        self.options = options;
        self
    }

    /// Stores each caption in `memory` with a reference to its artifact.
    pub fn captions(mut self, memory: Arc<dyn MemoryService>) -> Self {
        self.captions = Some(memory);
        self
    }

    /// Records each source as an entity of `graph`, its captions as observations, related
    /// to the labels seen in it.
    pub fn graph(mut self, graph: Arc<GraphMemoryService>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// Deletes a source's artifacts from `artifacts` when it is erased.
    pub fn artifacts(mut self, artifacts: Arc<dyn ArtifactService>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    /// Creates the collection if needed and checks its space: a new collection records the
    /// runtime embedder's space; an existing one must hold that space.
    ///
    /// # Errors
    ///
    /// [`VisionError::SpaceMismatch`] when the collection holds another space,
    /// [`VisionError::BackendNotFound`] without an embedder, and the store's errors.
    pub async fn open(self) -> Result<VisualMemory> {
        let space = self.runtime.embedding_space(&self.options)?;
        let memory = VisualMemory {
            space_key: space_key(&space),
            space,
            runtime: self.runtime,
            options: self.options,
            store: self.store,
            collection: self.collection,
            captions: self.captions,
            graph: self.graph,
            artifacts: self.artifacts,
        };
        let spec = CollectionSpec::new(memory.space.dimensions).with_filterable(FILTERABLE);
        memory.store.create_collection(&memory.collection, &spec).await.map_err(rag)?;
        let reserved = space_scope();
        if memory.store.count(&memory.collection, &reserved, None).await.map_err(rag)? == 0 {
            memory
                .store
                .upsert(&memory.collection, &reserved, &[memory.space_record()?])
                .await
                .map_err(rag)?;
            tracing::info!(
                collection = %memory.collection,
                space = %memory.space_key,
                "visual memory collection created"
            );
        } else {
            memory.check_space().await?;
        }
        Ok(memory)
    }
}

fn space_scope() -> Scope {
    Scope::new(SPACE_SCOPE).expect("the reserved scope key is valid")
}

fn rag(err: RagError) -> VisionError {
    VisionError::Core(AdkError::from(err))
}

fn kind() -> AuditRecordKind {
    AuditRecordKind::Other(VISION_AUDIT_KIND.to_string())
}

impl VisualMemory {
    /// A builder over `runtime`'s embedder and collection `collection` of `store`.
    pub fn builder(
        runtime: Arc<VisionRuntime>,
        store: Arc<dyn VectorStore>,
        collection: impl Into<String>,
    ) -> VisualMemoryBuilder {
        VisualMemoryBuilder {
            runtime,
            options: CallOptions::default(),
            store,
            collection: collection.into(),
            captions: None,
            graph: None,
            artifacts: None,
        }
    }

    /// The collection's embedding space.
    pub fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    /// The collection.
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// The vector store.
    pub fn store(&self) -> &Arc<dyn VectorStore> {
        &self.store
    }

    /// The text embedder over the same runtime and route, as an `adk_rag::EmbeddingProvider`
    /// for a [`RagPipeline`](adk_rag::RagPipeline) over this collection.
    pub fn text_embedding(&self) -> SiglipTextEmbedding {
        SiglipTextEmbedding::from_parts(
            Arc::clone(&self.runtime),
            self.options.clone(),
            self.space.clone(),
        )
    }

    /// A unit vector of the space: the space record's embedding and the query that lists a
    /// filter's chunks.
    fn axis(&self) -> Vec<f32> {
        let mut axis = vec![0.0; self.space.dimensions];
        if let Some(first) = axis.first_mut() {
            *first = 1.0;
        }
        axis
    }

    fn space_record(&self) -> Result<Chunk> {
        let text = serde_json::to_string(&self.space).map_err(|e| VisionError::Backend {
            backend: "visual-memory".into(),
            message: format!("the space record could not be written: {e}"),
        })?;
        Ok(Chunk::new(SPACE_RECORD, SPACE_RECORD, text)
            .with_embedding(self.axis())
            .with_metadata(keys::SPACE, self.space_key.clone()))
    }

    /// Refuses unless the collection's space record is this memory's space.
    async fn check_space(&self) -> Result<()> {
        let reserved = space_scope();
        let mine = Filter::eq(Field::metadata(keys::SPACE), self.space_key.clone());
        if self.store.count(&self.collection, &reserved, Some(&mine)).await.map_err(rag)? == 1 {
            return Ok(());
        }
        let recorded = match self
            .store
            .search(&self.collection, &reserved, &VectorQuery::new(&self.axis(), 1))
            .await
        {
            Ok(found) => found
                .first()
                .and_then(|r| serde_json::from_str::<EmbeddingSpace>(&r.chunk.text).ok())
                .map_or_else(
                    || "an unreadable space record".to_string(),
                    |space| space_key(&space),
                ),
            Err(err) => format!(
                "a space whose record cannot be read with {} dimensions ({err})",
                self.space.dimensions
            ),
        };
        Err(VisionError::SpaceMismatch {
            collection: self.collection.clone(),
            recorded,
            offered: self.space_key.clone(),
        })
    }

    /// Refuses when the runtime's embedder for this memory's route is not the collection's
    /// space, or the collection's record changed since it was opened.
    async fn check_embedder(&self) -> Result<()> {
        let current = self.runtime.embedding_space(&self.options)?;
        if current != self.space {
            return Err(VisionError::SpaceMismatch {
                collection: self.collection.clone(),
                recorded: self.space_key.clone(),
                offered: space_key(&current),
            });
        }
        self.check_space().await
    }

    /// Embeds `items` and stores one chunk per item in `scope`; with captions or a graph
    /// configured, stores those too. Indexing an item again replaces its chunk.
    ///
    /// # Errors
    ///
    /// [`VisionError::SpaceMismatch`] when the embedder is not the collection's space;
    /// [`VisionError::InvalidInput`] when an item's pixels are not its image; the runtime's
    /// errors (budget, kill switch, cancellation); the stores' errors.
    pub async fn index(
        &self,
        cx: &VisionCx,
        scope: &Scope,
        items: &[VisualItem],
    ) -> Result<VisionOutput<Vec<String>>> {
        cx.check()?;
        self.check_embedder().await?;
        for item in items {
            if item.pixels.digest() != item.image.digest {
                return Err(VisionError::InvalidInput(format!(
                    "item `{}` has pixels of {} but names image {}; index the image it names",
                    item.chunk_id(),
                    item.pixels.digest(),
                    item.image.digest
                )));
            }
        }
        let (vectors, calls) = self.embed_items(cx, items).await?;
        let mut chunks = Vec::with_capacity(items.len());
        for (item, vector) in items.iter().zip(vectors) {
            chunks.push(self.chunk(item, vector)?);
        }
        // The chunks go first: erasure finds a source's captions through them.
        self.store.upsert(&self.collection, scope, &chunks).await.map_err(rag)?;
        if let Some(captions) = &self.captions {
            for (item, chunk) in items.iter().zip(&chunks) {
                let at = &item.image.artifact;
                // Each caption is a memory session of its own, named by its chunk:
                // `add_session` replaces a session on some backends and appends on others,
                // and one caption per session behaves the same on both.
                captions.delete_memory(&at.app_name, &at.user_id, &chunk.id).await?;
                let entry = MemoryEntry::new(
                    Content::new("model").with_text(item.caption.clone()),
                    "vision",
                )
                .with_id(chunk.id.clone())
                .with_timestamp(item.captured_at)
                .with_artifact(artifact_uri(at), item.image.mime_type.clone());
                captions.add_session(&at.app_name, &at.user_id, &chunk.id, vec![entry]).await?;
            }
        }
        if let Some(graph) = &self.graph {
            self.record_graph(graph, items).await?;
        }
        let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
        let digests: BTreeSet<String> = items.iter().map(|i| i.image.digest.to_string()).collect();
        let sources: BTreeSet<&str> = items.iter().map(|i| i.source.as_str()).collect();
        let runs: BTreeSet<&str> = items.iter().filter_map(|i| i.run.as_deref()).collect();
        let mut draft = AuditRecordDraft::new(kind(), VISION_MEMORY_INDEXED)
            .outcome(AuditRecordOutcome::Succeeded)
            .ext("collection", json!(self.collection))
            .ext("scope", json!(scope.as_str()))
            .ext("space", json!(self.space_key))
            .ext("chunks", json!(ids.len()))
            .ext("digests", json!(digests))
            .ext("sources", json!(sources))
            .ext("runs", json!(runs));
        if let Some(first) = digests.first() {
            draft = draft.payload_ref(first.clone());
        }
        cx.audit(draft).await?;
        tracing::debug!(
            collection = %self.collection,
            chunks = ids.len(),
            "visual memory indexed"
        );
        Ok(VisionOutput { value: ids, calls })
    }

    /// Embeds whole images in one call and each region in its own.
    async fn embed_items(
        &self,
        cx: &VisionCx,
        items: &[VisualItem],
    ) -> Result<(Vec<Vec<f32>>, Vec<CallRecord>)> {
        let mut vectors: Vec<Option<Vec<f32>>> = vec![None; items.len()];
        let mut calls = Vec::new();
        let whole: Vec<usize> = (0..items.len()).filter(|&i| items[i].region.is_none()).collect();
        if !whole.is_empty() {
            let images: Vec<&SourceImage> = whole.iter().map(|&i| &items[i].pixels).collect();
            let out = self.runtime.embed_images(cx, &images, &self.options).await?;
            calls.extend(out.calls);
            for (&i, v) in whole.iter().zip(out.value) {
                vectors[i] = Some(v);
            }
        }
        for (i, item) in items.iter().enumerate() {
            if let Some(region) = item.region {
                let mut options = self.options.clone();
                options.region = Some(region);
                let out = self.runtime.embed_images(cx, &[&item.pixels], &options).await?;
                calls.extend(out.calls);
                vectors[i] = out.value.into_iter().next();
            }
        }
        let vectors = vectors
            .into_iter()
            .map(|v| {
                v.ok_or_else(|| VisionError::Parse {
                    backend: self.space.model.clone(),
                    message: "the embedder returned fewer vectors than images".into(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((vectors, calls))
    }

    fn chunk(&self, item: &VisualItem, vector: Vec<f32>) -> Result<Chunk> {
        let at = &item.image.artifact;
        let locator = serde_json::to_string(at).map_err(|e| VisionError::Backend {
            backend: "visual-memory".into(),
            message: format!("the artifact locator could not be written: {e}"),
        })?;
        let mut chunk = Chunk::new(item.chunk_id(), item.source.clone(), item.caption.clone())
            .with_embedding(vector)
            .with_source_uri(artifact_uri(at))
            .with_metadata(keys::KIND, item.kind.as_str())
            .with_metadata(keys::DIGEST, item.image.digest.to_string())
            .with_metadata(keys::ARTIFACT, locator)
            .with_metadata(keys::MIME_TYPE, item.image.mime_type.clone())
            .with_metadata(keys::SIZE, format!("{}x{}", item.image.width, item.image.height))
            .with_metadata(keys::SOURCE, item.source.clone())
            .with_metadata(
                keys::CAPTURED_AT,
                item.captured_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            )
            .with_metadata(keys::DAY, item.captured_at.date_naive().format("%Y-%m-%d").to_string())
            .with_metadata(keys::SPACE, self.space_key.clone());
        if let Some(region) = item.region {
            let region = serde_json::to_string(&region).map_err(|e| VisionError::Backend {
                backend: "visual-memory".into(),
                message: format!("the region could not be written: {e}"),
            })?;
            chunk = chunk.with_metadata(keys::REGION, region);
        }
        if let Some(run) = &item.run {
            chunk = chunk.with_metadata(keys::RUN, run.clone());
        }
        if !item.labels.is_empty() {
            chunk = chunk.with_metadata(keys::LABELS, item.labels.join("\n"));
        }
        if self.captions.is_some() {
            chunk = chunk.with_metadata(keys::CAPTION, item.chunk_id());
        }
        Ok(chunk)
    }

    async fn record_graph(&self, graph: &GraphMemoryService, items: &[VisualItem]) -> Result<()> {
        // One entity per source and user; the labels are entities of their own.
        let mut by_user: BTreeMap<(&str, &str), Vec<&VisualItem>> = BTreeMap::new();
        for item in items {
            let at = &item.image.artifact;
            by_user.entry((at.app_name.as_str(), at.user_id.as_str())).or_default().push(item);
        }
        for ((app, user), items) in by_user {
            let mut sources: BTreeMap<&str, Vec<String>> = BTreeMap::new();
            let mut labels: BTreeSet<(&str, &str)> = BTreeSet::new();
            for item in &items {
                sources.entry(item.source.as_str()).or_default().push(format!(
                    "{}: {} [{}]",
                    item.captured_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    item.caption,
                    artifact_uri(&item.image.artifact)
                ));
                for label in &item.labels {
                    labels.insert((item.source.as_str(), label.as_str()));
                }
            }
            let mut entities: Vec<CreateEntityInput> = sources
                .into_iter()
                .map(|(source, observations)| CreateEntityInput {
                    name: source.to_string(),
                    entity_type: "vision.source".to_string(),
                    observations,
                })
                .collect();
            let names: BTreeSet<&str> = labels.iter().map(|(_, label)| *label).collect();
            entities.extend(names.into_iter().map(|label| CreateEntityInput {
                name: label.to_string(),
                entity_type: "vision.label".to_string(),
                observations: Vec::new(),
            }));
            graph.create_entities(app, user, entities).await?;
            if !labels.is_empty() {
                let relations = labels
                    .into_iter()
                    .map(|(source, label)| CreateRelationInput {
                        source: source.to_string(),
                        relation_type: "observed".to_string(),
                        target: label.to_string(),
                    })
                    .collect();
                graph.create_relations(app, user, relations).await?;
            }
        }
        Ok(())
    }

    async fn search_vector(
        &self,
        scope: &Scope,
        vector: &[f32],
        query: &VisualQuery,
    ) -> Result<Vec<VisualHit>> {
        let filter = query.compile(&self.space_key);
        let found = self
            .store
            .search(
                &self.collection,
                scope,
                &VectorQuery::new(vector, query.top_k).with_filter(&filter),
            )
            .await
            .map_err(rag)?;
        found.iter().map(VisualHit::from_result).collect()
    }

    /// The images nearest to `text` in the collection's space.
    ///
    /// # Errors
    ///
    /// [`VisionError::SpaceMismatch`] when the embedder is not the collection's space; the
    /// runtime's and the store's errors.
    pub async fn search_text(
        &self,
        cx: &VisionCx,
        scope: &Scope,
        text: &str,
        query: &VisualQuery,
    ) -> Result<VisionOutput<Vec<VisualHit>>> {
        cx.check()?;
        self.check_embedder().await?;
        let out = self.runtime.embed_texts(cx, &[text], &self.options).await?;
        let vector = out.value.into_iter().next().ok_or_else(|| VisionError::Parse {
            backend: self.space.model.clone(),
            message: "the embedder returned no vector for the query".into(),
        })?;
        let hits = self.search_vector(scope, &vector, query).await?;
        Ok(VisionOutput { value: hits, calls: out.calls })
    }

    /// The images nearest to an example image in the collection's space.
    ///
    /// # Errors
    ///
    /// As [`search_text`](Self::search_text).
    pub async fn search_image(
        &self,
        cx: &VisionCx,
        scope: &Scope,
        example: &SourceImage,
        query: &VisualQuery,
    ) -> Result<VisionOutput<Vec<VisualHit>>> {
        cx.check()?;
        self.check_embedder().await?;
        let out = self.runtime.embed_images(cx, &[example], &self.options).await?;
        let vector = out.value.into_iter().next().ok_or_else(|| VisionError::Parse {
            backend: self.space.model.clone(),
            message: "the embedder returned no vector for the example".into(),
        })?;
        let hits = self.search_vector(scope, &vector, query).await?;
        Ok(VisionOutput { value: hits, calls: out.calls })
    }

    /// Every chunk `filter` selects in `scope`, as hits (score is meaningless).
    async fn list(&self, scope: &Scope, filter: &Filter) -> Result<Vec<VisualHit>> {
        let n = self.store.count(&self.collection, scope, Some(filter)).await.map_err(rag)?;
        if n == 0 {
            return Ok(Vec::new());
        }
        let top_k = usize::try_from(n).unwrap_or(usize::MAX);
        let axis = self.axis();
        let found = self
            .store
            .search(&self.collection, scope, &VectorQuery::new(&axis, top_k).with_filter(filter))
            .await
            .map_err(rag)?;
        found.iter().map(VisualHit::from_result).collect()
    }

    /// Deletes everything stored from `source` in `scope`: its chunks, its captions, its
    /// graph entity (with its observations and relations), and each of its artifacts that no
    /// remaining chunk of the scope refers to. Writes one `vision.memory.erased` record.
    ///
    /// A frozen kill switch does not refuse an erasure, so data can be erased while agents
    /// are stopped; cancellation does.
    ///
    /// # Errors
    ///
    /// [`VisionError::Cancelled`], the stores' and the artifact service's errors, and the
    /// audit trail's when it fails closed. What was deleted before an error stays deleted.
    pub async fn erase_source(
        &self,
        cx: &VisionCx,
        scope: &Scope,
        source: &str,
    ) -> Result<Erasure> {
        if cx.cancellation().is_cancelled() {
            return Err(VisionError::Cancelled);
        }
        let by_source = Filter::eq(Field::DocumentId, source);
        let hits = self.list(scope, &by_source).await?;
        let chunks =
            self.store.delete_where(&self.collection, scope, &by_source).await.map_err(rag)?;
        let mut erasure = Erasure { source: source.to_string(), chunks, ..Erasure::default() };

        if let Some(captions) = &self.captions {
            let ids: BTreeSet<(&str, &str, &str)> = hits
                .iter()
                .filter_map(|h| {
                    h.caption_id
                        .as_deref()
                        .map(|id| (h.artifact.app_name.as_str(), h.artifact.user_id.as_str(), id))
                })
                .collect();
            for (app, user, id) in ids {
                erasure.captions += captions.delete_memory(app, user, id).await?;
            }
        }
        if let Some(graph) = &self.graph {
            let users: BTreeSet<(&str, &str)> = hits
                .iter()
                .map(|h| (h.artifact.app_name.as_str(), h.artifact.user_id.as_str()))
                .collect();
            for (app, user) in users {
                erasure.entities +=
                    graph.delete_entities(app, user, vec![source.to_string()]).await?.len();
            }
        }
        let mut seen = HashMap::new();
        for hit in &hits {
            seen.entry(hit.artifact_uri.clone()).or_insert_with(|| hit.artifact.clone());
        }
        let mut uris: Vec<_> = seen.into_iter().collect();
        uris.sort_by(|a, b| a.0.cmp(&b.0));
        for (uri, locator) in uris {
            let still = Filter::eq(Field::SourceUri, uri);
            if self.store.count(&self.collection, scope, Some(&still)).await.map_err(rag)? > 0 {
                erasure.artifacts_kept.push(locator);
                continue;
            }
            if let Some(artifacts) = &self.artifacts {
                let deleted = artifacts
                    .delete(DeleteRequest {
                        app_name: locator.app_name.clone(),
                        user_id: locator.user_id.clone(),
                        session_id: locator.session_id.clone(),
                        file_name: locator.file_name.clone(),
                        version: None,
                    })
                    .await;
                match deleted {
                    Ok(()) => {}
                    Err(err) if err.category == ErrorCategory::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
                erasure.artifacts.push(locator);
            }
        }
        let digests: BTreeSet<String> =
            hits.iter().filter_map(|h| h.digest.map(|d| d.to_string())).collect();
        cx.audit(
            AuditRecordDraft::new(kind(), VISION_MEMORY_ERASED)
                .outcome(AuditRecordOutcome::Succeeded)
                .payload_ref(source.to_string())
                .ext("collection", json!(self.collection))
                .ext("scope", json!(scope.as_str()))
                .ext("source", json!(source))
                .ext("chunks", json!(erasure.chunks))
                .ext("captions", json!(erasure.captions))
                .ext("entities", json!(erasure.entities))
                .ext("digests", json!(digests))
                .ext("artifacts", json!(erasure.artifacts))
                .ext("artifactsKept", json!(erasure.artifacts_kept)),
        )
        .await?;
        tracing::info!(
            collection = %self.collection,
            chunks = erasure.chunks,
            captions = erasure.captions,
            artifacts = erasure.artifacts.len(),
            "visual memory source erased"
        );
        Ok(erasure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_ids_are_stable_per_source_image_and_region() {
        let pixels =
            SourceImage::from_pixels(crate::media::DecodedImage::filled(4, 4, [9; 4])).unwrap();
        let image = ImageRef {
            artifact: ArtifactLocator::new("a", "u", "s", "f.png", 0),
            digest: pixels.digest(),
            mime_type: "image/png".into(),
            width: 4,
            height: 4,
            provenance: crate::media::Provenance::Upload,
        };
        let at = DateTime::from_timestamp(1_763_380_800, 0).unwrap();
        let item = VisualItem::new(image, pixels, "c", "cam-1", at);
        assert_eq!(item.chunk_id(), item.clone().with_run("r2").chunk_id());
        assert_ne!(item.chunk_id(), item.clone().with_region(BBox::new(0., 0., 2., 2.)).chunk_id());
        let mut other = item.clone();
        other.source = "cam-2".into();
        assert_ne!(item.chunk_id(), other.chunk_id());
        assert!(item.chunk_id().starts_with("vm-") && item.chunk_id().len() == 35);
    }

    #[test]
    fn queries_always_filter_by_space() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
        let filter = VisualQuery::top(5).captured_on([day]).kind(VisualKind::Frame).compile("s");
        let mut chunk = Chunk::new("c", "cam-1", "t")
            .with_metadata(keys::SPACE, "s")
            .with_metadata(keys::DAY, "2026-11-17")
            .with_metadata(keys::KIND, "frame");
        assert!(filter.matches(&chunk));
        chunk.metadata.insert(keys::SPACE.into(), "other".into());
        assert!(!filter.matches(&chunk));
        assert!(filter.validate().is_ok());
    }
}
