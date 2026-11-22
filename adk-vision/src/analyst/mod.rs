//! The video analyst (spec 3.7 §7.13, §7.15, scenario S17), feature `analyst`: a video cut
//! into chunks, each chunk captioned by a vision-language model, the captions indexed, and
//! questions answered from the chunks a retrieval finds, each answer citing the chunks it
//! rests on with their time ranges. It is NVIDIA's chunk, caption and index pattern from its
//! Video Search and Summarization blueprint, built from workspace parts.
//!
//! | Step | What happens |
//! |------|--------------|
//! | Chunk | Frames from any [`FrameSource`](crate::stream::FrameSource) are cut into [`Chunking::fixed`] windows or at [`Chunking::scene_change`] cuts, one chunk at a time, so an hour never sits in memory |
//! | Caption | Each chunk's frames, at most [`frames_per_chunk`](VideoAnalystBuilder::frames_per_chunk) of them evenly spaced, go to the runtime's video analyzer as one `VideoRequest::Caption` call — Qwen3-VL on this machine, or a remote model whose frames each pass the egress gate — metered as `UsageUnit::Frames` |
//! | Index | The caption and the things it names are embedded by any `adk_rag::EmbeddingProvider` and stored as one [`Chunk`] in any [`VectorStore`], with the chunk's time range, keyframe and clip in its metadata |
//! | Remember | With [`captions`](VideoAnalystBuilder::captions), each caption is an `adk-memory` entry naming its time range, with references to its keyframe and clip artifacts; with [`graph`](VideoAnalystBuilder::graph), the clip is a graph entity whose observations are the captions, related to what they name |
//! | Keyframes | With [`keyframes`](VideoAnalystBuilder::keyframes), each chunk's middle frame passes the privacy hook and is stored as an image artifact; with [`visual_memory`](VideoAnalystBuilder::visual_memory) it is also embedded in a [`VisualMemory`], so a chunk can be found by an example image |
//! | Ask | [`VideoAnalyst::ask`] has the [`answerer`](VideoAnalystBuilder::answerer) write the caption it expects the answer's chunk to have, retrieves the chunks nearest the question and that caption, and asks it to answer from their captions alone, citing chunks by number; without an answerer the chunk nearest the question is the answer |
//! | Cost | [`IndexedClip::cost_per_hour_micro_usd`] is the captioning's cost per hour of video |
//! | Erasure | [`VideoAnalyst::erase_source`] deletes a clip's chunks, captions, graph entity and keyframes and writes `vision.analyst.erased` |
//!
//! No backend is assumed: the describer is whatever the runtime routes video to, the store
//! any `adk-rag` backend, and the text embedding any provider — [`WordHashEmbedding`] needs
//! no model. The answerer reads caption text and time ranges, never frames.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use adk_core::{Content, LlmResponse};
//! use adk_model::MockLlm;
//! use adk_rag::{InMemoryVectorStore, Scope};
//! use adk_vision::analyst::{AnalystQuery, Chunking, ClipSource, VideoAnalyst, WordHashEmbedding};
//! use adk_vision::analyst::fixtures::{WarehouseClip, WarehouseSpec};
//! use adk_vision::{FramesAnalyzer, LlmDescriber, MediaScope, Sampling, VisionCx, VisionRuntime};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! // A captioner on this machine that always sees a forklift.
//! let caption = r#"{"caption": "An orange forklift stops at aisle 2.", "objects": ["forklift"]}"#;
//! let qwen = MockLlm::new("qwen3-vl-4b-instruct")
//!     .with_response(LlmResponse::new(Content::new("model").with_text(caption)));
//! let describer = LlmDescriber::local(Arc::new(qwen)).unwrap();
//! let runtime = VisionRuntime::builder()
//!     .video_analyzer(Arc::new(FramesAnalyzer::local(&describer)))
//!     .build()
//!     .unwrap();
//! let analyst = VideoAnalyst::builder(
//!     Arc::new(runtime),
//!     Arc::new(InMemoryVectorStore::new()),
//!     Arc::new(WordHashEmbedding::default()),
//!     "warehouse",
//! )
//! .chunking(Chunking::fixed(30_000))
//! .open()
//! .await
//! .unwrap();
//!
//! let clip = WarehouseClip::new(WarehouseSpec::default().with_duration(600_000));
//! let scope = Scope::new("site-7").unwrap();
//! let source = ClipSource::new("cam-3", MediaScope::new("site-7", "ops", "s1"));
//! let cx = VisionCx::new();
//! let indexed = analyst
//!     .index(&cx, &scope, &source, clip.frames(10_000), Sampling::at(0.1))
//!     .await
//!     .unwrap()
//!     .value;
//! assert_eq!(indexed.chunks.len(), 20);
//! assert_eq!(indexed.cost_per_hour_micro_usd(), Some(0));
//!
//! let answer = analyst
//!     .ask(&cx, &scope, "When does the forklift stop?", &AnalystQuery::top(3))
//!     .await
//!     .unwrap();
//! assert_eq!(answer.citations.len(), 1);
//! # });
//! ```

mod chunking;
mod embedding;
pub mod fixtures;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_artifact::{ArtifactService, DeleteRequest};
use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{AdkError, Content, ErrorCategory, GenerateContentConfig, Llm, LlmRequest};
use adk_memory::graph::{CreateEntityInput, CreateRelationInput};
use adk_memory::{GraphMemoryService, MemoryEntry, MemoryService};
use adk_rag::{
    Chunk, CollectionSpec, EmbeddingProvider, Field, Filter, RagError, Scope, SearchResult,
    VectorQuery, VectorStore,
};
use chrono::{DateTime, NaiveDate, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use chunking::Chunking;
pub use embedding::WordHashEmbedding;

use self::chunking::{ChunkCutter, PendingChunk};
use crate::audit::VISION_AUDIT_KIND;
use crate::backend::BackendDescriptor;
use crate::cloud::{LlmDescriber, extract_json};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::manifest::VisionTask;
use crate::media::{
    ArtifactLocator, Frame, FrameMeta, FramePixels, FrameSourceId, ImageRef, MediaScope,
    MediaStore, Provenance, SourceImage, VideoRef, artifact_uri,
};
use crate::memory::{VisualItem, VisualMemory};
use crate::runtime::{CallOptions, CallRecord, VideoSource, VisionOutput, VisionRuntime};
use crate::sent::VisionInput;
use crate::stream::{FramePrivacy, FrameStream};
use crate::traits::{Describer, Sampling, VideoRequest, VideoUsage};
use crate::video::answer::{ModelAnswer, add_usage, generate};
use crate::video::{evenly_spaced, timestamp};

/// Action of the record an indexing writes.
pub const VISION_ANALYST_INDEXED: &str = "vision.analyst.indexed";
/// Action of the record an answer writes.
pub const VISION_ANALYST_ASKED: &str = "vision.analyst.asked";
/// Action of the record an erasure writes.
pub const VISION_ANALYST_ERASED: &str = "vision.analyst.erased";

/// The `kind` of every chunk the analyst stores.
pub const CHUNK_KIND: &str = "videoChunk";

/// The metadata keys of a stored chunk.
pub mod keys {
    /// Always [`CHUNK_KIND`](super::CHUNK_KIND).
    pub const KIND: &str = "kind";
    /// The chunk's number in its clip, from 0.
    pub const INDEX: &str = "index";
    /// Where the chunk starts, in milliseconds from the start of the clip.
    pub const START_MS: &str = "startMs";
    /// Where the chunk ends, in milliseconds from the start of the clip.
    pub const END_MS: &str = "endMs";
    /// What the caption names, joined with `\n`.
    pub const LABELS: &str = "labels";
    /// The keyframe's `ImageRef`, as JSON.
    pub const KEYFRAME: &str = "keyframe";
    /// The clip's `VideoRef`, as JSON.
    pub const CLIP: &str = "clip";
    /// The application the captions and keyframes belong to.
    pub const APP: &str = "app";
    /// The user the captions and keyframes belong to.
    pub const USER: &str = "user";
    /// When the chunk starts in wall time, RFC 3339 in UTC.
    pub const CAPTURED_AT: &str = "capturedAt";
    /// The UTC date the chunk starts on, `YYYY-MM-DD`: what a day filter matches.
    pub const DAY: &str = "day";
    /// The run (invocation) that indexed the chunk.
    pub const RUN: &str = "run";
    /// The backend that captioned the chunk.
    pub const DESCRIBER: &str = "describer";
}

/// The metadata keys an analyst collection declares filterable.
pub const FILTERABLE: [&str; 3] = [keys::KIND, keys::DAY, keys::RUN];

fn kind() -> AuditRecordKind {
    AuditRecordKind::Other(VISION_AUDIT_KIND.to_string())
}

fn rag(err: RagError) -> VisionError {
    VisionError::Core(AdkError::from(err))
}

/// The clip an [`index`](VideoAnalyst::index) call reads: what it is called, whose it is,
/// and where it is kept.
///
/// # Example
///
/// ```
/// use adk_vision::MediaScope;
/// use adk_vision::analyst::ClipSource;
///
/// let started = chrono::DateTime::parse_from_rfc3339("2026-11-18T06:00:00Z").unwrap().to_utc();
/// let clip = ClipSource::new("dock-cam-2026-11-18", MediaScope::new("site-7", "ops", "s1"))
///     .starting_at(started)
///     .with_run("run-42");
/// assert_eq!(clip.source, "dock-cam-2026-11-18");
/// ```
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ClipSource {
    /// The clip's name: each chunk's `document_id`, and what erasure names.
    pub source: String,
    /// The application, user and session its keyframes and captions belong to.
    pub scope: MediaScope,
    /// The clip as stored, which captions refer to beside the keyframe.
    pub clip: Option<VideoRef>,
    /// The wall time of presentation time zero; by default each chunk takes its first
    /// frame's capture time.
    pub started_at: Option<DateTime<Utc>>,
    /// The run (invocation) that indexes it.
    pub run: Option<String>,
}

impl ClipSource {
    /// A clip named `source`, belonging to `scope`.
    pub fn new(source: impl Into<String>, scope: MediaScope) -> Self {
        Self { source: source.into(), scope, clip: None, started_at: None, run: None }
    }

    /// The clip as stored.
    pub fn with_clip(mut self, clip: VideoRef) -> Self {
        self.clip = Some(clip);
        self
    }

    /// The wall time of presentation time zero.
    pub fn starting_at(mut self, at: DateTime<Utc>) -> Self {
        self.started_at = Some(at);
        self
    }

    /// The run that indexes it.
    pub fn with_run(mut self, run: impl Into<String>) -> Self {
        self.run = Some(run.into());
        self
    }
}

/// One captioned chunk of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoChunk {
    /// The stored chunk's id: stable for one clip and time range, so indexing again
    /// replaces it.
    pub id: String,
    /// The clip.
    pub source: String,
    /// The chunk's number in the clip, from 0.
    pub index: u64,
    /// Where it starts, in milliseconds from the start of the clip.
    pub start_ms: u64,
    /// Where it ends, in milliseconds from the start of the clip.
    pub end_ms: u64,
    /// What it shows.
    pub caption: String,
    /// The people, vehicles and objects the caption names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Its middle frame as stored, after the privacy hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyframe: Option<ImageRef>,
    /// The clip as stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<VideoRef>,
    /// When it starts in wall time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<DateTime<Utc>>,
    /// The run that indexed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The backend that captioned it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub describer: Option<String>,
}

impl VideoChunk {
    /// The id of the chunk of `source` from `start_ms` to `end_ms`.
    pub fn id_for(source: &str, start_ms: u64, end_ms: u64) -> String {
        let key = format!("{source}\0{start_ms}\0{end_ms}");
        format!("vc-{}", &Digest::of(key.as_bytes()).hex()[..32])
    }

    /// Whether `ms` falls inside the chunk: from its start, up to and not including its end.
    pub fn contains(&self, ms: u64) -> bool {
        (self.start_ms..self.end_ms).contains(&ms)
    }

    /// Its time range as `m:ss.s–m:ss.s`.
    pub fn time_range(&self) -> String {
        format!("{}–{}", timestamp(self.start_ms), timestamp(self.end_ms))
    }

    /// The artifact its caption shows: the keyframe, else the clip.
    pub fn artifact_uri(&self) -> Option<String> {
        self.keyframe
            .as_ref()
            .map(|k| artifact_uri(&k.artifact))
            .or_else(|| self.clip.as_ref().map(|c| artifact_uri(&c.artifact)))
    }

    /// The text its vector embeds: the caption and what it names.
    fn embedded_text(&self) -> String {
        if self.labels.is_empty() {
            self.caption.clone()
        } else {
            format!("{} Seen: {}.", self.caption, self.labels.join(", "))
        }
    }

    /// Reads a chunk back from the store.
    ///
    /// # Errors
    ///
    /// [`VisionError::Parse`] for a chunk the analyst did not store.
    pub fn from_chunk(chunk: &Chunk) -> Result<Self> {
        let meta = |key: &str| chunk.metadata.get(key);
        let number = |key: &str| meta(key).and_then(|v| v.parse::<u64>().ok());
        let (Some(start_ms), Some(end_ms)) = (number(keys::START_MS), number(keys::END_MS)) else {
            return Err(VisionError::Parse {
                backend: "video-analyst".into(),
                message: format!("chunk `{}` carries no time range", chunk.id),
            });
        };
        Ok(Self {
            id: chunk.id.clone(),
            source: chunk.document_id.clone(),
            index: number(keys::INDEX).unwrap_or_default(),
            start_ms,
            end_ms,
            caption: chunk.text.clone(),
            labels: meta(keys::LABELS)
                .map(|l| l.split('\n').filter(|s| !s.is_empty()).map(str::to_string).collect())
                .unwrap_or_default(),
            keyframe: meta(keys::KEYFRAME).and_then(|k| serde_json::from_str(k).ok()),
            clip: meta(keys::CLIP).and_then(|c| serde_json::from_str(c).ok()),
            captured_at: meta(keys::CAPTURED_AT)
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.to_utc()),
            run: meta(keys::RUN).cloned(),
            describer: meta(keys::DESCRIBER).cloned(),
        })
    }

    fn to_chunk(&self, embedding: Vec<f32>, scope: &MediaScope) -> Result<Chunk> {
        let mut chunk = Chunk::new(self.id.clone(), self.source.clone(), self.caption.clone())
            .with_embedding(embedding)
            .with_metadata(keys::KIND, CHUNK_KIND)
            .with_metadata(keys::INDEX, self.index.to_string())
            .with_metadata(keys::START_MS, self.start_ms.to_string())
            .with_metadata(keys::END_MS, self.end_ms.to_string())
            .with_metadata(keys::APP, scope.app_name.clone())
            .with_metadata(keys::USER, scope.user_id.clone());
        if let Some(uri) = self.artifact_uri() {
            chunk = chunk.with_source_uri(uri);
        }
        if !self.labels.is_empty() {
            chunk = chunk.with_metadata(keys::LABELS, self.labels.join("\n"));
        }
        if let Some(keyframe) = &self.keyframe {
            chunk = chunk.with_metadata(keys::KEYFRAME, to_json(keyframe)?);
        }
        if let Some(clip) = &self.clip {
            chunk = chunk.with_metadata(keys::CLIP, to_json(clip)?);
        }
        if let Some(at) = self.captured_at {
            chunk = chunk
                .with_metadata(
                    keys::CAPTURED_AT,
                    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                )
                .with_metadata(keys::DAY, at.date_naive().format("%Y-%m-%d").to_string());
        }
        if let Some(run) = &self.run {
            chunk = chunk.with_metadata(keys::RUN, run.clone());
        }
        if let Some(describer) = &self.describer {
            chunk = chunk.with_metadata(keys::DESCRIBER, describer.clone());
        }
        Ok(chunk)
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| VisionError::Backend {
        backend: "video-analyst".into(),
        message: format!("chunk metadata could not be written: {e}"),
    })
}

/// What indexing a clip stored and spent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct IndexedClip {
    /// The clip.
    pub source: String,
    /// Its chunks, in time order.
    pub chunks: Vec<VideoChunk>,
    /// The video time the chunks cover, in milliseconds.
    pub duration_ms: u64,
    /// Frames the source gave.
    pub frames_read: u64,
    /// The captioning calls' frames, tokens and cost, summed.
    pub usage: VideoUsage,
}

impl IndexedClip {
    /// The captioning's cost per hour of video, in micro-USD: its cost times one hour over
    /// the video time it covers; zero on the device, `None` when a call's cost is unknown or
    /// the clip covers no time.
    pub fn cost_per_hour_micro_usd(&self) -> Option<u64> {
        let cost = self.usage.cost_micro_usd?;
        if self.duration_ms == 0 {
            return None;
        }
        let hourly = u128::from(cost) * 3_600_000 / u128::from(self.duration_ms);
        Some(u64::try_from(hourly).unwrap_or(u64::MAX))
    }
}

/// A chunk a retrieval found, with the store's score.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkHit {
    /// The chunk.
    pub chunk: VideoChunk,
    /// The store's similarity, higher is nearer.
    pub score: f32,
}

/// A retrieval over an analyst's collection.
///
/// # Example
///
/// ```
/// use adk_vision::analyst::AnalystQuery;
///
/// let query = AnalystQuery::top(8).source("dock-cam-2026-11-18");
/// assert_eq!(query.top_k, 8);
/// ```
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct AnalystQuery {
    /// The most chunks to retrieve.
    pub top_k: usize,
    /// Only chunks of this clip.
    pub source: Option<String>,
    /// Only chunks starting on these UTC dates; empty means any day.
    pub days: Vec<NaiveDate>,
    /// A further filter over [`FILTERABLE`] keys, the clip or the artifact URI.
    pub filter: Option<Filter>,
}

impl AnalystQuery {
    /// The `top_k` nearest chunks.
    pub fn top(top_k: usize) -> Self {
        Self { top_k, ..Self::default() }
    }

    /// Only chunks of `source`.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Only chunks starting on `days`.
    pub fn on(mut self, days: impl IntoIterator<Item = NaiveDate>) -> Self {
        self.days.extend(days);
        self
    }

    /// Adds a filter.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    fn compile(&self) -> Filter {
        let mut all = vec![Filter::eq(Field::metadata(keys::KIND), CHUNK_KIND)];
        if let Some(source) = &self.source {
            all.push(Filter::eq(Field::DocumentId, source.clone()));
        }
        if !self.days.is_empty() {
            all.push(Filter::is_in(
                Field::metadata(keys::DAY),
                self.days.iter().map(|d| d.format("%Y-%m-%d").to_string()),
            ));
        }
        if let Some(filter) = &self.filter {
            all.push(filter.clone());
        }
        Filter::and(all)
    }
}

/// An answer from a clip's captions, citing the chunks it rests on.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AnalystAnswer {
    /// The question.
    pub question: String,
    /// The answer in words.
    pub answer: String,
    /// The chunks the answer cites, the one that answers best first; empty when no caption
    /// answers the question.
    pub citations: Vec<VideoChunk>,
    /// The chunks the answerer was shown, nearest first.
    pub retrieved: Vec<ChunkHit>,
    /// The caption the answerer expected the answer's chunk to have, searched for with the
    /// question; `None` without an answerer or with
    /// [`expand_questions(false)`](VideoAnalystBuilder::expand_questions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<String>,
    /// The answerer's calls' tokens and cost; nothing without an answerer.
    pub usage: VideoUsage,
    /// The answerer's backend id; `None` when the nearest chunk is the answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
}

impl AnalystAnswer {
    /// The chunk the answer rests on most.
    pub fn primary(&self) -> Option<&VideoChunk> {
        self.citations.first()
    }
}

/// What an erasure removed.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ClipErasure {
    /// The erased clip.
    pub source: String,
    /// Chunks deleted from the collection.
    pub chunks: u64,
    /// Caption entries deleted from `adk-memory`.
    pub captions: u64,
    /// Graph entities deleted.
    pub entities: usize,
    /// Keyframe artifacts deleted.
    pub keyframes: Vec<ArtifactLocator>,
    /// Keyframe chunks deleted from the visual memory.
    pub visual_chunks: u64,
}

/// Where keyframes go.
struct Keyframes {
    artifacts: Arc<dyn ArtifactService>,
    media: MediaStore,
    privacy: FramePrivacy,
    visual: Option<Arc<VisualMemory>>,
}

/// The model that answers from captions.
struct Answerer {
    llm: Arc<dyn Llm>,
    descriptor: BackendDescriptor,
    max_output_tokens: u32,
}

/// Chunks, captions, indexes and answers over video. See the [module documentation](self).
pub struct VideoAnalyst {
    runtime: Arc<VisionRuntime>,
    options: CallOptions,
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    collection: String,
    chunking: Chunking,
    frames_per_chunk: usize,
    captions: Option<Arc<dyn MemoryService>>,
    graph: Option<Arc<GraphMemoryService>>,
    keyframes: Option<Keyframes>,
    answerer: Option<Answerer>,
    expand_questions: bool,
}

impl std::fmt::Debug for VideoAnalyst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoAnalyst")
            .field("collection", &self.collection)
            .field("chunking", &self.chunking)
            .field("frames_per_chunk", &self.frames_per_chunk)
            .field("captions", &self.captions.is_some())
            .field("graph", &self.graph.is_some())
            .field("keyframes", &self.keyframes.is_some())
            .field("answerer", &self.answerer.as_ref().map(|a| a.descriptor.id.to_string()))
            .finish_non_exhaustive()
    }
}

/// Builds a [`VideoAnalyst`].
#[must_use]
pub struct VideoAnalystBuilder {
    analyst: VideoAnalyst,
    visual: Option<Arc<VisualMemory>>,
}

impl VideoAnalystBuilder {
    /// Cuts videos with `chunking`; thirty-second windows by default.
    pub fn chunking(mut self, chunking: Chunking) -> Self {
        self.analyst.chunking = chunking;
        self
    }

    /// Sends at most `frames` of each chunk's frames to the captioner, evenly spaced; 6 by
    /// default, at least 1.
    pub fn frames_per_chunk(mut self, frames: usize) -> Self {
        self.analyst.frames_per_chunk = frames.max(1);
        self
    }

    /// Captions with these call options: a route naming the video analyzer, the data
    /// class remote frames are released as.
    pub fn call_options(mut self, options: CallOptions) -> Self {
        self.analyst.options = options;
        self
    }

    /// Stores each caption in `memory` with references to its keyframe and clip.
    pub fn captions(mut self, memory: Arc<dyn MemoryService>) -> Self {
        self.analyst.captions = Some(memory);
        self
    }

    /// Records each clip as an entity of `graph`, its captions as observations, related to
    /// what they name.
    pub fn graph(mut self, graph: Arc<GraphMemoryService>) -> Self {
        self.analyst.graph = Some(graph);
        self
    }

    /// Stores each chunk's middle frame in `artifacts` after `privacy` redacts it, and
    /// deletes it from there when the clip is erased.
    pub fn keyframes(mut self, artifacts: Arc<dyn ArtifactService>, privacy: FramePrivacy) -> Self {
        self.analyst.keyframes = Some(Keyframes {
            media: MediaStore::new(Arc::clone(&artifacts)),
            artifacts,
            privacy,
            visual: None,
        });
        self
    }

    /// Also embeds each keyframe in `memory`, with the chunk's caption, so a chunk can be
    /// found by an example image. Needs [`keyframes`](Self::keyframes). Build `memory`
    /// without captions or a graph: the analyst writes those.
    pub fn visual_memory(mut self, memory: Arc<VisualMemory>) -> Self {
        self.visual = Some(memory);
        self
    }

    /// Answers questions with `describer`'s model, from caption text alone: a model on
    /// this machine, or a remote one, whose token price meters the call.
    pub fn answerer<I: VisionInput>(mut self, describer: &LlmDescriber<I>) -> Self
    where
        LlmDescriber<I>: Describer<I>,
    {
        self.analyst.answerer = Some(Answerer {
            llm: Arc::clone(describer.model()),
            descriptor: Describer::descriptor(describer).clone(),
            max_output_tokens: 1_024,
        });
        self
    }

    /// Before retrieving, asks the answerer for the caption a chunk answering the question
    /// would have, and searches for the question and that caption together: a question in
    /// other words than the captions — "when do the lights go out?" over "the picture turns
    /// dark" — still finds its chunk. On by default; it applies only with an answerer.
    pub fn expand_questions(mut self, expand: bool) -> Self {
        self.analyst.expand_questions = expand;
        self
    }

    /// Creates the collection if needed.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a chunking that cannot cut, or a visual memory
    /// without keyframes; [`VisionError::BackendNotFound`] when the runtime has no video
    /// analyzer; the store's errors.
    pub async fn open(mut self) -> Result<VideoAnalyst> {
        self.analyst.chunking.validate()?;
        // The analyst captions through the runtime's video analyzer: refuse now, not at the
        // first chunk.
        self.analyst.runtime.route(VisionTask::AnalyzeVideo, &self.analyst.options)?;
        if let Some(visual) = self.visual.take() {
            let Some(keyframes) = &mut self.analyst.keyframes else {
                return Err(VisionError::InvalidInput(
                    "a visual memory embeds keyframes; configure `keyframes` too".into(),
                ));
            };
            keyframes.visual = Some(visual);
        }
        let analyst = self.analyst;
        let spec = CollectionSpec::new(analyst.embedder.dimensions()).with_filterable(FILTERABLE);
        analyst.store.create_collection(&analyst.collection, &spec).await.map_err(rag)?;
        Ok(analyst)
    }
}

impl VideoAnalyst {
    /// A builder captioning through `runtime`'s video analyzer, indexing in `collection`
    /// of `store` with vectors from `embedder`.
    pub fn builder(
        runtime: Arc<VisionRuntime>,
        store: Arc<dyn VectorStore>,
        embedder: Arc<dyn EmbeddingProvider>,
        collection: impl Into<String>,
    ) -> VideoAnalystBuilder {
        VideoAnalystBuilder {
            analyst: Self {
                runtime,
                options: CallOptions::default(),
                store,
                embedder,
                collection: collection.into(),
                chunking: Chunking::default(),
                frames_per_chunk: 6,
                captions: None,
                graph: None,
                keyframes: None,
                answerer: None,
                expand_questions: true,
            },
            visual: None,
        }
    }

    /// The collection.
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// How videos are cut.
    pub fn chunking(&self) -> Chunking {
        self.chunking
    }

    /// Cuts `frames` into chunks, captions each, and indexes it, one chunk at a time.
    /// `sampling` is the rate the frames were sampled at; the last chunk ends one frame
    /// interval after its last frame.
    ///
    /// Indexing a clip that already has chunks replaces them: its earlier chunks, captions,
    /// graph entity and keyframes are erased first, with their own `vision.analyst.erased`
    /// record. Writes `vision.analyst.indexed`.
    ///
    /// # Errors
    ///
    /// The source's errors; [`VisionError::InvalidInput`] for frames out of order or
    /// already stored; the runtime's errors for each caption call (budget, frame caps, the
    /// kill switch, cancellation, egress); the stores' and the embedder's errors. Chunks
    /// indexed before an error stay indexed.
    pub async fn index(
        &self,
        cx: &VisionCx,
        scope: &Scope,
        clip: &ClipSource,
        mut frames: FrameStream,
        sampling: Sampling,
    ) -> Result<VisionOutput<IndexedClip>> {
        cx.check()?;
        if self.count(scope, &clip.source).await? > 0 {
            self.erase_source(cx, scope, &clip.source).await?;
        }
        let step_ms = if sampling.fps > 0.0 { (1_000.0 / sampling.fps) as u64 } else { 0 };
        let mut cutter = ChunkCutter::new(self.chunking, step_ms);
        let mut indexed = IndexedClip {
            source: clip.source.clone(),
            chunks: Vec::new(),
            duration_ms: 0,
            frames_read: 0,
            usage: VideoUsage { cost_micro_usd: Some(0), ..VideoUsage::default() },
        };
        let mut calls = Vec::new();
        while let Some(frame) = frames.next().await {
            cx.check()?;
            indexed.frames_read += 1;
            if let Some(pending) = cutter.push(frame?)? {
                self.index_chunk(cx, scope, clip, pending, sampling, &mut indexed, &mut calls)
                    .await?;
            }
        }
        if let Some(pending) = cutter.finish() {
            self.index_chunk(cx, scope, clip, pending, sampling, &mut indexed, &mut calls).await?;
        }
        if let Some(graph) = &self.graph {
            self.record_graph(graph, clip, &indexed.chunks).await?;
        }
        indexed.duration_ms = match (indexed.chunks.first(), indexed.chunks.last()) {
            (Some(first), Some(last)) => last.end_ms.saturating_sub(first.start_ms),
            _ => 0,
        };
        let describers: BTreeSet<&str> =
            indexed.chunks.iter().filter_map(|c| c.describer.as_deref()).collect();
        let mut draft = AuditRecordDraft::new(kind(), VISION_ANALYST_INDEXED)
            .outcome(AuditRecordOutcome::Succeeded)
            .payload_ref(clip.source.clone())
            .ext("collection", json!(self.collection))
            .ext("scope", json!(scope.as_str()))
            .ext("source", json!(clip.source))
            .ext("chunks", json!(indexed.chunks.len()))
            .ext("durationMs", json!(indexed.duration_ms))
            .ext("framesRead", json!(indexed.frames_read))
            .ext("framesSent", json!(indexed.usage.frames))
            .ext("requests", json!(indexed.usage.requests))
            .ext("costMicroUsd", json!(indexed.usage.cost_micro_usd))
            .ext("costPerHourMicroUsd", json!(indexed.cost_per_hour_micro_usd()))
            .ext("describers", json!(describers))
            .ext("run", json!(clip.run));
        if let Some(video) = &clip.clip {
            draft = draft.ext("clip", json!(video.digest.to_string()));
        }
        cx.audit(draft).await?;
        tracing::info!(
            collection = %self.collection,
            video.source = %clip.source,
            video.chunks = indexed.chunks.len(),
            video.duration_ms = indexed.duration_ms,
            "video analyst indexed a clip"
        );
        Ok(VisionOutput { value: indexed, calls })
    }

    /// Captions one chunk, stores its keyframe, and indexes it.
    #[allow(clippy::too_many_arguments)]
    async fn index_chunk(
        &self,
        cx: &VisionCx,
        scope: &Scope,
        clip: &ClipSource,
        pending: PendingChunk,
        sampling: Sampling,
        indexed: &mut IndexedClip,
        calls: &mut Vec<CallRecord>,
    ) -> Result<()> {
        let PendingChunk { index, start_ms, end_ms, frames } = pending;
        let picked = evenly_spaced(&frames, self.frames_per_chunk);
        let images = picked
            .iter()
            .map(|(meta, pixels)| {
                Ok(((*meta).clone(), SourceImage::from_pixels((**pixels).clone())?))
            })
            .collect::<Result<Vec<(FrameMeta, SourceImage)>>>()?;
        let span = end_ms.saturating_sub(start_ms).max(1);
        let fps = (images.len() as f32 * 1_000.0 / span as f32).max(f32::MIN_POSITIVE);
        let rate = Sampling { fps, max_long_edge: sampling.max_long_edge };
        let out = self
            .runtime
            .analyze_video_with(
                cx,
                VideoSource::frames(&images, rate),
                &VideoRequest::Caption { chunk_ms: span },
                &self.options,
            )
            .await?;
        let describer = out.calls.first().map(|c| c.backend.to_string());
        calls.extend(out.calls);
        let analysis = out.value;
        merge_usage(&mut indexed.usage, &analysis.usage);
        let mut caption: Vec<&str> = analysis.events.iter().map(|e| e.text.trim()).collect();
        caption.retain(|c| !c.is_empty());
        let caption = match (caption.is_empty(), analysis.answer.as_deref()) {
            (false, _) => caption.join(" "),
            (true, Some(answer)) if !answer.trim().is_empty() => answer.trim().to_string(),
            (true, _) => {
                return Err(VisionError::Parse {
                    backend: describer.unwrap_or_default(),
                    message: format!(
                        "no caption for the chunk {}–{}",
                        timestamp(start_ms),
                        timestamp(end_ms)
                    ),
                });
            }
        };
        let mut labels: Vec<String> = Vec::new();
        for label in analysis.events.iter().flat_map(|e| e.labels.iter()) {
            if !labels.iter().any(|l| l.eq_ignore_ascii_case(label)) {
                labels.push(label.clone());
            }
        }
        let captured_at = match clip.started_at {
            Some(at) => Some(
                at + chrono::TimeDelta::milliseconds(i64::try_from(start_ms).unwrap_or(i64::MAX)),
            ),
            None => frames.first().map(|(m, _)| m.captured_at),
        };
        let mut chunk = VideoChunk {
            id: VideoChunk::id_for(&clip.source, start_ms, end_ms),
            source: clip.source.clone(),
            index,
            start_ms,
            end_ms,
            caption,
            labels,
            keyframe: None,
            clip: clip.clip.clone(),
            captured_at,
            run: clip.run.clone(),
            describer,
        };
        if let Some(keyframes) = &self.keyframes {
            let (meta, pixels) = picked[picked.len() / 2];
            let frame = Frame {
                source: meta.source.clone(),
                index: meta.index,
                pts: std::time::Duration::from_millis(meta.pts_ms),
                captured_at: meta.captured_at,
                pixels: FramePixels::Ephemeral(Arc::clone(pixels)),
            };
            let redacted = keyframes.privacy.apply(cx, frame).await?;
            let provenance = Provenance::Camera { source: FrameSourceId(clip.source.clone()) };
            let (image, stored) = keyframes
                .media
                .put_pixels(&clip.scope, redacted.pixels().clone(), provenance)
                .await?;
            if let Some(visual) = &keyframes.visual {
                let mut item = VisualItem::new(
                    image.clone(),
                    stored,
                    chunk.caption.clone(),
                    clip.source.clone(),
                    captured_at.unwrap_or(meta.captured_at),
                )
                .with_labels(chunk.labels.clone());
                if let Some(run) = &clip.run {
                    item = item.with_run(run.clone());
                }
                calls.extend(visual.index(cx, scope, &[item]).await?.calls);
            }
            chunk.keyframe = Some(image);
        }
        let vector = self.embedder.embed(&chunk.embedded_text()).await.map_err(rag)?;
        self.store
            .upsert(&self.collection, scope, &[chunk.to_chunk(vector, &clip.scope)?])
            .await
            .map_err(rag)?;
        if let Some(captions) = &self.captions {
            let (app, user) = (&clip.scope.app_name, &clip.scope.user_id);
            let mut entry = MemoryEntry::new(
                Content::new("model").with_text(format!(
                    "[{}] {}",
                    chunk.time_range(),
                    chunk.caption
                )),
                "vision",
            )
            .with_id(chunk.id.clone());
            if let Some(at) = chunk.captured_at {
                entry = entry.with_timestamp(at);
            }
            if let Some(keyframe) = &chunk.keyframe {
                entry = entry
                    .with_artifact(artifact_uri(&keyframe.artifact), keyframe.mime_type.clone());
            }
            if let Some(video) = &chunk.clip {
                entry = entry.with_artifact(artifact_uri(&video.artifact), video.mime_type.clone());
            }
            // One caption per memory session, as visual memory does, so a replacement
            // behaves the same on every backend.
            captions.delete_memory(app, user, &chunk.id).await?;
            captions.add_session(app, user, &chunk.id, vec![entry]).await?;
        }
        tracing::debug!(
            collection = %self.collection,
            video.source = %clip.source,
            video.chunk = chunk.index,
            video.start_ms = chunk.start_ms,
            video.end_ms = chunk.end_ms,
            "video chunk indexed"
        );
        indexed.chunks.push(chunk);
        Ok(())
    }

    async fn record_graph(
        &self,
        graph: &GraphMemoryService,
        clip: &ClipSource,
        chunks: &[VideoChunk],
    ) -> Result<()> {
        let (app, user) = (clip.scope.app_name.as_str(), clip.scope.user_id.as_str());
        let observations = chunks
            .iter()
            .map(|c| format!("[{}] {} ({})", c.time_range(), c.caption, c.id))
            .collect();
        let labels: BTreeSet<String> =
            chunks.iter().flat_map(|c| c.labels.iter().map(|l| l.to_lowercase())).collect();
        let mut entities = vec![CreateEntityInput {
            name: clip.source.clone(),
            entity_type: "vision.clip".to_string(),
            observations,
        }];
        entities.extend(labels.iter().map(|label| CreateEntityInput {
            name: label.clone(),
            entity_type: "vision.label".to_string(),
            observations: Vec::new(),
        }));
        graph.create_entities(app, user, entities).await?;
        if !labels.is_empty() {
            let relations = labels
                .into_iter()
                .map(|label| CreateRelationInput {
                    source: clip.source.clone(),
                    relation_type: "observed".to_string(),
                    target: label,
                })
                .collect();
            graph.create_relations(app, user, relations).await?;
        }
        Ok(())
    }

    /// The chunks nearest to `question`, nearest first.
    ///
    /// # Errors
    ///
    /// The embedder's and the store's errors.
    pub async fn retrieve(
        &self,
        scope: &Scope,
        question: &str,
        query: &AnalystQuery,
    ) -> Result<Vec<ChunkHit>> {
        let vector = self.embedder.embed(question).await.map_err(rag)?;
        let filter = query.compile();
        let found = self
            .store
            .search(
                &self.collection,
                scope,
                &VectorQuery::new(&vector, query.top_k).with_filter(&filter),
            )
            .await
            .map_err(rag)?;
        found
            .iter()
            .map(|r: &SearchResult| {
                Ok(ChunkHit { chunk: VideoChunk::from_chunk(&r.chunk)?, score: r.score })
            })
            .collect()
    }

    /// Answers `question` from the captions of the chunks a retrieval finds, citing the
    /// chunks the answer rests on. With [`expand_questions`](VideoAnalystBuilder::expand_questions)
    /// (the default), the answerer first writes the caption it expects the answer's chunk to
    /// have, and the retrieval searches for the question and that caption. The answerer
    /// sees each retrieved chunk's number, time range and caption, in time order, and cites
    /// by number; a number it was not shown is dropped.
    /// Without an answerer, the nearest chunk is the answer. Writes `vision.analyst.asked`.
    ///
    /// # Errors
    ///
    /// The retrieval's errors; the answer call's (budget, kill switch, cancellation, a
    /// refusal); [`VisionError::Parse`] for an answer without JSON.
    pub async fn ask(
        &self,
        cx: &VisionCx,
        scope: &Scope,
        question: &str,
        query: &AnalystQuery,
    ) -> Result<AnalystAnswer> {
        cx.check()?;
        let mut usage = VideoUsage::default();
        let expanded = match (&self.answerer, self.expand_questions) {
            (Some(answerer), true) => {
                Some(self.hypothetical(cx, answerer, question, &mut usage).await?)
            }
            _ => None,
        };
        let search = match &expanded {
            Some(caption) => format!("{question} {caption}"),
            None => question.to_string(),
        };
        let retrieved = self.retrieve(scope, &search, query).await?;
        let mut answer = AnalystAnswer {
            question: question.to_string(),
            answer: String::new(),
            citations: Vec::new(),
            retrieved,
            expanded,
            usage,
            answered_by: None,
        };
        match &self.answerer {
            None => match answer.retrieved.first() {
                Some(hit) => {
                    answer.answer = format!("{}: {}", hit.chunk.time_range(), hit.chunk.caption);
                    answer.citations.push(hit.chunk.clone());
                }
                None => {
                    answer.answer = "No chunk of the video is indexed for this question.".into()
                }
            },
            Some(answerer) => self.answer(cx, answerer, &mut answer).await?,
        }
        let cited: Vec<&str> = answer.citations.iter().map(|c| c.id.as_str()).collect();
        let sources: BTreeSet<&str> =
            answer.retrieved.iter().map(|h| h.chunk.source.as_str()).collect();
        cx.audit(
            AuditRecordDraft::new(kind(), VISION_ANALYST_ASKED)
                .outcome(AuditRecordOutcome::Succeeded)
                .payload_ref(Digest::of(question.as_bytes()).to_string())
                .ext("collection", json!(self.collection))
                .ext("scope", json!(scope.as_str()))
                .ext("sources", json!(sources))
                .ext("retrieved", json!(answer.retrieved.len()))
                .ext("cited", json!(cited))
                .ext("answeredBy", json!(answer.answered_by))
                .ext("costMicroUsd", json!(answer.usage.cost_micro_usd)),
        )
        .await?;
        Ok(answer)
    }

    async fn answer(
        &self,
        cx: &VisionCx,
        answerer: &Answerer,
        answer: &mut AnalystAnswer,
    ) -> Result<()> {
        let mut shown: Vec<&VideoChunk> = answer.retrieved.iter().map(|h| &h.chunk).collect();
        shown.sort_by(|a, b| (a.source.as_str(), a.start_ms).cmp(&(b.source.as_str(), b.start_ms)));
        let several = shown.iter().map(|c| c.source.as_str()).collect::<BTreeSet<_>>().len() > 1;
        let lines: Vec<String> = shown
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let clip = if several { format!(" of {}", c.source) } else { String::new() };
                format!("[{}] {}{clip}: {}", i + 1, c.time_range(), c.caption)
            })
            .collect();
        let prompt = format!(
            "You answer questions about a video from captions of its chunks. Each line is one \
             chunk: its number, its time range from the start of the video, and its caption.\n\n\
             {}\n\nQuestion: {}\n\nAnswer with JSON only: {{\"answer\": <the answer in words, \
             naming the time>, \"chunks\": [<the numbers of the chunks that show it, the one \
             where it is first seen first>]}}. Cite only chunks listed above. The captions \
             come from a model that saw a few frames of each chunk and can be imprecise about \
             details such as positions and colours: when a caption describes the asked event \
             approximately, cite it. If no caption answers the question, say so and give no \
             chunks.",
            if lines.is_empty() { "(no chunks)".to_string() } else { lines.join("\n") },
            answer.question
        );
        let schema = json!({
            "type": "object",
            "properties": {
                "answer": {"type": "string"},
                "chunks": {"type": "array", "items": {"type": "integer"}}
            },
            "required": ["answer", "chunks"]
        });
        let root = Self::text_call(cx, answerer, prompt, schema, &mut answer.usage).await?;
        answer.answer =
            root.get("answer").and_then(Value::as_str).unwrap_or_default().trim().to_string();
        for n in root.get("chunks").and_then(Value::as_array).into_iter().flatten() {
            let Some(n) =
                n.as_u64().or_else(|| n.as_str()?.trim().trim_matches(['[', ']']).parse().ok())
            else {
                continue;
            };
            let Some(chunk) =
                usize::try_from(n).ok().and_then(|n| n.checked_sub(1)).and_then(|i| shown.get(i))
            else {
                tracing::debug!(video.cited = n, "the answer cited a chunk it was not shown");
                continue;
            };
            if !answer.citations.iter().any(|c| c.id == chunk.id) {
                answer.citations.push((*chunk).clone());
            }
        }
        answer.answered_by = Some(answerer.descriptor.id.to_string());
        Ok(())
    }

    /// The caption the answerer expects a chunk answering `question` to have.
    async fn hypothetical(
        &self,
        cx: &VisionCx,
        answerer: &Answerer,
        question: &str,
        usage: &mut VideoUsage,
    ) -> Result<String> {
        let prompt = format!(
            "A question about a video: {question}\n\nWrite the caption that a chunk of the video \
             showing the answer would have: one sentence naming what is seen and what happens, \
             in plain words. Answer with JSON only: {{\"caption\": <the sentence>}}."
        );
        let schema = json!({
            "type": "object",
            "properties": {"caption": {"type": "string"}},
            "required": ["caption"]
        });
        let root = Self::text_call(cx, answerer, prompt, schema, usage).await?;
        Ok(root.get("caption").and_then(Value::as_str).unwrap_or_default().trim().to_string())
    }

    /// One text-only call to the answerer, metered and added to `usage`: its JSON.
    async fn text_call(
        cx: &VisionCx,
        answerer: &Answerer,
        prompt: String,
        schema: Value,
        usage: &mut VideoUsage,
    ) -> Result<Value> {
        let bound = answerer
            .descriptor
            .token_estimate
            .map(|e| e.estimate_micro_usd(&[], prompt.len(), Some(answerer.max_output_tokens)));
        let config = GenerateContentConfig {
            max_output_tokens: i32::try_from(answerer.max_output_tokens).ok(),
            response_schema: Some(schema),
            ..GenerateContentConfig::default()
        };
        let request = LlmRequest {
            config: Some(config),
            ..LlmRequest::new(answerer.llm.name(), vec![Content::new("user").with_text(prompt)])
        };
        let backend = answerer.descriptor.id.to_string();
        let ModelAnswer { text, usage: reported } =
            generate(cx, &answerer.llm, bound, request, &backend).await?;
        add_usage(usage, &answerer.descriptor, reported.as_ref());
        extract_json(&text).ok_or_else(|| VisionError::Parse {
            backend,
            message: format!("the answer holds no JSON: {text}"),
        })
    }

    /// Every chunk of `source` in `scope`, in time order.
    ///
    /// # Errors
    ///
    /// The store's errors, and [`VisionError::Parse`] for a chunk the analyst did not store.
    pub async fn chunks(&self, scope: &Scope, source: &str) -> Result<Vec<VideoChunk>> {
        let mut chunks = self
            .stored(scope, source)
            .await?
            .iter()
            .map(VideoChunk::from_chunk)
            .collect::<Result<Vec<_>>>()?;
        chunks.sort_by_key(|c| (c.start_ms, c.end_ms));
        Ok(chunks)
    }

    /// Every stored chunk of `source` in `scope`, as the store holds it.
    async fn stored(&self, scope: &Scope, source: &str) -> Result<Vec<Chunk>> {
        let filter = AnalystQuery::top(0).source(source).compile();
        let n = self.store.count(&self.collection, scope, Some(&filter)).await.map_err(rag)?;
        if n == 0 {
            return Ok(Vec::new());
        }
        // Any vector of the space lists a filter's chunks when top_k is their count.
        let mut axis = vec![0.0; self.embedder.dimensions()];
        if let Some(first) = axis.first_mut() {
            *first = 1.0;
        }
        let top_k = usize::try_from(n).unwrap_or(usize::MAX);
        let found = self
            .store
            .search(&self.collection, scope, &VectorQuery::new(&axis, top_k).with_filter(&filter))
            .await
            .map_err(rag)?;
        Ok(found.into_iter().map(|r| r.chunk).collect())
    }

    async fn count(&self, scope: &Scope, source: &str) -> Result<u64> {
        let filter = AnalystQuery::top(0).source(source).compile();
        self.store.count(&self.collection, scope, Some(&filter)).await.map_err(rag)
    }

    /// Deletes everything indexed from `source` in `scope`: its chunks, its captions, its
    /// graph entity (with its observations and relations), its keyframes and their visual
    /// memory chunks. Writes one `vision.analyst.erased` record.
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
    ) -> Result<ClipErasure> {
        if cx.cancellation().is_cancelled() {
            return Err(VisionError::Cancelled);
        }
        let filter = AnalystQuery::top(0).source(source).compile();
        let raw = self.stored(scope, source).await?;
        let stored = raw.iter().map(VideoChunk::from_chunk).collect::<Result<Vec<_>>>()?;
        // Each chunk's caption belongs to the application and user it was indexed for.
        let found: BTreeMap<String, (String, String)> = raw
            .iter()
            .filter_map(|c| {
                let app = c.metadata.get(keys::APP)?;
                let user = c.metadata.get(keys::USER)?;
                Some((c.id.clone(), (app.clone(), user.clone())))
            })
            .collect();
        let chunks =
            self.store.delete_where(&self.collection, scope, &filter).await.map_err(rag)?;
        let mut erasure =
            ClipErasure { source: source.to_string(), chunks, ..ClipErasure::default() };
        if let Some(captions) = &self.captions {
            for (id, (app, user)) in &found {
                erasure.captions += captions.delete_memory(app, user, id).await?;
            }
        }
        let users: BTreeSet<&(String, String)> = found.values().collect();
        if let Some(graph) = &self.graph {
            for (app, user) in &users {
                erasure.entities +=
                    graph.delete_entities(app, user, vec![source.to_string()]).await?.len();
            }
        }
        if let Some(keyframes) = &self.keyframes {
            if let Some(visual) = &keyframes.visual {
                erasure.visual_chunks = visual.erase_source(cx, scope, source).await?.chunks;
            }
            let mut locators: Vec<ArtifactLocator> = stored
                .iter()
                .filter_map(|c| c.keyframe.as_ref().map(|k| k.artifact.clone()))
                .collect();
            locators.sort_by_key(artifact_uri);
            locators.dedup();
            for locator in locators {
                let deleted = keyframes
                    .artifacts
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
                erasure.keyframes.push(locator);
            }
        }
        cx.audit(
            AuditRecordDraft::new(kind(), VISION_ANALYST_ERASED)
                .outcome(AuditRecordOutcome::Succeeded)
                .payload_ref(source.to_string())
                .ext("collection", json!(self.collection))
                .ext("scope", json!(scope.as_str()))
                .ext("source", json!(source))
                .ext("chunks", json!(erasure.chunks))
                .ext("captions", json!(erasure.captions))
                .ext("entities", json!(erasure.entities))
                .ext("keyframes", json!(erasure.keyframes))
                .ext("visualChunks", json!(erasure.visual_chunks)),
        )
        .await?;
        tracing::info!(
            collection = %self.collection,
            video.source = %source,
            video.chunks = erasure.chunks,
            video.captions = erasure.captions,
            "video analyst erased a clip"
        );
        Ok(erasure)
    }
}

/// Adds one call's usage to a clip's: an unknown cost makes the total unknown.
fn merge_usage(total: &mut VideoUsage, call: &VideoUsage) {
    total.frames += call.frames;
    total.requests += call.requests;
    for (sum, add) in [
        (&mut total.input_tokens, call.input_tokens),
        (&mut total.output_tokens, call.output_tokens),
    ] {
        if let Some(add) = add {
            *sum.get_or_insert(0) += add;
        }
    }
    total.cost_micro_usd = match (total.cost_micro_usd, call.cost_micro_usd) {
        (Some(sum), Some(cost)) => Some(sum.saturating_add(cost)),
        _ => None,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_round_trip_through_the_store_metadata() {
        let at = DateTime::from_timestamp(1_794_981_630, 0).unwrap();
        let chunk = VideoChunk {
            id: VideoChunk::id_for("cam", 30_000, 60_000),
            source: "cam".into(),
            index: 1,
            start_ms: 30_000,
            end_ms: 60_000,
            caption: "A forklift stops at aisle 2.".into(),
            labels: vec!["forklift".into(), "aisle 2".into()],
            keyframe: None,
            clip: None,
            captured_at: Some(at),
            run: Some("run-1".into()),
            describer: Some("qwen3-vl-4b-instruct".into()),
        };
        let stored = chunk.to_chunk(vec![1.0, 0.0], &MediaScope::new("a", "u", "s")).unwrap();
        assert_eq!(stored.metadata[keys::DAY], "2026-11-18");
        assert_eq!(VideoChunk::from_chunk(&stored).unwrap(), chunk);
        assert!(chunk.contains(30_000) && chunk.contains(59_999) && !chunk.contains(60_000));
        assert_eq!(chunk.time_range(), "0:30.0–1:00.0");
        assert_ne!(chunk.id, VideoChunk::id_for("cam", 30_000, 60_001));
    }

    #[test]
    fn costs_scale_to_an_hour_and_unknown_costs_stay_unknown() {
        let mut clip = IndexedClip {
            source: "cam".into(),
            chunks: Vec::new(),
            duration_ms: 900_000,
            frames_read: 0,
            usage: VideoUsage { cost_micro_usd: Some(0), ..VideoUsage::default() },
        };
        merge_usage(
            &mut clip.usage,
            &VideoUsage {
                frames: 6,
                requests: 1,
                cost_micro_usd: Some(5_000),
                ..VideoUsage::default()
            },
        );
        assert_eq!(clip.cost_per_hour_micro_usd(), Some(20_000));
        merge_usage(
            &mut clip.usage,
            &VideoUsage { frames: 6, requests: 1, cost_micro_usd: None, ..VideoUsage::default() },
        );
        assert_eq!(
            (clip.usage.frames, clip.usage.requests, clip.cost_per_hour_micro_usd()),
            (12, 2, None)
        );
    }
}
