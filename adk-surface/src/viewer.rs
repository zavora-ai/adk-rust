//! The annotated viewer (spec 3.7 §8.2, X12): looking at what an agent saw.
//!
//! Every perception call of the vision tools leaves an answer record beside its image in the
//! session — `vision-answer-<call id>.json`, the image itself as `vision-<hex>.<ext>` — with
//! the call's `RegionSet` in source pixels, each region's backend, coordinate frame and
//! transform chain, a read's blocks in reading order, and each backend call's duration and
//! cost. The engine reads those records through the [`Source`](crate::Source) and projects
//! them into a [`ViewerView`]; it never depends on `adk-vision`, whose JSON is the contract
//! (`adk-vision/schema/region-set.json`).
//!
//! | The view | From |
//! |---|---|
//! | The picture, its size and digest | The record's `image`; the bytes from the source, checked against the digest |
//! | Regions, filterable by label, score and track | The `RegionSet`; a read's block kinds, texts and reading order |
//! | Backend, model and frame per region, and the transform chain | Each region's `origin`; the record's `models` |
//! | Measurements, tracks | The record's `measurements` and `tracks`, when a feature adds them |
//! | The run and the call | The record's `agent`, `invocationId`, `callId` and `calls` |
//! | Two backends on one image | Two records for the same digest, matched by box overlap |
//!
//! A person may save a region, corrected or kept, as a labelled example: an `adk-eval` case
//! that points at the image by path and digest with `expected_vision`, so `cargo adk eval`
//! scores an agent against it ([`example_case`]). Saving never changes the run's answer.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::evidence::short_digest;

/// The artifact name prefix of an answer record `adk-vision` keeps for each perception call.
pub const VISION_ANSWER_PREFIX: &str = "vision-answer-";

/// The vision tools whose answers the viewer shows.
pub const VIEWER_TOOLS: [&str; 4] =
    ["vision_detect", "vision_segment", "vision_read", "vision_describe"];

/// Whether `tool`'s answer is about an image the viewer can show.
pub(crate) fn answers_about_image(tool: &str) -> bool {
    VIEWER_TOOLS.contains(&tool)
}

/// The score a region needs to be shown before a person moves the slider (board choice V3).
pub const DEFAULT_MIN_SCORE: f64 = 0.5;

// ---------------------------------------------------------------------------
// The record, as adk-vision writes it
// ---------------------------------------------------------------------------

/// One answer record, parsed loosely: a field this version does not know is skipped, and a
/// shape or transform kind it does not know is kept as `Other` and not drawn.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    #[serde(default)]
    pub(crate) tool: String,
    #[serde(default)]
    pub(crate) call_id: String,
    #[serde(default)]
    pub(crate) agent: String,
    #[serde(default)]
    pub(crate) at: Option<DateTime<Utc>>,
    pub(crate) image: RecordImage,
    #[serde(default)]
    pub(crate) args: Value,
    pub(crate) region_set: WireRegionSet,
    #[serde(default)]
    pub(crate) page: Option<WirePage>,
    #[serde(default)]
    pub(crate) calls: Vec<WireCall>,
    #[serde(default)]
    pub(crate) models: BTreeMap<String, WireModel>,
    #[serde(default)]
    pub(crate) measurements: Vec<WireMeasurement>,
    #[serde(default)]
    pub(crate) tracks: Vec<WireTrack>,
    #[serde(default)]
    pub(crate) video: Option<WireVideo>,
    #[serde(default)]
    pub(crate) display: WireDisplay,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordImage {
    pub(crate) digest: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    #[serde(default = "png")]
    pub(crate) mime_type: String,
    #[serde(default)]
    pub(crate) artifact: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) provenance: Value,
}

fn png() -> String {
    "image/png".into()
}

/// What a feature says about an answer's picture that the record cannot: an Inspector's
/// still, a video's title.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireDisplay {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    meta: Option<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    kept_until: Option<String>,
    #[serde(default)]
    min_score: Option<f64>,
    #[serde(default)]
    asked: Option<String>,
    #[serde(default)]
    covered: Option<String>,
}

/// The wire form of `adk_vision::RegionSet`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireRegionSet {
    pub(crate) image: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    #[serde(default)]
    pub(crate) regions: Vec<WireRegion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireRegion {
    #[serde(default)]
    pub(crate) image: String,
    pub(crate) shape: WireShape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) track: Option<u64>,
    pub(crate) origin: WireOrigin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum WireShape {
    Point {
        x: f64,
        y: f64,
    },
    Box {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
    },
    Polygon {
        points: Vec<[f64; 2]>,
    },
    Mask {
        width: u32,
        height: u32,
        counts: Vec<u32>,
    },
    Keypoints {
        #[serde(default)]
        skeleton: String,
        points: Vec<WireKeypoint>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireKeypoint {
    x: f64,
    y: f64,
    #[serde(default)]
    visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireOrigin {
    #[serde(default)]
    pub(crate) backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) manifest: Option<String>,
    pub(crate) frame: WireFrame,
    pub(crate) chain: WireChain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) escalated_from: Option<WireEscalatedFrom>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum WireFrame {
    SentPixels,
    Normalized {
        scale: u32,
        order: String,
    },
    ModelInput {
        width: u32,
        height: u32,
        letterbox: String,
    },
    #[serde(other)]
    Other,
}

/// The wire form of `adk_vision::TransformChain`: the source size and the steps, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireChain {
    pub(crate) source_width: u32,
    pub(crate) source_height: u32,
    #[serde(default)]
    pub(crate) steps: Vec<WireTransform>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "op")]
pub(crate) enum WireTransform {
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Resize {
        width: u32,
        height: u32,
    },
    Pad {
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
    },
    Rotate {
        #[serde(rename = "quarterTurns")]
        quarter_turns: u8,
    },
    Redact,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireEscalatedFrom {
    #[serde(default)]
    pub(crate) backend: String,
    #[serde(default)]
    pub(crate) reason: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WirePage {
    #[serde(default)]
    pub(crate) blocks: Vec<WireBlock>,
    #[serde(default)]
    pub(crate) reading_order: Vec<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WireBlock {
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) confidence: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireCall {
    #[serde(default)]
    task: String,
    #[serde(default)]
    backend: String,
    #[serde(default)]
    locality: Value,
    #[serde(default)]
    egress: Vec<String>,
    #[serde(default)]
    cost_micro_usd: Option<u64>,
    #[serde(default)]
    duration_ms: u64,
    #[serde(default)]
    escalation: Option<Value>,
    /// A tracker's library, when a feature names it: "trackforge 0.5".
    #[serde(default)]
    library: Option<String>,
    /// The prompts a segmenter was given, when a feature names them: "from the box".
    #[serde(default)]
    prompted: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireModel {
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    licence: Option<String>,
    #[serde(default)]
    built_in: bool,
    /// What a person calls it: "RF-DETR S".
    #[serde(default)]
    name: Option<String>,
    /// "your fine-tune".
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireMeasurement {
    region: usize,
    /// The measuring call, when it was its own: `c-2208`.
    #[serde(default)]
    call: Option<String>,
    /// Its tool: `vision_measure`.
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    longest_mm: Option<f64>,
    #[serde(default)]
    longest_uncertainty_mm: Option<f64>,
    #[serde(default)]
    area_mm2: Option<f64>,
    #[serde(default)]
    area_uncertainty_mm2: Option<f64>,
    #[serde(default)]
    line: Option<[[f64; 2]; 2]>,
    calibration: WireCalibration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireCalibration {
    reference_mm: f64,
    reference_px: f64,
    /// "the belt's plane".
    plane: String,
    /// "the fiducial".
    from: String,
    /// The reference region, when it is one of the answer's.
    #[serde(default)]
    region: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireTrack {
    id: u64,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    frames: u64,
    /// When it is seen, in seconds of the video.
    #[serde(default)]
    spans: Vec<[f64; 2]>,
    /// Where it was, oldest first, in this frame's pixels.
    #[serde(default)]
    trail: Vec<[f64; 2]>,
    #[serde(default)]
    blurred: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireVideo {
    frame: u64,
    at_s: f64,
    length_s: f64,
    sampled_per_s: u32,
}

impl Record {
    /// Parses an answer record.
    ///
    /// # Errors
    ///
    /// A sentence naming what is wrong when `value` is not an answer record.
    pub(crate) fn parse(value: Value) -> std::result::Result<Self, String> {
        serde_json::from_value(value)
            .map_err(|error| format!("not a vision answer record: {error}"))
    }
}

// ---------------------------------------------------------------------------
// Geometry: the transform chain
// ---------------------------------------------------------------------------

/// The size of the image after each step of `chain`, starting from the source.
pub(crate) fn chain_sizes(chain: &WireChain) -> Vec<(u32, u32)> {
    let mut size = (chain.source_width, chain.source_height);
    let mut sizes = vec![size];
    for step in &chain.steps {
        size = match *step {
            WireTransform::Crop { width, height, .. } | WireTransform::Resize { width, height } => {
                (width, height)
            }
            WireTransform::Pad { left, top, right, bottom } => {
                (size.0 + left + right, size.1 + top + bottom)
            }
            WireTransform::Rotate { quarter_turns } if quarter_turns % 2 == 1 => (size.1, size.0),
            WireTransform::Rotate { .. } | WireTransform::Redact | WireTransform::Other => size,
        };
        sizes.push(size);
    }
    sizes
}

/// A point in the source image's pixels, carried through `chain` into the pixels the backend
/// saw.
#[cfg(test)]
pub(crate) fn to_backend(chain: &WireChain, point: [f64; 2]) -> [f64; 2] {
    let sizes = chain_sizes(chain);
    let mut p = point;
    for (index, step) in chain.steps.iter().enumerate() {
        let (w, h) = (f64::from(sizes[index].0), f64::from(sizes[index].1));
        p = match *step {
            WireTransform::Crop { x, y, .. } => [p[0] - f64::from(x), p[1] - f64::from(y)],
            WireTransform::Resize { width, height } => {
                [p[0] * f64::from(width) / w, p[1] * f64::from(height) / h]
            }
            WireTransform::Pad { left, top, .. } => [p[0] + f64::from(left), p[1] + f64::from(top)],
            WireTransform::Rotate { quarter_turns } => rotate(p, quarter_turns, w, h),
            WireTransform::Redact | WireTransform::Other => p,
        };
    }
    p
}

/// A point in the pixels the backend saw, carried back through `chain` into the source
/// image's pixels: the inverse of `to_backend`.
#[cfg(test)]
pub(crate) fn to_source(chain: &WireChain, point: [f64; 2]) -> [f64; 2] {
    let sizes = chain_sizes(chain);
    let mut p = point;
    for (index, step) in chain.steps.iter().enumerate().rev() {
        let (w, h) = (f64::from(sizes[index].0), f64::from(sizes[index].1));
        p = match *step {
            WireTransform::Crop { x, y, .. } => [p[0] + f64::from(x), p[1] + f64::from(y)],
            WireTransform::Resize { width, height } => {
                [p[0] * w / f64::from(width), p[1] * h / f64::from(height)]
            }
            WireTransform::Pad { left, top, .. } => [p[0] - f64::from(left), p[1] - f64::from(top)],
            WireTransform::Rotate { quarter_turns } => {
                // Undoing a turn is turning the rest of the way, in the turned image.
                let (tw, th) = if quarter_turns % 2 == 1 { (h, w) } else { (w, h) };
                rotate(p, (4 - quarter_turns % 4) % 4, tw, th)
            }
            WireTransform::Redact | WireTransform::Other => p,
        };
    }
    p
}

/// `p` in a `w` × `h` image turned clockwise by `quarter_turns`.
#[cfg(test)]
fn rotate(p: [f64; 2], quarter_turns: u8, w: f64, h: f64) -> [f64; 2] {
    match quarter_turns % 4 {
        1 => [h - p[1], p[0]],
        2 => [w - p[0], h - p[1]],
        3 => [p[1], w - p[0]],
        _ => p,
    }
}

/// The bounds of a mask, run-length encoded in COCO's column-major order with the unset run
/// first, and how many pixels it sets; `None` when it sets none.
pub(crate) fn mask_bounds(width: u32, height: u32, counts: &[u32]) -> Option<([f64; 4], u64)> {
    if width == 0 || height == 0 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (u64::MAX, u64::MAX, 0u64, 0u64);
    let (mut at, mut area) = (0u64, 0u64);
    let h = u64::from(height);
    for (run, &count) in counts.iter().enumerate() {
        let count = u64::from(count);
        if run % 2 == 1 && count > 0 {
            area += count;
            let (first, last) = (at, at + count - 1);
            let (c0, c1) = (first / h, last / h);
            x0 = x0.min(c0);
            x1 = x1.max(c1 + 1);
            if c0 == c1 {
                y0 = y0.min(first % h);
                y1 = y1.max(last % h + 1);
            } else {
                // The run wraps a column: it reaches both the top and the bottom of the image.
                y0 = 0;
                y1 = h;
            }
        }
        at += count;
    }
    (area > 0).then_some(([x0 as f64, y0 as f64, x1 as f64, y1 as f64], area))
}

/// The bounds of a shape in source pixels, clamped to the image.
pub(crate) fn shape_bounds(shape: &WireShape, width: u32, height: u32) -> Option<[f64; 4]> {
    let (w, h) = (f64::from(width), f64::from(height));
    let bounds = match shape {
        WireShape::Box { x0, y0, x1, y1 } => [*x0, *y0, *x1, *y1],
        WireShape::Point { x, y } => [*x - 1.0, *y - 1.0, *x + 1.0, *y + 1.0],
        WireShape::Polygon { points } => bounds_of(points.iter().copied())?,
        WireShape::Keypoints { points, .. } => bounds_of(points.iter().map(|p| [p.x, p.y]))?,
        WireShape::Mask { width, height, counts } => mask_bounds(*width, *height, counts)?.0,
        WireShape::Other => return None,
    };
    if !bounds.iter().all(|v| v.is_finite()) {
        return None;
    }
    let clamped = [
        bounds[0].clamp(0.0, w),
        bounds[1].clamp(0.0, h),
        bounds[2].clamp(0.0, w),
        bounds[3].clamp(0.0, h),
    ];
    (clamped[2] > clamped[0] && clamped[3] > clamped[1]).then_some(clamped)
}

fn bounds_of(points: impl Iterator<Item = [f64; 2]>) -> Option<[f64; 4]> {
    let mut out: Option<[f64; 4]> = None;
    for [x, y] in points {
        out = Some(match out {
            None => [x, y, x, y],
            Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
        });
    }
    out
}

/// The overlap of two boxes over their union, from 0 to 1.
pub(crate) fn iou(a: [f64; 4], b: [f64; 4]) -> f64 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = w * h;
    let area = |r: [f64; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 { 0.0 } else { (inter / union).clamp(0.0, 1.0) }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// What kind of picture an answer is about, which decides its layers and words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PictureKind {
    /// A photo or a still: regions, masks and measurements.
    Photo,
    /// A document page: blocks by kind in reading order.
    Document,
    /// A frame of a video: regions with tracks, trails and a timeline.
    VideoFrame,
}

/// The kind of a region's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ShapeKind {
    /// A point.
    Point,
    /// A box.
    Box,
    /// A polygon.
    Polygon,
    /// A mask.
    Mask,
    /// Keypoints.
    Keypoints,
}

/// How part of a fact reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FactStyle {
    /// Plain words.
    Plain,
    /// Data: an identifier, a box, a digest.
    Mono,
    /// A quieter note after the value.
    Muted,
    /// Bold, quoted words.
    Strong,
}

/// One part of a fact's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FactPart {
    /// The words.
    pub text: String,
    /// How they read.
    pub style: FactStyle,
}

/// A labelled line of a region's details: "Answered in · Model-input pixels, 960 × 960,
/// letterboxed".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    /// "Answered in".
    pub label: String,
    /// The value, in parts.
    pub parts: Vec<FactPart>,
}

impl Fact {
    fn new(label: &str) -> Self {
        Self { label: label.into(), parts: Vec::new() }
    }

    fn plain(mut self, text: impl Into<String>) -> Self {
        self.parts.push(FactPart { text: text.into(), style: FactStyle::Plain });
        self
    }

    fn mono(mut self, text: impl Into<String>) -> Self {
        self.parts.push(FactPart { text: text.into(), style: FactStyle::Mono });
        self
    }

    fn muted(mut self, text: impl Into<String>) -> Self {
        self.parts.push(FactPart { text: text.into(), style: FactStyle::Muted });
        self
    }
}

/// A value replaced by an escalation (board choice V6): what the local backend read, what
/// the escalation reads now, and the released crop's digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EscalationView {
    /// What the first backend read: `PO-7781S`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_as: Option<String>,
    /// Its score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_as_score: Option<f64>,
    /// Who escalated: "Gemini".
    pub by: String,
    /// The released crop's digest, short.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop_digest: Option<String>,
}

/// A mask to draw: COCO run-length encoding, column-major, unset run first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaskView {
    /// Width of the mask's raster.
    pub width: u32,
    /// Height of the mask's raster.
    pub height: u32,
    /// The runs.
    pub counts: Vec<u32>,
    /// Pixels set.
    pub area: u64,
}

/// One region of an answer, as the viewer draws and lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegionView {
    /// Its place in the answer's `RegionSet`: what an example and the export name.
    pub index: u32,
    /// Its place in reading order, from 1, for a page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    /// The label or block kind: "handwriting", "seal tear".
    pub label: String,
    /// The shape's kind.
    pub shape: ShapeKind,
    /// The text it reads, for a page's block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The list's quieter words: the text, "mask · 14.3 mm", "box · at the edge".
    pub detail: String,
    /// The score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// The score as the list shows it: "0.58".
    pub score_text: String,
    /// The track, for a video frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<u64>,
    /// "T4".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_name: Option<String>,
    /// Left edge in source pixels.
    pub x0: f64,
    /// Top edge in source pixels.
    pub y0: f64,
    /// Right edge in source pixels, exclusive.
    pub x1: f64,
    /// Bottom edge in source pixels, exclusive.
    pub y1: f64,
    /// The box as the details show it: `1250, 466 → 1532, 540`, in source pixels.
    pub box_text: String,
    /// The mask, when the shape is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskView>,
    /// The polygon's points, when the shape is one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub polygon: Vec<[f64; 2]>,
    /// The tag over the picture: "part · 0.96", "seal tear · 0.91 · 14.3 mm".
    pub tag: String,
    /// The details' heading: "5 · handwriting", "Seal tear", "T4 · forklift".
    pub heading: String,
    /// The details, in order (board choices V4, V5, V6).
    pub facts: Vec<Fact>,
    /// The transform chain as steps (board choice V4).
    pub chain: Vec<String>,
    /// A value an escalation replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<EscalationView>,
    /// Its measurement in one line: "14.3 mm".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<String>,
    /// It touches the picture's edge.
    pub at_edge: bool,
    /// Its pixels are blurred in the picture: a person.
    pub blurred: bool,
}

/// A label and how many regions carry it: a filter chip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelCount {
    /// The label.
    pub label: String,
    /// How many.
    pub count: u32,
}

/// A measurement tile (board choice V9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeasureTile {
    /// "Longest".
    pub label: String,
    /// "14.3 mm".
    pub value: String,
    /// "± 0.3 mm".
    pub uncertainty: String,
}

/// An answer's measurements (board choice V9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementsView {
    /// The region measured.
    pub region: u32,
    /// The tiles.
    pub tiles: Vec<MeasureTile>,
    /// "In the belt's plane, from the fiducial: 40.0 mm is 245 px, so 6.12 px per mm. Valid
    /// only in that plane."
    pub calibration: String,
    /// The longest length's ends, in source pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<[[f64; 2]; 2]>,
}

/// One track of a video (board choice V10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackView {
    /// The track's identifier.
    pub id: u64,
    /// "T4".
    pub name: String,
    /// "forklift".
    pub label: String,
    /// "139", "1.2k": frames seen.
    pub frames_text: String,
    /// When it is seen, in seconds.
    pub spans: Vec<[f64; 2]>,
    /// Where it was, oldest first.
    pub trail: Vec<[f64; 2]>,
    /// People are blurred.
    pub blurred: bool,
}

/// The video a frame comes from (board choice V10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VideoView {
    /// "10:34.1".
    pub at_text: String,
    /// "20:00".
    pub length_text: String,
    /// "frame 19,023 · sampled 5 a second".
    pub frame_text: String,
    /// Where the frame is, in seconds.
    pub at_s: f64,
    /// How long the video is, in seconds.
    pub length_s: f64,
    /// The tracks.
    pub tracks: Vec<TrackView>,
    /// "Tracks are anonymous …".
    pub note: String,
}

/// An answer with nothing in it (board choice V12): an answer, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EmptyView {
    /// "Nothing found".
    pub title: String,
    /// The sentence, in parts.
    pub text: Vec<FactPart>,
    /// Asked, Sent, Cost, Run.
    pub facts: Vec<Fact>,
    /// "Hidden by your filters." and its line.
    pub hidden: Vec<FactPart>,
    /// "The call failed." and its line.
    pub failed: Vec<FactPart>,
}

/// The run and the call that produced an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    /// "Document desk · 10:31".
    pub text: String,
    /// The agent, by its name in the runtime.
    pub agent: String,
    /// The tool and the call: "vision_read · c-31", in parts.
    pub call: Vec<FactPart>,
}

/// One answer for the picture, as the "Answer from" picker lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnswerChoice {
    /// The call's identifier.
    pub id: String,
    /// "PP-OCRv6 small".
    pub name: String,
    /// "on this computer", "Meta, in the cloud".
    pub place: String,
    /// The tool.
    pub tool: String,
}

/// One answer, in full.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnswerView {
    /// The call's identifier.
    pub id: String,
    /// The tool.
    pub tool: String,
    /// "PP-OCRv6 small".
    pub name: String,
    /// "on this computer".
    pub place: String,
    /// The regions, in the `RegionSet`'s order; a page's in reading order.
    pub regions: Vec<RegionView>,
    /// Labels with counts, in first-seen order.
    pub labels: Vec<LabelCount>,
    /// "11 regions", "1.1 s", "$0".
    pub stats: Vec<String>,
    /// The run and the call.
    pub run: RunView,
    /// When the answer holds nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty: Option<EmptyView>,
    /// Measurements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurements: Option<MeasurementsView>,
    /// The video and its tracks, for a frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoView>,
    /// The region selected first: the escalated or lowest-scored one on a page, the measured
    /// one in a photo, the first otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<u32>,
}

/// How a region of one answer stands against the other (board choice V11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Agreement {
    /// Both found it, with the same reading.
    Agree,
    /// Both found it and read it differently.
    Differ,
    /// Only this answer found it.
    OnlyHere,
}

/// A region's standing in a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompareMark {
    /// The region's index in its answer.
    pub index: u32,
    /// How it stands.
    pub agreement: Agreement,
    /// The tag over it: its reading where they differ, "only here".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// One side of a comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComparePane {
    /// The answer.
    pub answer: AnswerView,
    /// "PP-OCRv6" — the short name the differences use.
    pub short_name: String,
    /// Each region's standing.
    pub marks: Vec<CompareMark>,
}

/// One difference between two answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Difference {
    /// The region's number: its reading order, or its place.
    pub number: u32,
    /// Its label.
    pub label: String,
    /// "handwriting · read differently", "text · only B found it".
    pub title: String,
    /// What A answered: "PO-7781S · 0.58", or "—".
    pub a: String,
    /// What B answered.
    pub b: String,
    /// The region in A.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a_region: Option<u32>,
    /// The region in B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b_region: Option<u32>,
}

/// Two answers for one image, side by side (board choice V11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompareView {
    /// A.
    pub a: ComparePane,
    /// B.
    pub b: ComparePane,
    /// Regions both found and read alike.
    pub agree: u32,
    /// Regions both found and read differently.
    pub differ: u32,
    /// Regions only one found.
    pub only_one: u32,
    /// "Where both found a region, the boxes overlap closely (mean IoU 0.94)."
    pub overlap_text: String,
    /// The differences, A's order first.
    pub differences: Vec<Difference>,
}

/// The picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewerImage {
    /// `sha256:<hex>` in full.
    pub digest: String,
    /// `sha256:556b49…a22654` (board choice S4).
    pub digest_short: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `image/png`.
    pub mime_type: String,
    /// Where the surface loads it with the viewer's own authorisation.
    pub href: String,
}

/// Where a labelled example may be saved (board choice V13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamplesView {
    /// The source saves examples.
    pub can_save: bool,
    /// The eval fixture files there are, relative to the fixtures' root.
    pub files: Vec<String>,
    /// The file picked first.
    pub file: String,
    /// The case name offered first, for the selected region.
    pub case_name: String,
}

/// What the annotated viewer shows of one image in one session (spec 3.7 §8.2, X12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewerView {
    /// The session.
    pub session_id: String,
    /// The picture.
    pub image: ViewerImage,
    /// What kind of picture it is.
    pub kind: PictureKind,
    /// "INV-2291 · page 1".
    pub title: String,
    /// "1654 × 2339 · INV-2291.pdf, page 1 at 200 dpi".
    pub meta: String,
    /// The agent the back link returns to.
    pub agent: String,
    /// Its display name: "Document desk".
    pub agent_name: String,
    /// Every answer for this image in the session, newest first.
    pub answers: Vec<AnswerChoice>,
    /// The answer shown, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<AnswerView>,
    /// Two answers side by side, when asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<CompareView>,
    /// A feature's line under the picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// The score a region needs to be shown at first.
    pub min_score: f64,
    /// Saving labelled examples.
    pub examples: ExamplesView,
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

/// What a person calls a backend or a model by its identifier.
fn display_name(id: &str, models: &BTreeMap<String, WireModel>) -> String {
    if let Some(name) = models.get(id).and_then(|model| model.name.clone()) {
        return name;
    }
    match id {
        "rf-detr-n" => "RF-DETR Nano",
        "rf-detr-s" => "RF-DETR S",
        "rf-detr-m" => "RF-DETR M",
        "rf-detr-seg-n" => "RF-DETR-Seg Nano",
        "sam2.1-tiny" | "sam2-tiny" => "SAM 2.1 Tiny",
        "efficientsam-ti" => "EfficientSAM-Ti",
        "siglip2-base" => "SigLIP 2",
        "pp-ocrv6" | "pp-ocrv6-small" => "PP-OCRv6 small",
        "pp-ocrv6-tiny" => "PP-OCRv6 tiny",
        "paddleocr-vl" | "paddleocr-vl-1.6" => "PaddleOCR-VL-1.6",
        "bytetrack" => "ByteTrack",
        "qwen3-vl-2b-instruct" => "Qwen3-VL 2B",
        other => other,
    }
    .to_string()
}

/// The name of a provider by the host it answers on.
fn provider_of(hosts: &[String]) -> Option<&'static str> {
    hosts.iter().find_map(|host| {
        Some(match host.as_str() {
            h if h.ends_with("meta.ai") || h.ends_with("llama.com") => "Meta",
            h if h.ends_with("anthropic.com") => "Anthropic",
            h if h.ends_with("googleapis.com") => "Google",
            h if h.ends_with("nvidia.com") => "NVIDIA",
            h if h.ends_with("openai.com") => "OpenAI",
            _ => return None,
        })
    })
}

fn hosts_of(locality: &Value) -> Vec<String> {
    locality
        .get("hosts")
        .and_then(Value::as_array)
        .map(|hosts| hosts.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

fn is_remote(locality: &Value) -> bool {
    locality.get("kind").and_then(Value::as_str) == Some("remote")
}

/// "on this computer" or "Meta, in the cloud".
fn place_of(locality: &Value) -> String {
    if is_remote(locality) {
        match provider_of(&hosts_of(locality)) {
            Some(provider) => format!("{provider}, in the cloud"),
            None => "in the cloud".to_string(),
        }
    } else {
        "on this computer".to_string()
    }
}

/// "Gemini" for an escalation's backend: the model family a person knows.
fn escalation_name(backend: &str, locality: &Value) -> String {
    let lower = backend.to_ascii_lowercase();
    for (needle, name) in [
        ("gemini", "Gemini"),
        ("claude", "Claude"),
        ("sam-3", "sam-3.1"),
        ("nemotron", "NVIDIA Nemotron"),
        ("muse", "Muse"),
    ] {
        if lower.contains(needle) {
            return name.to_string();
        }
    }
    provider_of(&hosts_of(locality)).map_or_else(|| backend.to_string(), str::to_string)
}

/// "Model-input pixels, 960 × 960, letterboxed" (board choice V4).
pub(crate) fn frame_words(frame: &WireFrame) -> String {
    match frame {
        WireFrame::SentPixels => "Pixels of the image as sent".to_string(),
        WireFrame::Normalized { scale, order } => {
            let order = if order == "yx" { "y before x" } else { "x before y" };
            format!("0–{scale} on each axis, {order}")
        }
        WireFrame::ModelInput { width, height, letterbox } => {
            let how = if letterbox == "stretch" { "stretched" } else { "letterboxed" };
            format!("Model-input pixels, {width} × {height}, {how}")
        }
        WireFrame::Other => "A frame this viewer does not know".to_string(),
    }
}

/// The first step of a chain: what the source is.
fn source_words(record: &Record, width: u32, height: u32) -> String {
    let provenance = &record.image.provenance;
    match provenance.get("kind").and_then(Value::as_str) {
        Some("documentPage") => {
            let page = provenance.get("page").and_then(Value::as_u64).unwrap_or(1);
            let dpi = provenance.get("dpi").and_then(Value::as_u64);
            let name = document_name(record);
            match dpi {
                Some(dpi) => format!("Page {page} of {name} at {dpi} dpi · {width} × {height}"),
                None => format!("Page {page} of {name} · {width} × {height}"),
            }
        }
        Some("camera") => format!("Frame {width} × {height}"),
        Some("screen") => format!("Screenshot {width} × {height}"),
        _ if record.video.is_some() => format!("Video frame {width} × {height}"),
        _ => format!("Image {width} × {height}"),
    }
}

fn document_name(record: &Record) -> String {
    record
        .image
        .provenance
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| record.image.name.clone().filter(|name| !name.starts_with("sha256:")))
        .unwrap_or_else(|| "the document".to_string())
}

/// "Padded 140 left, 141 right · 960 × 960".
fn pad_words(left: u32, top: u32, right: u32, bottom: u32, after: (u32, u32)) -> String {
    let mut sides = Vec::new();
    if left > 0 && left == right {
        sides.push(format!("{left} left and right"));
    } else {
        if left > 0 {
            sides.push(format!("{left} left"));
        }
        if right > 0 {
            sides.push(format!("{right} right"));
        }
    }
    if top > 0 && top == bottom {
        sides.push(format!("{top} top and bottom"));
    } else {
        if top > 0 {
            sides.push(format!("{top} top"));
        }
        if bottom > 0 {
            sides.push(format!("{bottom} bottom"));
        }
    }
    format!("Padded {} · {} × {}", sides.join(", "), after.0, after.1)
}

/// The chain as steps (board choice V4): what the source is, each transform, and how the
/// answer came back.
pub(crate) fn chain_words(record: &Record, origin: &WireOrigin, shape: ShapeKind) -> Vec<String> {
    let chain = &origin.chain;
    let sizes = chain_sizes(chain);
    let mut steps = vec![source_words(record, chain.source_width, chain.source_height)];
    for (index, step) in chain.steps.iter().enumerate() {
        let after = sizes[index + 1];
        steps.push(match *step {
            WireTransform::Crop { x, y, width, height } => {
                format!("Cut to {width} × {height} at {x}, {y}")
            }
            WireTransform::Resize { width, height } => format!("Resized to {width} × {height}"),
            WireTransform::Pad { left, top, right, bottom } => {
                pad_words(left, top, right, bottom, after)
            }
            WireTransform::Rotate { quarter_turns } => {
                format!("Turned {}°", u32::from(quarter_turns % 4) * 90)
            }
            WireTransform::Redact => "Covered before it left; the boxes did not move".to_string(),
            WireTransform::Other => "A step this viewer does not know".to_string(),
        });
    }
    steps.push(
        match (shape, record.page.is_some()) {
            (_, true) => "Answer mapped back to source pixels",
            (ShapeKind::Mask, _) => "Mask mapped back to source pixels",
            (ShapeKind::Polygon, _) => "Outline mapped back to source pixels",
            (ShapeKind::Point | ShapeKind::Keypoints, _) => "Points mapped back to source pixels",
            (ShapeKind::Box, _) => "Box mapped back to source pixels",
        }
        .to_string(),
    );
    steps
}

/// `1250, 466 → 1532, 540`: a box in whole source pixels.
pub(crate) fn box_words(bounds: [f64; 4]) -> String {
    let r = |v: f64| v.round() as i64;
    format!("{}, {} → {}, {}", r(bounds[0]), r(bounds[1]), r(bounds[2]), r(bounds[3]))
}

/// `2,412`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "139", "980", "1.2k".
fn count_words(n: u64) -> String {
    if n >= 1_000 { format!("{:.1}k", n as f64 / 1_000.0) } else { n.to_string() }
}

/// "$0", "$0.0025".
fn cost_words(micro_usd: u64) -> String {
    if micro_usd == 0 { "$0".to_string() } else { crate::words::money(micro_usd) }
}

/// "1.1 s".
fn seconds_words(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1_000.0)
}

/// "10:34.1" for a time in a video.
fn video_time(seconds: f64) -> String {
    let tenths = (seconds * 10.0).round() as u64;
    format!("{}:{:02}.{}", tenths / 600, (tenths / 10) % 60, tenths % 10)
}

/// "20:00" for a video's length.
fn video_length(seconds: f64) -> String {
    let whole = seconds.round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// "Seal tear" from "seal tear".
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The call that answered, as the run shows it: `vision_read · c-31`; with its measuring
/// call, `vision_segment c-2207 · vision_measure c-2208`.
fn call_words(record: &Record) -> Vec<FactPart> {
    let mono = |text: &str| FactPart { text: text.to_string(), style: FactStyle::Mono };
    let plain = |text: String| FactPart { text, style: FactStyle::Plain };
    let measured = record.measurements.iter().find_map(|m| {
        Some((m.tool.clone().unwrap_or_else(|| "vision_measure".into()), m.call.clone()?))
    });
    match measured {
        Some((tool, call)) => vec![
            mono(&record.tool),
            plain(format!(" {} · ", record.call_id)),
            mono(&tool),
            plain(format!(" {call}")),
        ],
        None if record.video.is_some() => {
            vec![mono(&record.tool), plain(format!(" over sampled frames · {}", record.call_id))]
        }
        None => vec![mono(&record.tool), plain(format!(" · {}", record.call_id))],
    }
}

/// What the call asked for: "torn seal", "person".
fn asked_of(args: &Value) -> Option<String> {
    for key in ["concepts", "classes"] {
        if let Some(list) = args.get(key).and_then(Value::as_array) {
            let words: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            if !words.is_empty() {
                return Some(words.join(", "));
            }
        }
    }
    args.get("question").and_then(Value::as_str).map(str::to_string)
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// What the projection needs beyond the records.
pub(crate) struct ViewerInput<'a> {
    pub(crate) session: &'a str,
    pub(crate) agent_name: String,
    pub(crate) offset: FixedOffset,
    pub(crate) can_save: bool,
    pub(crate) files: Vec<String>,
}

/// The kind of picture `record` is about.
fn kind_of(record: &Record) -> PictureKind {
    if record.page.is_some() || record.tool == "vision_read" {
        PictureKind::Document
    } else if record.video.is_some() || !record.tracks.is_empty() {
        PictureKind::VideoFrame
    } else {
        PictureKind::Photo
    }
}

/// The backend calls that answered, not the escalations that replaced a value.
fn answering_calls(record: &Record) -> impl Iterator<Item = &WireCall> {
    record.calls.iter().filter(|call| call.escalation.is_none())
}

/// "PP-OCRv6 small", "RF-DETR S + SAM 2.1 Tiny".
fn answer_name(record: &Record) -> String {
    let mut names: Vec<String> = Vec::new();
    for call in answering_calls(record) {
        let manifest = manifest_of(record, &call.backend);
        let name = display_name(&manifest, &record.models);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        names.extend(
            record
                .region_set
                .regions
                .first()
                .map(|r| display_name(&r.origin.backend, &record.models)),
        );
    }
    names.join(" + ")
}

fn answer_place(record: &Record) -> String {
    answering_calls(record)
        .next()
        .map_or_else(|| "on this computer".to_string(), |c| place_of(&c.locality))
}

/// The manifest a backend ran: from a region it answered, the backend's own id otherwise.
fn manifest_of(record: &Record, backend: &str) -> String {
    record
        .region_set
        .regions
        .iter()
        .find(|region| region.origin.backend == backend)
        .and_then(|region| region.origin.manifest.clone())
        .unwrap_or_else(|| backend.to_string())
}

/// A model's line (board choice V5): `pp-ocrv6-small@3f2a9c1 · Apache-2.0`, or `· your
/// manifest` for one the built-in manifest does not hold.
fn model_fact(label: &str, record: &Record, id: &str, extra: Option<String>) -> Fact {
    let model = record.models.get(id).cloned().unwrap_or_default();
    let mut fact = Fact::new(label);
    fact = match model.revision.as_deref() {
        Some(revision) if model.built_in => {
            fact.mono(format!("{id}@{}", &revision[..revision.len().min(7)]))
        }
        _ => fact.mono(id),
    };
    // A built-in model's licence, unless the line says how it was prompted (board choice V5).
    let mut notes: Vec<String> = Vec::new();
    if model.built_in {
        if extra.is_none() {
            notes.extend(model.licence.clone());
        }
    } else if record.models.contains_key(id) {
        notes.push(match &model.note {
            Some(note) => format!("{note}, your manifest"),
            None => "your manifest".to_string(),
        });
    }
    notes.extend(extra);
    if !notes.is_empty() {
        fact = fact.muted(format!(" · {}", notes.join(" · ")));
    }
    fact
}

/// The run's line: the feature's, or "Document desk · 10:31".
fn run_view(record: &Record, input: &ViewerInput<'_>) -> RunView {
    let text = record.display.run.clone().unwrap_or_else(|| match record.at {
        Some(at) => {
            format!("{} · {}", input.agent_name, at.with_timezone(&input.offset).format("%H:%M"))
        }
        None => input.agent_name.clone(),
    });
    RunView { text, agent: record.agent.clone(), call: call_words(record) }
}

/// One answer, projected.
pub(crate) fn answer_view(record: &Record, input: &ViewerInput<'_>) -> AnswerView {
    let kind = kind_of(record);
    let (width, height) = (record.region_set.width, record.region_set.height);
    let order: Vec<usize> = match &record.page {
        Some(page) if !page.reading_order.is_empty() => page.reading_order.clone(),
        _ => (0..record.region_set.regions.len()).collect(),
    };
    let position: BTreeMap<usize, u32> =
        order.iter().enumerate().map(|(place, &index)| (index, place as u32 + 1)).collect();
    let measurement = record.measurements.first();
    let calibration_region = measurement.and_then(|m| m.calibration.region);
    let measured_text = |index: usize| {
        measurement
            .filter(|m| m.region == index)
            .and_then(|m| m.longest_mm)
            .map(|mm| format!("{mm:.1} mm"))
    };
    let tracks: BTreeMap<u64, &WireTrack> = record.tracks.iter().map(|t| (t.id, t)).collect();
    let segmented = record.calls.iter().any(|call| call.task == "segment");

    let mut regions = Vec::new();
    for &index in &order {
        let Some(region) = record.region_set.regions.get(index) else { continue };
        let Some(bounds) = shape_bounds(&region.shape, width, height) else { continue };
        let shape = match &region.shape {
            WireShape::Point { .. } => ShapeKind::Point,
            WireShape::Box { .. } | WireShape::Other => ShapeKind::Box,
            WireShape::Polygon { .. } => ShapeKind::Polygon,
            WireShape::Mask { .. } => ShapeKind::Mask,
            WireShape::Keypoints { .. } => ShapeKind::Keypoints,
        };
        let block = record.page.as_ref().and_then(|page| page.blocks.get(index));
        let track = region.track.and_then(|id| tracks.get(&id).copied());
        let base_label = block
            .map(|b| b.kind.clone())
            .or_else(|| region.label.clone())
            .unwrap_or_else(|| "region".to_string());
        let blurred = track.is_some_and(|t| t.blurred);
        let label = if blurred { format!("{base_label} · blurred") } else { base_label.clone() };
        let text = block.map(|b| b.text.clone()).filter(|t| !t.is_empty());
        let measured = measured_text(index);
        let at_edge = bounds[0] <= 0.5
            || bounds[1] <= 0.5
            || bounds[2] >= f64::from(width) - 0.5
            || bounds[3] >= f64::from(height) - 0.5;
        let shape_word = if shape == ShapeKind::Mask { "mask" } else { "box" };
        let read_as = region.origin.escalated_from.as_ref().and_then(|from| from.text.clone());
        let detail = if let Some(text) = read_as.as_ref().or(text.as_ref()) {
            text.clone()
        } else if track.is_some() {
            String::new()
        } else {
            let mut words = vec![shape_word.to_string()];
            if let Some(mm) = &measured {
                words.push(mm.clone());
            } else if at_edge {
                words.push("at the edge".into());
            } else if calibration_region == Some(index) {
                words.push("calibration".into());
            }
            words.join(" · ")
        };
        // A page's region carries its block's confidence.
        let score = region.score.or(block.and_then(|b| b.confidence));
        let score_text = score.map(|s| format!("{s:.2}")).unwrap_or_default();
        let track_name = region.track.map(|id| format!("T{id}"));
        let tag = match (&track_name, kind) {
            (Some(name), _) => [
                Some(label.clone()),
                Some(name.clone()),
                (!score_text.is_empty()).then(|| score_text.clone()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · "),
            (None, PictureKind::Document) => {
                let number = position.get(&index).copied().unwrap_or(index as u32 + 1);
                format!("{number} · {base_label} · {score_text}")
            }
            _ => [
                Some(label.clone()),
                (!score_text.is_empty()).then(|| score_text.clone()),
                measured.clone(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · "),
        };
        let heading = match (kind, &track_name) {
            (_, Some(name)) => format!("{name} · {base_label}"),
            (PictureKind::Document, None) => {
                format!(
                    "{} · {base_label}",
                    position.get(&index).copied().unwrap_or(index as u32 + 1)
                )
            }
            _ => capitalised(&label),
        };

        // The details (board choices V4–V6).
        let mut facts = Vec::new();
        let escalation = region.origin.escalated_from.as_ref().map(|from| {
            let call = record.calls.iter().find(|call| call.escalation.is_some());
            EscalationView {
                read_as: from.text.clone(),
                read_as_score: from.score,
                by: call.map_or_else(
                    || region.origin.backend.clone(),
                    |c| escalation_name(&c.backend, &c.locality),
                ),
                crop_digest: call.and_then(|c| c.egress.first()).map(|d| short_digest(d)),
            }
        });
        let primary = region
            .origin
            .escalated_from
            .as_ref()
            .map_or_else(|| region.origin.backend.clone(), |from| from.backend.clone());
        if let (Some(escalation), Some(text)) = (&escalation, &text) {
            let mut read_as = Fact::new("Read as");
            if let Some(was) = &escalation.read_as {
                read_as = read_as.mono(was.clone());
            }
            if let Some(score) = escalation.read_as_score {
                read_as = read_as.muted(format!(" · {score:.2}"));
            }
            facts.push(read_as);
            let mut now = Fact::new("Now").mono(text.clone()).muted(format!(
                " · {} read {} of it,",
                escalation.by,
                if escalation.crop_digest.is_some() { "a covered crop" } else { "a crop" }
            ));
            if let Some(digest) = &escalation.crop_digest {
                now = now.plain(" ").mono(digest.clone());
            }
            facts.push(now);
        }
        match (kind, &region.shape) {
            (PictureKind::Document, _) => {
                facts.push(Fact::new("Box").mono(box_words(bounds)).muted(" source pixels"));
                facts.push(model_fact(
                    "Model",
                    record,
                    &manifest_of_backend(record, region, &primary),
                    None,
                ));
            }
            (PictureKind::VideoFrame, _) => {
                if let Some(track) = track {
                    let mut seen = Fact::new("Seen");
                    // The stretch of the video this frame is in, else the first.
                    let at = record.video.as_ref().map_or(0.0, |video| video.at_s);
                    let span = track
                        .spans
                        .iter()
                        .find(|span| span[0] <= at && at <= span[1])
                        .or(track.spans.first());
                    if let Some(span) = span {
                        seen = seen.plain(format!(
                            "{} → {}",
                            video_time(span[0]),
                            video_time(span[1])
                        ));
                    }
                    facts.push(seen.muted(format!(" · {} frames", thousands(track.frames))));
                }
                facts.push(Fact::new("Box now").mono(box_words(bounds)));
                for call in answering_calls(record) {
                    match call.task.as_str() {
                        "detect" => facts.push(model_fact(
                            "Found by",
                            record,
                            &manifest_of(record, &call.backend),
                            None,
                        )),
                        "track" => {
                            let mut fact = Fact::new("Tracked by")
                                .plain(display_name(&call.backend, &record.models));
                            let mut note = Vec::new();
                            note.extend(call.library.clone());
                            note.push(place_of(&call.locality));
                            fact = fact.muted(format!(" · {}", note.join(", ")));
                            facts.push(fact);
                        }
                        _ => {}
                    }
                }
            }
            (PictureKind::Photo, WireShape::Mask { width: mw, height: mh, counts }) => {
                let area = mask_bounds(*mw, *mh, counts).map_or(0, |(_, area)| area);
                facts.push(
                    Fact::new("Shape")
                        .plain(format!("Mask, {} px · box ", thousands(area)))
                        .mono(box_words(bounds)),
                );
                photo_models(record, &mut facts, segmented);
            }
            (PictureKind::Photo, _) => {
                facts.push(Fact::new("Shape").plain("Box ").mono(box_words(bounds)));
                photo_models(record, &mut facts, false);
            }
        }
        facts.push(Fact::new("Answered in").plain(frame_words(&region.origin.frame)));

        let (mask, polygon) = match &region.shape {
            WireShape::Mask { width, height, counts } => (
                Some(MaskView {
                    width: *width,
                    height: *height,
                    counts: counts.clone(),
                    area: mask_bounds(*width, *height, counts).map_or(0, |(_, area)| area),
                }),
                Vec::new(),
            ),
            WireShape::Polygon { points } => (None, points.clone()),
            _ => (None, Vec::new()),
        };
        regions.push(RegionView {
            index: index as u32,
            order: (kind == PictureKind::Document).then(|| position.get(&index).copied()).flatten(),
            label,
            shape,
            text,
            detail,
            score,
            score_text,
            track: region.track,
            track_name,
            x0: bounds[0],
            y0: bounds[1],
            x1: bounds[2],
            y1: bounds[3],
            box_text: box_words(bounds),
            mask,
            polygon,
            tag,
            heading,
            facts,
            chain: chain_words(record, &region.origin, shape),
            escalation,
            measurement: measured,
            at_edge,
            blurred,
        });
    }
    // Chips in the answer's own order (board choice V3).
    let mut labels: Vec<LabelCount> = Vec::new();
    let mut by_index: Vec<&RegionView> = regions.iter().collect();
    by_index.sort_by_key(|region| {
        if kind == PictureKind::Document { region.order.unwrap_or(0) } else { region.index }
    });
    for region in by_index {
        let label = region.label.trim_end_matches(" · blurred").to_string();
        match labels.iter_mut().find(|entry| entry.label == label) {
            Some(entry) => entry.count += 1,
            None => labels.push(LabelCount { label, count: 1 }),
        }
    }
    // A photo lists its measured region first, then the rest by score.
    if kind == PictureKind::Photo {
        let measured = measurement.map(|m| m.region as u32);
        regions.sort_by(|a, b| {
            let key = |r: &RegionView| (Some(r.index) != measured, r.shape != ShapeKind::Mask);
            key(a).cmp(&key(b)).then(b.score.unwrap_or(0.0).total_cmp(&a.score.unwrap_or(0.0)))
        });
        // The calibration reference goes last.
        if let Some(reference) = calibration_region
            && let Some(at) = regions.iter().position(|r| r.index as usize == reference)
        {
            let region = regions.remove(at);
            regions.push(region);
        }
    }

    if kind == PictureKind::Document {
        // Chips in the order a page's kinds are read about: title first, then by first sight.
        labels.sort_by_key(|entry| match entry.label.as_str() {
            "title" => 0,
            "text" => 1,
            "table" => 2,
            "seal" => 3,
            "handwriting" => 4,
            "checkbox" => 5,
            _ => 6,
        });
    }

    let duration: u64 = record.calls.iter().map(|call| call.duration_ms).sum();
    let cost: u64 = record.calls.iter().filter_map(|call| call.cost_micro_usd).sum();
    let stats = vec![
        format!("{} region{}", regions.len(), if regions.len() == 1 { "" } else { "s" }),
        seconds_words(duration),
        cost_words(cost),
    ];

    let measurements = measurement.map(|m| {
        let mut tiles = Vec::new();
        if let Some(mm) = m.longest_mm {
            tiles.push(MeasureTile {
                label: "Longest".into(),
                value: format!("{mm:.1} mm"),
                uncertainty: format!("± {:.1} mm", m.longest_uncertainty_mm.unwrap_or(0.0)),
            });
        }
        if let Some(mm2) = m.area_mm2 {
            tiles.push(MeasureTile {
                label: "Area".into(),
                value: format!("{mm2:.1} mm²"),
                uncertainty: format!("± {:.1} mm²", m.area_uncertainty_mm2.unwrap_or(0.0)),
            });
        }
        let c = &m.calibration;
        let per_mm = if c.reference_mm > 0.0 { c.reference_px / c.reference_mm } else { 0.0 };
        MeasurementsView {
            region: m.region as u32,
            tiles,
            calibration: format!(
                "In {}, from {}: {:.1} mm is {} px, so {per_mm:.2} px per mm. Valid only in that plane.",
                c.plane,
                c.from,
                c.reference_mm,
                c.reference_px.round() as i64
            ),
            line: m.line,
        }
    });

    let video = record.video.as_ref().map(|v| VideoView {
        at_text: video_time(v.at_s),
        length_text: video_length(v.length_s),
        frame_text: format!("frame {} · sampled {} a second", thousands(v.frame), v.sampled_per_s),
        at_s: v.at_s,
        length_s: v.length_s,
        tracks: record
            .tracks
            .iter()
            .map(|t| {
                let label = t.label.clone().unwrap_or_default();
                TrackView {
                    id: t.id,
                    name: format!("T{}", t.id),
                    label: if t.blurred { format!("{label} · blurred") } else { label },
                    frames_text: count_words(t.frames),
                    spans: t.spans.clone(),
                    trail: t.trail.clone(),
                    blurred: t.blurred,
                }
            })
            .collect(),
        note: "Tracks are anonymous and start again when the video is opened again. People are \
               blurred in every kept frame."
            .into(),
    });

    let empty = regions.is_empty().then(|| empty_view(record, input, kind, cost));
    let selected = match kind {
        PictureKind::Document => regions
            .iter()
            .find(|r| r.escalation.is_some())
            .or_else(|| {
                regions
                    .iter()
                    .filter(|r| r.score.is_some_and(|s| s >= DEFAULT_MIN_SCORE))
                    .min_by(|a, b| a.score.unwrap_or(1.0).total_cmp(&b.score.unwrap_or(1.0)))
            })
            .or_else(|| regions.first())
            .map(|r| r.index),
        _ => regions.first().map(|r| r.index),
    };
    AnswerView {
        id: record.call_id.clone(),
        tool: record.tool.clone(),
        name: answer_name(record),
        place: answer_place(record),
        regions,
        labels,
        stats,
        run: run_view(record, input),
        empty,
        measurements,
        video,
        selected,
    }
}

fn manifest_of_backend(record: &Record, region: &WireRegion, backend: &str) -> String {
    if region.origin.backend == backend {
        region.origin.manifest.clone().unwrap_or_else(|| manifest_of(record, backend))
    } else {
        manifest_of(record, backend)
    }
}

/// A photo's "Found by" and "Outlined by" lines.
fn photo_models(record: &Record, facts: &mut Vec<Fact>, segmented: bool) {
    for call in answering_calls(record) {
        let manifest = manifest_of(record, &call.backend);
        match call.task.as_str() {
            "detect" => facts.push(model_fact("Found by", record, &manifest, None)),
            "segment" if segmented => {
                facts.push(model_fact("Outlined by", record, &manifest, call.prompted.clone()));
            }
            "describe" => facts.push(model_fact("Described by", record, &manifest, None)),
            _ => {}
        }
    }
}

/// "Nothing found … That is an answer, not an error." (board choice V12).
fn empty_view(record: &Record, input: &ViewerInput<'_>, kind: PictureKind, cost: u64) -> EmptyView {
    let call = answering_calls(record).next();
    let backend =
        call.map_or_else(|| record.tool.clone(), |c| display_name(&c.backend, &record.models));
    let picture = match kind {
        PictureKind::Document => "page",
        PictureKind::VideoFrame | PictureKind::Photo
            if record.image.provenance.get("kind").and_then(Value::as_str) == Some("camera") =>
        {
            "frame"
        }
        PictureKind::VideoFrame => "frame",
        PictureKind::Photo => "image",
    };
    let asked = record.display.asked.clone().or_else(|| asked_of(&record.args));
    let mut text = vec![FactPart {
        text: format!(
            "{backend} answered with an empty result: nothing in this {picture} matched "
        ),
        style: FactStyle::Plain,
    }];
    match &asked {
        Some(asked) => {
            text.push(FactPart { text: format!("“{asked}”"), style: FactStyle::Strong })
        }
        None => text.push(FactPart { text: "what it was asked".into(), style: FactStyle::Plain }),
    }
    text.push(FactPart {
        text: ". That is an answer, not an error.".into(),
        style: FactStyle::Plain,
    });

    let mut asked_fact = Fact::new("Asked").mono(record.tool.clone());
    if let Some(asked) = &asked {
        let what = if record.args.get("concepts").is_some() { "concept" } else { "class" };
        asked_fact = asked_fact.plain(format!(" · {what} “{asked}”"));
    }
    asked_fact = asked_fact.plain(format!(" · {}", record.call_id));
    let mut facts = vec![asked_fact];
    if let Some(call) = call {
        let sent = if is_remote(&call.locality) {
            let mut fact = Fact::new("Sent").plain("A released copy to ");
            fact = fact.mono(hosts_of(&call.locality).join(", "));
            if let Some(covered) = &record.display.covered {
                fact = fact.plain(format!(" · {covered}"));
            }
            fact
        } else {
            Fact::new("Sent").plain("Nothing left this computer")
        };
        facts.push(sent);
    }
    let mut cost_fact = Fact::new("Cost").plain(cost_words(cost));
    if cost > 0 {
        cost_fact = cost_fact.muted(" · billed though nothing matched");
    }
    facts.push(cost_fact);
    facts.push(Fact::new("Run").plain(run_view(record, input).text));
    EmptyView {
        title: "Nothing found".into(),
        text,
        facts,
        hidden: vec![
            FactPart { text: "Hidden by your filters.".into(), style: FactStyle::Strong },
            FactPart {
                text: " “4 regions are hidden by Score at least 0.95.” ".into(),
                style: FactStyle::Plain,
            },
        ],
        failed: vec![
            FactPart { text: "The call failed.".into(), style: FactStyle::Strong },
            FactPart {
                text: format!(
                    " “{backend} was rate limited; tried 3 times.” No answer, so nothing to show or save."
                ),
                style: FactStyle::Plain,
            },
        ],
    }
}

/// The viewer's title and meta line for `record`.
fn title_of(record: &Record) -> (String, String) {
    let (w, h) = (record.image.width, record.image.height);
    let provenance = &record.image.provenance;
    let page = (provenance.get("kind").and_then(Value::as_str) == Some("documentPage"))
        .then(|| provenance.get("page").and_then(Value::as_u64).unwrap_or(1));
    let name = record.image.name.clone().filter(|name| !name.starts_with("sha256:"));
    let stem = name.as_deref().map(|n| n.rsplit_once('.').map_or(n, |(stem, _)| stem).to_string());
    let title = record.display.title.clone().unwrap_or_else(|| match (&stem, page) {
        (Some(stem), Some(page)) => format!("{stem} · page {page}"),
        (Some(stem), None) => stem.clone(),
        (None, Some(page)) => format!("Page {page}"),
        (None, None) => "An image".to_string(),
    });
    let meta = record.display.meta.clone().unwrap_or_else(|| {
        let mut parts = vec![format!("{w} × {h}")];
        if let Some(page) = page {
            let dpi = provenance.get("dpi").and_then(Value::as_u64);
            let doc = document_name(record);
            parts.push(match dpi {
                Some(dpi) => format!("{doc}, page {page} at {dpi} dpi"),
                None => format!("{doc}, page {page}"),
            });
        } else if let Some(name) = &name {
            parts.push(name.clone());
        }
        if let Some(kept) = &record.display.kept_until {
            parts.push(format!("kept as evidence until {kept}"));
        }
        parts.join(" · ")
    });
    (title, meta)
}

/// The web home's route for image `digest` of `session`, under `/api/surface`.
pub(crate) fn image_href(session: &str, digest: &str) -> String {
    format!(
        "/api/surface/vision/{}/images/{}",
        crate::evidence::percent_segment(session),
        crate::evidence::percent_segment(digest)
    )
}

/// Where the viewer opens image `image` of `session` in the web home.
pub fn viewer_path(session: &str, image: &str) -> String {
    format!(
        "/ui/vision/{}/{}",
        crate::evidence::percent_segment(session),
        crate::evidence::percent_segment(image)
    )
}

/// A slug for a case name: "inv-2291-p1-handwriting".
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The viewer of the image `records` (every answer for one digest, oldest first) are about.
/// `answer` picks the answer shown — the newest otherwise — and `compare` a second one.
pub(crate) fn view(
    records: &[Record],
    answer: Option<&str>,
    compare: Option<&str>,
    input: &ViewerInput<'_>,
) -> Option<ViewerView> {
    let newest = records.last()?;
    let shown = answer.and_then(|id| records.iter().find(|r| r.call_id == id)).unwrap_or(newest);
    let (title, meta) = title_of(shown);
    let shown_view = answer_view(shown, input);
    let compare = compare
        .and_then(|id| records.iter().find(|r| r.call_id == id && r.call_id != shown.call_id))
        .map(|other| compare_view(shown_view.clone(), answer_view(other, input)));
    let selected = shown_view
        .selected
        .and_then(|index| shown_view.regions.iter().find(|r| r.index == index))
        .map_or_else(|| "region".to_string(), |r| r.label.clone());
    let files = input.files.clone();
    let file = files.first().cloned().unwrap_or_else(|| "evals/vision/examples.test.json".into());
    Some(ViewerView {
        session_id: input.session.to_string(),
        image: ViewerImage {
            digest: shown.image.digest.clone(),
            digest_short: short_digest(&shown.image.digest),
            width: shown.image.width,
            height: shown.image.height,
            mime_type: shown.image.mime_type.clone(),
            href: image_href(input.session, &shown.image.digest),
        },
        kind: kind_of(shown),
        title: title.clone(),
        meta,
        agent: shown.agent.clone(),
        agent_name: input.agent_name.clone(),
        answers: records
            .iter()
            .rev()
            .map(|record| AnswerChoice {
                id: record.call_id.clone(),
                name: answer_name(record),
                place: answer_place(record),
                tool: record.tool.clone(),
            })
            .collect(),
        answer: Some(shown_view),
        compare,
        caption: shown.display.caption.clone(),
        min_score: shown.display.min_score.unwrap_or(DEFAULT_MIN_SCORE),
        examples: ExamplesView {
            can_save: input.can_save,
            files,
            file,
            // "INV-2291 · page 1" and "handwriting": `inv-2291-p1-handwriting`.
            case_name: slug(&format!("{} {selected}", title.replace("page ", "p"))),
        },
    })
}

/// The size of an encoded PNG, GIF or JPEG from its header; `None` for anything else.
pub(crate) fn encoded_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 =
        |at: usize| bytes.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let be16 =
        |at: usize| bytes.get(at..at + 2).map(|b| u32::from(u16::from_be_bytes([b[0], b[1]])));
    let le16 =
        |at: usize| bytes.get(at..at + 2).map(|b| u32::from(u16::from_le_bytes([b[0], b[1]])));
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((be32(16)?, be32(20)?));
    }
    if bytes.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        // Walk the segments to the first start-of-frame marker.
        let mut at = 2;
        while at + 4 <= bytes.len() {
            if bytes[at] != 0xFF {
                return None;
            }
            let marker = bytes[at + 1];
            let length = be16(at + 2)? as usize;
            if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                return Some((be16(at + 7)?, be16(at + 5)?));
            }
            at += 2 + length;
        }
    }
    None
}

/// The viewer of an image nothing has answered for yet: the picture alone, with the answers
/// list empty, so a person opening a held call's source image sees it before the call runs.
pub(crate) fn unanswered(
    session: &str,
    image: &str,
    digest: &str,
    bytes: &crate::evidence::EvidenceBytes,
    input: &ViewerInput<'_>,
) -> ViewerView {
    let (width, height) = encoded_size(&bytes.bytes).unwrap_or((0, 0));
    let name = (!image.starts_with("sha256:")).then(|| image.to_string());
    let title = name.as_deref().map_or_else(
        || "An image".to_string(),
        |n| n.rsplit_once('.').map_or(n, |(stem, _)| stem).to_string(),
    );
    let mut meta = vec![format!("{width} × {height}")];
    meta.extend(name);
    meta.push("nothing has answered for it yet".into());
    ViewerView {
        session_id: session.to_string(),
        image: ViewerImage {
            digest: digest.to_string(),
            digest_short: short_digest(digest),
            width,
            height,
            mime_type: bytes.mime_type.clone(),
            href: image_href(session, digest),
        },
        kind: PictureKind::Photo,
        title: title.clone(),
        meta: meta.join(" · "),
        agent: String::new(),
        agent_name: input.agent_name.clone(),
        answers: Vec::new(),
        answer: None,
        compare: None,
        caption: None,
        min_score: DEFAULT_MIN_SCORE,
        examples: ExamplesView {
            can_save: false,
            files: input.files.clone(),
            file: input.files.first().cloned().unwrap_or_default(),
            case_name: slug(&title),
        },
    }
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// The IoU two regions need to count as the same one.
const SAME_REGION: f64 = 0.5;

fn reading(region: &RegionView) -> String {
    region.text.clone().unwrap_or_else(|| region.label.clone())
}

fn reading_with_score(region: &RegionView) -> String {
    if region.score_text.is_empty() {
        reading(region)
    } else {
        format!("{} · {}", reading(region), region.score_text)
    }
}

/// The two answers matched region by region, greedily by overlap (board choice V11).
pub(crate) fn compare_view(a: AnswerView, b: AnswerView) -> CompareView {
    let bounds = |r: &RegionView| [r.x0, r.y0, r.x1, r.y1];
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (i, ra) in a.regions.iter().enumerate() {
        for (j, rb) in b.regions.iter().enumerate() {
            let overlap = iou(bounds(ra), bounds(rb));
            if overlap >= SAME_REGION {
                pairs.push((overlap, i, j));
            }
        }
    }
    pairs.sort_by(|x, y| y.0.total_cmp(&x.0));
    let (mut matched_a, mut matched_b) = (vec![None; a.regions.len()], vec![None; b.regions.len()]);
    let mut overlaps = Vec::new();
    for (overlap, i, j) in pairs {
        if matched_a[i].is_none() && matched_b[j].is_none() {
            matched_a[i] = Some(j);
            matched_b[j] = Some(i);
            overlaps.push(overlap);
        }
    }
    let (mut marks_a, mut marks_b) = (Vec::new(), Vec::new());
    let (mut agree, mut differ, mut only_one) = (0u32, 0u32, 0u32);
    let mut differences = Vec::new();
    let number = |r: &RegionView| r.order.unwrap_or(r.index + 1);
    for (i, ra) in a.regions.iter().enumerate() {
        match matched_a[i] {
            Some(j) => {
                let rb = &b.regions[j];
                if reading(ra) == reading(rb) {
                    agree += 1;
                    marks_a.push(CompareMark {
                        index: ra.index,
                        agreement: Agreement::Agree,
                        tag: None,
                    });
                } else {
                    differ += 1;
                    marks_a.push(CompareMark {
                        index: ra.index,
                        agreement: Agreement::Differ,
                        tag: Some(reading(ra)),
                    });
                    differences.push(Difference {
                        number: number(ra),
                        label: ra.label.clone(),
                        title: format!("{} · read differently", ra.label),
                        a: reading_with_score(ra),
                        b: reading_with_score(rb),
                        a_region: Some(ra.index),
                        b_region: Some(rb.index),
                    });
                }
            }
            None => {
                only_one += 1;
                marks_a.push(CompareMark {
                    index: ra.index,
                    agreement: Agreement::OnlyHere,
                    tag: Some("only here".into()),
                });
                differences.push(Difference {
                    number: number(ra),
                    label: ra.label.clone(),
                    title: format!("{} · only A found it", ra.label),
                    a: reading_with_score(ra),
                    b: "—".into(),
                    a_region: Some(ra.index),
                    b_region: None,
                });
            }
        }
    }
    for (j, rb) in b.regions.iter().enumerate() {
        match matched_b[j] {
            Some(i) => {
                let ra = &a.regions[i];
                let same = reading(ra) == reading(rb);
                marks_b.push(CompareMark {
                    index: rb.index,
                    agreement: if same { Agreement::Agree } else { Agreement::Differ },
                    tag: (!same).then(|| reading(rb)),
                });
            }
            None => {
                only_one += 1;
                marks_b.push(CompareMark {
                    index: rb.index,
                    agreement: Agreement::OnlyHere,
                    tag: Some("only here".into()),
                });
                differences.push(Difference {
                    number: number(rb),
                    label: rb.label.clone(),
                    title: format!("{} · only B found it", rb.label),
                    a: "—".into(),
                    b: reading_with_score(rb),
                    a_region: None,
                    b_region: Some(rb.index),
                });
            }
        }
    }
    // Readings that differ first, the least sure first; then what only one answer found.
    let score_of = |difference: &Difference| {
        let region = difference.a_region.and_then(|i| a.regions.iter().find(|r| r.index == i));
        region.and_then(|r| r.score).unwrap_or(1.0)
    };
    differences.sort_by(|x, y| {
        let only = |d: &Difference| d.a_region.is_none() || d.b_region.is_none();
        only(x).cmp(&only(y)).then(score_of(x).total_cmp(&score_of(y)))
    });
    let mean = if overlaps.is_empty() {
        0.0
    } else {
        overlaps.iter().sum::<f64>() / overlaps.len() as f64
    };
    let overlap_text = if overlaps.is_empty() {
        "The two answers found no region in common.".to_string()
    } else {
        let closely = if mean >= 0.9 {
            "closely"
        } else if mean >= 0.7 {
            "well"
        } else {
            "loosely"
        };
        format!("Where both found a region, the boxes overlap {closely} (mean IoU {mean:.2}).")
    };
    let short = |name: &str| {
        name.split_whitespace().next().unwrap_or(name).trim_end_matches("-1.6").to_string()
    };
    CompareView {
        a: ComparePane { short_name: short(&a.name), answer: a, marks: marks_a },
        b: ComparePane { short_name: short(&b.name), answer: b, marks: marks_b },
        agree,
        differ,
        only_one,
        overlap_text,
        differences,
    }
}

// ---------------------------------------------------------------------------
// Labelled examples
// ---------------------------------------------------------------------------

/// What an example holds (board choice V13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ExampleScope {
    /// This region only.
    Region,
    /// Every region on the page, with this correction.
    Page,
    /// Nothing: the answer is that nothing is there.
    Nothing,
}

/// A region as a person corrected or drew it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegionCorrection {
    /// The region it corrects, by its index in the answer; absent for a region the person
    /// drew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// The label.
    pub label: String,
    /// The text, for a page's block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The box `[x0, y0, x1, y1]` in the image's pixels.
    #[serde(rename = "box")]
    pub bbox: [f64; 4],
}

/// `POST /api/surface/vision/{session}/examples`: save a labelled example (board choices V11,
/// V12, V13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelledExample {
    /// The answer the example starts from.
    pub answer: String,
    /// The eval fixtures file, relative to the fixtures' root: `evals/vision/invoices.test.json`.
    pub file: String,
    /// The case's name: `inv-2291-p1-po-number`.
    pub case_name: String,
    /// What the example holds.
    pub scope: ExampleScope,
    /// The region, corrected, kept or drawn; absent for an example with nothing in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<RegionCorrection>,
}

/// What saving an example did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedExample {
    /// The file, relative to the fixtures' root.
    pub file: String,
    /// The case's name.
    pub case_name: String,
    /// The image file the case points at, relative to the eval file.
    pub image: String,
    /// "Saved to evals/vision/invoices.test.json as inv-2291-p1-po-number. The run's answer
    /// stays as it was."
    pub text: String,
}

/// The extension a fixture image takes for its MIME type.
pub(crate) fn extension_of(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    }
}

/// The image path a case names, relative to its eval file: `images/<hex>.png`.
pub(crate) fn fixture_image_path(digest: &str, mime: &str) -> String {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    format!("images/{hex}.{}", extension_of(mime))
}

/// Checks a fixtures file name a person picked: relative, inside the root, a `.test.json`.
///
/// # Errors
///
/// A sentence for the person.
pub(crate) fn check_file(file: &str) -> std::result::Result<(), String> {
    let path = std::path::Path::new(file);
    let inside = path.components().all(|part| matches!(part, std::path::Component::Normal(_)));
    if file.is_empty() || !inside {
        return Err(format!("“{file}” must be a path inside the eval fixtures folder"));
    }
    if !file.ends_with(".test.json") {
        return Err(format!("“{file}” must end in .test.json, the files `cargo adk eval` reads"));
    }
    Ok(())
}

/// Checks a case name: letters, digits, `-`, `_` and `.`.
///
/// # Errors
///
/// A sentence for the person.
pub(crate) fn check_case_name(name: &str) -> std::result::Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 120
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(format!("the case name “{name}” takes letters, digits, - _ and . only"))
    }
}

/// The `adk-eval` case an example makes of `record` (board choices V11–V13): a turn that shows
/// the image by path and digest, asks what the call asked, and expects the regions or text the
/// person kept, in the image's pixels.
///
/// # Errors
///
/// A sentence for the person when the example does not fit the answer.
pub(crate) fn example_case(
    record: &Record,
    example: &LabelledExample,
) -> std::result::Result<Value, String> {
    check_case_name(&example.case_name)?;
    let (w, h) = (f64::from(record.image.width), f64::from(record.image.height));
    if let Some(region) = &example.region {
        let [x0, y0, x1, y1] = region.bbox;
        if !(x0 >= 0.0 && y0 >= 0.0 && x1 <= w && y1 <= h && x0 < x1 && y0 < y1) {
            return Err(format!("the box must lie inside the {w} × {h} image and cover something"));
        }
        if region.label.trim().is_empty() {
            return Err("a region needs a label".into());
        }
    }
    let round = |v: f64| (v * 10.0).round() / 10.0;
    let boxed = |b: [f64; 4]| json!([round(b[0]), round(b[1]), round(b[2]), round(b[3])]);
    let read = record.tool == "vision_read";
    let blocks = record.page.as_ref().map(|page| &page.blocks);
    let corrected_index = example.region.as_ref().and_then(|r| r.index).map(|i| i as usize);

    let (prompt, expected) = match (example.scope, &example.region) {
        (ExampleScope::Nothing, _) => {
            let asked = asked_of(&record.args).unwrap_or_else(|| "anything".to_string());
            (format!("Find every {asked} in this image."), json!({ "count": 0 }))
        }
        (ExampleScope::Region, None) | (ExampleScope::Page, None) => {
            return Err("pick a region to keep, or save an example with nothing in it".into());
        }
        (ExampleScope::Region, Some(region)) if read => {
            let text = region.text.clone().unwrap_or_default();
            let b = region.bbox;
            (
                format!(
                    "Read the {} in the region [{}, {}, {}, {}] of this page.",
                    region.label,
                    round(b[0]),
                    round(b[1]),
                    round(b[2]),
                    round(b[3])
                ),
                json!({ "text": text }),
            )
        }
        (ExampleScope::Page, Some(region)) if read => {
            let order: Vec<usize> = record
                .page
                .as_ref()
                .map(|page| page.reading_order.clone())
                .filter(|order| !order.is_empty())
                .unwrap_or_else(|| (0..blocks.map_or(0, Vec::len)).collect());
            let mut lines: Vec<String> = Vec::new();
            for index in order {
                if Some(index) == corrected_index {
                    lines.extend(region.text.clone());
                } else if let Some(block) = blocks.and_then(|b| b.get(index)) {
                    lines.push(block.text.clone());
                }
            }
            if corrected_index.is_none() {
                lines.extend(region.text.clone());
            }
            ("Read this page.".to_string(), json!({ "text": lines.join("\n") }))
        }
        (ExampleScope::Region, Some(region)) => (
            format!("Find every {} in this image.", region.label),
            json!({ "regions": [{ "label": region.label, "box": boxed(region.bbox) }] }),
        ),
        (ExampleScope::Page, Some(region)) => {
            let mut regions = Vec::new();
            for (index, r) in record.region_set.regions.iter().enumerate() {
                if Some(index) == corrected_index {
                    continue;
                }
                let Some(bounds) = shape_bounds(&r.shape, record.image.width, record.image.height)
                else {
                    continue;
                };
                let label = r.label.clone().unwrap_or_else(|| "region".into());
                regions.push(json!({ "label": label, "box": boxed(bounds) }));
            }
            regions.push(json!({ "label": region.label, "box": boxed(region.bbox) }));
            let asked = asked_of(&record.args).unwrap_or_else(|| region.label.clone());
            (format!("Find every {asked} in this image."), json!({ "regions": regions }))
        }
    };
    let backend = answer_name(record);
    Ok(json!({
        "eval_id": example.case_name,
        "description": format!(
            "Kept in the annotated viewer from {} {} ({backend}) on {}",
            record.tool,
            record.call_id,
            short_digest(&record.image.digest)
        ),
        "conversation": [{
            "invocation_id": "t0",
            "user_content": {
                "role": "user",
                "parts": [
                    { "image": {
                        "path": fixture_image_path(&record.image.digest, &record.image.mime_type),
                        "digest": record.image.digest,
                    } },
                    { "text": prompt },
                ],
            },
            "expected_vision": expected,
        }],
        "tags": ["vision", record.tool.trim_start_matches("vision_"), "annotated-viewer"],
    }))
}

/// `set` (an eval set file's JSON, or nothing for a new file) with `case` in it: a case of the
/// same name is replaced.
pub(crate) fn with_case(set: Option<Value>, file: &str, case: Value) -> Value {
    let mut set = set.unwrap_or_else(|| {
        let stem = file.rsplit('/').next().unwrap_or(file).trim_end_matches(".test.json");
        json!({
            "eval_set_id": stem,
            "name": stem,
            "description": "Labelled examples kept in the annotated viewer",
            "eval_cases": [],
        })
    });
    let name = case.get("eval_id").cloned();
    if let Some(cases) = set.get_mut("eval_cases").and_then(Value::as_array_mut) {
        cases.retain(|existing| existing.get("eval_id") != name.as_ref());
        cases.push(case);
    } else {
        set["eval_cases"] = json!([case]);
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A one-region `vision_detect` record over a `w` × `h` image, its box `b` answered in
    /// model-input pixels through `steps`.
    fn one_box(w: u32, h: u32, b: [f64; 4], steps: Value) -> Record {
        let origin = json!({
            "backend": "rf-detr-s",
            "frame": { "kind": "modelInput", "width": 640, "height": 640, "letterbox": "center" },
            "chain": { "sourceWidth": w, "sourceHeight": h, "steps": steps },
        });
        Record::parse(json!({
            "tool": "vision_detect",
            "callId": "c-1",
            "agent": "desk",
            "image": { "digest": "sha256:00", "width": w, "height": h },
            "regionSet": { "image": "sha256:00", "width": w, "height": h, "regions": [
                { "shape": { "kind": "box", "x0": b[0], "y0": b[1], "x1": b[2], "y1": b[3] }, "label": "part", "score": 0.9, "origin": origin },
            ] },
        }))
        .unwrap()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// A point carried into the backend's pixels and back is the same source pixel, for
        /// any crop, resize, padding and turn.
        #[test]
        fn the_chain_maps_back_to_source_pixels(
            (w, h) in (16u32..4000, 16u32..4000),
            crop in (0.0f64..0.4, 0.0f64..0.4, 0.5f64..1.0, 0.5f64..1.0),
            target in (64u32..2048, 64u32..2048),
            pad in (0u32..300, 0u32..300, 0u32..300, 0u32..300),
            turns in 0u8..4,
            at in (0.0f64..1.0, 0.0f64..1.0),
        ) {
            let (cx, cy) = ((crop.0 * f64::from(w)) as u32, (crop.1 * f64::from(h)) as u32);
            let cw = ((crop.2 * f64::from(w - cx)) as u32).max(1);
            let ch = ((crop.3 * f64::from(h - cy)) as u32).max(1);
            let chain = WireChain {
                source_width: w,
                source_height: h,
                steps: vec![
                    WireTransform::Crop { x: cx, y: cy, width: cw, height: ch },
                    WireTransform::Resize { width: target.0, height: target.1 },
                    WireTransform::Pad { left: pad.0, top: pad.1, right: pad.2, bottom: pad.3 },
                    WireTransform::Rotate { quarter_turns: turns },
                ],
            };
            let point = [f64::from(cx) + at.0 * f64::from(cw), f64::from(cy) + at.1 * f64::from(ch)];
            let back = to_source(&chain, to_backend(&chain, point));
            prop_assert!((back[0] - point[0]).abs() < 1e-6 && (back[1] - point[1]).abs() < 1e-6, "{point:?} -> {back:?}");
        }

        /// The box the viewer draws and lists is the RegionSet's box in source pixels, clamped
        /// to the image, whatever frame and chain the backend answered through.
        #[test]
        fn shown_coordinates_are_source_pixels(
            (w, h) in (32u32..3000, 32u32..3000),
            corner in (-50.0f64..1.2, -50.0f64..1.2),
            size in (0.02f64..0.9, 0.02f64..0.9),
            letterbox in any::<bool>(),
        ) {
            let x0 = corner.0 * f64::from(w);
            let y0 = corner.1 * f64::from(h);
            let b = [x0, y0, x0 + size.0 * f64::from(w), y0 + size.1 * f64::from(h)];
            let steps = if letterbox {
                json!([{ "op": "resize", "width": 640, "height": 360 }, { "op": "pad", "left": 0, "top": 140, "right": 0, "bottom": 140 }])
            } else {
                json!([])
            };
            let record = one_box(w, h, b, steps);
            let input = ViewerInput {
                session: "s",
                agent_name: "Desk".into(),
                offset: FixedOffset::east_opt(0).unwrap(),
                can_save: false,
                files: Vec::new(),
            };
            let view = answer_view(&record, &input);
            let clamped = [
                b[0].clamp(0.0, f64::from(w)),
                b[1].clamp(0.0, f64::from(h)),
                b[2].clamp(0.0, f64::from(w)),
                b[3].clamp(0.0, f64::from(h)),
            ];
            if clamped[2] > clamped[0] && clamped[3] > clamped[1] {
                prop_assert_eq!(view.regions.len(), 1);
                let region = &view.regions[0];
                prop_assert_eq!([region.x0, region.y0, region.x1, region.y1], clamped);
                prop_assert_eq!(&region.box_text, &box_words(clamped));
            } else {
                // A box wholly outside the image draws nothing.
                prop_assert!(view.regions.is_empty());
            }
        }

        /// A mask's bounds and area are the rectangle of pixels it sets.
        #[test]
        fn a_masks_bounds_are_the_pixels_it_sets(
            (w, h) in (2u32..40, 2u32..40),
            rect in (0.0f64..1.0, 0.0f64..1.0, 0.0f64..1.0, 0.0f64..1.0),
        ) {
            let (x0, x1) = ((rect.0 * f64::from(w - 1)) as u32, (rect.2 * f64::from(w - 1)) as u32);
            let (y0, y1) = ((rect.1 * f64::from(h - 1)) as u32, (rect.3 * f64::from(h - 1)) as u32);
            let (x0, x1, y0, y1) = (x0.min(x1), x0.max(x1) + 1, y0.min(y1), y0.max(y1) + 1);
            let mut counts = Vec::new();
            let (mut current, mut run) = (false, 0u32);
            for x in 0..w {
                for y in 0..h {
                    let set = (x0..x1).contains(&x) && (y0..y1).contains(&y);
                    if set == current {
                        run += 1;
                    } else {
                        counts.push(run);
                        current = set;
                        run = 1;
                    }
                }
            }
            counts.push(run);
            let (bounds, area) = mask_bounds(w, h, &counts).unwrap();
            prop_assert_eq!(bounds, [f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1)]);
            prop_assert_eq!(area, u64::from((x1 - x0) * (y1 - y0)));
        }
    }

    fn chain(steps: Vec<WireTransform>) -> WireChain {
        WireChain { source_width: 1654, source_height: 2339, steps }
    }

    #[test]
    fn a_letterboxed_page_maps_back_to_its_pixels() {
        let chain = chain(vec![
            WireTransform::Resize { width: 679, height: 960 },
            WireTransform::Pad { left: 140, top: 0, right: 141, bottom: 0 },
        ]);
        assert_eq!(chain_sizes(&chain), vec![(1654, 2339), (679, 960), (960, 960)]);
        let there = to_backend(&chain, [1250.0, 466.0]);
        let back = to_source(&chain, there);
        assert!((back[0] - 1250.0).abs() < 1e-6 && (back[1] - 466.0).abs() < 1e-6, "{back:?}");
        assert_eq!(pad_words(140, 0, 141, 0, (960, 960)), "Padded 140 left, 141 right · 960 × 960");
        assert_eq!(
            pad_words(0, 224, 0, 224, (1024, 1024)),
            "Padded 224 top and bottom · 1024 × 1024"
        );
    }

    #[test]
    fn a_mask_is_bounded_by_the_pixels_it_sets() {
        // A 4 × 3 raster with (1, 1) and (2, 1) set: column-major, unset run first.
        assert_eq!(mask_bounds(4, 3, &[4, 1, 2, 1, 4]), Some(([1.0, 1.0, 3.0, 2.0], 2)));
        assert_eq!(mask_bounds(4, 3, &[12]), None);
    }

    #[test]
    fn overlap_is_symmetric_and_whole_for_one_box() {
        let a = [0.0, 0.0, 10.0, 10.0];
        let b = [5.0, 0.0, 15.0, 10.0];
        assert_eq!(iou(a, a), 1.0);
        assert_eq!(iou(a, b), iou(b, a));
        assert!((iou(a, b) - 50.0 / 150.0).abs() < 1e-9);
    }

    #[test]
    fn files_stay_inside_the_fixtures_folder() {
        assert!(check_file("evals/vision/invoices.test.json").is_ok());
        assert!(check_file("../invoices.test.json").is_err());
        assert!(check_file("/etc/invoices.test.json").is_err());
        assert!(check_file("evals/vision/invoices.json").is_err());
        assert!(check_case_name("inv-2291-p1-po-number").is_ok());
        assert!(check_case_name("a/b").is_err());
    }

    #[test]
    fn words_read_as_the_boards() {
        assert_eq!(box_words([1250.0, 466.2, 1531.6, 540.0]), "1250, 466 → 1532, 540");
        assert_eq!(thousands(19_023), "19,023");
        assert_eq!(count_words(1_200), "1.2k");
        assert_eq!(video_time(634.1), "10:34.1");
        assert_eq!(video_length(1_200.0), "20:00");
        assert_eq!(cost_words(2_500), "$0.0025");
        assert_eq!(cost_words(0), "$0");
        assert_eq!(seconds_words(14_800), "14.8 s");
        assert_eq!(slug("INV-2291 · page 1 handwriting"), "inv-2291-page-1-handwriting");
        assert_eq!(
            frame_words(&WireFrame::ModelInput {
                width: 960,
                height: 960,
                letterbox: "center".into()
            }),
            "Model-input pixels, 960 × 960, letterboxed"
        );
    }
}
