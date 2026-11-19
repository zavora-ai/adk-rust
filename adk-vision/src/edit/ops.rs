//! Pixel operations shared by the editor and the egress gate.

use image::imageops::{self, FilterType};

use super::Rgba;
use crate::geometry::BBox;
use crate::media::DecodedImage;

/// The whole-pixel rectangle at `(x, y)` of `width` × `height`.
pub(crate) fn crop(pixels: &DecodedImage, x: u32, y: u32, width: u32, height: u32) -> DecodedImage {
    DecodedImage::from_inner(imageops::crop_imm(pixels.inner(), x, y, width, height).to_image())
}

/// Scaled to `width` × `height` with a triangle (bilinear) filter.
pub(crate) fn resize(pixels: &DecodedImage, width: u32, height: u32) -> DecodedImage {
    DecodedImage::from_inner(imageops::resize(pixels.inner(), width, height, FilterType::Triangle))
}

/// Fills every pixel the box touches.
pub(crate) fn fill(pixels: &mut DecodedImage, bbox: &BBox, color: Rgba) -> bool {
    let (w, h) = pixels.size();
    let (x0, y0, bw, bh) = bbox.pixel_bounds(w, h);
    if bw == 0 || bh == 0 {
        return false;
    }
    let image = pixels.inner_mut();
    for y in y0..y0 + bh {
        for x in x0..x0 + bw {
            image.put_pixel(x, y, image::Rgba(color.0));
        }
    }
    true
}

/// Gaussian-blurs the pixels the box touches.
pub(crate) fn blur(pixels: &mut DecodedImage, bbox: &BBox, sigma: f32) {
    let (w, h) = pixels.size();
    let (x0, y0, bw, bh) = bbox.pixel_bounds(w, h);
    if bw == 0 || bh == 0 {
        return;
    }
    let region = imageops::crop_imm(pixels.inner(), x0, y0, bw, bh).to_image();
    let blurred = imageops::blur(&region, sigma.max(0.1));
    imageops::replace(pixels.inner_mut(), &blurred, i64::from(x0), i64::from(y0));
}

/// Replaces each `block`-pixel square the box touches with its average colour.
pub(crate) fn pixelate(pixels: &mut DecodedImage, bbox: &BBox, block: u32) {
    let (w, h) = pixels.size();
    let (x0, y0, bw, bh) = bbox.pixel_bounds(w, h);
    let image = pixels.inner_mut();
    let mut by = y0;
    while by < y0 + bh {
        let mut bx = x0;
        let cell_h = block.min(y0 + bh - by);
        while bx < x0 + bw {
            let cell_w = block.min(x0 + bw - bx);
            let mut sum = [0u64; 4];
            for y in by..by + cell_h {
                for x in bx..bx + cell_w {
                    let p = image.get_pixel(x, y).0;
                    for (s, v) in sum.iter_mut().zip(p) {
                        *s += u64::from(v);
                    }
                }
            }
            let n = u64::from(cell_w) * u64::from(cell_h);
            let avg = sum.map(|s| ((s + n / 2) / n) as u8);
            for y in by..by + cell_h {
                for x in bx..bx + cell_w {
                    image.put_pixel(x, y, image::Rgba(avg));
                }
            }
            bx += cell_w;
        }
        by += cell_h;
    }
}

/// Blends `color` over one pixel, by the colour's alpha.
pub(crate) fn blend(pixels: &mut DecodedImage, x: u32, y: u32, color: Rgba) {
    let (w, h) = pixels.size();
    if x >= w || y >= h {
        return;
    }
    let image = pixels.inner_mut();
    let alpha = u32::from(color.0[3]);
    let under = image.get_pixel(x, y).0;
    let mix =
        |c: u8, u: u8| ((u32::from(c) * alpha + u32::from(u) * (255 - alpha) + 127) / 255) as u8;
    image.put_pixel(
        x,
        y,
        image::Rgba([
            mix(color.0[0], under[0]),
            mix(color.0[1], under[1]),
            mix(color.0[2], under[2]),
            under[3].max(color.0[3]),
        ]),
    );
}
