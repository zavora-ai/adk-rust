//! Vision scoring (spec 3.7 §7.16): regions, masks, OCR text, document fields, detections
//! and click targets, compared in the source image's pixels.
//!
//! A turn states what it expects in [`Turn::expected_vision`](crate::Turn::expected_vision)
//! ([`VisionExpected`]); the agent's perception answer ([`VisionAnswer`]) is read from the
//! responses of the `adk-vision` tools it called — `vision_detect`, `vision_segment`,
//! `vision_read` and `vision_describe` — in the wire form those tools return. The scorers
//! depend on no vision crate: boxes are `[x0, y0, x1, y1]` in source pixels with `x1` and `y1`
//! exclusive, and masks are COCO run-length encodings, as `adk-vision` publishes them.
//!
//! | Score | Range | Compares |
//! |-------|-------|----------|
//! | `region_iou` | 0–1 | mean box IoU of each expected region with its matched answer region |
//! | `box_hit_rate` | 0–1 | share of expected regions whose matched box has every edge within `box_tolerance_px` |
//! | `mask_iou` | 0–1 | mean mask IoU of each expected mask with its matched answer mask |
//! | `ocr_char_accuracy` | 0–1 | `1 − CER`: character edit distance over the expected text's length |
//! | `ocr_word_accuracy` | 0–1 | `1 − WER`: word edit distance over the expected word count |
//! | `field_exact_match` | 0–1 | share of expected fields whose value is identical |
//! | `field_normalized_match` | 0–1 | the same, ignoring case, spacing and punctuation, and numbers by value |
//! | `detection_precision`, `detection_recall` | 0–1 | greedy matching by score at `iou_threshold`, labels compared |
//! | `detection_ap` | 0–1 | COCO 101-point average precision at `iou_threshold`, averaged over labels |
//! | `detection_map` | 0–1 | the same averaged over IoU thresholds 0.50, 0.55, …, 0.95 |
//! | `grounding_hit_rate` | 0–1 | share of targets whose matched answer's centre falls inside the target |
//! | `count_accuracy` | 0–1 | `1 − |found − expected| / expected` |
//!
//! Every score is higher-is-better, so [`BaselineStore`](crate::BaselineStore) gates it.
//! Labels match when they are equal ignoring case and punctuation, or one contains the
//! other; an expected region without a label matches any answer region.
//!
//! # Example
//!
//! ```
//! use adk_eval::vision::{VisionAnswer, VisionCriteria, VisionExpected, score};
//! use serde_json::json;
//!
//! let expected: VisionExpected = serde_json::from_value(json!({
//!     "regions": [{"label": "seal", "box": [10, 10, 50, 30]}],
//!     "text": "Seal torn"
//! }))
//! .unwrap();
//! let detected = VisionAnswer::from_tool_response(
//!     "vision_detect",
//!     &json!({"regions": [{"box": [10, 10, 50, 34], "label": "seal", "score": 0.9}]}),
//! )
//! .unwrap();
//! let read = VisionAnswer::from_tool_response(
//!     "vision_read",
//!     &json!({"blocks": [{"kind": "text", "text": "Seal torm", "box": [10, 40, 90, 50]}]}),
//! )
//! .unwrap();
//! let answer = detected.merged(read);
//!
//! let scores = score(&expected, &answer, &VisionCriteria::default());
//! assert_eq!(scores.get("region_iou"), Some(800.0 / 960.0));
//! assert_eq!(scores.get("ocr_char_accuracy"), Some(1.0 - 1.0 / 9.0));
//! assert_eq!(scores.get("ocr_word_accuracy"), Some(0.5));
//! ```

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `adk-vision` tools whose responses carry a perception answer.
pub const VISION_TOOLS: [&str; 4] =
    ["vision_detect", "vision_segment", "vision_read", "vision_describe"];

/// A box as `[x0, y0, x1, y1]` in source pixels, `x1` and `y1` exclusive.
pub type BoxPx = [f64; 4];

/// A binary mask, run-length encoded in COCO's column-major order: `counts` alternates runs
/// of unset and set pixels, starting with unset, scanning each column top to bottom. The
/// wire form of `adk_vision::Mask`.
///
/// # Example
///
/// ```
/// use adk_eval::vision::RleMask;
///
/// // A 4×3 raster with pixels (1, 1) and (2, 1) set.
/// let mask = RleMask { width: 4, height: 3, counts: vec![4, 1, 2, 1, 4] };
/// assert_eq!(mask.area(), 2);
/// assert_eq!(mask.bbox(), Some([1.0, 1.0, 3.0, 2.0]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RleMask {
    /// Width of the raster.
    pub width: u32,
    /// Height of the raster.
    pub height: u32,
    /// Alternating run lengths, unset first.
    pub counts: Vec<u32>,
}

impl RleMask {
    /// The `[start, end)` linear (column-major) ranges of set pixels.
    fn set_runs(&self) -> Vec<(u64, u64)> {
        let mut runs = Vec::with_capacity(self.counts.len() / 2);
        let mut at = 0u64;
        for (index, &count) in self.counts.iter().enumerate() {
            let next = at + u64::from(count);
            if index % 2 == 1 && count > 0 {
                runs.push((at, next));
            }
            at = next;
        }
        runs
    }

    /// The number of set pixels.
    pub fn area(&self) -> u64 {
        self.counts.iter().skip(1).step_by(2).map(|&c| u64::from(c)).sum()
    }

    /// `true` when the runs sum to `width × height`.
    pub fn is_valid(&self) -> bool {
        let total: u64 = self.counts.iter().map(|&c| u64::from(c)).sum();
        total == u64::from(self.width) * u64::from(self.height)
    }

    /// The smallest box covering every set pixel, or `None` for an empty mask.
    pub fn bbox(&self) -> Option<BoxPx> {
        let height = u64::from(self.height.max(1));
        let mut bounds: Option<(u64, u64, u64, u64)> = None;
        for (start, end) in self.set_runs() {
            // A run can wrap from the bottom of one column to the top of the next.
            let (first_col, last_col) = (start / height, (end - 1) / height);
            let (y_top, y_bottom) = if first_col == last_col {
                (start % height, (end - 1) % height)
            } else {
                (0, height - 1)
            };
            bounds = Some(match bounds {
                None => (first_col, y_top, last_col, y_bottom),
                Some((x0, y0, x1, y1)) => {
                    (x0.min(first_col), y0.min(y_top), x1.max(last_col), y1.max(y_bottom))
                }
            });
        }
        bounds.map(|(x0, y0, x1, y1)| [x0 as f64, y0 as f64, (x1 + 1) as f64, (y1 + 1) as f64])
    }
}

/// Intersection over union of two masks, or `None` when their rasters differ in size.
///
/// # Example
///
/// ```
/// use adk_eval::vision::{RleMask, mask_iou};
///
/// let a = RleMask { width: 2, height: 2, counts: vec![0, 2, 2] }; // left column
/// let b = RleMask { width: 2, height: 2, counts: vec![1, 2, 1] }; // (0, 1) and (1, 0)
/// assert_eq!(mask_iou(&a, &b), Some(1.0 / 3.0));
/// ```
pub fn mask_iou(a: &RleMask, b: &RleMask) -> Option<f64> {
    if (a.width, a.height) != (b.width, b.height) {
        return None;
    }
    let (runs_a, runs_b) = (a.set_runs(), b.set_runs());
    let (mut i, mut j, mut inter) = (0, 0, 0u64);
    while i < runs_a.len() && j < runs_b.len() {
        let (start, end) = (runs_a[i].0.max(runs_b[j].0), runs_a[i].1.min(runs_b[j].1));
        if start < end {
            inter += end - start;
        }
        if runs_a[i].1 < runs_b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    let union = a.area() + b.area() - inter;
    Some(if union == 0 { 1.0 } else { inter as f64 / union as f64 })
}

/// Intersection over union of two boxes, `0.0` for disjoint or empty boxes.
///
/// # Example
///
/// ```
/// use adk_eval::vision::box_iou;
///
/// assert_eq!(box_iou(&[0.0, 0.0, 10.0, 10.0], &[5.0, 0.0, 15.0, 10.0]), 50.0 / 150.0);
/// ```
pub fn box_iou(a: &BoxPx, b: &BoxPx) -> f64 {
    let area = |b: &BoxPx| (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let inter =
        (a[2].min(b[2]) - a[0].max(b[0])).max(0.0) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// The largest difference between corresponding edges of two boxes, in pixels.
///
/// # Example
///
/// ```
/// use adk_eval::vision::box_edge_error;
///
/// assert_eq!(box_edge_error(&[10.0, 10.0, 50.0, 30.0], &[11.0, 8.5, 50.0, 30.0]), 1.5);
/// ```
pub fn box_edge_error(a: &BoxPx, b: &BoxPx) -> f64 {
    (0..4).map(|i| (a[i] - b[i]).abs()).fold(0.0, f64::max)
}

/// Edit distance between two sequences: insertions, deletions and substitutions, each 1.
///
/// # Example
///
/// ```
/// use adk_eval::vision::edit_distance;
///
/// let a: Vec<char> = "kitten".chars().collect();
/// let b: Vec<char> = "sitting".chars().collect();
/// assert_eq!(edit_distance(&a, &b), 3);
/// ```
pub fn edit_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, item) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, other) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(item != other);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// Collapses every run of whitespace, line breaks included, to one space and trims.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Character error rate: edit distance between the texts' characters over the expected
/// text's length, after collapsing whitespace. `0.0` for two empty texts; `1.0` (or more)
/// when nothing matches.
///
/// # Example
///
/// ```
/// use adk_eval::vision::character_error_rate;
///
/// assert_eq!(character_error_rate("INVOICE 4721", "INV0ICE  4721"), 1.0 / 12.0);
/// ```
pub fn character_error_rate(expected: &str, actual: &str) -> f64 {
    let expected: Vec<char> = collapse_whitespace(expected).chars().collect();
    let actual: Vec<char> = collapse_whitespace(actual).chars().collect();
    rate(edit_distance(&expected, &actual), expected.len(), actual.len())
}

/// Word error rate: edit distance between the texts' whitespace-separated words over the
/// expected word count.
///
/// # Example
///
/// ```
/// use adk_eval::vision::word_error_rate;
///
/// assert_eq!(word_error_rate("total due 58.20", "total due 58.2O"), 1.0 / 3.0);
/// ```
pub fn word_error_rate(expected: &str, actual: &str) -> f64 {
    let expected: Vec<&str> = expected.split_whitespace().collect();
    let actual: Vec<&str> = actual.split_whitespace().collect();
    rate(edit_distance(&expected, &actual), expected.len(), actual.len())
}

fn rate(distance: usize, expected: usize, actual: usize) -> f64 {
    match (expected, actual) {
        (0, 0) => 0.0,
        (0, _) => 1.0,
        _ => distance as f64 / expected as f64,
    }
}

/// A value reduced for the normalised comparison: lowercase letters and digits only.
fn normalize_field(value: &str) -> String {
    value.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A field value read as a number, ignoring currency signs and thousands separators.
fn field_number(value: &str) -> Option<f64> {
    let kept: String =
        value.chars().filter(|c| c.is_ascii_digit() || matches!(c, '.' | '-')).collect();
    let has_digit = kept.chars().any(|c| c.is_ascii_digit());
    let only_number_chars = value.chars().all(|c| {
        c.is_ascii_digit()
            || c.is_whitespace()
            || matches!(c, '.' | ',' | '-' | '+' | '$' | '€' | '£')
    });
    (has_digit && only_number_chars).then(|| kept.parse().ok()).flatten()
}

/// Whether two field values match ignoring case, spacing and punctuation, or as numbers.
///
/// # Example
///
/// ```
/// use adk_eval::vision::fields_match_normalized;
///
/// assert!(fields_match_normalized("+1 415 555 0142", "+14155550142"));
/// assert!(fields_match_normalized("$1,234.50", "1234.5"));
/// assert!(!fields_match_normalized("1234.5", "1234.6"));
/// ```
pub fn fields_match_normalized(expected: &str, actual: &str) -> bool {
    if let (Some(a), Some(b)) = (field_number(expected), field_number(actual)) {
        return (a - b).abs() < 1e-9;
    }
    let expected = normalize_field(expected);
    !expected.is_empty() && expected == normalize_field(actual)
}

/// A labelled region an answer must find, in source pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedRegion {
    /// The class, concept or target name; an unlabelled region matches any answer label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The box; derived from `mask` when absent.
    #[serde(rename = "box", default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BoxPx>,
    /// The exact mask, for mask IoU.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<RleMask>,
}

impl ExpectedRegion {
    /// A labelled box.
    pub fn boxed(label: impl Into<String>, bbox: BoxPx) -> Self {
        Self { label: Some(label.into()), bbox: Some(bbox), mask: None }
    }

    /// The box, or the mask's covering box.
    pub fn bbox(&self) -> Option<BoxPx> {
        self.bbox.or_else(|| self.mask.as_ref().and_then(RleMask::bbox))
    }
}

/// What a turn expects the agent to perceive. Each part present is scored by the
/// [`VisionCriteria`] thresholds that apply to it; parts left out are not scored.
///
/// # Example
///
/// ```
/// use adk_eval::vision::VisionExpected;
///
/// let expected: VisionExpected = serde_json::from_str(r#"{
///     "regions": [{"label": "bear", "box": [1.4, 68.8, 586.0, 632.8]}],
///     "fields": {"invoice_number": "INV-4721"},
///     "count": 1
/// }"#).unwrap();
/// assert_eq!(expected.regions[0].bbox(), Some([1.4, 68.8, 586.0, 632.8]));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VisionExpected {
    /// Objects to find: region IoU, box error, mask IoU and detection scores.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<ExpectedRegion>,
    /// The page's text in reading order: OCR character and word accuracy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Field values a document answer extracts: exact and normalised field match.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    /// Click targets: each matched answer region's centre must fall inside its target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<ExpectedRegion>,
    /// How many regions the answer should hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

/// One region of an answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerRegion {
    /// The label the backend gave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The box in source pixels.
    #[serde(rename = "box", default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BoxPx>,
    /// The mask, for segmentation answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<RleMask>,
    /// The backend's confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

impl AnswerRegion {
    fn bbox(&self) -> Option<BoxPx> {
        self.bbox.or_else(|| self.mask.as_ref().and_then(RleMask::bbox))
    }

    /// Reads one region in the vision tools' compact form (`box` as an array, optional
    /// `mask`) or the region-set form (`shape` with a `kind`).
    fn from_json(value: &Value) -> Option<Self> {
        let label = value.get("label").and_then(Value::as_str).map(str::to_string);
        let score = value.get("score").and_then(Value::as_f64);
        let mut bbox = value.get("box").and_then(box_from_json);
        let mut mask: Option<RleMask> =
            value.get("mask").and_then(|m| serde_json::from_value(m.clone()).ok());
        if let Some(shape) = value.get("shape") {
            match shape.get("kind").and_then(Value::as_str) {
                Some("box") => {
                    bbox = bbox.or_else(|| {
                        let edge = |k: &str| shape.get(k).and_then(Value::as_f64);
                        Some([edge("x0")?, edge("y0")?, edge("x1")?, edge("y1")?])
                    });
                }
                Some("mask") => {
                    mask = mask.or_else(|| serde_json::from_value(shape.clone()).ok());
                }
                Some("point") => {
                    let (x, y) = (shape.get("x")?.as_f64()?, shape.get("y")?.as_f64()?);
                    bbox = bbox.or(Some([x, y, x, y]));
                }
                Some("polygon") => {
                    let points = shape.get("points")?.as_array()?;
                    let xy: Vec<(f64, f64)> = points
                        .iter()
                        .filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?)))
                        .collect();
                    bbox = bbox.or_else(|| covering(&xy));
                }
                _ => {}
            }
        }
        let mask = mask.filter(RleMask::is_valid);
        (bbox.is_some() || mask.is_some()).then_some(Self { label, bbox, mask, score })
    }
}

fn box_from_json(value: &Value) -> Option<BoxPx> {
    let edges = value.as_array()?;
    if edges.len() != 4 {
        return None;
    }
    Some([edges[0].as_f64()?, edges[1].as_f64()?, edges[2].as_f64()?, edges[3].as_f64()?])
}

fn covering(points: &[(f64, f64)]) -> Option<BoxPx> {
    let first = points.first()?;
    Some(points.iter().fold([first.0, first.1, first.0, first.1], |b, &(x, y)| {
        [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]
    }))
}

/// What an agent perceived in one turn, read from its vision tool responses.
///
/// # Example
///
/// ```
/// use adk_eval::vision::VisionAnswer;
/// use serde_json::json;
///
/// let answer = VisionAnswer::from_tool_response(
///     "vision_describe",
///     &json!({"text": "An invoice.", "value": {"total": 58.2, "currency": "USD"}}),
/// )
/// .unwrap();
/// assert_eq!(answer.fields["total"], "58.2");
/// assert_eq!(answer.text.as_deref(), Some("An invoice."));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VisionAnswer {
    /// Regions found, in source pixels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<AnswerRegion>,
    /// Text read or answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Field values, from a describer's structured value or a reader's fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
}

impl VisionAnswer {
    /// Reads the response of one of the [`VISION_TOOLS`]; `None` for any other tool or a
    /// response that holds no regions, text or fields.
    ///
    /// | Key | Becomes |
    /// |-----|---------|
    /// | `regions` | regions (`box` arrays, optional `mask`, or region-set `shape`s) |
    /// | `blocks` | text: the blocks' text in the order given, one line each |
    /// | `text` | text (preferred over `blocks`) |
    /// | `value`, `fields` | fields: each top-level scalar, as a string |
    pub fn from_tool_response(tool: &str, response: &Value) -> Option<Self> {
        if !VISION_TOOLS.contains(&tool) {
            return None;
        }
        Self::from_json(response)
    }

    /// Reads an answer from any JSON object with the keys
    /// [`from_tool_response`](Self::from_tool_response) lists.
    pub fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let regions: Vec<AnswerRegion> = object
            .get("regions")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(AnswerRegion::from_json).collect())
            .unwrap_or_default();
        let blocks = object.get("blocks").and_then(Value::as_array).map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        });
        let text = object.get("text").and_then(Value::as_str).map(str::to_string).or(blocks);
        let mut fields = BTreeMap::new();
        for key in ["value", "fields"] {
            if let Some(map) = object.get(key).and_then(Value::as_object) {
                fields.extend(scalar_fields(map));
            }
        }
        let answer = Self { regions, text, fields };
        (answer != Self::default()).then_some(answer)
    }

    /// This answer with `later`'s parts in place of its own, part by part.
    pub fn merged(mut self, later: Self) -> Self {
        if !later.regions.is_empty() {
            self.regions = later.regions;
        }
        if later.text.is_some() {
            self.text = later.text;
        }
        if !later.fields.is_empty() {
            self.fields = later.fields;
        }
        self
    }
}

fn scalar_fields(map: &serde_json::Map<String, Value>) -> impl Iterator<Item = (String, String)> {
    map.iter().filter_map(|(key, value)| {
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null | Value::Array(_) | Value::Object(_) => return None,
        };
        Some((key.clone(), text))
    })
}

/// Thresholds for the vision scores, and the settings they are computed with. A score with
/// no threshold is not computed; a threshold whose part the turn does not expect is not
/// applied to that turn.
///
/// # Example
///
/// ```
/// use adk_eval::vision::VisionCriteria;
///
/// let criteria: VisionCriteria = serde_json::from_str(
///     r#"{"region_iou": 0.75, "ocr_char_accuracy": 0.98, "iou_threshold": 0.5}"#,
/// )
/// .unwrap();
/// assert!(criteria.has_thresholds());
/// assert_eq!(criteria.box_tolerance_px, 2.0);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisionCriteria {
    /// IoU at which a detection matches an expected region (default 0.5).
    #[serde(default = "default_iou_threshold")]
    pub iou_threshold: f64,
    /// Largest edge error, in source pixels, a box may have and count as a hit (default 2).
    #[serde(default = "default_box_tolerance")]
    pub box_tolerance_px: f64,
    /// Minimum `region_iou`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_iou: Option<f64>,
    /// Minimum `box_hit_rate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_hit_rate: Option<f64>,
    /// Minimum `mask_iou`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_iou: Option<f64>,
    /// Minimum `ocr_char_accuracy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_char_accuracy: Option<f64>,
    /// Minimum `ocr_word_accuracy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_word_accuracy: Option<f64>,
    /// Minimum `field_exact_match`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_exact_match: Option<f64>,
    /// Minimum `field_normalized_match`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_normalized_match: Option<f64>,
    /// Minimum `detection_precision`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_precision: Option<f64>,
    /// Minimum `detection_recall`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_recall: Option<f64>,
    /// Minimum `detection_ap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_ap: Option<f64>,
    /// Minimum `detection_map`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_map: Option<f64>,
    /// Minimum `grounding_hit_rate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_hit_rate: Option<f64>,
    /// Minimum `count_accuracy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_accuracy: Option<f64>,
}

fn default_iou_threshold() -> f64 {
    0.5
}

fn default_box_tolerance() -> f64 {
    2.0
}

impl Default for VisionCriteria {
    fn default() -> Self {
        Self {
            iou_threshold: default_iou_threshold(),
            box_tolerance_px: default_box_tolerance(),
            region_iou: None,
            box_hit_rate: None,
            mask_iou: None,
            ocr_char_accuracy: None,
            ocr_word_accuracy: None,
            field_exact_match: None,
            field_normalized_match: None,
            detection_precision: None,
            detection_recall: None,
            detection_ap: None,
            detection_map: None,
            grounding_hit_rate: None,
            count_accuracy: None,
        }
    }
}

impl VisionCriteria {
    /// Every score name with its threshold, in a fixed order.
    pub fn thresholds(&self) -> [(&'static str, Option<f64>); 13] {
        [
            ("region_iou", self.region_iou),
            ("box_hit_rate", self.box_hit_rate),
            ("mask_iou", self.mask_iou),
            ("ocr_char_accuracy", self.ocr_char_accuracy),
            ("ocr_word_accuracy", self.ocr_word_accuracy),
            ("field_exact_match", self.field_exact_match),
            ("field_normalized_match", self.field_normalized_match),
            ("detection_precision", self.detection_precision),
            ("detection_recall", self.detection_recall),
            ("detection_ap", self.detection_ap),
            ("detection_map", self.detection_map),
            ("grounding_hit_rate", self.grounding_hit_rate),
            ("count_accuracy", self.count_accuracy),
        ]
    }

    /// Whether any score has a threshold.
    pub fn has_thresholds(&self) -> bool {
        self.thresholds().iter().any(|(_, threshold)| threshold.is_some())
    }

    /// Checks every threshold lies in `[0, 1]`, the IoU threshold in `(0, 1]` and the box
    /// tolerance is a non-negative number of pixels.
    ///
    /// # Errors
    ///
    /// Returns a message naming the first value out of range.
    pub fn validate(&self) -> Result<(), String> {
        for (name, threshold) in self.thresholds() {
            if let Some(t) = threshold
                && !(0.0..=1.0).contains(&t)
            {
                return Err(format!("vision threshold {name} is {t}; use a value from 0.0 to 1.0"));
            }
        }
        if !(self.iou_threshold > 0.0 && self.iou_threshold <= 1.0) {
            return Err(format!(
                "vision iou_threshold is {}; use a value above 0.0 and at most 1.0",
                self.iou_threshold
            ));
        }
        if !(self.box_tolerance_px.is_finite() && self.box_tolerance_px >= 0.0) {
            return Err(format!(
                "vision box_tolerance_px is {}; use a non-negative number of pixels",
                self.box_tolerance_px
            ));
        }
        Ok(())
    }
}

/// Every vision score one turn produced, with notes on what missed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VisionScores {
    /// Score name to value, for the parts the turn expects.
    pub scores: BTreeMap<String, f64>,
    /// Largest box edge error among matched regions, in source pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_box_error_px: Option<f64>,
    /// What missed, one note per expected item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl VisionScores {
    /// One score by name.
    pub fn get(&self, name: &str) -> Option<f64> {
        self.scores.get(name).copied()
    }
}

/// Whether an answer label names an expected label: equal ignoring case and punctuation,
/// or one containing the other.
fn labels_match(expected: Option<&str>, actual: Option<&str>) -> bool {
    match (expected, actual) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(e), Some(a)) => {
            let (e, a) = (normalize_field(e), normalize_field(a));
            !e.is_empty() && !a.is_empty() && (e == a || e.contains(&a) || a.contains(&e))
        }
    }
}

/// Pairs each expected region with one answer region of a matching label, best IoU first;
/// returns `(expected index, answer index, IoU)` for every pair with a positive IoU.
fn match_by_iou(
    expected: &[ExpectedRegion],
    answer: &[AnswerRegion],
    iou: impl Fn(&ExpectedRegion, &AnswerRegion) -> Option<f64>,
) -> Vec<(usize, usize, f64)> {
    let mut candidates: Vec<(usize, usize, f64)> = Vec::new();
    for (e, truth) in expected.iter().enumerate() {
        for (a, found) in answer.iter().enumerate() {
            if labels_match(truth.label.as_deref(), found.label.as_deref())
                && let Some(value) = iou(truth, found)
                && value > 0.0
            {
                candidates.push((e, a, value));
            }
        }
    }
    candidates.sort_by(|x, y| y.2.total_cmp(&x.2).then(x.0.cmp(&y.0)).then(x.1.cmp(&y.1)));
    let (mut used_e, mut used_a) = (HashSet::new(), HashSet::new());
    candidates
        .into_iter()
        .filter(|(e, a, _)| {
            let free = !used_e.contains(e) && !used_a.contains(a);
            if free {
                used_e.insert(*e);
                used_a.insert(*a);
            }
            free
        })
        .collect()
}

/// Answer indexes, highest score first; an answer without a score ranks as 1.0.
fn by_score(answer: &[AnswerRegion]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..answer.len()).collect();
    order.sort_by(|&x, &y| {
        answer[y].score.unwrap_or(1.0).total_cmp(&answer[x].score.unwrap_or(1.0))
    });
    order
}

/// The best still-unmatched expected region for an answer box at `threshold`.
fn best_match(
    expected: &[ExpectedRegion],
    matched: &[bool],
    found: &AnswerRegion,
    threshold: f64,
) -> Option<usize> {
    let bbox = found.bbox()?;
    expected
        .iter()
        .enumerate()
        .filter(|(e, truth)| {
            !matched[*e] && labels_match(truth.label.as_deref(), found.label.as_deref())
        })
        .filter_map(|(e, truth)| truth.bbox().map(|b| (e, box_iou(&b, &bbox))))
        .filter(|(_, iou)| *iou >= threshold)
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .map(|(e, _)| e)
}

/// Detection matching at an IoU threshold, highest-scoring answers first (COCO's greedy
/// rule): `(true positives, answers, expected)`.
fn detection_counts(
    expected: &[ExpectedRegion],
    answer: &[AnswerRegion],
    threshold: f64,
) -> (usize, usize, usize) {
    let mut matched = vec![false; expected.len()];
    let mut hits = 0;
    for a in by_score(answer) {
        if let Some(e) = best_match(expected, &matched, &answer[a], threshold) {
            matched[e] = true;
            hits += 1;
        }
    }
    (hits, answer.len(), expected.len())
}

/// COCO's 101-point interpolated average precision at one IoU threshold, averaged over the
/// expected labels (unlabelled regions form one class matched by every answer).
///
/// # Example
///
/// ```
/// use adk_eval::vision::{AnswerRegion, ExpectedRegion, average_precision};
///
/// let truth = vec![ExpectedRegion::boxed("cat", [0.0, 0.0, 10.0, 10.0])];
/// let found = vec![AnswerRegion {
///     label: Some("cat".into()),
///     bbox: Some([0.0, 0.0, 10.0, 9.0]),
///     mask: None,
///     score: Some(0.9),
/// }];
/// assert_eq!(average_precision(&truth, &found, 0.5), 1.0);
/// assert_eq!(average_precision(&truth, &found, 0.95), 0.0);
/// ```
pub fn average_precision(expected: &[ExpectedRegion], answer: &[AnswerRegion], iou: f64) -> f64 {
    let mut labels: Vec<Option<String>> =
        expected.iter().map(|r| r.label.as_deref().map(normalize_field)).collect();
    labels.sort();
    labels.dedup();
    if labels.is_empty() {
        return if answer.is_empty() { 1.0 } else { 0.0 };
    }
    let per_label: Vec<f64> = labels
        .iter()
        .map(|label| {
            let truth: Vec<ExpectedRegion> = expected
                .iter()
                .filter(|r| r.label.as_deref().map(normalize_field) == *label)
                .cloned()
                .collect();
            let found: Vec<AnswerRegion> = answer
                .iter()
                .filter(|r| labels_match(label.as_deref(), r.label.as_deref()))
                .cloned()
                .collect();
            class_ap(&truth, &found, iou)
        })
        .collect();
    per_label.iter().sum::<f64>() / per_label.len() as f64
}

fn class_ap(truth: &[ExpectedRegion], answer: &[AnswerRegion], threshold: f64) -> f64 {
    let mut matched = vec![false; truth.len()];
    let (mut tp, mut fp) = (0usize, 0usize);
    let mut curve: Vec<(f64, f64)> = Vec::with_capacity(answer.len());
    for a in by_score(answer) {
        match best_match(truth, &matched, &answer[a], threshold) {
            Some(e) => {
                matched[e] = true;
                tp += 1;
            }
            None => fp += 1,
        }
        curve.push((tp as f64 / truth.len().max(1) as f64, tp as f64 / (tp + fp) as f64));
    }
    // Precision envelope from the right, then sampled at recall 0, 0.01, …, 1.
    for i in (0..curve.len().saturating_sub(1)).rev() {
        curve[i].1 = curve[i].1.max(curve[i + 1].1);
    }
    let total: f64 = (0..=100)
        .map(|step| {
            let recall = f64::from(step) / 100.0;
            curve.iter().find(|(r, _)| *r >= recall - 1e-12).map_or(0.0, |(_, p)| *p)
        })
        .sum();
    total / 101.0
}

fn put(scores: &mut BTreeMap<String, f64>, name: &str, value: f64) {
    scores.insert(name.to_string(), value.clamp(0.0, 1.0));
}

/// Scores an answer against what a turn expects, for every part the turn expects.
///
/// | Expected part | Scores |
/// |---------------|--------|
/// | `regions` | `region_iou`, `box_hit_rate`, `detection_precision`, `detection_recall`, `detection_ap`, `detection_map`, and `mask_iou` when a region has a mask |
/// | `text` | `ocr_char_accuracy`, `ocr_word_accuracy` |
/// | `fields` | `field_exact_match`, `field_normalized_match` |
/// | `targets` | `grounding_hit_rate` |
/// | `count` | `count_accuracy` |
///
/// A missing answer part scores 0; detection precision with no answer region is 1, and
/// recall carries the miss. `criteria` supplies the IoU threshold and box tolerance; its
/// score thresholds are not applied here.
pub fn score(
    expected: &VisionExpected,
    answer: &VisionAnswer,
    criteria: &VisionCriteria,
) -> VisionScores {
    let mut out = VisionScores::default();

    if !expected.regions.is_empty() {
        let pairs = match_by_iou(&expected.regions, &answer.regions, |t, a| {
            Some(box_iou(&t.bbox()?, &a.bbox()?))
        });
        let n = expected.regions.len() as f64;
        put(&mut out.scores, "region_iou", pairs.iter().map(|p| p.2).sum::<f64>() / n);
        let mut hits = 0u32;
        let mut worst: Option<f64> = None;
        for (e, a, _) in &pairs {
            if let (Some(t), Some(f)) = (expected.regions[*e].bbox(), answer.regions[*a].bbox()) {
                let error = box_edge_error(&t, &f);
                worst = Some(worst.map_or(error, |w| w.max(error)));
                if error <= criteria.box_tolerance_px {
                    hits += 1;
                }
            }
        }
        put(&mut out.scores, "box_hit_rate", f64::from(hits) / n);
        let matched: HashSet<usize> = pairs.iter().map(|p| p.0).collect();
        for (e, region) in expected.regions.iter().enumerate() {
            if !matched.contains(&e) {
                out.notes.push(format!(
                    "no answer region matches {}",
                    region.label.as_deref().unwrap_or("an unlabelled region")
                ));
            }
        }
        out.max_box_error_px = worst;

        let (tp, found, truth) =
            detection_counts(&expected.regions, &answer.regions, criteria.iou_threshold);
        let precision = if found == 0 { 1.0 } else { tp as f64 / found as f64 };
        put(&mut out.scores, "detection_precision", precision);
        put(&mut out.scores, "detection_recall", tp as f64 / truth as f64);
        let ap = average_precision(&expected.regions, &answer.regions, criteria.iou_threshold);
        put(&mut out.scores, "detection_ap", ap);
        let coco: f64 = (0..10)
            .map(|step| {
                let iou = 0.5 + 0.05 * f64::from(step);
                average_precision(&expected.regions, &answer.regions, iou)
            })
            .sum();
        put(&mut out.scores, "detection_map", coco / 10.0);

        let masked: Vec<ExpectedRegion> =
            expected.regions.iter().filter(|r| r.mask.is_some()).cloned().collect();
        if !masked.is_empty() {
            let pairs = match_by_iou(&masked, &answer.regions, |t, a| {
                mask_iou(t.mask.as_ref()?, a.mask.as_ref()?)
            });
            let mean = pairs.iter().map(|p| p.2).sum::<f64>() / masked.len() as f64;
            put(&mut out.scores, "mask_iou", mean);
        }
    }

    if let Some(text) = &expected.text {
        let actual = answer.text.as_deref().unwrap_or_default();
        put(&mut out.scores, "ocr_char_accuracy", 1.0 - character_error_rate(text, actual));
        put(&mut out.scores, "ocr_word_accuracy", 1.0 - word_error_rate(text, actual));
    }

    if !expected.fields.is_empty() {
        // Field names compare like values, so `Invoice Number` finds `invoice_number`.
        let actual: BTreeMap<String, &str> =
            answer.fields.iter().map(|(k, v)| (normalize_field(k), v.as_str())).collect();
        let (mut exact, mut normalized) = (0u32, 0u32);
        for (key, want) in &expected.fields {
            match actual.get(&normalize_field(key)) {
                Some(got) => {
                    if got.trim() == want.trim() {
                        exact += 1;
                    }
                    if fields_match_normalized(want, got) {
                        normalized += 1;
                    } else {
                        out.notes.push(format!("field {key}: expected {want:?}, got {got:?}"));
                    }
                }
                None => out.notes.push(format!("field {key}: missing")),
            }
        }
        let n = expected.fields.len() as f64;
        put(&mut out.scores, "field_exact_match", f64::from(exact) / n);
        put(&mut out.scores, "field_normalized_match", f64::from(normalized) / n);
    }

    if !expected.targets.is_empty() {
        let mut hits = 0u32;
        for target in &expected.targets {
            let Some(area) = target.bbox() else { continue };
            let name = target.label.as_deref().unwrap_or("?");
            let point = answer
                .regions
                .iter()
                .find(|r| labels_match(target.label.as_deref(), r.label.as_deref()))
                .and_then(AnswerRegion::bbox)
                .map(|b| ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0));
            match point {
                Some((x, y)) if x >= area[0] && x < area[2] && y >= area[1] && y < area[3] => {
                    hits += 1;
                }
                Some((x, y)) => out
                    .notes
                    .push(format!("target {name}: click at ({x:.1}, {y:.1}) falls outside")),
                None => out.notes.push(format!("target {name}: not found")),
            }
        }
        let rate = f64::from(hits) / expected.targets.len() as f64;
        put(&mut out.scores, "grounding_hit_rate", rate);
    }

    if let Some(count) = expected.count {
        let found = answer.regions.len() as f64;
        let want = count as f64;
        put(&mut out.scores, "count_accuracy", 1.0 - (found - want).abs() / want.max(1.0));
    }
    out
}

/// The perception answer in one turn's events: the [`VISION_TOOLS`] responses, later ones
/// replacing earlier parts, and, when no tool gave fields, a final response that is a JSON
/// object.
pub(crate) fn answer_from_events(
    events: &[adk_core::Event],
    final_text: Option<&str>,
) -> Option<VisionAnswer> {
    let mut answer: Option<VisionAnswer> = None;
    for event in events {
        let Some(content) = event.content() else { continue };
        for part in &content.parts {
            if let adk_core::Part::FunctionResponse { function_response, .. } = part
                && let Some(found) = VisionAnswer::from_tool_response(
                    &function_response.name,
                    &function_response.response,
                )
            {
                answer = Some(match answer {
                    Some(earlier) => earlier.merged(found),
                    None => found,
                });
            }
        }
    }
    let structured = final_text
        .and_then(|text| serde_json::from_str::<Value>(text.trim()).ok())
        .and_then(|value| {
            value.as_object().map(|map| scalar_fields(map).collect::<BTreeMap<_, _>>())
        })
        .filter(|fields| !fields.is_empty());
    match (answer, structured) {
        (Some(answer), Some(fields)) if answer.fields.is_empty() => {
            Some(VisionAnswer { fields, ..answer })
        }
        (None, Some(fields)) => Some(VisionAnswer { fields, ..VisionAnswer::default() }),
        (answer, _) => answer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn found(label: &str, bbox: BoxPx, score: f64) -> AnswerRegion {
        AnswerRegion { label: Some(label.into()), bbox: Some(bbox), mask: None, score: Some(score) }
    }

    fn mask_from_bits(width: u32, height: u32, set: impl Fn(u32, u32) -> bool) -> RleMask {
        let mut counts = Vec::new();
        let (mut current, mut run) = (false, 0u32);
        for x in 0..width {
            for y in 0..height {
                let bit = set(x, y);
                if bit != current {
                    counts.push(run);
                    current = bit;
                    run = 0;
                }
                run += 1;
            }
        }
        counts.push(run);
        RleMask { width, height, counts }
    }

    #[test]
    fn box_iou_by_hand() {
        // Overlap 5×10 = 50; union 100 + 100 − 50 = 150.
        assert_eq!(box_iou(&[0.0, 0.0, 10.0, 10.0], &[5.0, 0.0, 15.0, 10.0]), 50.0 / 150.0);
        // Contained: 4×4 inside 8×8 → 16 / 64.
        assert_eq!(box_iou(&[0.0, 0.0, 8.0, 8.0], &[2.0, 2.0, 6.0, 6.0]), 0.25);
        assert_eq!(box_iou(&[0.0, 0.0, 1.0, 1.0], &[2.0, 2.0, 3.0, 3.0]), 0.0);
        assert_eq!(box_iou(&[0.0, 0.0, 0.0, 0.0], &[0.0, 0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn box_edge_error_is_the_worst_edge() {
        assert_eq!(box_edge_error(&[80.0, 60.0, 320.0, 220.0], &[81.0, 57.5, 320.0, 222.0]), 2.5);
    }

    #[test]
    fn mask_runs_decode_by_hand() {
        // 3×2 raster, column-major: (0,0),(0,1),(1,0),(1,1),(2,0),(2,1).
        // Set (1,0), (1,1), (2,0): runs unset 2, set 3, unset 1.
        let mask = RleMask { width: 3, height: 2, counts: vec![2, 3, 1] };
        assert!(mask.is_valid());
        assert_eq!(mask.area(), 3);
        // The set run wraps from column 1 into column 2.
        assert_eq!(mask.bbox(), Some([1.0, 0.0, 3.0, 2.0]));
        assert_eq!(mask_from_bits(3, 2, |x, y| x == 1 || (x == 2 && y == 0)), mask);
        assert!(!RleMask { width: 3, height: 2, counts: vec![2, 3] }.is_valid());
        assert_eq!(RleMask { width: 3, height: 2, counts: vec![6] }.bbox(), None);
    }

    #[test]
    fn mask_iou_by_hand() {
        // A: columns 0–1 of a 4×4 raster (8 px). B: rows 0–1 (8 px). Overlap 2×2 = 4.
        let a = mask_from_bits(4, 4, |x, _| x < 2);
        let b = mask_from_bits(4, 4, |_, y| y < 2);
        assert_eq!(mask_iou(&a, &b), Some(4.0 / 12.0));
        assert_eq!(mask_iou(&a, &a), Some(1.0));
        let empty = RleMask { width: 4, height: 4, counts: vec![16] };
        assert_eq!(mask_iou(&empty, &empty), Some(1.0));
        assert_eq!(mask_iou(&a, &empty), Some(0.0));
        assert_eq!(mask_iou(&a, &RleMask { width: 2, height: 8, counts: vec![16] }), None);
    }

    #[test]
    fn error_rates_by_hand() {
        assert_eq!(character_error_rate("abc", "abc"), 0.0);
        // One substitution in 12 characters, the doubled space collapsed.
        assert_eq!(character_error_rate("INVOICE 4721", "INV0ICE  4721"), 1.0 / 12.0);
        // "Seal torn" (9 characters) against "Seal torm": one substitution.
        assert_eq!(character_error_rate("Seal torn", "Seal\ntorm"), 1.0 / 9.0);
        assert_eq!(character_error_rate("ab", ""), 1.0);
        assert_eq!(character_error_rate("", ""), 0.0);
        assert_eq!(character_error_rate("", "x"), 1.0);
        // Two insertions over a 1-character reference: CER 2, so accuracy clamps at 0.
        assert_eq!(character_error_rate("a", "abc"), 2.0);
        // Words: one of three substituted; then one deleted and one inserted of four.
        assert_eq!(word_error_rate("total due 58.20", "total due 58.2O"), 1.0 / 3.0);
        assert_eq!(word_error_rate("a b c d", "a c d e"), 2.0 / 4.0);
    }

    #[test]
    fn fields_match_by_hand() {
        assert!(fields_match_normalized("Amara Okafor", "amara  okafor"));
        assert!(fields_match_normalized("amara.okafor@example.com", "Amara.Okafor@Example.com"));
        assert!(fields_match_normalized("2026-11-19", "2026/11/19"));
        assert!(fields_match_normalized("58.20", "$58.2"));
        assert!(!fields_match_normalized("58.20", "58.21"));
        assert!(!fields_match_normalized("", ""));
        assert!(!fields_match_normalized("INV-4721", "INV-4712"));
    }

    #[test]
    fn region_scores_by_hand() {
        let expected: VisionExpected = serde_json::from_value(json!({
            "regions": [
                {"label": "red rectangle", "box": [80, 60, 320, 220]},
                {"label": "blue circle", "box": [550, 60, 730, 240]},
                {"label": "green triangle", "box": [140, 330, 301, 481]}
            ]
        }))
        .unwrap();
        let answer = VisionAnswer {
            regions: vec![
                // Exact.
                found("red rectangle", [80.0, 60.0, 320.0, 220.0], 0.9),
                // 3 px right: overlap 177×180 over union 183×180 → 177/183.
                found("blue circle", [553.0, 60.0, 733.0, 240.0], 0.8),
                // A wrong label is never matched.
                found("yellow star", [140.0, 330.0, 301.0, 481.0], 0.7),
            ],
            ..VisionAnswer::default()
        };
        let scores = score(&expected, &answer, &VisionCriteria::default());
        assert_eq!(scores.get("region_iou"), Some((1.0 + 177.0 / 183.0) / 3.0));
        // Only the exact rectangle is within 2 px; the circle is 3 px off.
        assert_eq!(scores.get("box_hit_rate"), Some(1.0 / 3.0));
        assert_eq!(scores.max_box_error_px, Some(3.0));
        assert_eq!(scores.get("detection_precision"), Some(2.0 / 3.0));
        assert_eq!(scores.get("detection_recall"), Some(2.0 / 3.0));
        // Per label AP at 0.5: rectangle 1, circle 1, triangle 0 → 2/3.
        assert_eq!(scores.get("detection_ap"), Some(2.0 / 3.0));
        // The circle's IoU 0.967 clears every threshold to 0.95, so each of the ten APs is
        // again (1 + 1 + 0) / 3.
        let map = scores.get("detection_map").unwrap();
        assert!((map - 2.0 / 3.0).abs() < 1e-12, "{map}");
        assert_eq!(scores.notes, vec!["no answer region matches green triangle".to_string()]);
        assert!(scores.get("mask_iou").is_none());
    }

    #[test]
    fn detection_counts_take_the_highest_score_first() {
        let expected = vec![ExpectedRegion::boxed("sheep", [0.0, 0.0, 10.0, 10.0])];
        let answer = vec![
            found("sheep", [0.0, 0.0, 10.0, 10.0], 0.4),
            // The higher score claims the only sheep, at IoU 0.81 ≥ 0.5.
            found("sheep", [0.0, 0.0, 9.0, 9.0], 0.9),
        ];
        assert_eq!(detection_counts(&expected, &answer, 0.5), (1, 2, 1));
        // AP at 0.5: the first answer is a hit at recall 1, precision 1 → 1.
        assert_eq!(average_precision(&expected, &answer, 0.5), 1.0);
        // At 0.85 the first answer (IoU 0.81) misses, then the second hits at precision 0.5.
        assert_eq!(average_precision(&expected, &answer, 0.85), 0.5);
    }

    #[test]
    fn average_precision_by_hand() {
        // Two cats; answers by score: hit, miss, hit. Curve (r, p): (0.5, 1), (0.5, 0.5),
        // (1, 2/3). Envelope: 1 for r ≤ 0.5 (51 points), 2/3 above (50 points).
        let expected = vec![
            ExpectedRegion::boxed("cat", [0.0, 0.0, 10.0, 10.0]),
            ExpectedRegion::boxed("cat", [20.0, 0.0, 30.0, 10.0]),
        ];
        let answer = vec![
            found("cat", [0.0, 0.0, 10.0, 10.0], 0.9),
            found("cat", [50.0, 50.0, 60.0, 60.0], 0.8),
            found("cat", [20.0, 0.0, 30.0, 10.0], 0.7),
        ];
        let ap = average_precision(&expected, &answer, 0.5);
        assert!((ap - (51.0 + 50.0 * 2.0 / 3.0) / 101.0).abs() < 1e-12, "{ap}");
        assert_eq!(average_precision(&[], &[], 0.5), 1.0);
        assert_eq!(average_precision(&[], &answer, 0.5), 0.0);
    }

    #[test]
    fn mask_scores_by_hand() {
        let truth = mask_from_bits(4, 4, |x, _| x < 2);
        let half = mask_from_bits(4, 4, |x, y| x < 2 && y < 2);
        let expected = VisionExpected {
            regions: vec![ExpectedRegion {
                label: Some("seal".into()),
                bbox: None,
                mask: Some(truth),
            }],
            ..VisionExpected::default()
        };
        let answer = VisionAnswer::from_tool_response(
            "vision_segment",
            &json!({"regions": [{"label": "seal", "mask": half}]}),
        )
        .unwrap();
        let scores = score(&expected, &answer, &VisionCriteria::default());
        assert_eq!(scores.get("mask_iou"), Some(0.5));
        // Boxes from the masks: [0, 0, 2, 4] and [0, 0, 2, 2] → IoU 4/8.
        assert_eq!(scores.get("region_iou"), Some(0.5));
    }

    #[test]
    fn text_fields_targets_and_counts_by_hand() {
        let expected: VisionExpected = serde_json::from_value(json!({
            "text": "INVOICE INV-4721\nTotal 58.20",
            "fields": {"invoice_number": "INV-4721", "total": "58.20", "phone": "+1 415 555 0142"},
            "targets": [
                {"label": "save", "box": [1920, 1180, 2100, 1260]},
                {"label": "cancel", "box": [1660, 1180, 1880, 1260]}
            ],
            "count": 4
        }))
        .unwrap();
        let answer = VisionAnswer {
            regions: vec![
                found("Save button", [1930.0, 1190.0, 2090.0, 1250.0], 1.0),
                found("cancel", [1500.0, 1180.0, 1600.0, 1260.0], 1.0),
            ],
            text: Some("INVOICE INV-4721 Total 58.2O".into()),
            fields: BTreeMap::from([
                ("Invoice Number".to_string(), "INV-4721".to_string()),
                ("total".to_string(), "$58.2".to_string()),
            ]),
        };
        let scores = score(&expected, &answer, &VisionCriteria::default());
        // 28 characters after collapsing; one substitution.
        assert_eq!(scores.get("ocr_char_accuracy"), Some(1.0 - 1.0 / 28.0));
        assert_eq!(scores.get("ocr_word_accuracy"), Some(1.0 - 1.0 / 4.0));
        assert_eq!(scores.get("field_exact_match"), Some(1.0 / 3.0));
        assert_eq!(scores.get("field_normalized_match"), Some(2.0 / 3.0));
        // "Save button" contains "save" and its centre (2010, 1220) is inside; cancel's
        // centre (1550, 1220) is left of its target.
        assert_eq!(scores.get("grounding_hit_rate"), Some(0.5));
        // Two regions of an expected four.
        assert_eq!(scores.get("count_accuracy"), Some(0.5));
        assert!(scores.notes.contains(&"field phone: missing".to_string()));
    }

    #[test]
    fn missing_answer_parts_score_zero() {
        let expected: VisionExpected = serde_json::from_value(json!({
            "regions": [{"label": "bear", "box": [0, 0, 10, 10]}],
            "text": "hello",
            "fields": {"a": "1"}
        }))
        .unwrap();
        let scores = score(&expected, &VisionAnswer::default(), &VisionCriteria::default());
        for name in [
            "region_iou",
            "box_hit_rate",
            "detection_recall",
            "detection_ap",
            "ocr_char_accuracy",
            "ocr_word_accuracy",
            "field_exact_match",
        ] {
            assert_eq!(scores.get(name), Some(0.0), "{name}");
        }
        // Nothing found is vacuously precise; recall carries the miss.
        assert_eq!(scores.get("detection_precision"), Some(1.0));
    }

    #[test]
    fn tool_responses_parse_in_every_form() {
        let detect = json!({
            "image": "sha256:00",
            "regions": [
                {"box": [1.0, 2.0, 3.0, 4.0], "label": "a", "score": 0.5},
                {"shape": {"kind": "box", "x0": 5, "y0": 6, "x1": 7, "y1": 8}, "label": "b"},
                {"shape": {"kind": "polygon", "points": [[1, 1], [4, 2], [2, 5]]}},
                {"shape": {"kind": "point", "x": 9, "y": 9}},
                {"label": "no geometry"}
            ],
            "calls": []
        });
        let answer = VisionAnswer::from_tool_response("vision_detect", &detect).unwrap();
        let boxes: Vec<Option<BoxPx>> = answer.regions.iter().map(|r| r.bbox).collect();
        assert_eq!(
            boxes,
            vec![
                Some([1.0, 2.0, 3.0, 4.0]),
                Some([5.0, 6.0, 7.0, 8.0]),
                Some([1.0, 1.0, 4.0, 5.0]),
                Some([9.0, 9.0, 9.0, 9.0]),
            ]
        );
        let read = json!({"blocks": [{"text": "Title"}, {"text": "Body line"}], "markdown": null});
        assert_eq!(
            VisionAnswer::from_tool_response("vision_read", &read).unwrap().text.as_deref(),
            Some("Title\nBody line")
        );
        assert!(VisionAnswer::from_tool_response("get_weather", &read).is_none());
        assert!(VisionAnswer::from_tool_response("vision_detect", &json!({"calls": []})).is_none());
        let bad_mask = json!({"regions": [{"mask": {"width": 2, "height": 2, "counts": [1]}}]});
        assert!(VisionAnswer::from_tool_response("vision_segment", &bad_mask).is_none());
    }

    #[test]
    fn later_answers_replace_earlier_parts() {
        let first = VisionAnswer {
            regions: vec![found("a", [0.0, 0.0, 1.0, 1.0], 1.0)],
            text: Some("first".into()),
            fields: BTreeMap::new(),
        };
        let second = VisionAnswer { text: Some("second".into()), ..VisionAnswer::default() };
        let merged = first.clone().merged(second);
        assert_eq!(merged, VisionAnswer { text: Some("second".into()), ..first });
    }

    #[test]
    fn answers_come_from_vision_tool_responses_and_json_final_text() {
        use adk_core::{Content, Event, FunctionResponseData, Part};

        let mut event = Event::new("inv");
        event.llm_response.content = Some(Content {
            role: "function".into(),
            parts: vec![
                Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        "vision_detect",
                        json!({"regions": [{"box": [0, 0, 2, 2], "label": "x"}]}),
                    ),
                    id: None,
                    annotations: None,
                },
                Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        "lookup",
                        json!({"regions": [{"box": [5, 5, 6, 6]}]}),
                    ),
                    id: None,
                    annotations: None,
                },
            ],
        });
        let answer = answer_from_events(&[event], Some(r#"{"total": "58.20"}"#)).unwrap();
        assert_eq!(answer.regions.len(), 1);
        assert_eq!(answer.fields["total"], "58.20");
        assert!(answer_from_events(&[], Some("plain text")).is_none());
    }

    #[test]
    fn criteria_validate_their_ranges() {
        assert!(VisionCriteria::default().validate().is_ok());
        assert!(!VisionCriteria::default().has_thresholds());
        let bad = VisionCriteria { mask_iou: Some(1.5), ..VisionCriteria::default() };
        assert!(bad.validate().unwrap_err().contains("mask_iou"));
        let bad = VisionCriteria { iou_threshold: 0.0, ..VisionCriteria::default() };
        assert!(bad.validate().unwrap_err().contains("iou_threshold"));
    }
}
