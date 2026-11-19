//! [`ScreenSpace`]: logical screen points for actuation, from screenshot pixels.

use serde::{Deserialize, Serialize};

use super::BBox;

/// Logical screen points for actuation, from source pixels of a screenshot.
///
/// A screenshot of a 1512×982-point display at device pixel ratio 2 is 3024×1964 pixels;
/// [`to_points`](Self::to_points) halves the coordinates before a click. A screen action's
/// target is derived through this type from a region of a screenshot whose digest the
/// action records (V-I14).
///
/// # Example
///
/// ```
/// use adk_vision::ScreenSpace;
///
/// let screen = ScreenSpace::new("built-in", [0.0, 0.0], 2.0);
/// assert_eq!(screen.to_points([3024.0, 1964.0]), [1512.0, 982.0]);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenSpace {
    /// The display the screenshot shows.
    pub display: String,
    /// The display's top-left corner in the global point space.
    pub origin: [f32; 2],
    /// Pixels per point.
    pub device_pixel_ratio: f32,
}

impl ScreenSpace {
    /// A display at `origin` with its device pixel ratio.
    pub fn new(display: impl Into<String>, origin: [f32; 2], device_pixel_ratio: f32) -> Self {
        Self { display: display.into(), origin, device_pixel_ratio }
    }

    fn ratio(&self) -> f32 {
        if self.device_pixel_ratio > 0.0 { self.device_pixel_ratio } else { 1.0 }
    }

    /// Maps a screenshot pixel to logical screen points.
    pub fn to_points(&self, pixel: [f32; 2]) -> [f32; 2] {
        let r = self.ratio();
        [self.origin[0] + pixel[0] / r, self.origin[1] + pixel[1] / r]
    }

    /// Maps logical screen points to a screenshot pixel.
    pub fn to_pixels(&self, point: [f32; 2]) -> [f32; 2] {
        let r = self.ratio();
        [(point[0] - self.origin[0]) * r, (point[1] - self.origin[1]) * r]
    }

    /// Maps a box in screenshot pixels to logical screen points.
    pub fn bbox_to_points(&self, bbox: &BBox) -> BBox {
        let a = self.to_points([bbox.x0, bbox.y0]);
        let b = self.to_points([bbox.x1, bbox.y1]);
        BBox::new(a[0], a[1], b[0], b[1])
    }
}
