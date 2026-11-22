//! The warehouse clip of scenario S17: an hour of a warehouse camera with ten labelled events.
//!
//! Every frame is drawn from the spec and its time alone — a loading dock, three racks of
//! cartons, a fire exit and a camera timestamp, with forklifts, a truck, workers, a spill, a
//! fire and a power cut at known times — so the clip needs no file and the truth is exact.
//! Between the events a worker pushes a cart across the floor now and then, which is not an
//! event.
//!
//! The spec's hour can be shortened: [`WarehouseSpec::with_duration`] scales the times the
//! events start at and keeps how long each lasts.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::media::{DecodedImage, Frame, FramePixels, FrameSourceId};
use crate::stream::FrameStream;

/// One labelled event: what happens, from when, and the question that asks for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarehouseEvent {
    /// A stable id, such as `forklift-aisle-2`.
    pub id: String,
    /// What happens.
    pub description: String,
    /// The question an operator asks about it.
    pub question: String,
    /// When it starts, in milliseconds from the start of the clip: the labelled time.
    pub at_ms: u64,
    /// When what it leaves behind is gone, in milliseconds.
    pub until_ms: u64,
}

/// The clip to draw.
///
/// # Example
///
/// ```
/// use adk_vision::analyst::fixtures::{WarehouseClip, WarehouseSpec};
///
/// let clip = WarehouseClip::new(WarehouseSpec::default());
/// assert_eq!(clip.events().len(), 10);
/// assert_eq!(clip.spec().duration_ms, 3_600_000);
/// assert_eq!(clip.render(0).size(), (480, 270));
/// // Ten minutes: the events keep their order and their length.
/// let short = WarehouseClip::new(WarehouseSpec::default().with_duration(600_000));
/// assert!(short.events().iter().all(|e| e.until_ms <= 600_000));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarehouseSpec {
    /// The clip's length in milliseconds: an hour by default.
    pub duration_ms: u64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// The camera's name, the frame source id.
    pub camera: String,
    /// The wall time of the first frame.
    pub started_at: DateTime<Utc>,
}

impl Default for WarehouseSpec {
    /// An hour at 480×270 from camera `warehouse-cam-3`, from 2026-11-18 06:00 UTC.
    fn default() -> Self {
        Self {
            duration_ms: HOUR_MS,
            width: 480,
            height: 270,
            camera: "warehouse-cam-3".into(),
            started_at: DateTime::from_timestamp(1_763_445_600, 0).unwrap_or_default(),
        }
    }
}

impl WarehouseSpec {
    /// Shortens or lengthens the clip to `duration_ms`, at least ten minutes, scaling the
    /// times the events start at; each event lasts as long as it does in the hour.
    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms.max(600_000);
        self
    }
}

const HOUR_MS: u64 = 3_600_000;

/// What each event is in the hour: id, start in seconds, length in seconds, description,
/// question.
const EVENTS: [(&str, u64, u64, &str, &str); 10] = [
    (
        "forklift-aisle-2",
        190,
        20,
        "an orange forklift carrying a pallet drives in from the right and stops at aisle 2",
        "When does the forklift drive up to aisle 2?",
    ),
    (
        "truck-docks",
        583,
        25,
        "the dock door rolls up and a white truck backs into the doorway",
        "When does a truck back into the dock door?",
    ),
    (
        "boxes-fall",
        927,
        20,
        "a stack of cartons falls from the top shelf of aisle 3 onto the floor",
        "When do boxes fall off the shelf?",
    ),
    (
        "vest-worker-crosses",
        1_266,
        18,
        "a worker in a yellow high-visibility vest walks across the floor from left to right",
        "When does the worker in the yellow vest walk across the floor?",
    ),
    (
        "spill",
        1_612,
        25,
        "a blue liquid spill spreads on the floor in front of aisle 1",
        "When is there a spill on the floor?",
    ),
    (
        "exit-blocked",
        1_938,
        25,
        "a pallet of red cartons is left in front of the green fire exit door",
        "When is the fire exit blocked?",
    ),
    (
        "ladder-climb",
        2_311,
        20,
        "a worker climbs a ladder leaning against the rack of aisle 1",
        "When does someone climb a ladder?",
    ),
    (
        "near-miss",
        2_647,
        15,
        "an orange and a blue forklift drive at each other and stop nose to nose",
        "When do two forklifts nearly collide?",
    ),
    (
        "fire",
        3_021,
        20,
        "flames and smoke rise from a carton on the middle shelf of aisle 2",
        "When is there a fire?",
    ),
    (
        "lights-out",
        3_342,
        20,
        "the warehouse lights go out and the picture turns dark",
        "When do the lights go out?",
    ),
];

/// When a worker pushes a cart across the floor, which is not an event: start and length in
/// seconds of the hour.
const CARTS: [(u64, u64); 4] = [(400, 15), (1_450, 15), (2_100, 15), (2_900, 15)];

/// The warehouse clip: an hour of frames drawn from their time, and the ten events' truth.
/// See the [module documentation](self).
#[derive(Clone, Debug)]
pub struct WarehouseClip {
    spec: WarehouseSpec,
    events: Vec<WarehouseEvent>,
    carts: Vec<(u64, u64)>,
}

impl WarehouseClip {
    /// The clip `spec` describes.
    pub fn new(spec: WarehouseSpec) -> Self {
        let scale = |s: u64| s * 1_000 * spec.duration_ms / HOUR_MS;
        let events = EVENTS
            .iter()
            .map(|&(id, at, length, description, question)| WarehouseEvent {
                id: id.into(),
                description: description.into(),
                question: question.into(),
                at_ms: scale(at),
                until_ms: scale(at) + length * 1_000,
            })
            .collect();
        let carts = CARTS.iter().map(|&(at, length)| (scale(at), length * 1_000)).collect();
        Self { spec, events, carts }
    }

    /// What was drawn.
    pub fn spec(&self) -> &WarehouseSpec {
        &self.spec
    }

    /// The ten events, in time order.
    pub fn events(&self) -> &[WarehouseEvent] {
        &self.events
    }

    /// The event `id`.
    pub fn event(&self, id: &str) -> Option<&WarehouseEvent> {
        self.events.iter().find(|e| e.id == id)
    }

    /// The frames sampled every `every_ms` from time zero, in order, decoded as a frame
    /// source would, each drawn when the stream reaches it.
    pub fn frames(&self, every_ms: u64) -> FrameStream {
        let clip = self.clone();
        let every = every_ms.max(1);
        let count = self.spec.duration_ms.div_ceil(every);
        let source = FrameSourceId(self.spec.camera.clone());
        futures::stream::iter(0..count)
            .map(move |i| {
                let pts = i * every;
                Ok(Frame {
                    source: source.clone(),
                    index: i,
                    pts: Duration::from_millis(pts),
                    captured_at: clip.spec.started_at
                        + chrono::TimeDelta::milliseconds(i64::try_from(pts).unwrap_or(i64::MAX)),
                    pixels: FramePixels::Ephemeral(Arc::new(clip.render(pts))),
                })
            })
            .boxed()
    }

    /// Where event `id` is at `t_ms`: `None` before it starts or after it is over, else how
    /// far into it, in milliseconds.
    fn during(&self, id: &str, t_ms: u64) -> Option<u64> {
        let event = self.event(id)?;
        (event.at_ms..event.until_ms).contains(&t_ms).then(|| t_ms - event.at_ms)
    }

    /// Draws the frame at `t_ms` from the start.
    pub fn render(&self, t_ms: u64) -> DecodedImage {
        let (w, h) = (self.spec.width, self.spec.height);
        let mut c = Canvas::new(w, h);
        // Drawn on a 480×270 plan, scaled to the frame.
        c.scale = (w as f32 / 480.0, h as f32 / 270.0);
        self.background(&mut c, t_ms);
        self.dock(&mut c, t_ms);
        self.racks(&mut c, t_ms);
        self.exit(&mut c, t_ms);
        self.actors(&mut c, t_ms);
        let wall = self.spec.started_at
            + chrono::TimeDelta::milliseconds(i64::try_from(t_ms).unwrap_or(i64::MAX));
        // The camera's own overlay, as on any CCTV recording.
        c.rect(6.0, 6.0, 150.0, 22.0, [0, 0, 0]);
        c.text(10.0, 10.0, 1.6, &format!("CAM 3 {}", wall.format("%H:%M:%S")), [255, 255, 255]);
        if self.during("lights-out", t_ms).is_some() {
            c.dim(0.18);
        }
        c.into_image()
    }

    fn background(&self, c: &mut Canvas, _t_ms: u64) {
        // Wall, then a concrete floor with a yellow lane line.
        c.rect(0.0, 0.0, 480.0, 150.0, [198, 200, 204]);
        c.rect(0.0, 150.0, 480.0, 270.0, [148, 148, 142]);
        c.rect(0.0, 150.0, 480.0, 153.0, [110, 110, 105]);
        c.rect(0.0, 236.0, 480.0, 240.0, [235, 200, 30]);
    }

    fn dock(&self, c: &mut Canvas, t_ms: u64) {
        c.rect(8.0, 46.0, 96.0, 150.0, [70, 72, 76]);
        c.rect(14.0, 30.0, 90.0, 44.0, [30, 50, 110]);
        c.text(22.0, 33.0, 1.4, "DOCK 1", [255, 255, 255]);
        // The door rolls up over 5 s and stays open while the truck is in.
        let open = self.during("truck-docks", t_ms).map_or(0.0, |d| (d as f32 / 5_000.0).min(1.0));
        let bottom = 150.0 - 100.0 * open;
        if open > 0.0 {
            c.rect(14.0, bottom, 90.0, 150.0, [170, 200, 230]);
            c.rect(14.0, 130.0, 90.0, 150.0, [90, 90, 90]);
            if open >= 1.0 {
                // The back of a white truck fills the doorway.
                c.rect(18.0, 56.0, 86.0, 146.0, [244, 244, 240]);
                c.rect(51.0, 58.0, 53.0, 140.0, [150, 150, 150]);
                c.rect(18.0, 138.0, 86.0, 146.0, [60, 60, 60]);
                c.rect(20.0, 128.0, 28.0, 136.0, [220, 30, 30]);
                c.rect(76.0, 128.0, 84.0, 136.0, [220, 30, 30]);
            }
        }
        let mut y = 50.0;
        while y < bottom {
            c.rect(14.0, y, 90.0, (y + 7.0).min(bottom), [132, 134, 138]);
            y += 9.0;
        }
    }

    fn racks(&self, c: &mut Canvas, t_ms: u64) {
        for (n, x0) in [(1, 112.0), (2, 214.0), (3, 316.0)] {
            let x1 = x0 + 92.0;
            c.rect(x0 + 6.0, 34.0, x0 + 86.0, 48.0, [30, 50, 110]);
            c.text(x0 + 14.0, 37.0, 1.4, &format!("AISLE {n}"), [255, 255, 255]);
            c.rect(x0, 56.0, x0 + 5.0, 150.0, [40, 70, 160]);
            c.rect(x1 - 5.0, 56.0, x1, 150.0, [40, 70, 160]);
            for (level, y) in [70.0, 105.0, 140.0].into_iter().enumerate() {
                c.rect(x0, y, x1, y + 4.0, [230, 120, 30]);
                // Cartons on each shelf, widths from the rack and level.
                let mut x = x0 + 7.0;
                let mut k = (n * 7 + level as u32 * 3) % 5;
                while x < x1 - 20.0 {
                    let width = 14.0 + k as f32 * 2.0;
                    let height = 18.0 + (k % 3) as f32 * 4.0;
                    let top_shelf_stack = n == 3 && level == 0 && x > x0 + 40.0;
                    let fallen = self
                        .during("boxes-fall", t_ms)
                        .is_some();
                    if !(top_shelf_stack && fallen) {
                        c.rect(x, y - height, (x + width).min(x1 - 6.0), y, [176, 126, 74]);
                        c.rect(x, y - height, (x + width).min(x1 - 6.0), y - height + 2.0, [140, 96, 52]);
                    }
                    x += width + 3.0;
                    k = (k + 2) % 5;
                }
            }
        }
        // Cartons fall over 2 s and lie on the floor in front of aisle 3.
        if let Some(d) = self.during("boxes-fall", t_ms) {
            let p = (d as f32 / 2_000.0).min(1.0);
            for (i, (dx, tilt)) in [(0.0, 0.3), (24.0, -0.5), (46.0, 0.8), (64.0, 0.1)].into_iter().enumerate() {
                let x = 356.0 + dx;
                let y = 50.0 + (205.0 - 50.0 - i as f32 * 3.0) * p;
                c.box_tilted(x, y, 20.0, 16.0, tilt * p, [176, 126, 74]);
            }
        }
        // Fire on the middle shelf of aisle 2.
        if let Some(d) = self.during("fire", t_ms) {
            let flicker = ((d / 250) % 3) as f32;
            for (i, (dx, h, colour)) in
                [(0.0, 26.0, [240, 90, 20]), (8.0, 34.0, [250, 180, 30]), (16.0, 24.0, [230, 50, 20])]
                    .into_iter()
                    .enumerate()
            {
                let x = 250.0 + dx;
                let top = 105.0 - h - flicker * 2.0 * (i as f32 - 1.0).abs();
                c.triangle([x, 105.0], [x + 12.0, 105.0], [x + 6.0, top], colour);
            }
            for (i, r) in [10.0, 13.0, 16.0].into_iter().enumerate() {
                let rise = (d as f32 / 1_000.0) * 1.5 + i as f32 * 16.0;
                c.disc(262.0 + i as f32 * 5.0, 66.0 - rise.min(40.0), r, [90, 90, 92]);
            }
        }
    }

    fn exit(&self, c: &mut Canvas, t_ms: u64) {
        c.rect(416.0, 56.0, 466.0, 70.0, [20, 150, 60]);
        c.text(424.0, 59.0, 1.4, "EXIT", [255, 255, 255]);
        c.rect(418.0, 74.0, 464.0, 150.0, [96, 120, 102]);
        c.rect(452.0, 108.0, 458.0, 112.0, [220, 220, 220]);
        if self.during("exit-blocked", t_ms).is_some() {
            c.pallet(412.0, 200.0, 60.0, [200, 40, 40], 3);
        }
    }

    fn actors(&self, c: &mut Canvas, t_ms: u64) {
        // A worker pushing a cart, not an event.
        for &(at, length) in &self.carts {
            if (at..at + length).contains(&t_ms) {
                let p = (t_ms - at) as f32 / length as f32;
                let x = 40.0 + 380.0 * p;
                c.worker(x, 214.0, [50, 90, 190], false, t_ms);
                c.rect(x + 10.0, 196.0, x + 44.0, 214.0, [120, 124, 130]);
                c.disc(x + 14.0, 218.0, 4.0, [20, 20, 20]);
                c.disc(x + 40.0, 218.0, 4.0, [20, 20, 20]);
            }
        }
        if let Some(d) = self.during("forklift-aisle-2", t_ms) {
            // From the right edge to the front of aisle 2 in 8 s, then it waits there.
            let p = (d as f32 / 8_000.0).min(1.0);
            let x = 500.0 + (250.0 - 500.0) * p;
            c.forklift(x, 222.0, [235, 130, 20], true, true);
        }
        if let Some(d) = self.during("vest-worker-crosses", t_ms) {
            let x = 20.0 + 440.0 * (d as f32 / 18_000.0);
            c.worker(x, 220.0, [245, 225, 40], true, t_ms);
        }
        if let Some(d) = self.during("spill", t_ms) {
            let r = 6.0 + 40.0 * (d as f32 / 8_000.0).min(1.0);
            c.ellipse(158.0, 214.0, r, r * 0.32, [50, 110, 220]);
            c.ellipse(158.0 + r * 0.4, 216.0, r * 0.5, r * 0.18, [80, 140, 235]);
        }
        if let Some(d) = self.during("ladder-climb", t_ms) {
            c.ladder(178.0, 214.0, 60.0);
            let climb = (d as f32 / 12_000.0).min(1.0);
            c.worker(190.0, 214.0 - 110.0 * climb, [240, 110, 30], true, t_ms);
        }
        if let Some(d) = self.during("near-miss", t_ms) {
            let p = (d as f32 / 5_000.0).min(1.0);
            c.forklift(40.0 + (188.0 - 40.0) * p, 222.0, [40, 90, 200], false, false);
            c.forklift(460.0 + (292.0 - 460.0) * p, 222.0, [235, 130, 20], true, false);
        }
    }
}

/// An RGB canvas on a 480×270 plan.
struct Canvas {
    w: u32,
    h: u32,
    scale: (f32, f32),
    rgb: Vec<[u8; 3]>,
}

/// 5×7 glyphs, one row a byte, the high bit of five the leftmost pixel.
const GLYPHS: &[(char, [u8; 7])] = &[
    ('0', [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E]),
    ('1', [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    ('2', [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F]),
    ('3', [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E]),
    ('4', [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02]),
    ('5', [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E]),
    ('6', [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E]),
    ('7', [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08]),
    ('8', [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E]),
    ('9', [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C]),
    (':', [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00]),
    ('A', [0x0E, 0x11, 0x11, 0x11, 0x1F, 0x11, 0x11]),
    ('C', [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E]),
    ('D', [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E]),
    ('E', [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F]),
    ('I', [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    ('K', [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11]),
    ('L', [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F]),
    ('M', [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11]),
    ('O', [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E]),
    ('S', [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E]),
    ('T', [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04]),
    ('X', [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11]),
];

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self { w, h, scale: (1.0, 1.0), rgb: vec![[0, 0, 0]; (w * h) as usize] }
    }

    fn put(&mut self, x: i64, y: i64, colour: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as u32) < self.w && (y as u32) < self.h {
            self.rgb[(y as u32 * self.w + x as u32) as usize] = colour;
        }
    }

    /// Pixel bounds of a plan rectangle, clipped to the frame.
    fn span(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> (i64, i64, i64, i64) {
        let (sx, sy) = self.scale;
        let clip = |v: f32, max: u32| (v.round() as i64).clamp(0, i64::from(max));
        (clip(x0 * sx, self.w), clip(y0 * sy, self.h), clip(x1 * sx, self.w), clip(y1 * sy, self.h))
    }

    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, colour: [u8; 3]) {
        let (x0, y0, x1, y1) = self.span(x0, y0, x1, y1);
        for y in y0..y1 {
            for x in x0..x1 {
                self.put(x, y, colour);
            }
        }
    }

    fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, colour: [u8; 3]) {
        let (x0, y0, x1, y1) = self.span(cx - rx, cy - ry, cx + rx, cy + ry);
        let (sx, sy) = self.scale;
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = (x as f32 + 0.5) / sx - cx;
                let dy = (y as f32 + 0.5) / sy - cy;
                if (dx / rx).powi(2) + (dy / ry.max(0.5)).powi(2) <= 1.0 {
                    self.put(x, y, colour);
                }
            }
        }
    }

    fn disc(&mut self, cx: f32, cy: f32, r: f32, colour: [u8; 3]) {
        self.ellipse(cx, cy, r, r, colour);
    }

    fn polygon(&mut self, points: &[[f32; 2]], colour: [u8; 3]) {
        let xs = points.iter().map(|p| p[0]);
        let ys = points.iter().map(|p| p[1]);
        let (lo_x, hi_x) = xs.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let (lo_y, hi_y) = ys.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let (x0, y0, x1, y1) = self.span(lo_x, lo_y, hi_x, hi_y);
        let (sx, sy) = self.scale;
        for y in y0..y1 {
            for x in x0..x1 {
                let p = [(x as f32 + 0.5) / sx, (y as f32 + 0.5) / sy];
                let mut inside = false;
                let mut j = points.len() - 1;
                for i in 0..points.len() {
                    let (a, b) = (points[i], points[j]);
                    if (a[1] > p[1]) != (b[1] > p[1])
                        && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
                    {
                        inside = !inside;
                    }
                    j = i;
                }
                if inside {
                    self.put(x, y, colour);
                }
            }
        }
    }

    fn triangle(&mut self, a: [f32; 2], b: [f32; 2], c: [f32; 2], colour: [u8; 3]) {
        self.polygon(&[a, b, c], colour);
    }

    /// A carton `w` by `h` centred on `(cx, cy)`, turned by `angle` radians.
    fn box_tilted(&mut self, cx: f32, cy: f32, w: f32, h: f32, angle: f32, colour: [u8; 3]) {
        let (s, c) = angle.sin_cos();
        let corner = |dx: f32, dy: f32| [cx + dx * c - dy * s, cy + dx * s + dy * c];
        let (hw, hh) = (w / 2.0, h / 2.0);
        self.polygon(&[corner(-hw, -hh), corner(hw, -hh), corner(hw, hh), corner(-hw, hh)], colour);
        let tape = [corner(-1.0, -hh), corner(1.0, -hh), corner(1.0, hh), corner(-1.0, hh)];
        self.polygon(&tape, [140, 96, 52]);
    }

    fn text(&mut self, x: f32, y: f32, size: f32, text: &str, colour: [u8; 3]) {
        let mut pen = x;
        for ch in text.chars() {
            if let Some((_, rows)) = GLYPHS.iter().find(|(g, _)| *g == ch) {
                for (row, bits) in rows.iter().enumerate() {
                    for col in 0..5 {
                        if bits & (0x10 >> col) != 0 {
                            let px = pen + col as f32 * size;
                            let py = y + row as f32 * size;
                            self.rect(px, py, px + size, py + size, colour);
                        }
                    }
                }
            }
            pen += 6.0 * size;
        }
    }

    /// A worker standing on `feet_y` at `x`, in a vest or shirt of `colour`, a hard hat when
    /// `hat`, legs swinging with time.
    fn worker(&mut self, x: f32, feet_y: f32, colour: [u8; 3], hat: bool, t_ms: u64) {
        let swing = if (t_ms / 500) % 2 == 0 { 3.0 } else { -3.0 };
        self.rect(x - 6.0 + swing, feet_y - 24.0, x - 1.0 + swing, feet_y, [40, 50, 80]);
        self.rect(x + 1.0 - swing, feet_y - 24.0, x + 6.0 - swing, feet_y, [40, 50, 80]);
        self.rect(x - 8.0, feet_y - 54.0, x + 8.0, feet_y - 24.0, colour);
        if colour[0] > 200 && colour[1] > 200 {
            // High-visibility vest: a reflective band.
            self.rect(x - 8.0, feet_y - 40.0, x + 8.0, feet_y - 37.0, [210, 210, 210]);
        }
        self.rect(x - 11.0, feet_y - 52.0, x - 8.0, feet_y - 32.0, colour);
        self.rect(x + 8.0, feet_y - 52.0, x + 11.0, feet_y - 32.0, colour);
        self.disc(x, feet_y - 61.0, 7.0, [224, 186, 150]);
        if hat {
            self.ellipse(x, feet_y - 66.0, 8.0, 4.0, [250, 250, 250]);
        }
    }

    /// A forklift on `ground_y` with its body at `x`, its forks to the left when `left`,
    /// carrying a pallet when `loaded`.
    fn forklift(&mut self, x: f32, ground_y: f32, colour: [u8; 3], left: bool, loaded: bool) {
        let dir = if left { -1.0 } else { 1.0 };
        let (body0, body1) = (x - 24.0, x + 24.0);
        self.rect(body0, ground_y - 34.0, body1, ground_y - 8.0, colour);
        // Overhead guard and seat.
        let cab = x - dir * 6.0;
        self.rect(cab - 14.0, ground_y - 66.0, cab + 14.0, ground_y - 62.0, [40, 40, 40]);
        self.rect(cab - 14.0, ground_y - 66.0, cab - 11.0, ground_y - 34.0, [40, 40, 40]);
        self.rect(cab + 11.0, ground_y - 66.0, cab + 14.0, ground_y - 34.0, [40, 40, 40]);
        self.rect(cab - 6.0, ground_y - 46.0, cab + 6.0, ground_y - 34.0, [30, 30, 30]);
        // Mast and forks at the front.
        let front = if left { body0 } else { body1 };
        let mast = (front - dir * 0.0, front + dir * 6.0);
        self.rect(mast.0.min(mast.1), ground_y - 78.0, mast.0.max(mast.1), ground_y - 6.0, [70, 70, 74]);
        let tip = front + dir * 30.0;
        self.rect(front.min(tip), ground_y - 9.0, front.max(tip), ground_y - 6.0, [70, 70, 74]);
        if loaded {
            let (p0, p1) = (front.min(tip), front.max(tip));
            self.rect(p0, ground_y - 14.0, p1, ground_y - 9.0, [150, 105, 60]);
            self.rect(p0 + 2.0, ground_y - 34.0, p1 - 2.0, ground_y - 14.0, [176, 126, 74]);
        }
        self.disc(x - 15.0, ground_y - 6.0, 7.0, [20, 20, 20]);
        self.disc(x + 15.0, ground_y - 6.0, 7.0, [20, 20, 20]);
    }

    /// A wooden pallet `w` wide on `ground_y` at `x`, with `stack` rows of cartons.
    fn pallet(&mut self, x: f32, ground_y: f32, w: f32, colour: [u8; 3], stack: u32) {
        self.rect(x, ground_y - 8.0, x + w, ground_y, [150, 105, 60]);
        for i in 0..3 {
            let bx = x + 2.0 + i as f32 * (w - 4.0) / 3.0;
            self.rect(bx, ground_y - 8.0, bx + 6.0, ground_y, [110, 76, 40]);
        }
        for row in 0..stack {
            let y1 = ground_y - 8.0 - row as f32 * 16.0;
            let mut bx = x + 2.0;
            while bx < x + w - 10.0 {
                self.rect(bx, y1 - 15.0, bx + 17.0, y1, colour);
                bx += 19.0;
            }
        }
    }

    /// A ladder leaning on a rack, from `ground_y` up to `top_y`, its foot at `x`.
    fn ladder(&mut self, x: f32, ground_y: f32, top_y: f32) {
        let grey = [170, 172, 176];
        let lean = 14.0;
        for dx in [0.0, 18.0] {
            self.polygon(
                &[
                    [x + dx, ground_y],
                    [x + dx + 3.0, ground_y],
                    [x + dx + lean + 3.0, top_y],
                    [x + dx + lean, top_y],
                ],
                grey,
            );
        }
        let mut y = ground_y - 12.0;
        while y > top_y + 4.0 {
            let shift = lean * (ground_y - y) / (ground_y - top_y);
            self.rect(x + shift, y, x + shift + 21.0, y + 2.5, grey);
            y -= 14.0;
        }
    }

    fn dim(&mut self, factor: f32) {
        for px in &mut self.rgb {
            for v in px.iter_mut() {
                *v = (f32::from(*v) * factor) as u8;
            }
        }
    }

    fn into_image(self) -> DecodedImage {
        let rgba: Vec<u8> = self.rgb.iter().flat_map(|[r, g, b]| [*r, *g, *b, 255]).collect();
        DecodedImage::from_rgba(self.w, self.h, rgba).expect("the canvas is w × h pixels")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_event_changes_the_picture_only_while_it_lasts() {
        let clip = WarehouseClip::new(WarehouseSpec::default());
        let quiet = clip.render(60_000);
        for event in clip.events() {
            let during = clip.render(event.at_ms + 9_000);
            let after = clip.render(event.until_ms + 1_000);
            assert_ne!(during, quiet, "{} draws nothing", event.id);
            assert_eq!(
                after.pixel(240, 200),
                quiet.pixel(240, 200),
                "{} leaves something behind",
                event.id
            );
        }
    }

    #[test]
    fn scaled_clips_keep_event_order_and_length() {
        let hour = WarehouseClip::new(WarehouseSpec::default());
        let short = WarehouseClip::new(WarehouseSpec::default().with_duration(900_000));
        for (a, b) in hour.events().iter().zip(short.events()) {
            assert_eq!(a.until_ms - a.at_ms, b.until_ms - b.at_ms);
            assert_eq!(b.at_ms, a.at_ms / 4);
        }
        assert!(short.events().windows(2).all(|w| w[0].until_ms <= w[1].at_ms));
    }
}
