//! Calibration and measurement in one plane (spec 3.7 §7.13).
//!
//! A fiducial of known size lying in the measured plane gives the image's scale in pixels per
//! millimetre ([`Calibration::from_fiducial`]); [`Calibration::measure`] then reports a
//! region's maximum length (its maximum Feret diameter) and its area in millimetres, each
//! with an uncertainty propagated from the calibration's.
//!
//! A single image holds no depth, so a measurement is valid only for an object lying in the
//! fiducial's plane, seen by the same camera at the same image size ([`MEASUREMENT_VALIDITY`]).
//! Depth arrives with the spatial outputs of phase D.
//!
//! # Uncertainty
//!
//! Each figure is one standard uncertainty, from these sources added in quadrature:
//!
//! | Quantity | Sources |
//! |----------|---------|
//! | Scale `k` (px/mm) | Each fiducial side located within ±1 px; the fiducial's printed tolerance; the difference between the scales its two sides give, which perspective and a tilted plane produce |
//! | Maximum length | Each end located within half a pixel (±1 px over the length), and the scale's uncertainty |
//! | Area | Half a pixel along the whole boundary, and twice the scale's relative uncertainty |
//!
//! # Example
//!
//! ```
//! use adk_vision::{BBox, Calibration, FiducialSpec, Mask, Shape};
//!
//! // A 20 mm fiducial drawn 150 px wide at 7.5 px/mm in a 1600x1000 frame.
//! let fiducial = Shape::Mask(Mask::from_box(1600, 1000, &BBox::new(100.0, 100.0, 250.0, 250.0)));
//! let calibration = Calibration::from_fiducial(&fiducial, FiducialSpec::square(20.0), (1600, 1000)).unwrap();
//! assert!((calibration.px_per_mm - 7.5).abs() < 1e-9);
//!
//! // A 3 px wide scratch 300 px long: 40 mm.
//! let scratch = Shape::Mask(Mask::from_box(1600, 1000, &BBox::new(400.0, 500.0, 700.0, 503.0)));
//! let measured = calibration.measure(&scratch, (1600, 1000)).unwrap();
//! assert!((measured.longest_mm - 40.0).abs() < 0.05);
//! assert!(measured.longest_uncertainty_mm > 0.0);
//! assert!((measured.area_mm2 - 900.0 / 56.25).abs() < 1e-6);
//! ```

mod geometry;

use serde::{Deserialize, Serialize};

use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::Shape;

use geometry::{Outline, max_feret, min_area_rect};

/// The words every measurement result carries: where it holds.
pub const MEASUREMENT_VALIDITY: &str = "Valid only for objects lying in the plane of the \
    calibration fiducial, seen by the same camera at the same image size: one image holds no \
    depth, so anything nearer to or farther from the camera than the fiducial measures wrongly.";

/// The fewest pixels a fiducial side may span: below it the ±1 px of locating each side is
/// more than a tenth of the scale.
const MIN_FIDUCIAL_PX: f64 = 10.0;

/// The largest relative difference between the scales a fiducial's two sides give before
/// the region is refused as not the fiducial, or the plane as tilted too far.
const MAX_ANISOTROPY: f64 = 0.2;

/// The physical size of a calibration fiducial: a printed square or rectangle.
///
/// # Example
///
/// ```
/// use adk_vision::FiducialSpec;
///
/// let card = FiducialSpec::rectangle(85.6, 54.0).with_tolerance(0.1);
/// assert!(card.validate().is_ok());
/// assert!(FiducialSpec::square(-1.0).validate().is_err());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FiducialSpec {
    /// Width in millimetres.
    pub width_mm: f64,
    /// Height in millimetres.
    pub height_mm: f64,
    /// The printed size's tolerance in millimetres, one standard uncertainty.
    #[serde(default)]
    pub tolerance_mm: f64,
}

impl FiducialSpec {
    /// A square fiducial with sides of `size_mm`.
    pub fn square(size_mm: f64) -> Self {
        Self { width_mm: size_mm, height_mm: size_mm, tolerance_mm: 0.0 }
    }

    /// A rectangular fiducial.
    pub fn rectangle(width_mm: f64, height_mm: f64) -> Self {
        Self { width_mm, height_mm, tolerance_mm: 0.0 }
    }

    /// Sets the printed size's tolerance.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance_mm: f64) -> Self {
        self.tolerance_mm = tolerance_mm;
        self
    }

    /// Checks the sizes: positive and finite, the tolerance smaller than either side.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] naming the field.
    pub fn validate(&self) -> Result<()> {
        let positive = |v: f64| v.is_finite() && v > 0.0;
        if !positive(self.width_mm) || !positive(self.height_mm) {
            return Err(VisionError::InvalidInput(format!(
                "a fiducial's size must be positive millimetres, not {} x {}",
                self.width_mm, self.height_mm
            )));
        }
        if !self.tolerance_mm.is_finite()
            || self.tolerance_mm < 0.0
            || self.tolerance_mm >= self.width_mm.min(self.height_mm)
        {
            return Err(VisionError::InvalidInput(format!(
                "a fiducial's tolerance must be zero or more and smaller than its sides, not {}",
                self.tolerance_mm
            )));
        }
        Ok(())
    }

    fn sides(&self) -> [f64; 2] {
        [self.width_mm.max(self.height_mm), self.width_mm.min(self.height_mm)]
    }
}

/// Where a [`Calibration`]'s scale came from.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum CalibrationSource {
    /// A fiducial of known size measured in the image.
    Fiducial {
        /// The fiducial's physical size.
        spec: FiducialSpec,
        /// Its sides as measured, in pixels, the longer first.
        measured_px: [f64; 2],
        /// The image it was measured in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<Digest>,
    },
    /// A scale given directly, such as one an earlier fiducial gave.
    Scale,
}

/// An image's scale in its measured plane: pixels per millimetre with its uncertainty, for
/// images of one size.
///
/// See the [module documentation](self) for the uncertainty model and an example.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    /// Pixels per millimetre in the measured plane.
    pub px_per_mm: f64,
    /// One standard uncertainty of `px_per_mm`.
    pub uncertainty_px_per_mm: f64,
    /// The width of the images the calibration holds for.
    pub image_width: u32,
    /// The height of the images the calibration holds for.
    pub image_height: u32,
    /// Where the scale came from.
    pub source: CalibrationSource,
}

impl Calibration {
    /// The scale a fiducial of `spec`'s size gives, from its region in an image of
    /// `image_size`: the sides of the smallest rectangle enclosing the region, matched long
    /// to long, so a fiducial at any rotation calibrates.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for an invalid spec, a region outside the image or with
    /// no area, a side under 10 px, or sides whose scales differ by more than 20% (the
    /// region is not the fiducial, or the plane is tilted too far to measure in).
    pub fn from_fiducial(
        shape: &Shape,
        spec: FiducialSpec,
        image_size: (u32, u32),
    ) -> Result<Self> {
        spec.validate()?;
        within(shape, image_size)?;
        let outline = Outline::of(shape)?;
        let [long_px, short_px] = min_area_rect(&outline.hull);
        if short_px < MIN_FIDUCIAL_PX {
            return Err(VisionError::InvalidInput(format!(
                "the fiducial spans {short_px:.1} px on its short side; it needs at least \
                 {MIN_FIDUCIAL_PX} px to calibrate: move the camera closer or use a larger fiducial"
            )));
        }
        let [long_mm, short_mm] = spec.sides();
        let (k_long, k_short) = (long_px / long_mm, short_px / short_mm);
        let k = (k_long + k_short) / 2.0;
        let anisotropy = (k_long - k_short).abs() / k;
        if anisotropy > MAX_ANISOTROPY {
            return Err(VisionError::InvalidInput(format!(
                "the fiducial measures {long_px:.1} x {short_px:.1} px, which does not match its \
                 {long_mm} x {short_mm} mm shape within {}%: the region is not the fiducial, or \
                 the plane is tilted too far to measure in",
                MAX_ANISOTROPY * 100.0
            )));
        }
        let quantisation = 0.5 * ((1.0 / long_mm).powi(2) + (1.0 / short_mm).powi(2)).sqrt();
        let tolerance = k * spec.tolerance_mm / ((long_mm + short_mm) / 2.0);
        let perspective = (k_long - k_short).abs() / 2.0;
        let uncertainty = (quantisation.powi(2) + tolerance.powi(2) + perspective.powi(2)).sqrt();
        Ok(Self {
            px_per_mm: k,
            uncertainty_px_per_mm: uncertainty,
            image_width: image_size.0,
            image_height: image_size.1,
            source: CalibrationSource::Fiducial {
                spec,
                measured_px: [long_px, short_px],
                image: None,
            },
        })
    }

    /// A scale given directly, such as one an earlier fiducial gave for this camera.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a scale that is not positive and finite, or an
    /// uncertainty that is negative or not finite.
    pub fn from_scale(
        px_per_mm: f64,
        uncertainty_px_per_mm: f64,
        image_size: (u32, u32),
    ) -> Result<Self> {
        if !px_per_mm.is_finite() || px_per_mm <= 0.0 {
            return Err(VisionError::InvalidInput(format!(
                "a scale must be positive pixels per millimetre, not {px_per_mm}"
            )));
        }
        if !uncertainty_px_per_mm.is_finite() || uncertainty_px_per_mm < 0.0 {
            return Err(VisionError::InvalidInput(format!(
                "a scale's uncertainty must be zero or more, not {uncertainty_px_per_mm}"
            )));
        }
        Ok(Self {
            px_per_mm,
            uncertainty_px_per_mm,
            image_width: image_size.0,
            image_height: image_size.1,
            source: CalibrationSource::Scale,
        })
    }

    /// Records the image a fiducial was measured in.
    #[must_use]
    pub fn with_image(mut self, digest: Digest) -> Self {
        if let CalibrationSource::Fiducial { image, .. } = &mut self.source {
            *image = Some(digest);
        }
        self
    }

    /// The scale's relative uncertainty.
    pub fn relative_uncertainty(&self) -> f64 {
        self.uncertainty_px_per_mm / self.px_per_mm
    }

    /// The reference the scale rests on, `(millimetres, pixels)`: the fiducial's long side
    /// and its measured length, or one millimetre and the scale.
    pub fn reference(&self) -> (f64, f64) {
        match &self.source {
            CalibrationSource::Fiducial { spec, measured_px, .. } => {
                (spec.sides()[0], measured_px[0])
            }
            CalibrationSource::Scale => (1.0, self.px_per_mm),
        }
    }

    /// Measures `shape`, in the pixels of an image of `image_size`: its maximum length and
    /// area, with their uncertainties. Valid only in the calibrated plane
    /// ([`MEASUREMENT_VALIDITY`]).
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for an image of another size than the calibration's, a
    /// region outside the image, or a shape with no area (a point, keypoints, an empty mask).
    pub fn measure(&self, shape: &Shape, image_size: (u32, u32)) -> Result<Measurement> {
        if image_size != (self.image_width, self.image_height) {
            return Err(VisionError::InvalidInput(format!(
                "the calibration holds for {}x{} images and this one is {}x{}: a scale holds only \
                 for the image size and camera position it was made at",
                self.image_width, self.image_height, image_size.0, image_size.1
            )));
        }
        within(shape, image_size)?;
        let outline = Outline::of(shape)?;
        let (k, sigma_k) = (self.px_per_mm, self.uncertainty_px_per_mm);
        let (longest_px, line) = max_feret(&outline.hull);
        let longest_mm = longest_px / k;
        let longest_uncertainty_mm =
            ((1.0 / k).powi(2) + (longest_px * sigma_k / (k * k)).powi(2)).sqrt();
        let area_mm2 = outline.area / (k * k);
        let area_uncertainty_mm2 = ((outline.boundary / 2.0 / (k * k)).powi(2)
            + (2.0 * outline.area * sigma_k / k.powi(3)).powi(2))
        .sqrt();
        Ok(Measurement {
            geometry: outline.geometry,
            longest_px,
            longest_mm,
            longest_uncertainty_mm,
            line,
            area_px: outline.area,
            area_mm2,
            area_uncertainty_mm2,
        })
    }
}

fn within(shape: &Shape, (width, height): (u32, u32)) -> Result<()> {
    if shape.is_within(width, height) {
        return Ok(());
    }
    Err(VisionError::InvalidInput(format!(
        "the region does not lie within the {width}x{height} image (a mask must be the image's size)"
    )))
}

/// What a region's mask, polygon or box was measured as.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MeasuredGeometry {
    /// A mask's set pixels, as unit squares.
    Mask,
    /// A polygon.
    Polygon,
    /// An axis-aligned box: an upper bound on the object's extent.
    Box,
}

/// A region measured in the calibrated plane.
///
/// Field names follow the annotated viewer's measurement record: `longestMm`,
/// `longestUncertaintyMm`, `areaMm2`, `areaUncertaintyMm2`, `line`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    /// What was measured.
    pub geometry: MeasuredGeometry,
    /// The maximum Feret diameter in pixels.
    pub longest_px: f64,
    /// The maximum length in millimetres.
    pub longest_mm: f64,
    /// One standard uncertainty of `longest_mm`.
    pub longest_uncertainty_mm: f64,
    /// The two points the maximum length joins, in source pixels.
    pub line: [[f64; 2]; 2],
    /// The area in square pixels.
    pub area_px: f64,
    /// The area in square millimetres.
    pub area_mm2: f64,
    /// One standard uncertainty of `area_mm2`.
    pub area_uncertainty_mm2: f64,
}
