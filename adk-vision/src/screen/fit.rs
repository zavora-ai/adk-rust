//! Screenshots resized to a model's image limits before they are sent, with the scale kept.
//!
//! Claude's computer use toolset takes no display size and the API does not downscale a
//! screenshot for it: an oversized image is refused. Claude then answers in pixels of the
//! image it received, so the caller resizes first and maps every coordinate back through
//! the scale it used.

use std::sync::{Arc, Mutex};

use adk_core::{
    ActuationOutput, ComputerAction, ComputerActionKind, ComputerActuator, ScreenPoint, Screenshot,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::edit::ops;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, Transform, TransformChain};
use crate::media::{DecodeLimits, SourceImage};

/// Claude counts one visual token per 28 × 28 patch and pads each side to a multiple of 28.
const PATCH: u32 = 28;

/// The image limits of a model that reads screenshots: the longest side and the visual
/// tokens of `⌈w/28⌉ × ⌈h/28⌉`, each side padded up to a multiple of 28.
///
/// # Example
///
/// ```
/// use adk_vision::screen::ScreenshotLimits;
///
/// let limits = ScreenshotLimits::for_claude_model("claude-sonnet-5-5");
/// assert_eq!(limits, ScreenshotLimits::CLAUDE_HIGH);
/// // A 3024×1964 screenshot of a DPR-2 display is resized to 2380×1546.
/// assert_eq!(limits.fitted_size((3024, 1964)), (2380, 1546));
/// // The documented standard-tier examples.
/// assert_eq!(ScreenshotLimits::CLAUDE_STANDARD.fitted_size((1920, 1080)), (1456, 819));
/// assert_eq!(ScreenshotLimits::CLAUDE_STANDARD.fitted_size((1075, 1520)), (924, 1307));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotLimits {
    /// The longest side, after padding to a multiple of 28.
    pub max_edge: u32,
    /// The visual tokens.
    pub max_tokens: u32,
}

impl ScreenshotLimits {
    /// Claude Opus 4.7 and later, Sonnet 5 and later, Haiku 5.5, Fable and Mythos — every
    /// model that takes `computer_toolset_20260801`: 2,576 px and 4,784 visual tokens.
    pub const CLAUDE_HIGH: Self = Self { max_edge: 2576, max_tokens: 4784 };
    /// Earlier Claude models: 1,568 px and 1,568 visual tokens.
    pub const CLAUDE_STANDARD: Self = Self { max_edge: 1568, max_tokens: 1568 };

    /// The limits of a Claude model ID, first-party, Bedrock or Vertex.
    pub fn for_claude_model(model: &str) -> Self {
        if claude_high_tier(model) { Self::CLAUDE_HIGH } else { Self::CLAUDE_STANDARD }
    }

    /// The visual tokens an image of `size` costs.
    pub fn tokens(size: (u32, u32)) -> u64 {
        u64::from(size.0.div_ceil(PATCH)) * u64::from(size.1.div_ceil(PATCH))
    }

    /// Whether an image of `size` is taken as it is.
    pub fn fits(&self, size: (u32, u32)) -> bool {
        let padded = |side: u32| u64::from(side.div_ceil(PATCH)) * u64::from(PATCH);
        padded(size.0) <= u64::from(self.max_edge)
            && padded(size.1) <= u64::from(self.max_edge)
            && Self::tokens(size) <= u64::from(self.max_tokens)
    }

    /// The largest aspect-preserving size within the limits, as the provider computes it:
    /// a binary search along the long edge. An image that fits is returned unchanged.
    pub fn fitted_size(&self, size: (u32, u32)) -> (u32, u32) {
        let (width, height) = (size.0.max(1), size.1.max(1));
        if self.fits((width, height)) {
            return (width, height);
        }
        if height > width {
            let (h, w) = self.fitted_size((height, width));
            return (w, h);
        }
        let aspect = f64::from(width) / f64::from(height);
        let short = |long: u32| ((f64::from(long) / aspect).round_ties_even() as u32).max(1);
        // `lo` always fits and `hi` never does.
        let (mut lo, mut hi) = (1u32, width);
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if self.fits((mid, short(mid))) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo, short(lo))
    }
}

/// Whether a Claude model ID is on the high-resolution image tier.
pub(crate) fn claude_high_tier(model: &str) -> bool {
    let id = model.to_ascii_lowercase();
    let Some(start) = id.find("claude-") else {
        return false;
    };
    let rest = &id[start + "claude-".len()..];
    let mut parts = rest.split(['-', '@', '.', ':']);
    let family = parts.next().unwrap_or_default();
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    match family {
        "fable" | "mythos" => true,
        "opus" => major > 4 || (major == 4 && minor >= 7),
        "sonnet" => major >= 5,
        "haiku" => major > 5 || (major == 5 && minor >= 5),
        _ => false,
    }
}

/// The scale between a screenshot and the copy a model saw: coordinates the model gives
/// are in `sent` pixels and map back to `source` pixels through it.
///
/// # Example
///
/// ```
/// use adk_vision::BBox;
/// use adk_vision::screen::ScreenshotScale;
///
/// let scale = ScreenshotScale::new((3024, 1964), (2380, 1546));
/// let save = scale.bbox_to_source(&BBox::new(1511.1, 928.9, 1652.8, 991.8));
/// assert!((save.x0 - 1920.0).abs() < 0.5 && (save.y1 - 1260.0).abs() < 0.5);
/// // Claude's zoom region, in the sent screenshot's pixels, in source pixels.
/// assert_eq!(scale.region_to_source([0, 0, 2380, 1546]), [0, 0, 3024, 1964]);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotScale {
    /// The screenshot as captured, `[width, height]`.
    pub source: [u32; 2],
    /// The image the model received, `[width, height]`.
    pub sent: [u32; 2],
}

impl ScreenshotScale {
    /// The scale from a `source` screenshot to the `sent` copy.
    pub fn new(source: (u32, u32), sent: (u32, u32)) -> Self {
        Self { source: [source.0.max(1), source.1.max(1)], sent: [sent.0.max(1), sent.1.max(1)] }
    }

    /// No resize.
    pub fn identity(size: (u32, u32)) -> Self {
        Self::new(size, size)
    }

    /// The scale a screenshot of `size` gets under `limits`.
    pub fn fitted(size: (u32, u32), limits: &ScreenshotLimits) -> Self {
        Self::new(size, limits.fitted_size(size))
    }

    /// Whether the model saw the screenshot unresized.
    pub fn is_identity(&self) -> bool {
        self.source == self.sent
    }

    fn factors(&self) -> (f64, f64) {
        (
            f64::from(self.source[0]) / f64::from(self.sent[0]),
            f64::from(self.source[1]) / f64::from(self.sent[1]),
        )
    }

    /// Maps a point in sent pixels to source pixels.
    pub fn to_source(&self, point: [f32; 2]) -> [f32; 2] {
        let (fx, fy) = self.factors();
        [(f64::from(point[0]) * fx) as f32, (f64::from(point[1]) * fy) as f32]
    }

    /// Maps a point in source pixels to sent pixels.
    pub fn to_sent(&self, point: [f32; 2]) -> [f32; 2] {
        let (fx, fy) = self.factors();
        [(f64::from(point[0]) / fx) as f32, (f64::from(point[1]) / fy) as f32]
    }

    /// Maps a box in sent pixels to source pixels.
    pub fn bbox_to_source(&self, bbox: &BBox) -> BBox {
        let a = self.to_source([bbox.x0, bbox.y0]);
        let b = self.to_source([bbox.x1, bbox.y1]);
        BBox::new(a[0], a[1], b[0], b[1])
    }

    /// Maps a whole-pixel point the model gave to the source pixel under its centre,
    /// clamped to the screenshot.
    pub fn point_to_source(&self, point: ScreenPoint) -> ScreenPoint {
        let (fx, fy) = self.factors();
        let map = |v: i64, f: f64, max: u32| {
            (((v as f64 + 0.5) * f).floor() as i64).clamp(0, i64::from(max) - 1)
        };
        ScreenPoint::new(map(point.x, fx, self.source[0]), map(point.y, fy, self.source[1]))
    }

    /// Maps a source pixel to the whole sent pixel that covers it.
    pub fn point_to_sent(&self, point: ScreenPoint) -> ScreenPoint {
        let (fx, fy) = self.factors();
        let map =
            |v: i64, f: f64, max: u32| ((v as f64 / f).floor() as i64).clamp(0, i64::from(max) - 1);
        ScreenPoint::new(map(point.x, fx, self.sent[0]), map(point.y, fy, self.sent[1]))
    }

    /// Maps a `zoom` region `[x0, y0, x1, y1]` in sent pixels to source pixels, rounded
    /// outwards so the whole region is captured.
    pub fn region_to_source(&self, region: [i64; 4]) -> [i64; 4] {
        let (fx, fy) = self.factors();
        let [x0, y0, x1, y1] = region;
        let w = i64::from(self.source[0]);
        let h = i64::from(self.source[1]);
        [
            ((x0 as f64 * fx).floor() as i64).clamp(0, w),
            ((y0 as f64 * fy).floor() as i64).clamp(0, h),
            ((x1 as f64 * fx).ceil() as i64).clamp(0, w),
            ((y1 as f64 * fy).ceil() as i64).clamp(0, h),
        ]
    }

    /// The transform chain from the source screenshot to the sent copy, for a
    /// [`RegionNormalizer`](crate::RegionNormalizer) or a region's origin.
    ///
    /// # Errors
    ///
    /// Never for a scale built by this type's constructors.
    pub fn chain(&self) -> Result<TransformChain> {
        let chain = TransformChain::new(self.source[0], self.source[1]);
        if self.is_identity() {
            return Ok(chain);
        }
        chain.then(Transform::Resize { width: self.sent[0], height: self.sent[1] })
    }
}

/// Resizes a screenshot to `limits` when it is larger, returning the PNG to send and the
/// scale to map the model's coordinates back with.
///
/// # Errors
///
/// Returns [`VisionError::InvalidInput`] when the bytes are not a PNG, JPEG or WebP image
/// within the decode limits.
///
/// # Example
///
/// ```
/// use adk_vision::DecodedImage;
/// use adk_vision::screen::{ScreenshotLimits, fit_screenshot};
///
/// let shot = DecodedImage::filled(3024, 1964, [236, 236, 236, 255]).encode_png().unwrap();
/// let (sent, scale) = fit_screenshot(&shot, &ScreenshotLimits::CLAUDE_HIGH).unwrap();
/// assert_eq!(scale.sent, [2380, 1546]);
/// assert!(sent.len() < shot.len() * 2);
/// ```
pub fn fit_screenshot(
    bytes: &[u8],
    limits: &ScreenshotLimits,
) -> Result<(Vec<u8>, ScreenshotScale)> {
    let image = SourceImage::decode(bytes, &DecodeLimits::default())?;
    let size = image.size();
    let scale = ScreenshotScale::fitted(size, limits);
    if scale.is_identity() {
        return Ok((bytes.to_vec(), scale));
    }
    let resized = ops::resize(image.pixels(), scale.sent[0], scale.sent[1]);
    Ok((resized.encode_png()?, scale))
}

fn image_output(png: Vec<u8>) -> Result<ActuationOutput> {
    let shot = Screenshot::png(png)
        .ok_or_else(|| VisionError::InvalidInput("the resized screenshot is not a PNG".into()))?;
    Ok(ActuationOutput::Image(shot))
}

/// A [`ComputerActuator`] whose screenshots fit a model's image limits: each screenshot is
/// resized before it is returned, the scale is recorded, and every coordinate the model
/// gives — clicks, drags, scrolls, moves and `zoom` regions — is mapped back to the inner
/// actuator's pixels through it.
///
/// `zoom` is always offered: when the inner actuator has no zoom, the region is cropped
/// from a fresh full-resolution screenshot. A zoom image is downscaled to fit within the
/// sent screenshot's size when it is larger, keeping its aspect ratio.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_core::{ActuationOutput, ComputerAction, ComputerActuator, Result, Screenshot};
/// use adk_vision::DecodedImage;
/// use adk_vision::screen::FittedActuator;
///
/// struct Retina;
///
/// #[async_trait::async_trait]
/// impl ComputerActuator for Retina {
///     fn display_size(&self) -> Option<(u32, u32)> {
///         Some((3024, 1964))
///     }
///     async fn perform(&self, _action: ComputerAction) -> Result<ActuationOutput> {
///         let png = DecodedImage::filled(3024, 1964, [0, 0, 0, 255]).encode_png().unwrap();
///         Ok(ActuationOutput::Image(Screenshot::png(png).unwrap()))
///     }
/// }
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let fitted = FittedActuator::for_claude_model(Arc::new(Retina), "claude-sonnet-5-5");
/// let ActuationOutput::Image(shot) = fitted.perform(ComputerAction::Screenshot).await.unwrap()
/// else { unreachable!() };
/// assert_eq!((shot.width, shot.height), (2380, 1546));
/// assert_eq!(fitted.scale().unwrap().source, [3024, 1964]);
/// # });
/// ```
pub struct FittedActuator {
    inner: Arc<dyn ComputerActuator>,
    limits: ScreenshotLimits,
    last: Mutex<Option<ScreenshotScale>>,
}

impl FittedActuator {
    /// Fits `inner`'s screenshots to `limits`.
    pub fn new(inner: Arc<dyn ComputerActuator>, limits: ScreenshotLimits) -> Self {
        Self { inner, limits, last: Mutex::new(None) }
    }

    /// Fits `inner`'s screenshots to a Claude model's limits.
    pub fn for_claude_model(inner: Arc<dyn ComputerActuator>, model: &str) -> Self {
        Self::new(inner, ScreenshotLimits::for_claude_model(model))
    }

    /// The scale of the last screenshot returned, or of the inner display before any.
    pub fn scale(&self) -> Option<ScreenshotScale> {
        let last = *self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        last.or_else(|| {
            self.inner.display_size().map(|size| ScreenshotScale::fitted(size, &self.limits))
        })
    }

    fn record(&self, scale: ScreenshotScale) {
        *self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(scale);
    }

    fn up(&self, point: ScreenPoint) -> ScreenPoint {
        self.scale().map_or(point, |scale| scale.point_to_source(point))
    }

    async fn inner_image(&self, action: ComputerAction) -> Result<Vec<u8>> {
        match self.inner.perform(action).await? {
            ActuationOutput::Image(shot) => Ok(shot.data),
            other => Err(VisionError::Backend {
                backend: "screen".into(),
                message: format!("the actuator answered an image request with {other:?}"),
            }),
        }
    }

    async fn screenshot(&self) -> Result<ActuationOutput> {
        let png = self.inner_image(ComputerAction::Screenshot).await?;
        let (sent, scale) = fit_screenshot(&png, &self.limits)?;
        self.record(scale);
        image_output(sent)
    }

    async fn zoom(&self, region: [i64; 4]) -> Result<ActuationOutput> {
        let scale = self.scale().ok_or_else(|| {
            VisionError::InvalidInput("zoom needs a screenshot first, to know its scale".into())
        })?;
        let [x0, y0, x1, y1] = scale.region_to_source(region);
        if x1 <= x0 || y1 <= y0 {
            return Err(VisionError::InvalidInput(format!(
                "zoom region {region:?} has no pixels on the {}x{} screenshot",
                scale.sent[0], scale.sent[1]
            )));
        }
        let crop = if self.inner.supports(ComputerActionKind::Zoom) {
            let png = self.inner_image(ComputerAction::Zoom { region: [x0, y0, x1, y1] }).await?;
            SourceImage::decode(&png, &DecodeLimits::default())?.pixels().clone()
        } else {
            let png = self.inner_image(ComputerAction::Screenshot).await?;
            let full = SourceImage::decode(&png, &DecodeLimits::default())?;
            let (w, h) = full.size();
            let to_u32 = |v: i64, max: u32| u32::try_from(v.max(0)).unwrap_or(max).min(max);
            let (cx0, cy0) = (to_u32(x0, w), to_u32(y0, h));
            let (cx1, cy1) = (to_u32(x1, w), to_u32(y1, h));
            if cx1 <= cx0 || cy1 <= cy0 {
                return Err(VisionError::InvalidInput(format!(
                    "zoom region {region:?} lies outside the {w}x{h} screen"
                )));
            }
            ops::crop(full.pixels(), cx0, cy0, cx1 - cx0, cy1 - cy0)
        };
        let (cw, ch) = crop.size();
        let fit = f64::from(scale.sent[0]) / f64::from(cw);
        let fit = fit.min(f64::from(scale.sent[1]) / f64::from(ch)).min(1.0);
        let mut size = (
            ((f64::from(cw) * fit).round() as u32).max(1),
            ((f64::from(ch) * fit).round() as u32).max(1),
        );
        size = self.limits.fitted_size(size);
        let out = if size == (cw, ch) { crop } else { ops::resize(&crop, size.0, size.1) };
        image_output(out.encode_png()?)
    }
}

#[async_trait]
impl ComputerActuator for FittedActuator {
    fn display_size(&self) -> Option<(u32, u32)> {
        self.inner.display_size().map(|size| self.limits.fitted_size(size))
    }

    fn supports(&self, kind: ComputerActionKind) -> bool {
        kind == ComputerActionKind::Zoom
            || (kind == ComputerActionKind::Screenshot)
            || self.inner.supports(kind)
    }

    async fn perform(&self, action: ComputerAction) -> adk_core::Result<ActuationOutput> {
        let mapped = match action {
            ComputerAction::Screenshot => return Ok(self.screenshot().await?),
            ComputerAction::Zoom { region } => return Ok(self.zoom(region).await?),
            ComputerAction::Click { button, count, at, modifiers } => {
                ComputerAction::Click { button, count, at: at.map(|p| self.up(p)), modifiers }
            }
            ComputerAction::Drag { from, to, modifiers } => {
                ComputerAction::Drag { from: self.up(from), to: self.up(to), modifiers }
            }
            ComputerAction::MoveTo { at } => ComputerAction::MoveTo { at: self.up(at) },
            ComputerAction::Scroll { at, direction, amount, modifiers } => {
                ComputerAction::Scroll { at: at.map(|p| self.up(p)), direction, amount, modifiers }
            }
            other => other,
        };
        let output = self.inner.perform(mapped).await?;
        Ok(match output {
            ActuationOutput::Cursor(point) => ActuationOutput::Cursor(
                self.scale().map_or(point, |scale| scale.point_to_sent(point)),
            ),
            other => other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_sizes_match_the_reference() {
        let standard = ScreenshotLimits::CLAUDE_STANDARD;
        assert_eq!(standard.fitted_size((1920, 1080)), (1456, 819));
        assert_eq!(standard.fitted_size((1075, 1520)), (924, 1307));
        // High tier: the same scan is taken unresized.
        assert_eq!(ScreenshotLimits::CLAUDE_HIGH.fitted_size((1075, 1520)), (1075, 1520));
    }

    #[test]
    fn fitted_sizes_stay_within_both_limits() {
        for limits in [ScreenshotLimits::CLAUDE_HIGH, ScreenshotLimits::CLAUDE_STANDARD] {
            for size in [(3024, 1964), (5120, 2880), (8000, 1000), (1000, 8000), (2576, 2576)] {
                let fitted = limits.fitted_size(size);
                assert!(limits.fits(fitted), "{size:?} -> {fitted:?}");
                // One more pixel on the long edge no longer fits.
                if fitted != size {
                    let grown = (fitted.0 + 28, fitted.1 + 28);
                    assert!(!limits.fits(grown), "{size:?}: {grown:?} also fits");
                }
            }
        }
    }

    #[test]
    fn tiers_follow_the_model() {
        for high in ["claude-sonnet-5-5", "claude-opus-5-5", "claude-fable-5-2", "claude-haiku-5-5"]
        {
            assert!(claude_high_tier(high), "{high}");
        }
        for standard in ["claude-sonnet-4-6", "claude-haiku-4-5", "gpt-5"] {
            assert!(!claude_high_tier(standard), "{standard}");
        }
    }

    #[test]
    fn points_round_trip_within_a_pixel() {
        let scale = ScreenshotScale::new((3024, 1964), (2380, 1546));
        for (x, y) in [(0, 0), (1, 1), (1189, 772), (2379, 1545)] {
            let source = scale.point_to_source(ScreenPoint::new(x, y));
            let back = scale.point_to_sent(source);
            assert_eq!((back.x, back.y), (x, y));
        }
    }
}
