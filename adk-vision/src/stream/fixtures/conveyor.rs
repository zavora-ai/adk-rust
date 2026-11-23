//! The conveyor clip of spec 3.7 §5 (S9, S11): printed parts on a belt, some with seeded
//! defects of known length, a fiducial of known size in the belt's plane, and a wall clock
//! (a COCO class) above the line so a general-purpose detector has something to find.
//!
//! Every frame is drawn from the spec and the frame index alone, so the clip needs no file
//! and any frame can be rendered on its own.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::media::DecodedImage;

/// What to draw.
///
/// # Example
///
/// ```
/// use adk_vision::stream::fixtures::{ConveyorClip, ConveyorSpec};
///
/// let clip = ConveyorClip::new(ConveyorSpec::default());
/// assert_eq!(clip.truth().parts.len(), 200);
/// assert_eq!(clip.truth().parts.iter().filter(|p| p.defect.is_some()).count(), 12);
/// let frame = clip.render(100);
/// assert_eq!(frame.size(), (640, 360));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConveyorSpec {
    /// Frame width in pixels; a multiple of 8, as RTP/JPEG needs.
    pub width: u32,
    /// Frame height in pixels; a multiple of 8.
    pub height: u32,
    /// Frames a second.
    pub fps: u32,
    /// Parts on the belt.
    pub parts: usize,
    /// Parts with a seeded defect.
    pub defective: usize,
    /// The seed every random choice derives from.
    pub seed: u64,
    /// Pixels per millimetre in the belt's plane.
    pub px_per_mm: f32,
    /// A part's length along the belt and its width across it, in millimetres.
    pub part_mm: [f32; 2],
    /// Milliseconds between two parts entering the frame.
    pub part_spacing_ms: u64,
    /// Belt speed in millimetres a second.
    pub belt_mm_per_s: f32,
    /// The fiducial's side in millimetres.
    pub fiducial_mm: f32,
    /// Seeded defect lengths are drawn from this range, in millimetres.
    pub defect_mm: [f32; 2],
}

impl Default for ConveyorSpec {
    /// The scenario clip: 640×360 at 10 fps, 200 parts, 12 with defects of 4–14 mm, a
    /// 24 mm fiducial, 2.5 px/mm; about five minutes.
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            fps: 10,
            parts: 200,
            defective: 12,
            seed: 0x5EED_C0DE,
            px_per_mm: 2.5,
            part_mm: [32.0, 20.0],
            part_spacing_ms: 1_500,
            belt_mm_per_s: 48.0,
            fiducial_mm: 24.0,
            defect_mm: [4.0, 14.0],
        }
    }
}

/// A seeded defect: a crack across a part's face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefectTruth {
    /// The crack's length in millimetres.
    pub length_mm: f32,
    /// Its angle from the belt's direction, in degrees.
    pub angle_deg: f32,
    /// Its midpoint on the part, in millimetres from the part's top-left corner.
    pub center_mm: [f32; 2],
}

/// One part.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartTruth {
    /// The part's number, from 0.
    pub index: usize,
    /// When its leading edge enters the frame, in milliseconds.
    pub enters_ms: u64,
    /// When its trailing edge leaves the frame, in milliseconds.
    pub exits_ms: u64,
    /// Its offset across the belt from the belt's centre line, in pixels.
    pub lateral_px: f32,
    /// Its defect, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defect: Option<DefectTruth>,
}

/// The fiducial: a square of known size fixed in the belt's plane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FiducialTruth {
    /// Its box in pixels, `[x0, y0, x1, y1]`, `x1` and `y1` exclusive.
    pub bbox: [f32; 4],
    /// Its side in millimetres.
    pub size_mm: f32,
}

/// Everything drawn, for scoring.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConveyorTruth {
    /// The spec the clip was drawn from.
    pub spec: ConveyorSpec,
    /// Frames in the clip.
    pub frames: u64,
    /// The fiducial.
    pub fiducial: FiducialTruth,
    /// The wall clock's box in pixels, `[x0, y0, x1, y1]`.
    pub clock: [f32; 4],
    /// Every part, in order.
    pub parts: Vec<PartTruth>,
}

/// A part in one frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisiblePart {
    /// The part's number.
    pub index: usize,
    /// Its box in pixels, clipped to the frame.
    pub bbox: [f32; 4],
    /// Its crack's end points in pixels, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defect: Option<[f32; 4]>,
}

/// splitmix64: deterministic and dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f32 {
        ((self.next() >> 40) as f32) / ((1u64 << 24) as f32)
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

// Layout, in pixels.
const WALL_BOTTOM: f32 = 0.36;
const BELT_TOP: f32 = 0.39;
const BELT_BOTTOM: f32 = 0.78;
const RAIL: f32 = 0.025;

/// The generated conveyor clip.
#[derive(Clone, Debug)]
pub struct ConveyorClip {
    truth: ConveyorTruth,
    /// Per-part colour tint.
    tints: Vec<[u8; 3]>,
}

impl ConveyorClip {
    /// Draws the layout and seeds the defects.
    pub fn new(spec: ConveyorSpec) -> Self {
        let mut rng = Rng(spec.seed);
        let (w, h) = (spec.width as f32, spec.height as f32);
        let speed = spec.belt_mm_per_s * spec.px_per_mm;
        let part_w = spec.part_mm[0] * spec.px_per_mm;
        let transit_ms = ((w + part_w) / speed * 1000.0).ceil() as u64;
        let belt_h = (BELT_BOTTOM - BELT_TOP) * h;
        let part_h = spec.part_mm[1] * spec.px_per_mm;
        let lateral_room = ((belt_h - part_h) / 2.0 - 8.0).max(0.0);

        // Defective parts: a partial Fisher-Yates over 1..parts, so the first part is clean.
        let mut order: Vec<usize> = (1..spec.parts).collect();
        let defective = spec.defective.min(order.len());
        for i in 0..defective {
            let j = i + (rng.next() % (order.len() - i) as u64) as usize;
            order.swap(i, j);
        }
        let flawed: std::collections::BTreeSet<usize> =
            order[..defective].iter().copied().collect();

        let mut parts = Vec::with_capacity(spec.parts);
        let mut tints = Vec::with_capacity(spec.parts);
        for index in 0..spec.parts {
            let enters_ms = index as u64 * spec.part_spacing_ms;
            let lateral_px = rng.range(-lateral_room, lateral_room);
            let shade = rng.range(-14.0, 14.0);
            tints.push([
                (226.0 + shade).clamp(0.0, 255.0) as u8,
                (118.0 + shade * 0.6).clamp(0.0, 255.0) as u8,
                (44.0 + shade * 0.3).clamp(0.0, 255.0) as u8,
            ]);
            let defect = flawed.contains(&index).then(|| {
                let length_mm = rng.range(spec.defect_mm[0], spec.defect_mm[1]);
                let angle_deg = rng.range(-35.0, 35.0);
                // Keep the whole crack on the part, clear of its rounded corners.
                let half = length_mm / 2.0;
                let (dx, dy) = (
                    half * angle_deg.to_radians().cos(),
                    half * angle_deg.to_radians().sin().abs(),
                );
                let cx = rng.range(2.0 + dx, (spec.part_mm[0] - 2.0 - dx).max(2.0 + dx));
                let cy = rng.range(2.0 + dy, (spec.part_mm[1] - 2.0 - dy).max(2.0 + dy));
                DefectTruth { length_mm, angle_deg, center_mm: [cx, cy] }
            });
            parts.push(PartTruth {
                index,
                enters_ms,
                exits_ms: enters_ms + transit_ms,
                lateral_px,
                defect,
            });
        }
        let last_exit = parts.last().map_or(0, |p| p.exits_ms);
        let frames = last_exit * u64::from(spec.fps) / 1000 + 1;
        let side = spec.fiducial_mm * spec.px_per_mm;
        let fx0 = 0.03 * w;
        let fy0 = (BELT_BOTTOM + RAIL) * h + ((1.0 - BELT_BOTTOM - RAIL) * h - side) / 2.0;
        let clock_r = 0.13 * h;
        let (ccx, ccy) = (0.86 * w, 0.18 * h);
        Self {
            truth: ConveyorTruth {
                frames,
                fiducial: FiducialTruth {
                    bbox: [fx0, fy0, fx0 + side, fy0 + side],
                    size_mm: spec.fiducial_mm,
                },
                clock: [ccx - clock_r, ccy - clock_r, ccx + clock_r, ccy + clock_r],
                parts,
                spec,
            },
            tints,
        }
    }

    /// What was drawn.
    pub fn truth(&self) -> &ConveyorTruth {
        &self.truth
    }

    /// Frames in the clip.
    pub fn frame_count(&self) -> u64 {
        self.truth.frames
    }

    /// The clip's length.
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.truth.frames * 1000 / u64::from(self.truth.spec.fps))
    }

    /// The presentation time of frame `index`.
    pub fn pts(&self, index: u64) -> Duration {
        Duration::from_micros(index * 1_000_000 / u64::from(self.truth.spec.fps))
    }

    fn part_box(&self, part: &PartTruth, t_ms: f32) -> Option<[f32; 4]> {
        let spec = &self.truth.spec;
        let (w, h) = (spec.width as f32, spec.height as f32);
        let speed = spec.belt_mm_per_s * spec.px_per_mm / 1000.0;
        let part_w = spec.part_mm[0] * spec.px_per_mm;
        let part_h = spec.part_mm[1] * spec.px_per_mm;
        let x1 = (t_ms - part.enters_ms as f32) * speed;
        let x0 = x1 - part_w;
        if x1 <= 0.0 || x0 >= w {
            return None;
        }
        let cy = (BELT_TOP + BELT_BOTTOM) / 2.0 * h + part.lateral_px;
        Some([x0, cy - part_h / 2.0, x1, cy + part_h / 2.0])
    }

    fn crack(&self, part: &PartTruth, bbox: &[f32; 4]) -> Option<[f32; 4]> {
        let defect = part.defect.as_ref()?;
        let px = self.truth.spec.px_per_mm;
        let (cx, cy) = (bbox[0] + defect.center_mm[0] * px, bbox[1] + defect.center_mm[1] * px);
        let half = defect.length_mm * px / 2.0;
        let (dx, dy) = (
            half * defect.angle_deg.to_radians().cos(),
            half * defect.angle_deg.to_radians().sin(),
        );
        Some([cx - dx, cy - dy, cx + dx, cy + dy])
    }

    /// The parts in frame `index`, boxes clipped to the frame.
    pub fn visible(&self, index: u64) -> Vec<VisiblePart> {
        let t_ms = self.pts(index).as_secs_f32() * 1000.0;
        let w = self.truth.spec.width as f32;
        self.truth
            .parts
            .iter()
            .filter_map(|part| {
                let bbox = self.part_box(part, t_ms)?;
                let defect = self.crack(part, &bbox);
                Some(VisiblePart {
                    index: part.index,
                    bbox: [bbox[0].max(0.0), bbox[1], bbox[2].min(w), bbox[3]],
                    defect,
                })
            })
            .collect()
    }

    /// Draws frame `index`.
    pub fn render(&self, index: u64) -> DecodedImage {
        let spec = &self.truth.spec;
        let (w, h) = (spec.width, spec.height);
        let mut canvas = Canvas { w, h, rgba: vec![255; (w * h * 4) as usize] };
        let (wf, hf) = (w as f32, h as f32);
        let t = self.pts(index).as_secs_f32();

        // Wall, a little lighter at the top.
        for y in 0..(WALL_BOTTOM * hf) as u32 {
            let shade = 222.0 - 18.0 * y as f32 / (WALL_BOTTOM * hf);
            canvas.rect(
                0.0,
                y as f32,
                wf,
                y as f32 + 1.0,
                [shade as u8, (shade - 6.0) as u8, (shade - 16.0) as u8],
            );
        }
        canvas.rect(0.0, WALL_BOTTOM * hf, wf, hf, [96, 98, 104]);
        // Rails and belt; the belt's ribs move with it.
        canvas.rect(0.0, (BELT_TOP - RAIL) * hf, wf, BELT_TOP * hf, [150, 152, 158]);
        canvas.rect(0.0, BELT_BOTTOM * hf, wf, (BELT_BOTTOM + RAIL) * hf, [150, 152, 158]);
        canvas.rect(0.0, BELT_TOP * hf, wf, BELT_BOTTOM * hf, [44, 46, 50]);
        let speed = spec.belt_mm_per_s * spec.px_per_mm;
        let pitch = 40.0;
        let offset = (t * speed) % pitch;
        let mut x = offset - pitch;
        while x < wf {
            canvas.rect(x, BELT_TOP * hf, x + 3.0, BELT_BOTTOM * hf, [56, 58, 63]);
            x += pitch;
        }

        // The fiducial: a black square with a white ring and an asymmetric inner pattern.
        let [fx0, fy0, fx1, fy1] = self.truth.fiducial.bbox;
        let cell = (fx1 - fx0) / 6.0;
        canvas.rect(fx0, fy0, fx1, fy1, [10, 10, 10]);
        canvas.rect(fx0 + cell, fy0 + cell, fx1 - cell, fy1 - cell, [245, 245, 245]);
        const PATTERN: [[bool; 4]; 4] = [
            [true, false, true, true],
            [false, true, false, false],
            [true, true, false, true],
            [false, false, true, false],
        ];
        for (r, row) in PATTERN.iter().enumerate() {
            for (c, on) in row.iter().enumerate() {
                if *on {
                    let (x0, y0) = (fx0 + cell * (c + 1) as f32, fy0 + cell * (r + 1) as f32);
                    canvas.rect(x0, y0, x0 + cell, y0 + cell, [10, 10, 10]);
                }
            }
        }

        // The wall clock: rim, face, hour marks, hands that turn with the clip.
        let [kx0, ky0, kx1, _] = self.truth.clock;
        let r = (kx1 - kx0) / 2.0;
        let (cx, cy) = (kx0 + r, ky0 + r);
        canvas.disc(cx, cy, r, [30, 30, 34]);
        canvas.disc(cx, cy, r * 0.88, [250, 250, 246]);
        for mark in 0..12 {
            let a = mark as f32 * std::f32::consts::TAU / 12.0;
            let (s, c) = a.sin_cos();
            let (inner, width) = if mark % 3 == 0 { (0.62, 4.0) } else { (0.70, 2.5) };
            canvas.line(
                cx + s * r * inner,
                cy - c * r * inner,
                cx + s * r * 0.82,
                cy - c * r * 0.82,
                width,
                [30, 30, 34],
            );
        }
        let minute = t / 60.0 * std::f32::consts::TAU + 1.0;
        let hour = minute / 12.0 + 3.6;
        for (angle, length, width) in [(hour, 0.45, 5.0), (minute, 0.72, 3.0)] {
            let (s, c) = angle.sin_cos();
            canvas.line(cx, cy, cx + s * r * length, cy - c * r * length, width, [24, 24, 28]);
        }
        canvas.disc(cx, cy, r * 0.07, [24, 24, 28]);

        // Parts: rounded blocks with print layers, two holes, and a crack when seeded.
        let t_ms = t * 1000.0;
        for (part, tint) in self.truth.parts.iter().zip(&self.tints) {
            let Some(bbox) = self.part_box(part, t_ms) else { continue };
            let [x0, y0, x1, y1] = bbox;
            let radius = (y1 - y0) * 0.18;
            canvas.round_rect(x0 + 2.0, y0 + 3.0, x1 + 2.0, y1 + 3.0, radius, [30, 31, 34]);
            canvas.round_rect(x0, y0, x1, y1, radius, *tint);
            let mut layer = y0 + 2.0;
            let darker = tint.map(|v| v.saturating_sub(22));
            while layer < y1 - 2.0 {
                canvas.rect(x0 + radius, layer, x1 - radius, layer + 1.0, darker);
                layer += 3.0;
            }
            let hole = (y1 - y0) * 0.14;
            for fx in [0.22, 0.78] {
                canvas.disc(x0 + (x1 - x0) * fx, (y0 + y1) / 2.0, hole, [44, 46, 50]);
            }
            if let Some([ax, ay, bx, by]) = self.crack(part, &bbox) {
                canvas.line(ax, ay, bx, by, 2.2, [22, 18, 16]);
            }
        }
        DecodedImage::from_rgba(w, h, canvas.rgba)
            .unwrap_or_else(|_| DecodedImage::filled(w, h, [0, 0, 0, 255]))
    }
}

pub(super) struct Canvas {
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) rgba: Vec<u8>,
}

impl Canvas {
    fn put(&mut self, x: i64, y: i64, color: [u8; 3]) {
        if x < 0 || y < 0 || x >= i64::from(self.w) || y >= i64::from(self.h) {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        self.rgba[i..i + 3].copy_from_slice(&color);
        self.rgba[i + 3] = 255;
    }

    /// Pixels whose centres fall in `[x0, x1) × [y0, y1)`, tested by `inside`.
    pub(super) fn fill(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [u8; 3],
        inside: impl Fn(f32, f32) -> bool,
    ) {
        let (xa, xb) = ((x0 - 0.5).floor() as i64, (x1 + 0.5).ceil() as i64);
        let (ya, yb) = ((y0 - 0.5).floor() as i64, (y1 + 0.5).ceil() as i64);
        for y in ya.max(0)..yb.min(i64::from(self.h)) {
            for x in xa.max(0)..xb.min(i64::from(self.w)) {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                if inside(px, py) {
                    self.put(x, y, color);
                }
            }
        }
    }

    pub(super) fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: [u8; 3]) {
        self.fill(x0, y0, x1, y1, color, |x, y| x >= x0 && x < x1 && y >= y0 && y < y1);
    }

    fn round_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, color: [u8; 3]) {
        self.fill(x0, y0, x1, y1, color, |x, y| {
            if x < x0 || x >= x1 || y < y0 || y >= y1 {
                return false;
            }
            let dx = (x0 + r - x).max(x - (x1 - r)).max(0.0);
            let dy = (y0 + r - y).max(y - (y1 - r)).max(0.0);
            dx * dx + dy * dy <= r * r
        });
    }

    fn disc(&mut self, cx: f32, cy: f32, r: f32, color: [u8; 3]) {
        self.fill(cx - r, cy - r, cx + r, cy + r, color, |x, y| {
            (x - cx).powi(2) + (y - cy).powi(2) <= r * r
        });
    }

    pub(super) fn line(&mut self, ax: f32, ay: f32, bx: f32, by: f32, width: f32, color: [u8; 3]) {
        let half = width / 2.0;
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = (dx * dx + dy * dy).max(f32::EPSILON);
        self.fill(
            ax.min(bx) - half,
            ay.min(by) - half,
            ax.max(bx) + half,
            ay.max(by) + half,
            color,
            |x, y| {
                let t = (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0);
                let (qx, qy) = (ax + t * dx - x, ay + t * dy - y);
                qx * qx + qy * qy <= half * half
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clip_is_deterministic_and_matches_its_truth() {
        let a = ConveyorClip::new(ConveyorSpec::default());
        let b = ConveyorClip::new(ConveyorSpec::default());
        assert_eq!(a.truth(), b.truth());
        assert_eq!(a.render(321), b.render(321));
        let defects: Vec<f32> =
            a.truth().parts.iter().filter_map(|p| p.defect.as_ref().map(|d| d.length_mm)).collect();
        assert_eq!(defects.len(), 12);
        assert!(defects.iter().all(|l| (4.0..=14.0).contains(l)), "{defects:?}");
        assert!(a.truth().parts[0].defect.is_none());
        // About five minutes at 10 fps.
        assert!((2_900..3_200).contains(&a.frame_count()), "{}", a.frame_count());
    }

    #[test]
    fn a_crack_is_drawn_where_the_truth_says() {
        let clip = ConveyorClip::new(ConveyorSpec::default());
        let flawed = clip.truth().parts.iter().find(|p| p.defect.is_some()).unwrap();
        // Mid-transit, the whole part is in view.
        let middle = (flawed.enters_ms + flawed.exits_ms) / 2;
        let index = middle * u64::from(clip.truth().spec.fps) / 1000;
        let seen = clip.visible(index);
        let part = seen.iter().find(|v| v.index == flawed.index).unwrap();
        let [ax, ay, bx, by] = part.defect.unwrap();
        let frame = clip.render(index);
        let (mx, my) = (((ax + bx) / 2.0) as u32, ((ay + by) / 2.0) as u32);
        assert!(frame.pixel(mx, my)[0] < 60, "crack pixel {:?}", frame.pixel(mx, my));
        let length = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt() / clip.truth().spec.px_per_mm;
        let truth = flawed.defect.as_ref().unwrap().length_mm;
        assert!((length - truth).abs() < 0.01, "{length} vs {truth}");
    }

    #[test]
    fn the_fiducial_has_its_stated_size() {
        let clip = ConveyorClip::new(ConveyorSpec::default());
        let [x0, y0, x1, y1] = clip.truth().fiducial.bbox;
        let px = clip.truth().spec.px_per_mm;
        assert!(((x1 - x0) / px - 24.0).abs() < 1e-3);
        assert!(((y1 - y0) / px - 24.0).abs() < 1e-3);
        assert!(y1 <= clip.truth().spec.height as f32);
        let frame = clip.render(0);
        assert!(frame.pixel(x0 as u32 + 1, y0 as u32 + 1)[0] < 30);
    }
}
