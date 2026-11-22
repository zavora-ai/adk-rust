//! Set-of-marks grounding: numbered candidates on one screenshot, and the target a chosen
//! number resolves to (spec 3.7 §7.14).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::fit::ScreenshotScale;
use crate::digest::Digest;
use crate::edit::{EditOp, Overlay, Rgba, apply_edit};
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, ScreenSpace, Shape, TransformChain};
use crate::media::DecodedImage;
use crate::traits::Word;

/// Where a candidate came from, and so how an action reaches it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum CandidateSource {
    /// An interactive element from `browser_elements`; a click on it is a `browser_click`
    /// by its selector, with no coordinates.
    BrowserElement {
        /// The element's stable CSS selector.
        selector: String,
        /// Its ARIA role or tag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
    },
    /// A word local OCR recognised on the screenshot.
    OcrWord {
        /// The reader's confidence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f32>,
    },
    /// A point a model gave, such as a coordinate from Claude's computer use toolset.
    ModelPoint {
        /// The backend that gave it.
        backend: String,
    },
}

/// One numbered candidate, its region in the screenshot's pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// The number drawn on it, from 1.
    pub mark: u32,
    /// Where it is, in screenshot pixels.
    pub region: BBox,
    /// Its text or accessible name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Where it came from.
    pub source: CandidateSource,
}

/// One interactive element as `browser_elements` returns it: a rectangle in CSS pixels of
/// the viewport and a selector that matches only it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserElement {
    /// A CSS selector matching exactly this element.
    pub selector: String,
    /// Its ARIA role or tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Its accessible name or text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its rectangle in CSS pixels of the viewport.
    pub rect: CssRect,
}

/// A rectangle in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CssRect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// The answer of `browser_elements`: the page's device pixel ratio and its elements.
///
/// # Example
///
/// ```
/// use adk_vision::screen::BrowserElements;
///
/// let page: BrowserElements = serde_json::from_value(serde_json::json!({
///     "devicePixelRatio": 2,
///     "elements": [{ "selector": "#save", "role": "button", "name": "Save",
///                    "rect": { "x": 960, "y": 590, "width": 90, "height": 40 } }]
/// })).unwrap();
/// assert_eq!(page.elements[0].selector, "#save");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserElements {
    /// Screenshot pixels per CSS pixel.
    pub device_pixel_ratio: f32,
    /// The elements, in document order.
    pub elements: Vec<BrowserElement>,
}

/// Numbered candidates on one screenshot: what `vision_annotate` draws and what a model
/// names by number.
///
/// Every region is in the screenshot's pixels; [`ScreenSpace`] maps them to the logical
/// points an action uses, so a target's click point is derived from a region of the
/// screenshot whose digest the target records (V-I14).
///
/// # Example
///
/// ```
/// use adk_vision::{BBox, Digest, ScreenSpace};
/// use adk_vision::screen::{CandidateSource, MarkSet};
///
/// let mut marks = MarkSet::new(Digest::of(b"shot"), (3024, 1964), ScreenSpace::new("built-in", [0.0, 0.0], 2.0));
/// let save = marks.push(
///     BBox::new(1920.0, 1180.0, 2100.0, 1260.0),
///     Some("Save".into()),
///     CandidateSource::OcrWord { confidence: Some(0.98) },
/// )
/// .unwrap();
/// let target = marks.ground(save).unwrap();
/// assert_eq!(target.click, [1005.0, 610.0]);
/// assert!(target.points.contains(target.click));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkSet {
    /// The screenshot the candidates are on.
    pub screenshot: Digest,
    /// Its `[width, height]` in pixels.
    pub size: [u32; 2],
    /// Its display: origin in the global point space and device pixel ratio.
    pub screen: ScreenSpace,
    /// The window the screenshot shows, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// The candidates, numbered from 1.
    pub candidates: Vec<Candidate>,
}

/// Labels compare without case or surrounding whitespace.
pub(crate) fn same_label(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.trim().eq_ignore_ascii_case(b.trim()),
        (None, None) => true,
        _ => false,
    }
}

impl MarkSet {
    /// An empty set on the screenshot with `digest` and `size`, shown on `screen`.
    pub fn new(screenshot: Digest, size: (u32, u32), screen: ScreenSpace) -> Self {
        Self { screenshot, size: [size.0, size.1], screen, window: None, candidates: Vec::new() }
    }

    /// Names the window the screenshot shows.
    #[must_use]
    pub fn with_window(mut self, window: impl Into<String>) -> Self {
        self.window = Some(window.into());
        self
    }

    /// Adds a candidate, clamped to the screenshot, and returns its mark; an empty region
    /// is skipped and returns `None`.
    pub fn push(
        &mut self,
        region: BBox,
        label: Option<String>,
        source: CandidateSource,
    ) -> Option<u32> {
        let region = region.clamped(self.size[0], self.size[1]);
        if region.is_empty() {
            return None;
        }
        let mark = u32::try_from(self.candidates.len() + 1).unwrap_or(u32::MAX);
        self.candidates.push(Candidate { mark, region, label, source });
        Some(mark)
    }

    /// Adds the elements `browser_elements` returned, their CSS rectangles scaled by the
    /// page's device pixel ratio into the screenshot's pixels; returns their marks.
    pub fn add_browser_elements(&mut self, page: &BrowserElements) -> Vec<u32> {
        let ratio = if page.device_pixel_ratio > 0.0 { page.device_pixel_ratio } else { 1.0 };
        page.elements
            .iter()
            .filter_map(|element| {
                let r = element.rect;
                let region =
                    BBox::from_xywh(r.x * ratio, r.y * ratio, r.width * ratio, r.height * ratio);
                self.push(
                    region,
                    element.name.clone(),
                    CandidateSource::BrowserElement {
                        selector: element.selector.clone(),
                        role: element.role.clone(),
                    },
                )
            })
            .collect()
    }

    /// Adds the words a local reader recognised on this screenshot; words of another image
    /// are skipped. Returns their marks.
    pub fn add_words(&mut self, words: &[Word]) -> Vec<u32> {
        let screenshot = self.screenshot;
        words
            .iter()
            .filter(|word| word.region.image == screenshot)
            .filter_map(|word| {
                let region = word.region.shape.bbox()?;
                self.push(
                    region,
                    Some(word.text.clone()),
                    CandidateSource::OcrWord { confidence: word.confidence },
                )
            })
            .collect()
    }

    /// Adds a point a model gave in the pixels of the copy it received, mapped through
    /// `scale` to this screenshot. The candidate is the smallest existing candidate that
    /// contains the point, taking its label, or a box of eight points around the point.
    /// Returns its mark.
    pub fn add_model_point(
        &mut self,
        point: [f32; 2],
        scale: &ScreenshotScale,
        backend: &str,
    ) -> Option<u32> {
        let at = scale.to_source(point);
        let containing = self
            .candidates
            .iter()
            .filter(|c| c.region.contains(at))
            .min_by(|a, b| a.region.area().total_cmp(&b.region.area()))
            .map(|c| (c.region, c.label.clone()));
        let (region, label) = containing.unwrap_or_else(|| {
            let half = 4.0 * self.ratio();
            (BBox::new(at[0] - half, at[1] - half, at[0] + half, at[1] + half), None)
        });
        self.push(region, label, CandidateSource::ModelPoint { backend: backend.to_string() })
    }

    fn ratio(&self) -> f32 {
        if self.screen.device_pixel_ratio > 0.0 { self.screen.device_pixel_ratio } else { 1.0 }
    }

    /// The candidate with `mark`.
    pub fn candidate(&self, mark: u32) -> Option<&Candidate> {
        self.candidates.iter().find(|c| c.mark == mark)
    }

    /// The candidates whose label is `label`, ignoring case and surrounding whitespace.
    pub fn by_label(&self, label: &str) -> Vec<&Candidate> {
        self.candidates.iter().filter(|c| same_label(c.label.as_deref(), Some(label))).collect()
    }

    /// What `vision_annotate` draws: each candidate's outline and its numbered badge at its
    /// top-left corner, sized by the device pixel ratio so they read the same in points.
    pub fn overlays(&self) -> Vec<Overlay> {
        let ratio = self.ratio();
        let thickness = (2.0 * ratio).round().max(1.0) as u32;
        self.candidates
            .iter()
            .flat_map(|c| {
                [
                    Overlay::Box { bbox: c.region, color: Rgba::RED, thickness },
                    Overlay::Mark {
                        at: [c.region.x0, c.region.y0],
                        number: c.mark,
                        color: Rgba::RED,
                        scale: ratio,
                    },
                ]
            })
            .collect()
    }

    /// The `vision_annotate` arguments that draw these marks: boxes in screenshot pixels,
    /// each with its mark, at the screenshot's device pixel ratio.
    pub fn annotate_args(&self) -> Value {
        let boxes: Vec<Value> = self
            .candidates
            .iter()
            .map(|c| {
                json!({
                    "box": [c.region.x0, c.region.y0, c.region.x1, c.region.y1],
                    "label": c.label,
                    "mark": c.mark,
                })
            })
            .collect();
        json!({ "image": self.screenshot, "boxes": boxes, "devicePixelRatio": self.ratio() })
    }

    /// Draws the marks on the screenshot's pixels.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when `pixels` is not the screenshot's size.
    pub fn annotate(&self, pixels: &DecodedImage) -> Result<DecodedImage> {
        if pixels.size() != (self.size[0], self.size[1]) {
            return Err(VisionError::InvalidInput(format!(
                "the marks are for a {}x{} screenshot, not {}x{}",
                self.size[0],
                self.size[1],
                pixels.width(),
                pixels.height()
            )));
        }
        let chain = TransformChain::new(self.size[0], self.size[1]);
        apply_edit(pixels, &chain, &EditOp::Annotate { overlays: self.overlays() })
    }

    /// The target `mark` names: its region in pixels and points and its click point.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] naming the marks there are when `mark` is not one.
    pub fn ground(&self, mark: u32) -> Result<GroundedTarget> {
        let candidate = self.candidate(mark).ok_or_else(|| {
            VisionError::InvalidInput(format!(
                "mark {mark} is not on this screenshot; its marks are 1 to {}",
                self.candidates.len()
            ))
        })?;
        Ok(self.target_of(candidate))
    }

    pub(crate) fn target_of(&self, candidate: &Candidate) -> GroundedTarget {
        let points = self.screen.bbox_to_points(&candidate.region);
        GroundedTarget {
            mark: candidate.mark,
            screenshot: self.screenshot,
            screenshot_size: self.size,
            screen: self.screen.clone(),
            window: self.window.clone(),
            pixels: candidate.region,
            points,
            click: self.screen.to_points(candidate.region.center()),
            label: candidate.label.clone(),
            source: candidate.source.clone(),
        }
    }
}

/// A chosen mark resolved for an action: the region in screenshot pixels and logical
/// points, the click point in points, and the screenshot it came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundedTarget {
    /// The mark chosen.
    pub mark: u32,
    /// The screenshot's digest, which the action records (V-I14).
    pub screenshot: Digest,
    /// The screenshot's `[width, height]` in pixels.
    pub screenshot_size: [u32; 2],
    /// The display the screenshot shows.
    pub screen: ScreenSpace,
    /// The window, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// The region in screenshot pixels.
    pub pixels: BBox,
    /// The region in logical points.
    pub points: BBox,
    /// The click point, the region's centre, in logical points.
    pub click: [f32; 2],
    /// The candidate's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Where the candidate came from.
    pub source: CandidateSource,
}

impl GroundedTarget {
    /// The click point in screenshot pixels.
    pub fn click_pixel(&self) -> [f32; 2] {
        self.screen.to_pixels(self.click)
    }

    /// `browser_click` arguments for a browser element: its selector, no coordinates.
    /// `None` for a desktop candidate.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{Digest, ScreenSpace};
    /// use adk_vision::screen::{BrowserElements, MarkSet};
    ///
    /// let page: BrowserElements = serde_json::from_value(serde_json::json!({
    ///     "devicePixelRatio": 2,
    ///     "elements": [{ "selector": "#save", "name": "Save",
    ///                    "rect": { "x": 960, "y": 590, "width": 90, "height": 40 } }]
    /// })).unwrap();
    /// let mut marks = MarkSet::new(Digest::of(b"page"), (3024, 1964), ScreenSpace::new("viewport", [0.0, 0.0], 2.0));
    /// let mark = marks.add_browser_elements(&page)[0];
    /// let target = marks.ground(mark).unwrap();
    /// assert_eq!(target.browser_click_args().unwrap(), serde_json::json!({ "selector": "#save" }));
    /// ```
    pub fn browser_click_args(&self) -> Option<Value> {
        match &self.source {
            CandidateSource::BrowserElement { selector, .. } => {
                Some(json!({ "selector": selector }))
            }
            CandidateSource::OcrWord { .. } | CandidateSource::ModelPoint { .. } => None,
        }
    }

    /// The click point as a whole pixel of the copy a model received under `scale`, for a
    /// `computer.left_click` on a fitted screenshot.
    pub fn toolset_coordinate(&self, scale: &ScreenshotScale) -> [i64; 2] {
        let sent = scale.to_sent(self.click_pixel());
        let clamp = |v: f32, max: u32| (v.floor() as i64).clamp(0, i64::from(max) - 1);
        [clamp(sent[0], scale.sent[0]), clamp(sent[1], scale.sent[1])]
    }

    /// The target as a region of its screenshot, for evidence and the annotated viewer.
    pub fn shape(&self) -> Shape {
        Shape::Box(self.pixels)
    }

    /// `adk-computer-use` target evidence for this target: bounds in logical points at the
    /// screenshot's device pixel ratio and the screenshot's digest (spec 3.7 V-I14).
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{BBox, Digest, ScreenSpace};
    /// use adk_vision::screen::{CandidateSource, MarkSet};
    ///
    /// let mut marks = MarkSet::new(Digest::of(b"shot"), (3024, 1964), ScreenSpace::new("built-in", [0.0, 0.0], 2.0));
    /// let mark = marks
    ///     .push(BBox::new(1920.0, 1180.0, 2100.0, 1260.0), Some("Save".into()), CandidateSource::OcrWord { confidence: None })
    ///     .unwrap();
    /// let evidence = marks.ground(mark).unwrap().target_evidence(
    ///     "darwin", "com.example.form", "observation-1", "2026-11-22T10:00:00Z",
    /// );
    /// let bounds = evidence.bounds.as_ref().unwrap();
    /// assert_eq!((bounds.x, bounds.y, bounds.width, bounds.height), (960.0, 590.0, 90.0, 40.0));
    /// assert!(evidence.check_screen_binding().is_ok());
    /// ```
    #[cfg(feature = "computer-use")]
    pub fn target_evidence(
        &self,
        platform: &str,
        app_id: &str,
        observation_id: &str,
        captured_at: &str,
    ) -> adk_computer_use::TargetEvidence {
        let ratio = f64::from(self.screen.device_pixel_ratio);
        let p = &self.points;
        adk_computer_use::TargetEvidence {
            platform: platform.to_string(),
            app_id: app_id.to_string(),
            pid: None,
            window_id: self.window.clone().map(Value::String),
            window_title_digest: None,
            display_id: Some(self.screen.display.clone()),
            role: match &self.source {
                CandidateSource::BrowserElement { role, .. } => role.clone(),
                CandidateSource::OcrWord { .. } | CandidateSource::ModelPoint { .. } => None,
            },
            label_digest: self
                .label
                .as_deref()
                .map(|label| Digest::of(label.as_bytes()).to_string()),
            bounds: Some(adk_computer_use::Bounds::points(
                f64::from(p.x0),
                f64::from(p.y0),
                f64::from(p.width()),
                f64::from(p.height()),
                ratio,
            )),
            observation_id: observation_id.to_string(),
            screenshot_hash: Some(self.screenshot.to_string()),
            device_pixel_ratio: Some(ratio),
            ui_tree_revision: None,
            confidence: match &self.source {
                CandidateSource::OcrWord { confidence: Some(c) } => f64::from(*c),
                CandidateSource::BrowserElement { .. }
                | CandidateSource::OcrWord { confidence: None }
                | CandidateSource::ModelPoint { .. } => 1.0,
            },
            captured_at: captured_at.to_string(),
        }
    }
}
