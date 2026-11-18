//! Geometry and regions (spec 3.7 §7.3).
//!
//! Every [`Region`] is in source-image pixels — origin top-left, x right, y down — and
//! carries its [`RegionOrigin`]: the backend, the [`CoordFrame`] it answered in, and the
//! [`TransformChain`] between the source image and what the backend saw. Backends answer
//! with [`RawRegion`]s in their own frame; only the runtime turns them into `Region`s
//! ([`RegionNormalizer`]), so no region crosses a component boundary in a provider's frame
//! (V-I13).

mod frame;
mod mask;
mod normalize;
mod screen;
mod transform;

use serde::{Deserialize, Serialize};

pub use frame::{AxisOrder, CoordFrame, Letterbox, LetterboxGeometry};
pub use mask::Mask;
pub use normalize::RegionNormalizer;
pub use screen::ScreenSpace;
pub(crate) use transform::resample;
pub use transform::{Transform, TransformChain};

use crate::backend::BackendId;
use crate::digest::Digest;

/// An axis-aligned box: origin top-left, x right, y down; `x1` and `y1` exclusive.
///
/// # Example
///
/// ```
/// use adk_vision::BBox;
///
/// let a = BBox::new(0.0, 0.0, 10.0, 10.0);
/// let b = BBox::new(5.0, 0.0, 15.0, 10.0);
/// assert_eq!(a.area(), 100.0);
/// assert!((a.iou(&b) - 1.0 / 3.0).abs() < 1e-6);
/// assert_eq!(BBox::new(-5.0, 2.0, 50.0, 8.0).clamped(20, 20), BBox::new(0.0, 2.0, 20.0, 8.0));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    /// Left edge.
    pub x0: f32,
    /// Top edge.
    pub y0: f32,
    /// Right edge, exclusive.
    pub x1: f32,
    /// Bottom edge, exclusive.
    pub y1: f32,
}

impl BBox {
    /// A box from its edges; the edges are reordered so `x0 <= x1` and `y0 <= y1`.
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) }
    }

    /// A box from its top-left corner and size.
    pub fn from_xywh(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self::new(x, y, x + width, y + height)
    }

    /// Width, zero for an inverted box.
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    /// Height, zero for an inverted box.
    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    /// Area in square pixels.
    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }

    /// The centre point.
    pub fn center(&self) -> [f32; 2] {
        [(self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0]
    }

    /// `true` when the box has no area.
    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }

    /// The overlap of two boxes, if any.
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let b = Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (!b.is_empty()).then_some(b)
    }

    /// Intersection over union, `0.0` for disjoint boxes.
    pub fn iou(&self, other: &Self) -> f32 {
        let inter = self.intersection(other).map_or(0.0, |b| b.area());
        let union = self.area() + other.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    /// The box clamped to an image of `width` × `height`.
    pub fn clamped(&self, width: u32, height: u32) -> Self {
        let (w, h) = (width as f32, height as f32);
        Self {
            x0: self.x0.clamp(0.0, w),
            y0: self.y0.clamp(0.0, h),
            x1: self.x1.clamp(0.0, w),
            y1: self.y1.clamp(0.0, h),
        }
    }

    /// The box grown by `margin` pixels on every side.
    pub fn expanded(&self, margin: f32) -> Self {
        Self::new(self.x0 - margin, self.y0 - margin, self.x1 + margin, self.y1 + margin)
    }

    /// `true` when the point lies inside the box.
    pub fn contains(&self, point: [f32; 2]) -> bool {
        point[0] >= self.x0 && point[0] < self.x1 && point[1] >= self.y0 && point[1] < self.y1
    }

    /// `true` when the box lies within an image of `width` × `height`.
    pub fn is_within(&self, width: u32, height: u32) -> bool {
        self.x0 >= 0.0
            && self.y0 >= 0.0
            && self.x1 <= width as f32
            && self.y1 <= height as f32
            && self.x0 <= self.x1
            && self.y0 <= self.y1
    }

    /// The smallest whole-pixel rectangle covering the box, clamped to the image:
    /// `(x, y, width, height)`.
    pub fn pixel_bounds(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let c = self.clamped(width, height);
        let x0 = c.x0.floor() as u32;
        let y0 = c.y0.floor() as u32;
        let x1 = (c.x1.ceil() as u32).min(width);
        let y1 = (c.y1.ceil() as u32).min(height);
        (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    /// The smallest box covering every point, or `None` for no points.
    pub fn covering(points: impl IntoIterator<Item = [f32; 2]>) -> Option<Self> {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut b = Self { x0: first[0], y0: first[1], x1: first[0], y1: first[1] };
        for [x, y] in iter {
            b.x0 = b.x0.min(x);
            b.y0 = b.y0.min(y);
            b.x1 = b.x1.max(x);
            b.y1 = b.y1.max(y);
        }
        Some(b)
    }
}

/// A point in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point2 {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
}

/// A closed polygon, vertices in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Polygon {
    /// The vertices as `[x, y]`.
    pub points: Vec<[f32; 2]>,
}

/// One keypoint of a skeleton.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keypoint {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
    /// Whether the point is visible.
    pub visible: bool,
    /// The model's confidence, when it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

/// Keypoints of a named skeleton, such as `coco-17`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keypoints {
    /// The skeleton the points follow.
    pub skeleton: String,
    /// The points, in the skeleton's order.
    pub points: Vec<Keypoint>,
}

/// The geometry of something found in an image.
///
/// Serialized with a `kind` tag: `{"kind": "box", "x0": 1, ...}`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Shape {
    /// A single point.
    Point(Point2),
    /// An axis-aligned box.
    Box(BBox),
    /// A polygon.
    Polygon(Polygon),
    /// A binary mask at the resolution of the frame it is in.
    Mask(Mask),
    /// Keypoints of a skeleton.
    Keypoints(Keypoints),
}

impl Shape {
    /// The smallest box covering the shape, or `None` for an empty one.
    pub fn bbox(&self) -> Option<BBox> {
        match self {
            Self::Point(p) => Some(BBox { x0: p.x, y0: p.y, x1: p.x, y1: p.y }),
            Self::Box(b) => Some(*b),
            Self::Polygon(p) => BBox::covering(p.points.iter().copied()),
            Self::Mask(m) => m.bbox(),
            Self::Keypoints(k) => BBox::covering(k.points.iter().map(|p| [p.x, p.y])),
        }
    }

    /// `true` when every coordinate lies within an image of `width` × `height`.
    pub fn is_within(&self, width: u32, height: u32) -> bool {
        let inside = |[x, y]: [f32; 2]| {
            (0.0..=width as f32).contains(&x) && (0.0..=height as f32).contains(&y)
        };
        match self {
            Self::Point(p) => inside([p.x, p.y]),
            Self::Box(b) => b.is_within(width, height),
            Self::Polygon(p) => p.points.iter().copied().all(inside),
            Self::Mask(m) => m.width == width && m.height == height,
            Self::Keypoints(k) => k.points.iter().all(|p| inside([p.x, p.y])),
        }
    }
}

/// An anonymous track identifier; identifiers reset with their source session (V-I8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackId(pub u64);

/// Why a region or value came from an escalation backend.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum EscalationReason {
    /// The primary's score was below the route's threshold.
    LowConfidence {
        /// The threshold.
        threshold: f32,
    },
    /// The primary found nothing.
    NoResult,
    /// A second local backend disagreed with the primary.
    Disagreement {
        /// The backend that disagreed.
        with: BackendId,
    },
}

/// The primary backend's answer an escalation replaced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalatedFrom {
    /// The primary backend.
    pub backend: BackendId,
    /// Why the runtime escalated.
    pub reason: EscalationReason,
    /// The primary's label, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The primary's score, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// The primary's text, for document blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Where a region came from: backend, model, the frame it answered in, the transforms
/// between the source image and what the backend saw, and any escalation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionOrigin {
    /// The backend that produced the region.
    pub backend: BackendId,
    /// The model manifest entry, for model-backed backends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// The coordinate frame the backend answered in.
    pub frame: CoordFrame,
    /// The transforms from the source image to the image the backend saw.
    pub chain: TransformChain,
    /// The primary answer this region replaced, when it came from an escalation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated_from: Option<EscalatedFrom>,
}

/// Something found in one image, always in that image's pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    /// The source image this region belongs to.
    pub image: Digest,
    /// The geometry, in source pixels.
    pub shape: Shape,
    /// The class or concept, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The backend's confidence in `[0, 1]`, when it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// The track this region belongs to, across frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackId>,
    /// Backend, model manifest entry, frame and transform chain.
    pub origin: RegionOrigin,
}

/// What a backend answers: a shape in its descriptor's [`CoordFrame`], with x horizontal
/// and y vertical whatever the provider's wire order.
///
/// The runtime turns every `RawRegion` into a [`Region`] in source pixels
/// ([`RegionNormalizer`]); a raw region never leaves the call that produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRegion {
    /// The geometry, in the backend's frame. A mask's size is the frame's raster: the sent
    /// image for `SentPixels`, the model input for `ModelInput`.
    pub shape: Shape,
    /// The class or concept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The backend's confidence in `[0, 1]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// A track identifier the backend assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackId>,
}

impl RawRegion {
    /// A box with a label and score.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{BBox, RawRegion};
    ///
    /// let raw = RawRegion::boxed(BBox::new(1.0, 2.0, 3.0, 4.0), "seal", 0.9);
    /// assert_eq!(raw.label.as_deref(), Some("seal"));
    /// ```
    pub fn boxed(bbox: BBox, label: impl Into<String>, score: f32) -> Self {
        Self { shape: Shape::Box(bbox), label: Some(label.into()), score: Some(score), track: None }
    }

    /// A region with this shape and nothing else.
    pub fn shape(shape: Shape) -> Self {
        Self { shape, label: None, score: None, track: None }
    }
}

/// The wire form of an image's regions: the image's digest and dimensions, and its regions.
///
/// Published as a JSON Schema in `adk-vision/schema/region-set.json`; approval evidence,
/// eval cases and surfaces use it, and `adk-surface` draws it without depending on this
/// crate.
///
/// # Example
///
/// ```
/// use adk_vision::{Digest, RegionSet};
///
/// let set = RegionSet::new(Digest::of(b"png bytes"), 640, 480, Vec::new());
/// let json = serde_json::to_value(&set).unwrap();
/// assert_eq!(json["width"], 640);
/// assert!(set.validate().is_ok());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionSet {
    /// The source image's digest.
    pub image: Digest,
    /// The source image's width in pixels.
    pub width: u32,
    /// The source image's height in pixels.
    pub height: u32,
    /// The regions, each in the source image's pixels.
    pub regions: Vec<Region>,
}

impl RegionSet {
    /// A region set for an image.
    pub fn new(image: Digest, width: u32, height: u32, regions: Vec<Region>) -> Self {
        Self { image, width, height, regions }
    }

    /// Checks that every region belongs to the image and lies within it.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::InvalidInput`](crate::VisionError::InvalidInput) naming the
    /// first region that does not.
    pub fn validate(&self) -> crate::Result<()> {
        for (index, region) in self.regions.iter().enumerate() {
            if region.image != self.image {
                return Err(crate::VisionError::InvalidInput(format!(
                    "region {index} belongs to image {}, not {}",
                    region.image, self.image
                )));
            }
            if !region.shape.is_within(self.width, self.height) {
                return Err(crate::VisionError::InvalidInput(format!(
                    "region {index} lies outside the {}x{} image",
                    self.width, self.height
                )));
            }
        }
        Ok(())
    }
}

/// One object across frames. Identifiers are anonymous and reset with the source session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// The track's identifier.
    pub id: TrackId,
    /// The object's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `(frame index, region)` pairs, in frame order.
    pub observations: Vec<(u64, Region)>,
}
