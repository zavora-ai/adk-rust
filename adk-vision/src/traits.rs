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
use crate::geometry::{BBox, EscalatedFrom, Mask, RawRegion, Region};
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
}

/// How a video or stream is sampled, which every provider prices by.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sampling {
    /// Frames per second.
    pub fps: f32,
    /// Longest side of each frame, in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_long_edge: Option<u32>,
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
    /// Caption fixed-length chunks.
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

/// Sampled frames, as the backend may see them. Video files are added in phase B together
/// with their release through the egress gate.
#[derive(Debug)]
#[non_exhaustive]
pub struct VideoInput<I = SentImage> {
    /// `(frame, image)` pairs, in order.
    pub frames: Vec<(FrameMeta, I)>,
    /// How the frames were sampled.
    pub sampling: Sampling,
}

/// A time range a video analysis names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoEvent {
    /// Start, in milliseconds.
    pub start_ms: u64,
    /// End, in milliseconds.
    pub end_ms: u64,
    /// What happens.
    pub text: String,
}

/// What a video analysis found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VideoAnalysis {
    /// The answer to an `Ask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Events or chunk captions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<VideoEvent>,
}

/// Tracks, captions or answers questions over a video, at an explicit sampling rate and
/// resolution.
#[async_trait]
pub trait VideoAnalyzer<I: VisionInput = SentImage>: Send + Sync {
    /// What the backend is.
    fn descriptor(&self) -> &BackendDescriptor;

    /// Analyses `video`.
    async fn analyze(
        &self,
        cx: &VisionCx,
        video: &VideoInput<I>,
        request: &VideoRequest,
    ) -> Result<VideoAnalysis>;
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
