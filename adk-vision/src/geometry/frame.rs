//! [`CoordFrame`]: the coordinate frame a backend answers in.

use serde::{Deserialize, Serialize};

use super::BBox;

/// The order of axes in a provider's raw coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AxisOrder {
    /// `[x, y]`, boxes as `[x0, y0, x1, y1]`.
    Xy,
    /// `[y, x]`, boxes as `[y0, x0, y1, x1]`: Gemini's `box_2d`.
    Yx,
}

/// How a backend fits the sent image into its fixed-size model input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Letterbox {
    /// Resized to the input size on each axis; aspect ratio not kept, no padding.
    Stretch,
    /// Scaled to fit, keeping the aspect ratio, and centred: padding split evenly, the odd
    /// pixel on the right and bottom.
    Center,
    /// Scaled to fit, keeping the aspect ratio, padded on the right and bottom only.
    TopLeft,
}

/// The resize and padding that put a sent image into a model input.
///
/// A sent pixel `x` lands at model-input pixel `x * scale_x + pad_left`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LetterboxGeometry {
    /// Horizontal scale from sent to model-input pixels.
    pub scale_x: f64,
    /// Vertical scale from sent to model-input pixels.
    pub scale_y: f64,
    /// Padding left of the image, in model-input pixels.
    pub pad_left: u32,
    /// Padding above the image, in model-input pixels.
    pub pad_top: u32,
    /// The image's size inside the model input, before padding.
    pub resized: (u32, u32),
}

impl Letterbox {
    /// The geometry that puts a `sent` image into an `input` tensor.
    ///
    /// The resized size is rounded to whole pixels and the scale follows the rounded size,
    /// so a backend that prepares its tensor with this geometry and the runtime that undoes
    /// it agree exactly.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::Letterbox;
    ///
    /// // A 3024x1964 screenshot into a 640x640 input.
    /// let g = Letterbox::Center.geometry((3024, 1964), (640, 640));
    /// assert_eq!(g.resized, (640, 416));
    /// assert_eq!((g.pad_left, g.pad_top), (0, 112));
    /// ```
    pub fn geometry(self, sent: (u32, u32), input: (u32, u32)) -> LetterboxGeometry {
        let (w, h) = (f64::from(sent.0.max(1)), f64::from(sent.1.max(1)));
        let (iw, ih) = (f64::from(input.0), f64::from(input.1));
        match self {
            Self::Stretch => LetterboxGeometry {
                scale_x: iw / w,
                scale_y: ih / h,
                pad_left: 0,
                pad_top: 0,
                resized: input,
            },
            Self::Center | Self::TopLeft => {
                let s = (iw / w).min(ih / h);
                let rw = ((w * s).round() as u32).clamp(1, input.0.max(1));
                let rh = ((h * s).round() as u32).clamp(1, input.1.max(1));
                let (pad_left, pad_top) = if self == Self::Center {
                    ((input.0 - rw) / 2, (input.1 - rh) / 2)
                } else {
                    (0, 0)
                };
                LetterboxGeometry {
                    scale_x: f64::from(rw) / w,
                    scale_y: f64::from(rh) / h,
                    pad_left,
                    pad_top,
                    resized: (rw, rh),
                }
            }
        }
    }
}

/// The coordinate frame a backend answers in. It is part of every answer: each backend
/// descriptor names its frame, and the runtime maps from it to source pixels.
///
/// # Example
///
/// ```
/// use adk_vision::{AxisOrder, CoordFrame};
///
/// // Gemini's box_2d: [ymin, xmin, ymax, xmax] on 0-1000, for a 2000x1000 sent image.
/// let gemini = CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx };
/// let b = gemini.box_from_raw([100.0, 250.0, 500.0, 750.0]);
/// let sent = gemini.bbox_to_sent(&b, (2000, 1000));
/// assert_eq!((sent.x0, sent.y0, sent.x1, sent.y1), (500.0, 100.0, 1500.0, 500.0));
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum CoordFrame {
    /// Pixels of the image as sent: Meta `sam-3.1`, Claude once the runtime has resized the
    /// image to its limit, and every local backend once it has undone its own letterbox.
    SentPixels,
    /// Numbers on a fixed scale with an axis order: Gemini's `box_2d` is
    /// `[ymin, xmin, ymax, xmax]` on 0–1000.
    Normalized {
        /// The value that stands for the full width or height.
        scale: u32,
        /// The provider's axis order.
        order: AxisOrder,
    },
    /// Model-input tensor pixels, with the letterbox that put the sent image there.
    ModelInput {
        /// The model input's width.
        width: u32,
        /// The model input's height.
        height: u32,
        /// How the sent image was fitted into the input.
        letterbox: Letterbox,
    },
}

impl CoordFrame {
    /// Reads a provider's raw `[a0, b0, a1, b1]` box in this frame's axis order into a box
    /// with x horizontal. Frames other than [`Normalized`](Self::Normalized) are `Xy`.
    pub fn box_from_raw(&self, raw: [f32; 4]) -> BBox {
        match self {
            Self::Normalized { order: AxisOrder::Yx, .. } => {
                BBox::new(raw[1], raw[0], raw[3], raw[2])
            }
            _ => BBox::new(raw[0], raw[1], raw[2], raw[3]),
        }
    }

    /// Reads a provider's raw `[a, b]` point in this frame's axis order.
    pub fn point_from_raw(&self, raw: [f32; 2]) -> [f32; 2] {
        match self {
            Self::Normalized { order: AxisOrder::Yx, .. } => [raw[1], raw[0]],
            _ => raw,
        }
    }

    /// Maps a point from this frame to pixels of a `sent` image.
    pub fn point_to_sent(&self, point: [f64; 2], sent: (u32, u32)) -> [f64; 2] {
        let (w, h) = (f64::from(sent.0), f64::from(sent.1));
        match self {
            Self::SentPixels => point,
            Self::Normalized { scale, .. } => {
                let s = f64::from((*scale).max(1));
                [point[0] / s * w, point[1] / s * h]
            }
            Self::ModelInput { width, height, letterbox } => {
                let g = letterbox.geometry(sent, (*width, *height));
                [
                    (point[0] - f64::from(g.pad_left)) / g.scale_x,
                    (point[1] - f64::from(g.pad_top)) / g.scale_y,
                ]
            }
        }
    }

    /// Maps a point from pixels of a `sent` image into this frame: the inverse of
    /// [`point_to_sent`](Self::point_to_sent), for prompts a backend takes in its own frame.
    pub fn point_from_sent(&self, point: [f64; 2], sent: (u32, u32)) -> [f64; 2] {
        let (w, h) = (f64::from(sent.0.max(1)), f64::from(sent.1.max(1)));
        match self {
            Self::SentPixels => point,
            Self::Normalized { scale, .. } => {
                let s = f64::from((*scale).max(1));
                [point[0] / w * s, point[1] / h * s]
            }
            Self::ModelInput { width, height, letterbox } => {
                let g = letterbox.geometry(sent, (*width, *height));
                [
                    point[0] * g.scale_x + f64::from(g.pad_left),
                    point[1] * g.scale_y + f64::from(g.pad_top),
                ]
            }
        }
    }

    /// Maps a box from this frame to pixels of a `sent` image.
    pub fn bbox_to_sent(&self, bbox: &BBox, sent: (u32, u32)) -> BBox {
        let a = self.point_to_sent([f64::from(bbox.x0), f64::from(bbox.y0)], sent);
        let b = self.point_to_sent([f64::from(bbox.x1), f64::from(bbox.y1)], sent);
        BBox::new(a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32)
    }

    /// The size of the raster a mask in this frame must have, or `None` when masks are not
    /// expressed in this frame (an adapter converts normalized masks first).
    pub fn mask_raster(&self, sent: (u32, u32)) -> Option<(u32, u32)> {
        match self {
            Self::SentPixels => Some(sent),
            Self::Normalized { .. } => None,
            Self::ModelInput { width, height, .. } => Some((*width, *height)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_round_trips_points() {
        for letterbox in [Letterbox::Stretch, Letterbox::Center, Letterbox::TopLeft] {
            let frame = CoordFrame::ModelInput { width: 640, height: 640, letterbox };
            let p = [1234.5, 777.25];
            let model = frame.point_from_sent(p, (3024, 1964));
            let back = frame.point_to_sent(model, (3024, 1964));
            assert!((back[0] - p[0]).abs() < 1e-6 && (back[1] - p[1]).abs() < 1e-6);
        }
    }

    #[test]
    fn top_left_letterbox_has_no_leading_padding() {
        let g = Letterbox::TopLeft.geometry((1000, 500), (640, 640));
        assert_eq!((g.pad_left, g.pad_top, g.resized), (0, 0, (640, 320)));
    }
}
