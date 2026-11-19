//! Overlay drawing, with a built-in 3×5 digit font for set-of-marks badges.

use super::ops::{blend, fill};
use super::{Overlay, Rgba};
use crate::error::Result;
use crate::geometry::{BBox, Shape, TransformChain};
use crate::media::DecodedImage;

/// Rows of each digit, three bits per row, most significant bit on the left.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

pub(super) fn overlay(
    out: &mut DecodedImage,
    chain: &TransformChain,
    overlay: &Overlay,
) -> Result<()> {
    let point = |p: [f32; 2]| {
        let q = chain.to_target([f64::from(p[0]), f64::from(p[1])]);
        [q[0] as f32, q[1] as f32]
    };
    match overlay {
        Overlay::Box { bbox, color, thickness } => {
            outline(out, &chain.bbox_to_target(bbox), *color, (*thickness).max(1));
        }
        Overlay::Polygon { polygon, color, thickness } => {
            let points: Vec<[f32; 2]> = polygon.points.iter().map(|&p| point(p)).collect();
            for (i, &a) in points.iter().enumerate() {
                let b = points[(i + 1) % points.len()];
                line(out, a, b, *color, (*thickness).max(1));
            }
        }
        Overlay::Mask { mask, color } => {
            if let Shape::Mask(target) = chain.shape_to_target(&Shape::Mask(mask.clone()))? {
                let bits = target.decode();
                for (i, set) in bits.into_iter().enumerate() {
                    if set {
                        let x = (i % target.width as usize) as u32;
                        let y = (i / target.width as usize) as u32;
                        blend(out, x, y, *color);
                    }
                }
            }
        }
        Overlay::Point { at, color, radius } => disc(out, point(*at), *radius as f32, *color),
        Overlay::Mark { at, number, color } => mark(out, point(*at), *number, *color),
    }
    Ok(())
}

fn outline(out: &mut DecodedImage, b: &BBox, color: Rgba, t: u32) {
    let t = t as f32;
    fill(out, &BBox::new(b.x0, b.y0, b.x1, b.y0 + t), color);
    fill(out, &BBox::new(b.x0, b.y1 - t, b.x1, b.y1), color);
    fill(out, &BBox::new(b.x0, b.y0, b.x0 + t, b.y1), color);
    fill(out, &BBox::new(b.x1 - t, b.y0, b.x1, b.y1), color);
}

fn line(out: &mut DecodedImage, a: [f32; 2], b: [f32; 2], color: Rgba, t: u32) {
    let steps = (b[0] - a[0]).abs().max((b[1] - a[1]).abs()).ceil().max(1.0) as u32;
    let half = t as f32 / 2.0;
    for i in 0..=steps {
        let f = i as f32 / steps as f32;
        let x = a[0] + (b[0] - a[0]) * f;
        let y = a[1] + (b[1] - a[1]) * f;
        fill(out, &BBox::new(x - half, y - half, x + half, y + half), color);
    }
}

fn disc(out: &mut DecodedImage, c: [f32; 2], r: f32, color: Rgba) {
    let (w, h) = out.size();
    let (x0, y0, bw, bh) = BBox::new(c[0] - r, c[1] - r, c[0] + r, c[1] + r).pixel_bounds(w, h);
    for y in y0..y0 + bh {
        for x in x0..x0 + bw {
            let (dx, dy) = (x as f32 + 0.5 - c[0], y as f32 + 0.5 - c[1]);
            if dx * dx + dy * dy <= r * r {
                blend(out, x, y, color);
            }
        }
    }
}

fn mark(out: &mut DecodedImage, c: [f32; 2], number: u32, color: Rgba) {
    let digits: Vec<usize> = number.to_string().bytes().map(|d| usize::from(d - b'0')).collect();
    let scale = 3.0f32;
    let text_w = (digits.len() as f32 * 4.0 - 1.0) * scale;
    let text_h = 5.0 * scale;
    let radius = text_w.max(text_h) / 2.0 + 2.0 * scale;
    disc(out, c, radius, color);
    let left = (c[0] - text_w / 2.0).round();
    let top = (c[1] - text_h / 2.0).round();
    for (i, &digit) in digits.iter().enumerate() {
        let gx = left + i as f32 * 4.0 * scale;
        for (row, bits) in DIGITS[digit].iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    let x = gx + col as f32 * scale;
                    let y = top + row as f32 * scale;
                    fill(out, &BBox::new(x, y, x + scale, y + scale), Rgba::WHITE);
                }
            }
        }
    }
}
