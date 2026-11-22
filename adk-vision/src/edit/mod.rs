//! Image edits: [`EditOp`] and the [`DeterministicEditor`] (spec 3.7 §7.5).
//!
//! The deterministic editor crops, resizes, blurs, pixelates, redacts and draws overlays on
//! the `image` crate, with no model: the same image and operation always give the same
//! pixels. Coordinates in an operation are source pixels; the editor maps them through the
//! image's transform chain.

mod draw;
mod editor;
pub(crate) mod ops;

use serde::{Deserialize, Serialize};

pub use editor::DeterministicEditor;

use crate::error::{Result, VisionError};
use crate::geometry::{BBox, Mask, Polygon, TransformChain};
use crate::media::DecodedImage;

/// An RGBA colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    /// Opaque black, the fill for redaction.
    pub const BLACK: Self = Self([0, 0, 0, 255]);
    /// Opaque white.
    pub const WHITE: Self = Self([255, 255, 255, 255]);
    /// Opaque red.
    pub const RED: Self = Self([230, 25, 75, 255]);
    /// Opaque green.
    pub const GREEN: Self = Self([60, 180, 75, 255]);
    /// Opaque blue.
    pub const BLUE: Self = Self([0, 130, 200, 255]);
}

/// Something drawn over an image.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Overlay {
    /// A box outline.
    Box {
        /// The box.
        bbox: BBox,
        /// Line colour.
        color: Rgba,
        /// Line thickness in pixels.
        thickness: u32,
    },
    /// A polygon outline.
    Polygon {
        /// The polygon.
        polygon: Polygon,
        /// Line colour.
        color: Rgba,
        /// Line thickness in pixels.
        thickness: u32,
    },
    /// A mask tinted over the image; the colour's alpha is the tint's opacity.
    Mask {
        /// The mask, at source resolution.
        mask: Mask,
        /// Tint colour.
        color: Rgba,
    },
    /// A filled dot.
    Point {
        /// `[x, y]`.
        at: [f32; 2],
        /// Dot colour.
        color: Rgba,
        /// Radius in pixels.
        radius: u32,
    },
    /// A numbered mark for set-of-marks prompting: a filled badge with the number in it.
    Mark {
        /// The badge's centre, `[x, y]`.
        at: [f32; 2],
        /// The number drawn.
        number: u32,
        /// Badge colour; the digits are white.
        color: Rgba,
        /// Badge size relative to the base size: the device pixel ratio when the marks are
        /// drawn on a screenshot of a high-density display, so they look the same size in
        /// points. 1 when unstated.
        #[serde(default = "unit_scale", skip_serializing_if = "is_unit_scale")]
        scale: f32,
    },
}

fn unit_scale() -> f32 {
    1.0
}

fn is_unit_scale(scale: &f32) -> bool {
    (*scale - 1.0).abs() < f32::EPSILON
}

/// One edit, with coordinates in source pixels.
///
/// Serialized with an `op` tag; its canonical JSON digest names the operation in
/// `Provenance::Derived`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "op")]
pub enum EditOp {
    /// Keep the pixels inside `bbox`.
    Crop {
        /// The box to keep.
        bbox: BBox,
    },
    /// Scale to `width` × `height`.
    Resize {
        /// New width.
        width: u32,
        /// New height.
        height: u32,
    },
    /// Fill regions with solid colour: the redaction that cannot be reversed.
    Redact {
        /// The regions.
        regions: Vec<BBox>,
        /// Fill colour.
        color: Rgba,
    },
    /// Gaussian-blur regions. Not a redaction for text, which blur does not hide reliably.
    Blur {
        /// The regions.
        regions: Vec<BBox>,
        /// The blur's standard deviation in pixels.
        sigma: f32,
    },
    /// Pixelate regions into square blocks.
    Pixelate {
        /// The regions.
        regions: Vec<BBox>,
        /// Block size in pixels.
        block: u32,
    },
    /// Draw overlays.
    Annotate {
        /// The overlays, drawn in order.
        overlays: Vec<Overlay>,
    },
    /// A generative edit, which only a model-backed editor performs.
    Generative {
        /// What to change.
        prompt: String,
    },
}

impl EditOp {
    /// The operation's identifier: the digest of its canonical JSON form.
    pub fn digest(&self) -> crate::Digest {
        crate::Digest::of_json(self)
    }
}

/// Applies `op` to `pixels`, whose transform chain from the source is `chain`. The
/// operation's coordinates are source pixels.
///
/// # Errors
///
/// [`VisionError::Unsupported`] for a generative edit, [`VisionError::InvalidInput`] for a
/// crop with no pixels, a resize to zero or a mask that is not at source resolution.
///
/// # Example
///
/// ```
/// use adk_vision::{BBox, DecodedImage, EditOp, Rgba, TransformChain, apply_edit};
///
/// let image = DecodedImage::filled(10, 10, [255, 255, 255, 255]);
/// let op = EditOp::Redact { regions: vec![BBox::new(2.0, 2.0, 4.0, 4.0)], color: Rgba::BLACK };
/// let out = apply_edit(&image, &TransformChain::new(10, 10), &op).unwrap();
/// assert_eq!(out.pixel(3, 3), [0, 0, 0, 255]);
/// assert_eq!(out.pixel(5, 5), [255, 255, 255, 255]);
/// ```
pub fn apply_edit(
    pixels: &DecodedImage,
    chain: &TransformChain,
    op: &EditOp,
) -> Result<DecodedImage> {
    let to_target = |b: &BBox| chain.bbox_to_target(b);
    match op {
        EditOp::Crop { bbox } => {
            let (w, h) = pixels.size();
            let (x, y, cw, ch) = to_target(bbox).pixel_bounds(w, h);
            if cw == 0 || ch == 0 {
                return Err(VisionError::InvalidInput(format!(
                    "crop {bbox:?} leaves no pixels of the {w}x{h} image"
                )));
            }
            Ok(ops::crop(pixels, x, y, cw, ch))
        }
        EditOp::Resize { width, height } => {
            if *width == 0 || *height == 0 {
                return Err(VisionError::InvalidInput(format!(
                    "resize to {width}x{height} leaves no pixels"
                )));
            }
            Ok(ops::resize(pixels, *width, *height))
        }
        EditOp::Redact { regions, color } => {
            let mut out = pixels.clone();
            for region in regions {
                ops::fill(&mut out, &to_target(region), *color);
            }
            Ok(out)
        }
        EditOp::Blur { regions, sigma } => {
            let mut out = pixels.clone();
            for region in regions {
                ops::blur(&mut out, &to_target(region), *sigma);
            }
            Ok(out)
        }
        EditOp::Pixelate { regions, block } => {
            let mut out = pixels.clone();
            for region in regions {
                ops::pixelate(&mut out, &to_target(region), (*block).max(1));
            }
            Ok(out)
        }
        EditOp::Annotate { overlays } => {
            let mut out = pixels.clone();
            for overlay in overlays {
                draw::overlay(&mut out, chain, overlay)?;
            }
            Ok(out)
        }
        EditOp::Generative { .. } => Err(VisionError::Unsupported(
            "generative edits need a model-backed editor; the deterministic editor crops, \
             resizes, redacts, blurs, pixelates and annotates"
                .into(),
        )),
    }
}
