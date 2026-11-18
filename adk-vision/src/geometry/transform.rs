//! [`TransformChain`]: what happened between the source image and what a backend saw.

use serde::{Deserialize, Serialize};

use super::{BBox, Keypoint, Keypoints, Mask, Point2, Polygon, Shape};
use crate::error::{Result, VisionError};

/// One step between the source image and what a backend saw.
///
/// Serialized with an `op` tag: `{"op": "resize", "width": 1568, "height": 1018}`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "op")]
pub enum Transform {
    /// Keep the whole-pixel rectangle at `(x, y)` of `width` × `height`.
    Crop {
        /// Left edge.
        x: u32,
        /// Top edge.
        y: u32,
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
    /// Scale to `width` × `height`.
    Resize {
        /// New width.
        width: u32,
        /// New height.
        height: u32,
    },
    /// Add padding on each side.
    Pad {
        /// Pixels added on the left.
        left: u32,
        /// Pixels added on top.
        top: u32,
        /// Pixels added on the right.
        right: u32,
        /// Pixels added at the bottom.
        bottom: u32,
    },
    /// Rotate clockwise by `quarter_turns` × 90°.
    Rotate {
        /// Clockwise quarter turns, taken modulo 4.
        quarter_turns: u8,
    },
    /// Pixels changed, coordinates did not.
    Redact,
}

/// The transforms from a source image of `source_width` × `source_height` to the image a
/// backend saw, in order.
///
/// Mapping a point to the backend's image and back returns it within floating-point error;
/// the property tests hold it within 0.5 px for random chains.
///
/// # Example
///
/// ```
/// use adk_vision::{Transform, TransformChain};
///
/// // A 3024x1964 screenshot, cropped, then resized to half size.
/// let chain = TransformChain::new(3024, 1964)
///     .then(Transform::Crop { x: 1000, y: 500, width: 800, height: 600 })
///     .unwrap()
///     .then(Transform::Resize { width: 400, height: 300 })
///     .unwrap();
/// assert_eq!(chain.output_size(), (400, 300));
/// assert_eq!(chain.to_target([1200.0, 700.0]), [100.0, 100.0]);
/// assert_eq!(chain.to_source([100.0, 100.0]), [1200.0, 700.0]);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformChain {
    /// The source image's width.
    pub source_width: u32,
    /// The source image's height.
    pub source_height: u32,
    /// The steps, in the order they were applied.
    pub steps: Vec<Transform>,
}

impl TransformChain {
    /// The empty chain of a source image: the backend sees the image as it is.
    pub fn new(source_width: u32, source_height: u32) -> Self {
        Self { source_width, source_height, steps: Vec::new() }
    }

    /// Appends a step.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::InvalidInput`] for a crop outside the current image, an empty
    /// crop or a resize to zero.
    pub fn then(mut self, step: Transform) -> Result<Self> {
        self.push(step)?;
        Ok(self)
    }

    /// Appends a step in place. See [`then`](Self::then).
    ///
    /// # Errors
    ///
    /// As [`then`](Self::then).
    pub fn push(&mut self, step: Transform) -> Result<()> {
        let (w, h) = self.output_size();
        match step {
            Transform::Crop { x, y, width, height } => {
                if width == 0
                    || height == 0
                    || u64::from(x) + u64::from(width) > u64::from(w)
                    || u64::from(y) + u64::from(height) > u64::from(h)
                {
                    return Err(VisionError::InvalidInput(format!(
                        "crop {width}x{height} at ({x}, {y}) does not fit in the {w}x{h} image"
                    )));
                }
            }
            Transform::Resize { width, height } => {
                if width == 0 || height == 0 {
                    return Err(VisionError::InvalidInput(format!(
                        "resize to {width}x{height} leaves no pixels"
                    )));
                }
            }
            Transform::Pad { .. } | Transform::Rotate { .. } | Transform::Redact => {}
        }
        self.steps.push(step);
        Ok(())
    }

    /// `true` when the chain has no step that moves coordinates.
    pub fn is_identity(&self) -> bool {
        self.steps.iter().all(|s| matches!(s, Transform::Redact))
    }

    /// The size of the image after every step.
    pub fn output_size(&self) -> (u32, u32) {
        self.sizes().last().copied().unwrap_or((self.source_width, self.source_height))
    }

    /// The image size before each step, then the output size.
    fn sizes(&self) -> Vec<(u32, u32)> {
        let mut sizes = Vec::with_capacity(self.steps.len() + 1);
        let mut size = (self.source_width, self.source_height);
        sizes.push(size);
        for step in &self.steps {
            size = match *step {
                Transform::Crop { width, height, .. } => (width, height),
                Transform::Resize { width, height } => (width, height),
                Transform::Pad { left, top, right, bottom } => (
                    size.0.saturating_add(left).saturating_add(right),
                    size.1.saturating_add(top).saturating_add(bottom),
                ),
                Transform::Rotate { quarter_turns } if quarter_turns % 2 == 1 => (size.1, size.0),
                Transform::Rotate { .. } | Transform::Redact => size,
            };
            sizes.push(size);
        }
        sizes
    }

    /// Maps a point in source pixels to the backend's image.
    pub fn to_target(&self, point: [f64; 2]) -> [f64; 2] {
        let sizes = self.sizes();
        let mut p = point;
        for (step, &(w, h)) in self.steps.iter().zip(&sizes) {
            let (w, h) = (f64::from(w), f64::from(h));
            p = match *step {
                Transform::Crop { x, y, .. } => [p[0] - f64::from(x), p[1] - f64::from(y)],
                Transform::Resize { width, height } => {
                    [p[0] * f64::from(width) / w, p[1] * f64::from(height) / h]
                }
                Transform::Pad { left, top, .. } => [p[0] + f64::from(left), p[1] + f64::from(top)],
                Transform::Rotate { quarter_turns } => match quarter_turns % 4 {
                    1 => [h - p[1], p[0]],
                    2 => [w - p[0], h - p[1]],
                    3 => [p[1], w - p[0]],
                    _ => p,
                },
                Transform::Redact => p,
            };
        }
        p
    }

    /// Maps a point in the backend's image back to source pixels.
    pub fn to_source(&self, point: [f64; 2]) -> [f64; 2] {
        let sizes = self.sizes();
        let mut p = point;
        for (step, &(w, h)) in self.steps.iter().zip(&sizes).rev() {
            let (w, h) = (f64::from(w), f64::from(h));
            p = match *step {
                Transform::Crop { x, y, .. } => [p[0] + f64::from(x), p[1] + f64::from(y)],
                Transform::Resize { width, height } => {
                    [p[0] * w / f64::from(width), p[1] * h / f64::from(height)]
                }
                Transform::Pad { left, top, .. } => [p[0] - f64::from(left), p[1] - f64::from(top)],
                Transform::Rotate { quarter_turns } => match quarter_turns % 4 {
                    1 => [p[1], h - p[0]],
                    2 => [w - p[0], h - p[1]],
                    3 => [w - p[1], p[0]],
                    _ => p,
                },
                Transform::Redact => p,
            };
        }
        p
    }

    /// Maps a box in the backend's image to source pixels; a rotation keeps it axis-aligned.
    pub fn bbox_to_source(&self, bbox: &BBox) -> BBox {
        map_box(bbox, |p| self.to_source(p))
    }

    /// Maps a box in source pixels to the backend's image.
    pub fn bbox_to_target(&self, bbox: &BBox) -> BBox {
        map_box(bbox, |p| self.to_target(p))
    }

    /// Maps a shape in the backend's image to source pixels. A mask must be the size of the
    /// backend's image and comes back resampled to the source size.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::InvalidInput`] for a mask whose size is not the target size.
    pub fn shape_to_source(&self, shape: &Shape) -> Result<Shape> {
        self.map_shape(shape, |p| self.to_source(p))
    }

    /// Maps a shape in source pixels to the backend's image, resampling a mask to the target
    /// size.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::InvalidInput`] for a mask whose size is not the source size.
    pub fn shape_to_target(&self, shape: &Shape) -> Result<Shape> {
        match shape {
            Shape::Mask(mask) => {
                if (mask.width, mask.height) != (self.source_width, self.source_height) {
                    return Err(VisionError::InvalidInput(format!(
                        "a {}x{} mask is not in the {}x{} source image's pixels",
                        mask.width, mask.height, self.source_width, self.source_height
                    )));
                }
                let (tw, th) = self.output_size();
                Ok(Shape::Mask(resample(
                    mask,
                    (tw, th),
                    |p| self.to_source(p),
                    |p| self.to_target(p),
                )))
            }
            other => self.map_shape(other, |p| self.to_target(p)),
        }
    }

    fn map_shape(&self, shape: &Shape, f: impl Fn([f64; 2]) -> [f64; 2]) -> Result<Shape> {
        let pt = |x: f32, y: f32| {
            let q = f([f64::from(x), f64::from(y)]);
            [q[0] as f32, q[1] as f32]
        };
        Ok(match shape {
            Shape::Point(p) => {
                let [x, y] = pt(p.x, p.y);
                Shape::Point(Point2 { x, y })
            }
            Shape::Box(b) => Shape::Box(map_box(b, &f)),
            Shape::Polygon(poly) => Shape::Polygon(Polygon {
                points: poly.points.iter().map(|p| pt(p[0], p[1])).collect(),
            }),
            Shape::Keypoints(k) => Shape::Keypoints(Keypoints {
                skeleton: k.skeleton.clone(),
                points: k
                    .points
                    .iter()
                    .map(|kp| {
                        let [x, y] = pt(kp.x, kp.y);
                        Keypoint { x, y, ..*kp }
                    })
                    .collect(),
            }),
            Shape::Mask(mask) => {
                let target = self.output_size();
                if (mask.width, mask.height) != target {
                    return Err(VisionError::InvalidInput(format!(
                        "a {}x{} mask is not in the {}x{} image the backend saw",
                        mask.width, mask.height, target.0, target.1
                    )));
                }
                let source = (self.source_width, self.source_height);
                Shape::Mask(resample(mask, source, |p| self.to_target(p), |p| self.to_source(p)))
            }
        })
    }
}

fn map_box(bbox: &BBox, f: impl Fn([f64; 2]) -> [f64; 2]) -> BBox {
    let corners = [[bbox.x0, bbox.y0], [bbox.x1, bbox.y0], [bbox.x0, bbox.y1], [bbox.x1, bbox.y1]];
    let mapped = corners.map(|[x, y]| {
        let q = f([f64::from(x), f64::from(y)]);
        [q[0] as f32, q[1] as f32]
    });
    BBox::covering(mapped).unwrap_or(*bbox)
}

/// Resamples `mask` into a raster of `size`: a pixel is set when its centre, mapped into the
/// mask's frame with `into_mask`, lands on a set pixel. `out_of_mask` maps the mask's frame
/// to the new one, to bound the pixels visited.
pub(crate) fn resample(
    mask: &Mask,
    size: (u32, u32),
    into_mask: impl Fn([f64; 2]) -> [f64; 2],
    out_of_mask: impl Fn([f64; 2]) -> [f64; 2],
) -> Mask {
    let Some(bounds) = mask.bbox() else {
        return Mask::empty(size.0, size.1);
    };
    let region = map_box(&bounds, out_of_mask).expanded(1.0);
    let (x0, y0, w, h) = region.pixel_bounds(size.0, size.1);
    let bits = mask.decode();
    let mut out = vec![false; size.0 as usize * size.1 as usize];
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let p = into_mask([f64::from(x) + 0.5, f64::from(y) + 0.5]);
            if p[0] < 0.0 || p[1] < 0.0 {
                continue;
            }
            let (mx, my) = (p[0].floor() as u64, p[1].floor() as u64);
            if mx < u64::from(mask.width)
                && my < u64::from(mask.height)
                && bits[my as usize * mask.width as usize + mx as usize]
            {
                out[y as usize * size.0 as usize + x as usize] = true;
            }
        }
    }
    Mask::encode(size.0, size.1, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_swap_dimensions_and_invert() {
        for q in 0..4u8 {
            let chain =
                TransformChain::new(300, 200).then(Transform::Rotate { quarter_turns: q }).unwrap();
            let expected = if q % 2 == 1 { (200, 300) } else { (300, 200) };
            assert_eq!(chain.output_size(), expected);
            let p = [17.0, 43.0];
            let back = chain.to_source(chain.to_target(p));
            assert!((back[0] - p[0]).abs() < 1e-9 && (back[1] - p[1]).abs() < 1e-9, "q={q}");
        }
    }

    #[test]
    fn a_crop_must_fit() {
        let err = TransformChain::new(100, 100)
            .then(Transform::Crop { x: 50, y: 0, width: 60, height: 10 })
            .unwrap_err();
        assert!(err.to_string().contains("does not fit"));
    }

    #[test]
    fn masks_resample_to_the_source() {
        // A 4x4 block in a half-size image becomes an 8x8 block in the source.
        let chain =
            TransformChain::new(40, 40).then(Transform::Resize { width: 20, height: 20 }).unwrap();
        let mut bits = vec![false; 400];
        for y in 5..9 {
            for x in 5..9 {
                bits[y * 20 + x] = true;
            }
        }
        let sent = Mask::encode(20, 20, &bits);
        let Shape::Mask(source) = chain.shape_to_source(&Shape::Mask(sent)).unwrap() else {
            panic!("not a mask")
        };
        assert_eq!((source.width, source.height), (40, 40));
        assert_eq!(source.area(), 64);
        assert_eq!(source.bbox(), Some(BBox::new(10.0, 10.0, 18.0, 18.0)));
    }
}
