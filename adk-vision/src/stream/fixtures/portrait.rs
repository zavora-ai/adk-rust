//! A drawn head and shoulders: a generated face for the privacy scenarios, not anyone's
//! likeness.

use image::imageops;

use super::conveyor::Canvas;
use crate::geometry::BBox;
use crate::media::DecodedImage;

/// Draws a head and shoulders into `at` (its width about four fifths of its height) over
/// `image`, in one of four skin, hair and shirt palettes picked by `palette`, and returns the
/// face's box — hair line to chin, ear to ear — in `image`'s pixels.
///
/// The figure is drawn shapes — a shaded oval face, ears, hair, eyes, brows, a nose and a
/// mouth, softened like a photograph — the same drawing as the generated identity-card
/// portraits of `examples/vision`. It stands in for a person passing a camera in the privacy
/// scenarios (spec 3.7 S18): YuNet finds its face, and no real person's photo is used.
///
/// # Example
///
/// ```
/// use adk_vision::stream::fixtures::draw_portrait;
/// use adk_vision::{BBox, DecodedImage};
///
/// let mut frame = DecodedImage::filled(320, 240, [110, 112, 118, 255]);
/// let face = draw_portrait(&mut frame, BBox::new(100.0, 20.0, 260.0, 220.0), 1);
/// assert!(face.x0 > 100.0 && face.x1 < 260.0 && face.y1 < 220.0);
/// ```
pub fn draw_portrait(image: &mut DecodedImage, at: BBox, palette: u32) -> BBox {
    let (iw, ih) = image.size();
    let (x0, y0) = (at.x0, at.y0);
    let (w, h) = (at.width(), at.height());
    let skin =
        [[232, 190, 160], [198, 146, 112], [156, 106, 76], [240, 205, 175]][palette as usize % 4];
    let shade = skin.map(|c: u8| c.saturating_sub(42));
    let hair = [[55, 38, 28], [20, 20, 22], [120, 80, 40], [70, 66, 62]][palette as usize % 4];
    let shirt = [[52, 70, 98], [120, 50, 50], [50, 100, 72], [80, 80, 96]][palette as usize % 4];
    let mut canvas = Canvas { w: iw, h: ih, rgba: image.rgba().to_vec() };
    let px = |fx: f32| x0 + w * fx;
    let py = |fy: f32| y0 + h * fy;
    // Sizes the identity-card drawing gives in pixels, at its 180 px width.
    let unit = w / 180.0;

    // Shoulders and collar.
    let shoulders = [
        (x0 + 10.0 * unit, py(1.0)),
        (x0 + 25.0 * unit, py(0.78)),
        (px(0.35), py(0.68)),
        (px(0.65), py(0.68)),
        (x0 + w - 25.0 * unit, py(0.78)),
        (x0 + w - 10.0 * unit, py(1.0)),
    ];
    polygon(&mut canvas, &shoulders, shirt);
    polygon(
        &mut canvas,
        &[(px(0.42), py(0.68)), (px(0.5), py(0.8)), (px(0.58), py(0.68))],
        [230, 230, 235],
    );
    canvas.rect(px(0.43), py(0.55), px(0.57), py(0.7), shade);

    // The face: an oval shaded from the skin tone at its centre to the shade at its rim.
    let (fx0, fy0, fx1, fy1) = (px(0.27), py(0.12), px(0.73), py(0.62));
    let (cx, cy, rx, ry) =
        ((fx0 + fx1) / 2.0, (fy0 + fy1) / 2.0, (fx1 - fx0) / 2.0, (fy1 - fy0) / 2.0);
    for y in fy0.floor().max(0.0) as u32..(fy1.ceil() as u32).min(ih) {
        for x in fx0.floor().max(0.0) as u32..(fx1.ceil() as u32).min(iw) {
            let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
            let r = (dx * dx + dy * dy).sqrt();
            if r > 1.0 {
                continue;
            }
            let t = ((1.0 - r) * 40.0).floor() / 40.0;
            let colour = [0, 1, 2].map(|k| {
                (f32::from(shade[k]) + (f32::from(skin[k]) - f32::from(shade[k])) * t) as u8
            });
            canvas.rect(x as f32, y as f32, x as f32 + 1.0, y as f32 + 1.0, colour);
        }
    }
    // Ears, then the hair: the upper half of an oval and two sideburns.
    ellipse(&mut canvas, px(0.24), py(0.3), px(0.31), py(0.42), shade);
    ellipse(&mut canvas, px(0.69), py(0.3), px(0.76), py(0.42), shade);
    let (hx0, hy0, hx1, hy1) = (px(0.25), py(0.07), px(0.75), py(0.42));
    let (hcx, hcy, hrx, hry) =
        ((hx0 + hx1) / 2.0, (hy0 + hy1) / 2.0, (hx1 - hx0) / 2.0, (hy1 - hy0) / 2.0);
    canvas.fill(hx0, hy0, hx1, hcy, hair, |x, y| {
        y <= hcy && ((x - hcx) / hrx).powi(2) + ((y - hcy) / hry).powi(2) <= 1.0
    });
    canvas.rect(px(0.25), py(0.2), px(0.3), py(0.34), hair);
    canvas.rect(px(0.7), py(0.2), px(0.75), py(0.34), hair);
    // Eyes and brows, nose, mouth.
    for ex in [0.4, 0.6] {
        let (ecx, ecy) = (px(ex), py(0.33));
        ellipse(
            &mut canvas,
            ecx - 8.0 * unit,
            ecy - 4.0 * unit,
            ecx + 8.0 * unit,
            ecy + 4.0 * unit,
            [245, 245, 245],
        );
        ellipse(
            &mut canvas,
            ecx - 4.0 * unit,
            ecy - 4.0 * unit,
            ecx + 4.0 * unit,
            ecy + 4.0 * unit,
            [60, 40, 30],
        );
        canvas.line(ecx - 10.0 * unit, py(0.29), ecx + 10.0 * unit, py(0.285), 3.0 * unit, hair);
    }
    canvas.line(px(0.5), py(0.36), px(0.47), py(0.45), 3.0 * unit, shade);
    canvas.line(px(0.47), py(0.45), px(0.52), py(0.46), 3.0 * unit, shade);
    let (mcx, mcy, mrx, mry) = (px(0.5), py(0.5), w * 0.08, h * 0.04);
    for step in 0..=28 {
        let a = (20.0 + 5.0 * step as f32).to_radians();
        let (x, y) = (mcx + mrx * a.cos(), mcy + mry * a.sin());
        canvas.fill(
            x - 2.0 * unit,
            y - 2.0 * unit,
            x + 2.0 * unit,
            y + 2.0 * unit,
            [150, 70, 70],
            |px, py| (px - x).powi(2) + (py - y).powi(2) <= (2.0 * unit).powi(2),
        );
    }

    let drawn = DecodedImage::from_rgba(iw, ih, canvas.rgba).expect("same size as the image");
    // Softened like a photograph, inside the figure's box only.
    let (bx, by, bw, bh) = at.pixel_bounds(iw, ih);
    let soft = imageops::blur(
        &imageops::crop_imm(drawn.inner(), bx, by, bw, bh).to_image(),
        1.2 * unit.max(0.5),
    );
    *image = drawn;
    imageops::replace(image.inner_mut(), &soft, i64::from(bx), i64::from(by));
    BBox::new(px(0.24), py(0.07), px(0.76), py(0.62)).clamped(iw, ih)
}

/// Fills the oval inscribed in `[x0, x1) × [y0, y1)`.
fn ellipse(canvas: &mut Canvas, x0: f32, y0: f32, x1: f32, y1: f32, colour: [u8; 3]) {
    let (cx, cy, rx, ry) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
    canvas.fill(x0, y0, x1, y1, colour, |x, y| {
        ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0
    });
}

/// Fills a polygon by the even-odd rule.
fn polygon(canvas: &mut Canvas, points: &[(f32, f32)], colour: [u8; 3]) {
    let xs = points.iter().map(|p| p.0);
    let ys = points.iter().map(|p| p.1);
    let (x0, x1) = (xs.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max));
    let (y0, y1) = (ys.clone().fold(f32::MAX, f32::min), ys.fold(f32::MIN, f32::max));
    canvas.fill(x0, y0, x1, y1, colour, |x, y| {
        let mut inside = false;
        let mut j = points.len() - 1;
        for i in 0..points.len() {
            let ((xi, yi), (xj, yj)) = (points[i], points[j]);
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_portrait_draws_inside_its_box_and_returns_its_face() {
        let mut frame = DecodedImage::filled(200, 160, [100, 100, 100, 255]);
        let before = frame.clone();
        let face = draw_portrait(&mut frame, BBox::new(40.0, 10.0, 160.0, 160.0), 2);
        let want = [68.8, 20.5, 131.2, 103.0];
        let got = [face.x0, face.y0, face.x1, face.y1];
        assert!(got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3), "{face:?}");
        let (w, _) = frame.size();
        let at = |image: &DecodedImage, x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            image.rgba()[i..i + 3].to_vec()
        };
        assert_eq!(at(&frame, 5, 5), at(&before, 5, 5), "outside the box nothing changes");
        assert_ne!(at(&frame, 100, 60), at(&before, 100, 60), "the face is drawn");
    }
}
