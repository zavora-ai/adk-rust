//! [`GeminiBox2d`]: Gemini's `box_2d`, `[ymin, xmin, ymax, xmax]` on 0–1000, and its masks.

use base64::Engine as _;
use serde_json::{Value, json};

use super::adapter::{RegionAdapter, image_index, label, quad};
use crate::budget::ImageTokens;
use crate::geometry::{AxisOrder, BBox, CoordFrame, Mask, Polygon, RawRegion, Shape};
use crate::traits::ImageRegion;

/// The two forms Gemini documents for segmentation masks, chosen per model ID.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeminiMaskForm {
    /// A base64 PNG probability map of the region inside its `box_2d`, thresholded at 127.
    Png,
    /// A polygon of `[y, x]` points on 0–1000.
    Polygon,
}

/// Gemini's frame: `box_2d` as `[ymin, xmin, ymax, xmax]` on 0–1000 of the image as sent,
/// whatever its aspect ratio ([`CoordFrame::Normalized`] with [`AxisOrder::Yx`]).
///
/// Masks are read in both documented forms whenever an answer carries one: a `mask` PNG
/// probability map inside the box, or a `polygon` of `[y, x]` points. Either becomes a mask
/// on the 1000 × 1000 grid of the frame, which the runtime maps to source pixels. Which
/// form a model returns is decided per model ID ([`with_masks`](Self::with_masks)); none
/// is requested by default.
///
/// # Example
///
/// ```
/// use adk_vision::cloud::{GeminiBox2d, RegionAdapter};
///
/// let gemini = GeminiBox2d::for_model("gemini-3.8-flash");
/// // One 258-token image up to 384 px a side, otherwise 258 per 768 px tile.
/// assert_eq!(gemini.image_tokens().tokens((384, 300)), 258);
/// assert_eq!(gemini.image_tokens().tokens((1920, 1080)), 6 * 258);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeminiBox2d {
    masks: Option<GeminiMaskForm>,
}

const FRAME: CoordFrame = CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx };

impl GeminiBox2d {
    /// The adapter for a Gemini model ID.
    pub fn for_model(_model: &str) -> Self {
        Self { masks: None }
    }

    /// Asks for masks in `form` as well as boxes.
    pub fn with_masks(mut self, form: GeminiMaskForm) -> Self {
        self.masks = Some(form);
        self
    }
}

impl RegionAdapter for GeminiBox2d {
    fn name(&self) -> &str {
        "gemini-box2d"
    }

    fn frame(&self) -> CoordFrame {
        FRAME
    }

    fn image_tokens(&self) -> ImageTokens {
        ImageTokens::GeminiTiles
    }

    fn region_schema(&self) -> Value {
        let mut properties = json!({
            "label": {"type": "string"},
            "box_2d": {
                "type": "array",
                "items": {"type": "integer"},
                "description": "[ymin, xmin, ymax, xmax] normalized to 0-1000."
            }
        });
        let mut required = vec!["label", "box_2d"];
        match self.masks {
            Some(GeminiMaskForm::Png) => {
                properties["mask"] = json!({
                    "type": "string",
                    "description": "A base64 PNG probability map of the region inside its box."
                });
                required.push("mask");
            }
            Some(GeminiMaskForm::Polygon) => {
                properties["polygon"] = json!({
                    "type": "array",
                    "items": {"type": "array", "items": {"type": "integer"}},
                    "description": "The region's outline as [y, x] points normalized to 0-1000."
                });
                required.push("polygon");
            }
            None => {}
        }
        json!({"type": "object", "properties": properties, "required": required})
    }

    fn instructions(&self, sizes: &[(u32, u32)]) -> String {
        let mut text = String::from(
            "Give each region's `box_2d` as [ymin, xmin, ymax, xmax] normalized to 0-1000.",
        );
        if sizes.len() > 1 {
            text.push_str(" Name the image each region is in by its index, from 0.");
        }
        match self.masks {
            Some(GeminiMaskForm::Png) => text.push_str(
                " Give each region's segmentation mask in `mask` as a base64 PNG probability \
                 map of the region inside its box.",
            ),
            Some(GeminiMaskForm::Polygon) => text.push_str(
                " Give each region's outline in `polygon` as [y, x] points normalized to 0-1000.",
            ),
            None => {}
        }
        text
    }

    fn parse_region(
        &self,
        item: &Value,
        sizes: &[(u32, u32)],
    ) -> Result<Option<ImageRegion>, String> {
        let image = image_index(item, sizes)?;
        let Some(raw) = quad(item, &["box_2d", "box", "bbox"])? else {
            return Ok(None);
        };
        let bbox = FRAME.box_from_raw(raw);
        let shape = if let Some(mask) = item.get("mask").and_then(Value::as_str) {
            Shape::Mask(png_mask(mask, &bbox)?)
        } else if let Some(points) = item.get("polygon").or_else(|| item.get("points")) {
            Shape::Polygon(polygon(points)?)
        } else {
            Shape::Box(bbox)
        };
        let mut region = RawRegion::shape(shape);
        region.label = label(item);
        Ok(Some(ImageRegion { image, region }))
    }
}

/// Decodes a probability-map PNG, scales it to the box's cells of the 1000 × 1000 grid and
/// thresholds it at 127, as Gemini's documentation does in pixels.
fn png_mask(data: &str, bbox: &BBox) -> Result<Mask, String> {
    let b64 = data.split_once("base64,").map_or(data, |(_, b)| b);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("`mask` is not base64: {e}"))?;
    let png = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| format!("`mask` is not a PNG: {e}"))?
        .to_luma8();
    let clamped = bbox.clamped(1000, 1000);
    let (x0, y0, w, h) = clamped.pixel_bounds(1000, 1000);
    if w == 0 || h == 0 {
        return Ok(Mask::empty(1000, 1000));
    }
    let scaled = image::imageops::resize(&png, w, h, image::imageops::FilterType::Triangle);
    let mut bits = vec![false; 1000 * 1000];
    for (x, y, p) in scaled.enumerate_pixels() {
        if p.0[0] > 127 {
            bits[(y0 + y) as usize * 1000 + (x0 + x) as usize] = true;
        }
    }
    Ok(Mask::encode(1000, 1000, &bits))
}

/// Reads `[y, x]` points on 0–1000 into a polygon with x first.
fn polygon(points: &Value) -> Result<Polygon, String> {
    let list = points.as_array().ok_or_else(|| format!("`polygon` is not a list: {points}"))?;
    let mut out = Vec::with_capacity(list.len());
    for p in list {
        let pair = p
            .as_array()
            .filter(|a| a.len() == 2)
            .and_then(|a| Some([a[0].as_f64()? as f32, a[1].as_f64()? as f32]))
            .filter(|[a, b]| a.is_finite() && b.is_finite())
            .ok_or_else(|| format!("`polygon` holds a point that is not [y, x]: {p}"))?;
        out.push(FRAME.point_from_raw(pair));
    }
    if out.len() < 3 {
        return Err(format!("`polygon` has {} points; a region needs three", out.len()));
    }
    Ok(Polygon { points: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> String {
        let img = image::GrayImage::from_fn(w, h, |x, y| image::Luma([f(x, y)]));
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    #[test]
    fn boxes_are_read_y_first() {
        let item = json!({"label": "cat", "box_2d": [100, 250, 500, 750]});
        let found = GeminiBox2d::for_model("gemini-3.8-flash")
            .parse_region(&item, &[(2000, 1000)])
            .unwrap()
            .unwrap();
        assert_eq!(found.region.shape, Shape::Box(BBox::new(250.0, 100.0, 750.0, 500.0)));
        assert_eq!(found.region.label.as_deref(), Some("cat"));
    }

    #[test]
    fn png_masks_fill_their_box_on_the_grid() {
        // The left half of the map is set.
        let item = json!({
            "label": "half",
            "box_2d": [100, 200, 300, 600],
            "mask": png(64, 32, |x, _| if x < 32 { 255 } else { 0 })
        });
        let found = GeminiBox2d::for_model("gemini-2.5-flash")
            .parse_region(&item, &[(1000, 1000)])
            .unwrap()
            .unwrap();
        let Shape::Mask(mask) = found.region.shape else { panic!("not a mask") };
        assert_eq!((mask.width, mask.height), (1000, 1000));
        let b = mask.bbox().unwrap();
        assert_eq!((b.x0, b.y0, b.y1), (200.0, 100.0, 300.0));
        assert!((b.x1 - 400.0).abs() <= 1.0, "{b:?}");
    }

    #[test]
    fn polygons_are_read_y_first() {
        let item = json!({"box_2d": [0, 0, 10, 10], "polygon": [[0, 0], [0, 10], [10, 5]]});
        let found = GeminiBox2d::for_model("gemini-2.5-flash")
            .parse_region(&item, &[(100, 100)])
            .unwrap()
            .unwrap();
        assert_eq!(
            found.region.shape,
            Shape::Polygon(Polygon { points: vec![[0.0, 0.0], [10.0, 0.0], [5.0, 10.0]] })
        );
    }

    #[test]
    fn malformed_masks_are_refused() {
        let gemini = GeminiBox2d::for_model("gemini-2.5-flash");
        let item = json!({"box_2d": [0, 0, 10, 10], "mask": "not base64 !!"});
        assert!(gemini.parse_region(&item, &[(100, 100)]).is_err());
        let item = json!({"box_2d": [0, 0, 10, 10], "polygon": [[0, 0]]});
        assert!(gemini.parse_region(&item, &[(100, 100)]).is_err());
    }
}
