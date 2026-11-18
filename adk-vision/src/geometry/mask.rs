//! [`Mask`]: a binary mask, run-length encoded in COCO's column-major order.

use serde::{Deserialize, Serialize};

use super::BBox;

/// A binary mask, run-length encoded in COCO's column-major order: `counts` alternates runs
/// of unset and set pixels, starting with unset, scanning each column top to bottom, columns
/// left to right. The runs always sum to `width × height`.
///
/// # Example
///
/// ```
/// use adk_vision::{BBox, Mask};
///
/// let mask = Mask::from_box(4, 3, &BBox::new(1.0, 1.0, 3.0, 2.0));
/// assert_eq!(mask.counts, vec![4, 1, 2, 1, 4]);
/// assert_eq!(mask.area(), 2);
/// assert_eq!(Mask::encode(4, 3, &mask.decode()), mask);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MaskWire")]
pub struct Mask {
    /// Width of the raster.
    pub width: u32,
    /// Height of the raster.
    pub height: u32,
    /// Alternating run lengths, unset first.
    pub counts: Vec<u32>,
}

#[derive(Deserialize)]
struct MaskWire {
    width: u32,
    height: u32,
    counts: Vec<u32>,
}

impl TryFrom<MaskWire> for Mask {
    type Error = String;

    fn try_from(wire: MaskWire) -> Result<Self, Self::Error> {
        let total: u64 = wire.counts.iter().map(|&c| u64::from(c)).sum();
        let expected = u64::from(wire.width) * u64::from(wire.height);
        if total != expected {
            return Err(format!(
                "mask runs sum to {total}, but a {}x{} mask has {expected} pixels",
                wire.width, wire.height
            ));
        }
        Ok(Self { width: wire.width, height: wire.height, counts: wire.counts })
    }
}

impl Mask {
    /// A mask with no pixel set.
    pub fn empty(width: u32, height: u32) -> Self {
        let total = width.saturating_mul(height);
        Self { width, height, counts: if total == 0 { Vec::new() } else { vec![total] } }
    }

    /// Encodes a row-major bitmap of `width × height` pixels.
    ///
    /// # Panics
    ///
    /// Panics when `bits` is not `width × height` long.
    pub fn encode(width: u32, height: u32, bits: &[bool]) -> Self {
        let (w, h) = (width as usize, height as usize);
        assert_eq!(bits.len(), w * h, "a {width}x{height} mask needs {} bits", w * h);
        let mut counts = Vec::new();
        let mut current = false;
        let mut run = 0u32;
        for x in 0..w {
            for y in 0..h {
                let bit = bits[y * w + x];
                if bit != current {
                    counts.push(run);
                    run = 0;
                    current = bit;
                }
                run += 1;
            }
        }
        if w * h > 0 {
            counts.push(run);
        }
        Self { width, height, counts }
    }

    /// Decodes to a row-major bitmap of `width × height` pixels.
    pub fn decode(&self) -> Vec<bool> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut bits = vec![false; w * h];
        let mut index = 0usize;
        for (run, &count) in self.counts.iter().enumerate() {
            let set = run % 2 == 1;
            for i in index..(index + count as usize).min(w * h) {
                if set {
                    let (x, y) = (i / h, i % h);
                    bits[y * w + x] = true;
                }
            }
            index += count as usize;
        }
        bits
    }

    /// A mask with every pixel whose centre lies in `bbox` set.
    pub fn from_box(width: u32, height: u32, bbox: &BBox) -> Self {
        let mut bits = vec![false; width as usize * height as usize];
        for y in 0..height {
            for x in 0..width {
                if bbox.contains([x as f32 + 0.5, y as f32 + 0.5]) {
                    bits[y as usize * width as usize + x as usize] = true;
                }
            }
        }
        Self::encode(width, height, &bits)
    }

    /// The number of set pixels.
    pub fn area(&self) -> u64 {
        self.counts.iter().skip(1).step_by(2).map(|&c| u64::from(c)).sum()
    }

    /// The smallest box covering every set pixel, or `None` for an empty mask.
    pub fn bbox(&self) -> Option<BBox> {
        let h = u64::from(self.height);
        if h == 0 {
            return None;
        }
        let mut bounds: Option<(u64, u64, u64, u64)> = None;
        let mut index = 0u64;
        for (run, &count) in self.counts.iter().enumerate() {
            let (start, len) = (index, u64::from(count));
            index += len;
            if run % 2 == 0 || len == 0 {
                continue;
            }
            let end = start + len - 1;
            let (c0, c1) = (start / h, end / h);
            let (r0, r1) = if c0 == c1 { (start % h, end % h) } else { (0, h - 1) };
            bounds = Some(match bounds {
                None => (c0, r0, c1, r1),
                Some((x0, y0, x1, y1)) => (x0.min(c0), y0.min(r0), x1.max(c1), y1.max(r1)),
            });
        }
        bounds.map(|(x0, y0, x1, y1)| {
            BBox::new(x0 as f32, y0 as f32, (x1 + 1) as f32, (y1 + 1) as f32)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_runs_are_refused_on_deserialization() {
        let err =
            serde_json::from_str::<Mask>(r#"{"width":2,"height":2,"counts":[1,1]}"#).unwrap_err();
        assert!(err.to_string().contains("sum to 2"));
    }

    #[test]
    fn a_full_mask_starts_with_an_empty_run() {
        let mask = Mask::encode(2, 2, &[true; 4]);
        assert_eq!(mask.counts, vec![0, 4]);
        assert_eq!(mask.bbox(), Some(BBox::new(0.0, 0.0, 2.0, 2.0)));
    }

    #[test]
    fn spanning_runs_cover_full_rows() {
        // Column 0 rows 1..3 and column 1 rows 0..1 are one run in column-major order.
        let mut bits = vec![false; 6];
        bits[2] = true; // (0,1)
        bits[4] = true; // (0,2)
        bits[1] = true; // (1,0)
        let mask = Mask::encode(2, 3, &bits);
        assert_eq!(mask.counts, vec![1, 3, 2]);
        assert_eq!(mask.bbox(), Some(BBox::new(0.0, 0.0, 2.0, 3.0)));
    }
}
