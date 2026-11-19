//! [`RegionNormalizer`]: a backend's [`RawRegion`]s to [`Region`]s in source pixels.

use super::transform::resample;
use super::{
    BBox, CoordFrame, Keypoint, Keypoints, Point2, Polygon, RawRegion, Region, RegionOrigin, Shape,
    TransformChain,
};
use crate::backend::BackendId;
use crate::digest::Digest;
use crate::error::{Result, VisionError};

/// Turns a backend's answers into regions of the source image.
///
/// A raw shape is mapped from the backend's [`CoordFrame`] to the pixels of the image it was
/// sent, back through the [`TransformChain`] to source pixels, and clamped to the source
/// image. A box or mask left with no area is dropped.
///
/// # Example
///
/// ```
/// use adk_vision::{
///     AxisOrder, BBox, BackendId, CoordFrame, Digest, RawRegion, RegionNormalizer, Shape,
///     Transform, TransformChain,
/// };
///
/// // The source was halved before it was sent; Gemini answered on 0-1000.
/// let chain = TransformChain::new(2000, 1000)
///     .then(Transform::Resize { width: 1000, height: 500 })
///     .unwrap();
/// let frame = CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx };
/// let normalizer =
///     RegionNormalizer::new(Digest::of(b"source"), chain, frame, BackendId::new("gemini"));
/// let raw = RawRegion::boxed(frame.box_from_raw([100.0, 250.0, 500.0, 750.0]), "cat", 0.9);
/// let region = normalizer.region(raw).unwrap().unwrap();
/// assert_eq!(region.shape, Shape::Box(BBox::new(500.0, 100.0, 1500.0, 500.0)));
/// ```
#[derive(Clone, Debug)]
pub struct RegionNormalizer {
    image: Digest,
    chain: TransformChain,
    frame: CoordFrame,
    backend: BackendId,
    manifest: Option<String>,
}

impl RegionNormalizer {
    /// A normalizer for answers about the source image `image`, seen through `chain`, given
    /// in `frame` by `backend`.
    pub fn new(
        image: Digest,
        chain: TransformChain,
        frame: CoordFrame,
        backend: BackendId,
    ) -> Self {
        Self { image, chain, frame, backend, manifest: None }
    }

    /// Names the model manifest entry the backend ran.
    pub fn with_manifest(mut self, manifest: Option<String>) -> Self {
        self.manifest = manifest;
        self
    }

    /// The transform chain.
    pub fn chain(&self) -> &TransformChain {
        &self.chain
    }

    /// The origin every region from this normalizer carries.
    pub fn origin(&self) -> RegionOrigin {
        RegionOrigin {
            backend: self.backend.clone(),
            manifest: self.manifest.clone(),
            frame: self.frame,
            chain: self.chain.clone(),
            escalated_from: None,
        }
    }

    /// Maps a shape from the backend's frame to source pixels, clamped to the source image.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Parse`] for a mask in a frame with no raster, or of the wrong
    /// size, and for a non-finite coordinate.
    pub fn shape(&self, raw: &Shape) -> Result<Shape> {
        let sent = self.chain.output_size();
        let parse =
            |message: String| VisionError::Parse { backend: self.backend.to_string(), message };
        let to_sent = |x: f32, y: f32| -> Result<[f32; 2]> {
            if !x.is_finite() || !y.is_finite() {
                return Err(parse(format!("coordinate ({x}, {y}) is not finite")));
            }
            let p = self.frame.point_to_sent([f64::from(x), f64::from(y)], sent);
            Ok([p[0] as f32, p[1] as f32])
        };
        let in_sent = match raw {
            Shape::Point(p) => {
                let [x, y] = to_sent(p.x, p.y)?;
                Shape::Point(Point2 { x, y })
            }
            Shape::Box(b) => {
                let a = to_sent(b.x0, b.y0)?;
                let c = to_sent(b.x1, b.y1)?;
                Shape::Box(BBox::new(a[0], a[1], c[0], c[1]))
            }
            Shape::Polygon(poly) => Shape::Polygon(Polygon {
                points: poly.points.iter().map(|p| to_sent(p[0], p[1])).collect::<Result<_>>()?,
            }),
            Shape::Keypoints(k) => Shape::Keypoints(Keypoints {
                skeleton: k.skeleton.clone(),
                points: k
                    .points
                    .iter()
                    .map(|kp| {
                        let [x, y] = to_sent(kp.x, kp.y)?;
                        Ok(Keypoint { x, y, ..*kp })
                    })
                    .collect::<Result<_>>()?,
            }),
            Shape::Mask(mask) => {
                let Some(raster) = self.frame.mask_raster(sent) else {
                    return Err(parse(format!(
                        "a mask cannot be given in the {:?} frame; convert it to sent pixels first",
                        self.frame
                    )));
                };
                if (mask.width, mask.height) != raster {
                    return Err(parse(format!(
                        "a {}x{} mask does not match the {}x{} raster of its frame",
                        mask.width, mask.height, raster.0, raster.1
                    )));
                }
                match self.frame {
                    CoordFrame::SentPixels => Shape::Mask(mask.clone()),
                    frame => Shape::Mask(resample(
                        mask,
                        sent,
                        |p| frame.point_from_sent(p, sent),
                        |p| frame.point_to_sent(p, sent),
                    )),
                }
            }
        };
        let source = self.chain.shape_to_source(&in_sent)?;
        Ok(clamp(source, self.chain.source_width, self.chain.source_height))
    }

    /// Turns one raw region into a region of the source image, or `None` when nothing of it
    /// lies inside the image.
    ///
    /// # Errors
    ///
    /// As [`shape`](Self::shape).
    pub fn region(&self, raw: RawRegion) -> Result<Option<Region>> {
        let shape = self.shape(&raw.shape)?;
        let empty = match &shape {
            Shape::Box(b) => b.is_empty(),
            Shape::Mask(m) => m.area() == 0,
            _ => false,
        };
        if empty {
            return Ok(None);
        }
        Ok(Some(Region {
            image: self.image,
            shape,
            label: raw.label,
            score: raw.score,
            track: raw.track,
            origin: self.origin(),
        }))
    }

    /// Turns raw regions into regions of the source image, dropping those outside it.
    ///
    /// # Errors
    ///
    /// As [`shape`](Self::shape).
    pub fn regions(&self, raw: Vec<RawRegion>) -> Result<Vec<Region>> {
        let mut regions = Vec::with_capacity(raw.len());
        for r in raw {
            if let Some(region) = self.region(r)? {
                regions.push(region);
            }
        }
        Ok(regions)
    }
}

fn clamp(shape: Shape, width: u32, height: u32) -> Shape {
    let (w, h) = (width as f32, height as f32);
    let pt = |x: f32, y: f32| [x.clamp(0.0, w), y.clamp(0.0, h)];
    match shape {
        Shape::Point(p) => {
            let [x, y] = pt(p.x, p.y);
            Shape::Point(Point2 { x, y })
        }
        Shape::Box(b) => Shape::Box(b.clamped(width, height)),
        Shape::Polygon(poly) => {
            Shape::Polygon(Polygon { points: poly.points.iter().map(|p| pt(p[0], p[1])).collect() })
        }
        Shape::Keypoints(k) => Shape::Keypoints(Keypoints {
            skeleton: k.skeleton,
            points: k
                .points
                .into_iter()
                .map(|kp| {
                    let [x, y] = pt(kp.x, kp.y);
                    Keypoint { x, y, ..kp }
                })
                .collect(),
        }),
        mask @ Shape::Mask(_) => mask,
    }
}
