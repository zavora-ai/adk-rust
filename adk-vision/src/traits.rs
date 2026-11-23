//! The perception traits (spec 3.7 §7.5): [`Detector`], [`Segmenter`], [`DocumentReader`],
//! [`Embedder`], [`Describer`], [`Tracker`], [`VideoAnalyzer`] and [`Editor`].
//!
//! Each image-taking trait is generic over what its backend is handed: on-device backends
//! implement it for [`SentImage`] (the default), remote backends for
//! [`ReleasedImage`](crate::ReleasedImage). Backends answer in their descriptor's
//! [`CoordFrame`](crate::CoordFrame) with [`RawRegion`]s; the runtime maps every answer to
//! source pixels.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::backend::BackendDescriptor;
use crate::cx::VisionCx;
use crate::edit::EditOp;
use crate::error::Result;
use crate::geometry::{BBox, EscalatedFrom, Mask, RawRegion, Region, TrackId};
use crate::media::{DecodedImage, FrameMeta};
use crate::sent::{SentImage, VisionInput};

/// What to detect.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "values")]
pub enum DetectQuery {
    /// Closed-set classes the model was trained on.
    Classes(Vec<String>),
    /// Open-vocabulary noun phrases; `sam-3.1` takes one per request.
    Concepts(Vec<String>),
}

/// Finds objects as boxes (and masks, for segmenting detectors).
///
/// # Example
///
/// ```
/// use adk_vision::{
///     BackendDescriptor, BBox, CoordFrame, DetectQuery, Detector, Locality, RawRegion,
///     SentImage, VisionCx,
/// };
///
/// struct Everything(BackendDescriptor);
///
/// #[async_trait::async_trait]
/// impl Detector for Everything {
///     fn descriptor(&self) -> &BackendDescriptor {
///         &self.0
///     }
///
///     async fn detect(
///         &self,
///         _cx: &VisionCx,
///         image: &SentImage,
///         _query: &DetectQuery,
///     ) -> adk_vision::Result<Vec<RawRegion>> {
///         let (w, h) = image.size();
///         Ok(vec![RawRegion::boxed(BBox::new(0.0, 0.0, w as f32, h as f32), "image", 1.0)])
///     }
/// }
///
/// let detector =
///     Everything(BackendDescriptor::new("everything", Locality::InProcess, CoordFrame::SentPixels));
/// assert_eq!(detector.descriptor().id.as_str(), "everything");
/// ```
#[async_trait]
pub trait Detector<I: VisionInput = SentImage>: Send + Sync {
    /// What the backend is.
    fn descriptor(&self) -> &BackendDescriptor;

    /// Detects objects in `image`, answering in the descriptor's frame.
    async fn detect(&self, cx: &VisionCx, image: &I, query: &DetectQuery)
    -> Result<Vec<RawRegion>>;
}

/// A segmentation prompt, given to the runtime in source pixels and handed to the backend
/// in its own frame.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SegmentPrompt {
    /// A noun phrase naming one concept.
    Concept {
        /// The phrase.
        text: String,
    },
    /// A point on (or off) the object.
    Point {
        /// `[x, y]`.
        at: [f32; 2],
        /// `true` for a point on the object.
        positive: bool,
    },
    /// A box around the object.
    Box {
        /// The box.
        bbox: BBox,
    },
    /// A rough mask of the object.
    Mask {
        /// The mask.
        mask: Mask,
    },
}

/// Segments objects into masks or polygons.
#[async_trait]
pub trait Segmenter<I: VisionInput = SentImage>: Send + Sync {
    /// What the backend is.
    fn descriptor(&self) -> &BackendDescriptor;

    /// Segments `image` for the prompts, answering in the descriptor's frame.
    async fn segment(
        &self,
        cx: &VisionCx,
        image: &I,
        prompts: &[SegmentPrompt],
    ) -> Result<Vec<RawRegion>>;
}

/// Options for reading a document page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ReadOptions {
    /// Expected languages, as BCP 47 tags; empty lets the backend decide.
    #[serde(default)]
    pub languages: Vec<String>,
    /// Ask for a Markdown rendering of the page.
    #[serde(default)]
    pub markdown: bool,
}

/// The kind of a document block.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    /// Body text.
    Text,
    /// A title or heading.
    Title,
    /// A table.
    Table,
    /// A formula.
    Formula,
    /// A figure.
    Figure,
    /// A seal or stamp.
    Seal,
    /// A checkbox.
    Checkbox,
    /// Handwriting.
    Handwriting,
}

/// A recognised word.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Word<R = Region> {
    /// Where it is.
    pub region: R,
    /// Its text.
    pub text: String,
    /// The backend's confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// A table cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableCell<R = Region> {
    /// Where it is.
    pub region: R,
    /// Row, from 0.
    pub row: u32,
    /// Column, from 0.
    pub col: u32,
    /// Rows spanned.
    pub row_span: u32,
    /// Columns spanned.
    pub col_span: u32,
    /// Its text.
    pub text: String,
}

/// A block of a document page with its region, text, words and, for tables, cells.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block<R = Region> {
    /// What kind of block.
    pub kind: BlockKind,
    /// Where it is.
    pub region: R,
    /// Its text.
    pub text: String,
    /// The backend's confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Its words.
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word<R>>,
    /// Its cells, for tables.
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<TableCell<R>>,
}

/// One page read: blocks with regions, the reading order, and optionally Markdown.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentPage<R = Region> {
    /// The blocks.
    pub blocks: Vec<Block<R>>,
    /// Indexes into `blocks`, in reading order.
    pub reading_order: Vec<usize>,
    /// The page as Markdown, when asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
}

impl<R> Default for DocumentPage<R> {
    fn default() -> Self {
        Self { blocks: Vec::new(), reading_order: Vec::new(), markdown: None }
    }
}

/// Reads document pages: layout, text, tables.
#[async_trait]
pub trait DocumentReader<I: VisionInput = SentImage>: Send + Sync {
    /// What the backend is.
    fn descriptor(&self) -> &BackendDescriptor;

    /// Reads `page`, answering in the descriptor's frame.
    async fn read(
        &self,
        cx: &VisionCx,
        page: &I,
        options: &ReadOptions,
    ) -> Result<DocumentPage<RawRegion>>;
}

/// The distance an embedding space is searched by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Metric {
    /// Cosine similarity.
    Cosine,
    /// Dot product.
    Dot,
    /// Euclidean distance.
    L2,
}

/// The space an embedder's vectors live in. An index belongs to one space.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingSpace {
    /// The model.
    pub model: String,
    /// The model revision.
    pub revision: String,
    /// Vector length.
    pub dimensions: usize,
    /// The metric.
    pub metric: Metric,
}

/// Embeds images and texts into one space.
#[async_trait]
pub trait Embedder<I: VisionInput = SentImage>: Send + Sync {
    /// What the backend is.
    fn descriptor(&self) -> &BackendDescriptor;

    /// Model, revision, dimensions and metric.
    fn space(&self) -> &EmbeddingSpace;

    /// One vector per image.
    async fn embed_images(&self, cx: &VisionCx, images: &[I]) -> Result<Vec<Vec<f32>>>;

    /// One vector per text.
    async fn embed_texts(&self, cx: &VisionCx, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// What to ask a describer.
#[derive(Debug)]
#[non_exhaustive]
pub struct DescribeRequest<I = SentImage> {
    /// The images, as the backend may see them.
    pub images: Vec<I>,
    /// The question or instruction.
    pub prompt: String,
    /// A JSON Schema the answer's `value` must match.
    pub output_schema: Option<serde_json::Value>,
    /// Ask for regions; a backend with no region adapter refuses with `Unsupported`.
    pub want_regions: bool,
    /// The most output tokens to spend.
    pub max_output_tokens: Option<u32>,
}

impl<I> DescribeRequest<I> {
    pub(crate) fn new(images: Vec<I>, spec: &DescribeSpec) -> Self {
        Self {
            images,
            prompt: spec.prompt.clone(),
            output_schema: spec.output_schema.clone(),
            want_regions: spec.want_regions,
            max_output_tokens: spec.max_output_tokens,
        }
    }
}

/// What a caller asks the runtime to describe, before images are prepared.
///
/// # Example
///
/// ```
/// use adk_vision::DescribeSpec;
///
/// let spec = DescribeSpec::new("What kind of document is this?").with_regions();
/// assert!(spec.want_regions);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DescribeSpec {
    /// The question or instruction.
    pub prompt: String,
    /// A JSON Schema the answer's `value` must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// Ask for regions.
    #[serde(default)]
    pub want_regions: bool,
    /// The most output tokens to spend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl DescribeSpec {
    /// A prompt.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self { prompt: prompt.into(), ..Self::default() }
    }

    /// Asks for a value matching `schema`.
    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Asks for regions.
    pub fn with_regions(mut self) -> Self {
        self.want_regions = true;
        self
    }

    /// Caps output tokens.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }
}

/// A region a describer found, with the index of the request image it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRegion {
    /// Index into the request's images.
    pub image: usize,
    /// The region, in the backend's frame.
    pub region: RawRegion,
}

/// A describer's answer: free text, a value matching the request's schema, and regions
/// when asked for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Description<R = Region> {
    /// Free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// A value matching the request's `output_schema`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Regions, when asked for.
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<R>,
    /// The backend's confidence in the answer, when it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// The primary answer this one replaced, when it came from an escalation. Set by the
    /// runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated_from: Option<EscalatedFrom>,
}

impl<R> Default for Description<R> {
    fn default() -> Self {
        Self {
            text: None,
            value: None,
            regions: Vec::new(),
            confidence: None,
            escalated_from: None,
        }
    }
}

/// Describes images in text, structured values and regions.
#[async_trait]
pub trait Describer<I: VisionInput = SentImage>: Send + Sync {
    /// What the backend is.
    fn descriptor(&self) -> &BackendDescriptor;

    /// Free text, a value matching `request.output_schema`, and regions when asked for.
    async fn describe(
        &self,
        cx: &VisionCx,
        request: &DescribeRequest<I>,
    ) -> Result<Description<ImageRegion>>;
}

/// Associates detections across frames. Pure CPU, no model.
pub trait Tracker: Send {
    /// Assigns track identifiers to one frame's detections.
    fn update(&mut self, frame: &FrameMeta, detections: Vec<Region>) -> Vec<Region>;

    /// Forgets every track; identifiers start again. Called when the source session ends.
    fn reset(&mut self);

    /// The tracks held after the last [`update`](Self::update), seen in that frame or kept
    /// through a brief miss. A watch condition keeps its state for these, so an object the
    /// detector misses for a frame does not fire again. None by default.
    fn live_tracks(&self) -> Vec<TrackId> {
        Vec::new()
    }
}

/// How a video or stream is sampled, which every provider prices by.
///
/// # Example
///
/// ```
/// use adk_vision::Sampling;
///
/// // Gemini's default: one frame a second at the clip's own resolution.
/// let sampling = Sampling::at(1.0);
/// assert_eq!(sampling.max_long_edge, None);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sampling {
    /// Frames per second.
    pub fps: f32,
    /// Longest side of each frame, in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_long_edge: Option<u32>,
}

impl Sampling {
    /// `fps` frames a second, at full resolution.
    pub fn at(fps: f32) -> Self {
        Self { fps, max_long_edge: None }
    }
}

/// What to do with a video.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum VideoRequest {
    /// Track objects matching the prompts.
    Track {
        /// The prompts.
        prompts: Vec<SegmentPrompt>,
    },
    /// Caption the video in chunks of `chunk_ms`, counted from its first frame: one event per
    /// chunk that holds a frame, whose text is the caption and whose labels name what it
    /// shows. The video analyst (`adk_vision::analyst`, feature `analyst`) sends one chunk
    /// per call.
    Caption {
        /// Chunk length in milliseconds.
        chunk_ms: u64,
    },
    /// Answer a question.
    Ask {
        /// The question.
        question: String,
    },
}

/// What a video backend is handed: sampled frames, a clip, or both, as the backend may see
/// them — [`SentImage`]s and a [`SentClip`](crate::SentClip) on the device,
/// [`ReleasedImage`](crate::ReleasedImage)s and a [`ReleasedClip`](crate::ReleasedClip)
/// off it. Only the runtime constructs one.
#[non_exhaustive]
pub struct VideoInput<I: VisionInput = SentImage> {
    /// `(frame, image)` pairs, in order.
    pub frames: Vec<(FrameMeta, I)>,
    /// The clip, for a backend that reads video itself.
    pub clip: Option<I::Clip>,
    /// How the frames were sampled, or how a backend that samples the clip itself should.
    pub sampling: Sampling,
}

impl<I: VisionInput> VideoInput<I> {
    pub(crate) fn new(
        frames: Vec<(FrameMeta, I)>,
        clip: Option<I::Clip>,
        sampling: Sampling,
    ) -> Self {
        Self { frames, clip, sampling }
    }
}

impl<I: VisionInput + std::fmt::Debug> std::fmt::Debug for VideoInput<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoInput")
            .field("frames", &self.frames)
            .field("clip", &self.clip)
            .field("sampling", &self.sampling)
            .finish()
    }
}

/// What a video call will send, known before anything is prepared or released: for a
/// backend's frame count and cost estimate.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct VideoPlan {
    /// The clip, when the call sends one.
    pub clip: Option<crate::video::ClipInfo>,
    /// The sizes of the sampled frames the call sends, in order.
    pub frame_sizes: Vec<(u32, u32)>,
    /// The time the call covers, in milliseconds.
    pub duration_ms: u64,
    /// The sampling.
    pub sampling: Sampling,
}

impl VideoPlan {
    /// Frames the call carries: the clip's frames, else the sampled frames.
    pub fn frames(&self) -> u64 {
        match &self.clip {
            Some(clip) => clip.frames,
            None => self.frame_sizes.len() as u64,
        }
    }
}

/// A time range a video analysis names: an event an `Ask` found, or a chunk a `Caption`
/// described.
///
/// # Example
///
/// ```
/// use adk_vision::VideoEvent;
///
/// let chunk = VideoEvent::new(30_000, 60_000, "An orange forklift drives into aisle 2.")
///     .with_labels(["forklift", "pallet"]);
/// assert_eq!(chunk.labels, ["forklift", "pallet"]);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoEvent {
    /// Start, in milliseconds.
    pub start_ms: u64,
    /// End, in milliseconds.
    pub end_ms: u64,
    /// What happens.
    pub text: String,
    /// The people, vehicles and objects a caption names, as short noun phrases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

impl VideoEvent {
    /// An event from `start_ms` to `end_ms` described by `text`, with no labels.
    pub fn new(start_ms: u64, end_ms: u64, text: impl Into<String>) -> Self {
        Self { start_ms, end_ms, text: text.into(), labels: Vec::new() }
    }

    /// Adds the things the event names.
    pub fn with_labels<I, S>(mut self, labels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.labels.extend(labels.into_iter().map(Into::into));
        self
    }
}

/// The regions found on one frame of a video, each with the track it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoFrame<R = Region> {
    /// The frame's index in the clip, from 0, or the sampled frame's index in its stream.
    pub index: u64,
    /// Presentation time in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pts_ms: Option<u64>,
    /// The regions, with their tracks.
    pub regions: Vec<R>,
}

/// What one analysis used: frames, tokens and cost, so each path reports its own (S13).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VideoUsage {
    /// Frames analysed: billed frames for a per-frame price, sampled frames otherwise.
    pub frames: u64,
    /// Input tokens, for a token-billed or local model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens, for a token-billed or local model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Cost in micro-USD: zero on the device, `None` when the catalog does not price the
    /// model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
    /// Calls made to the model or service.
    pub requests: u64,
}

/// What a video analysis found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VideoAnalysis<R = Region> {
    /// The answer to an `Ask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Events or chunk captions.
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<VideoEvent>,
    /// For a `Track`, the regions on each frame with an object, in frame order.
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<VideoFrame<R>>,
    /// Frames, tokens and cost.
    #[serde(default)]
    pub usage: VideoUsage,
}

impl<R> Default for VideoAnalysis<R> {
    fn default() -> Self {
        Self { answer: None, events: Vec::new(), frames: Vec::new(), usage: VideoUsage::default() }
    }
}

/// Tracks, captions or answers questions over a video, at an explicit sampling rate and
/// resolution.
///
/// A backend answers regions in its descriptor's frame as [`RawRegion`]s; the runtime maps
/// them to source pixels of the clip or of each sampled frame.
#[async_trait]
pub trait VideoAnalyzer<I: VisionInput = SentImage>: Send + Sync {
    /// What the backend is.
    fn descriptor(&self) -> &BackendDescriptor;

    /// The frames a call on `plan` analyses, which the runtime records as units and bills
    /// a per-frame price by: by default every frame the call carries.
    fn frames(&self, plan: &VideoPlan) -> u64 {
        plan.frames()
    }

    /// The most a call on `plan` can cost in micro-USD, which the runtime holds against the
    /// run's budget before anything is released (V-I6). Token-billed backends estimate it;
    /// `None` holds no more than a per-unit price.
    fn max_cost_micro_usd(&self, _plan: &VideoPlan, _request: &VideoRequest) -> Option<u64> {
        None
    }

    /// Analyses `video`.
    async fn analyze(
        &self,
        cx: &VisionCx,
        video: &VideoInput<I>,
        request: &VideoRequest,
    ) -> Result<VideoAnalysis<RawRegion>>;
}

/// Edits an image: crop, redact, annotate, resize, or a generative edit. The runtime stores
/// the result as a new image with `Provenance::Derived`.
#[async_trait]
pub trait Editor<I: VisionInput = SentImage>: Send + Sync {
    /// What the backend is.
    fn descriptor(&self) -> &BackendDescriptor;

    /// Applies `operation`, whose coordinates are in source pixels, to `image`.
    async fn edit(&self, cx: &VisionCx, image: &I, operation: &EditOp) -> Result<DecodedImage>;
}
