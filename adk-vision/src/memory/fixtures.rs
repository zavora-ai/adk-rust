//! The visual memory corpus of scenario S16: a week of Inspector frames and document pages.
//!
//! Every image is drawn from the spec and the item alone — cartons on a conveyor, sealed
//! with packing tape, some with a torn seal or a dented corner on known days, and generated
//! delivery notes, inspection reports and invoices whose data is invented — so the corpus
//! needs no file and the truth is exact.

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use super::VisualKind;
use crate::media::DecodedImage;

/// A carton's defect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Defect {
    /// The tape over the flaps is ripped, the flaps part and a curl of tape stands up.
    TornSeal,
    /// One corner is crushed in.
    DentedCorner,
}

/// A generated document's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageKind {
    /// A delivery note with a table of lines and a round stamp.
    DeliveryNote,
    /// An inspection report with check boxes.
    InspectionReport,
    /// An invoice with a totals block.
    Invoice,
}

/// What to generate.
///
/// # Example
///
/// ```
/// use adk_vision::memory::fixtures::{Defect, WeekCorpus, WeekSpec};
///
/// let corpus = WeekCorpus::new(WeekSpec::default());
/// assert_eq!(corpus.items().len(), 7 * (12 + 3));
/// let tuesday = corpus.day(chrono::Weekday::Tue);
/// assert_eq!(corpus.relevant(Defect::TornSeal, tuesday).len(), 3);
/// assert_eq!(corpus.render(&corpus.items()[0]).size(), (320, 240));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekSpec {
    /// The Monday the week starts on.
    pub monday: NaiveDate,
    /// Conveyor frames a day, split between the cameras.
    pub frames_per_day: usize,
    /// Document pages a day.
    pub pages_per_day: usize,
    /// The cameras, by frame source id.
    pub cameras: Vec<String>,
    /// Torn seals seeded per day.
    pub torn_seals: Vec<(Weekday, usize)>,
    /// Dented corners seeded per day.
    pub dented_corners: Vec<(Weekday, usize)>,
    /// The seed every random choice derives from.
    pub seed: u64,
}

impl Default for WeekSpec {
    /// The scenario's week: 2026-11-16 to 2026-11-22, twelve frames and three pages a day,
    /// two cameras, torn seals on Tuesday (3), Thursday (2) and Saturday (1), dented corners
    /// on Monday, Tuesday and Friday (2 each).
    fn default() -> Self {
        Self {
            monday: NaiveDate::from_ymd_opt(2026, 11, 16).expect("a date"),
            frames_per_day: 12,
            pages_per_day: 3,
            cameras: vec!["conveyor-1".into(), "conveyor-2".into()],
            torn_seals: vec![(Weekday::Tue, 3), (Weekday::Thu, 2), (Weekday::Sat, 1)],
            dented_corners: vec![(Weekday::Mon, 2), (Weekday::Tue, 2), (Weekday::Fri, 2)],
            seed: 0x5EA1_7EA2,
        }
    }
}

/// One generated image and its truth.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekItem {
    /// A stable id, such as `2026-11-17-frame-04`.
    pub id: String,
    /// A frame or a page.
    pub kind: VisualKind,
    /// The camera, or the document a page belongs to.
    pub source: String,
    /// When it was captured.
    pub captured_at: DateTime<Utc>,
    /// What an Inspector or a reader would say about it.
    pub caption: String,
    /// A frame's defect, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defect: Option<Defect>,
    /// A page's kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageKind>,
    /// The seed its picture is drawn from.
    pub seed: u64,
}

/// A week of generated frames and pages with their truth. See the
/// [module documentation](self).
#[derive(Clone, Debug)]
pub struct WeekCorpus {
    spec: WeekSpec,
    items: Vec<WeekItem>,
}

const DAYS: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

impl WeekCorpus {
    /// Generates the week's items; pictures are drawn on [`render`](Self::render).
    pub fn new(spec: WeekSpec) -> Self {
        let mut rng = Rng(spec.seed);
        let mut items = Vec::new();
        let mut carton = 100;
        let mut document = 4_200;
        for (d, weekday) in DAYS.iter().enumerate() {
            let date = spec.monday + Duration::days(d as i64);
            let count = |list: &[(Weekday, usize)]| {
                list.iter().filter(|(w, _)| w == weekday).map(|(_, n)| *n).sum::<usize>()
            };
            let torn = count(&spec.torn_seals);
            let dented = count(&spec.dented_corners);
            // Defective frames: a partial Fisher-Yates over the day's frames.
            let mut order: Vec<usize> = (0..spec.frames_per_day).collect();
            for i in 0..(torn + dented).min(order.len()) {
                let j = i + (rng.next() % (order.len() - i) as u64) as usize;
                order.swap(i, j);
            }
            let start =
                Utc.from_utc_datetime(&date.and_hms_opt(6, 0, 0).expect("a time")).timestamp();
            let span = 12 * 3600 / spec.frames_per_day.max(1) as i64;
            for f in 0..spec.frames_per_day {
                let defect = match order.iter().position(|&o| o == f) {
                    Some(p) if p < torn => Some(Defect::TornSeal),
                    Some(p) if p < torn + dented => Some(Defect::DentedCorner),
                    _ => None,
                };
                let at = start + f as i64 * span + (rng.next() % (span as u64 / 2)) as i64;
                let captured_at = Utc.timestamp_opt(at, 0).single().expect("a time");
                let camera = spec.cameras[f % spec.cameras.len().max(1)].clone();
                carton += 1 + (rng.next() % 9) as usize;
                let state = match defect {
                    Some(Defect::TornSeal) => "seal torn, flaps open",
                    Some(Defect::DentedCorner) => "corner dented",
                    None => "sealed, no defect seen",
                };
                items.push(WeekItem {
                    id: format!("{date}-frame-{f:02}"),
                    kind: VisualKind::Frame,
                    caption: format!(
                        "{camera} at {}: carton {carton:05} on the belt, {state}",
                        captured_at.format("%H:%M")
                    ),
                    source: camera,
                    captured_at,
                    defect,
                    page: None,
                    seed: rng.next(),
                });
            }
            for p in 0..spec.pages_per_day {
                let page = [PageKind::DeliveryNote, PageKind::InspectionReport, PageKind::Invoice]
                    [(d + p) % 3];
                document += 1 + (rng.next() % 5) as usize;
                let at = start + 3_600 + p as i64 * 4 * 3_600 + (rng.next() % 1_800) as i64;
                let captured_at = Utc.timestamp_opt(at, 0).single().expect("a time");
                let (name, source) = match page {
                    PageKind::DeliveryNote => ("delivery note", format!("DN-{document}")),
                    PageKind::InspectionReport => ("inspection report", format!("IR-{document}")),
                    PageKind::Invoice => ("invoice", format!("INV-{document}")),
                };
                items.push(WeekItem {
                    id: format!("{date}-page-{p}"),
                    kind: VisualKind::Page,
                    caption: format!(
                        "{name} {source}, page 1, scanned {}",
                        date.format("%a %d %b")
                    ),
                    source,
                    captured_at,
                    defect: None,
                    page: Some(page),
                    seed: rng.next(),
                });
            }
        }
        Self { spec, items }
    }

    /// The spec.
    pub fn spec(&self) -> &WeekSpec {
        &self.spec
    }

    /// Every item, day by day.
    pub fn items(&self) -> &[WeekItem] {
        &self.items
    }

    /// The date of `weekday` in the week.
    pub fn day(&self, weekday: Weekday) -> NaiveDate {
        self.spec.monday + Duration::days(i64::from(weekday.num_days_from_monday()))
    }

    /// The ids of the frames with `defect` captured on `day`: what a query for them should
    /// find.
    pub fn relevant(&self, defect: Defect, day: NaiveDate) -> Vec<&str> {
        self.items
            .iter()
            .filter(|i| i.defect == Some(defect) && i.captured_at.date_naive() == day)
            .map(|i| i.id.as_str())
            .collect()
    }

    /// Draws an item.
    pub fn render(&self, item: &WeekItem) -> DecodedImage {
        match (item.kind, item.page) {
            (VisualKind::Page, Some(page)) => draw_page(page, item.seed),
            _ => draw_carton(item.defect, item.seed),
        }
    }

    /// An example frame of `defect` that is not in the corpus: the query image of a search
    /// by example.
    pub fn example(&self, defect: Option<Defect>) -> DecodedImage {
        draw_carton(defect, self.spec.seed ^ 0xE8A3_91C5_0000_0001)
    }
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

struct Canvas {
    w: u32,
    h: u32,
    rgb: Vec<[f32; 3]>,
}

impl Canvas {
    fn new(w: u32, h: u32, colour: [u8; 3]) -> Self {
        Self { w, h, rgb: vec![colour.map(f32::from); (w * h) as usize] }
    }

    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, colour: [u8; 3]) {
        let (xa, xb) = (x0.max(0.0) as u32, (x1.min(self.w as f32)).max(0.0) as u32);
        let (ya, yb) = (y0.max(0.0) as u32, (y1.min(self.h as f32)).max(0.0) as u32);
        for y in ya..yb {
            for x in xa..xb {
                self.rgb[(y * self.w + x) as usize] = colour.map(f32::from);
            }
        }
    }

    /// Fills a polygon by the crossing rule at pixel centres.
    fn polygon(&mut self, points: &[[f32; 2]], colour: [u8; 3]) {
        let min_x = points.iter().map(|p| p[0]).fold(f32::MAX, f32::min).max(0.0) as u32;
        let max_x = points.iter().map(|p| p[0]).fold(f32::MIN, f32::max).min(self.w as f32) as u32;
        let min_y = points.iter().map(|p| p[1]).fold(f32::MAX, f32::min).max(0.0) as u32;
        let max_y = points.iter().map(|p| p[1]).fold(f32::MIN, f32::max).min(self.h as f32) as u32;
        for y in min_y..max_y {
            let py = y as f32 + 0.5;
            for x in min_x..max_x {
                let px = x as f32 + 0.5;
                let mut inside = false;
                let mut j = points.len() - 1;
                for i in 0..points.len() {
                    let ([xi, yi], [xj, yj]) = (points[i], points[j]);
                    if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                        inside = !inside;
                    }
                    j = i;
                }
                if inside {
                    self.rgb[(y * self.w + x) as usize] = colour.map(f32::from);
                }
            }
        }
    }

    fn line(&mut self, a: [f32; 2], b: [f32; 2], width: f32, colour: [u8; 3]) {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        let (nx, ny) = (-dy / len * width / 2.0, dx / len * width / 2.0);
        self.polygon(
            &[
                [a[0] + nx, a[1] + ny],
                [b[0] + nx, b[1] + ny],
                [b[0] - nx, b[1] - ny],
                [a[0] - nx, a[1] - ny],
            ],
            colour,
        );
    }

    fn disc(&mut self, cx: f32, cy: f32, r: f32, colour: [u8; 3]) {
        let points: Vec<[f32; 2]> = (0..32)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 32.0;
                [cx + r * a.cos(), cy + r * a.sin()]
            })
            .collect();
        self.polygon(&points, colour);
    }

    /// Scales the light and adds a little sensor noise, then encodes RGBA.
    fn finish(self, rng: &mut Rng, gain: f32) -> DecodedImage {
        let mut rgba = Vec::with_capacity(self.rgb.len() * 4);
        for px in &self.rgb {
            let noise = rng.range(-5.0, 5.0);
            for c in px {
                rgba.push((c * gain + noise).clamp(0.0, 255.0) as u8);
            }
            rgba.push(255);
        }
        DecodedImage::from_rgba(self.w, self.h, rgba).expect("the canvas size matches")
    }
}

fn shade(colour: [u8; 3], by: f32) -> [u8; 3] {
    colour.map(|c| (f32::from(c) * by).clamp(0.0, 255.0) as u8)
}

/// A carton seen from above on the belt, sealed with tape along its flap seam.
fn draw_carton(defect: Option<Defect>, seed: u64) -> DecodedImage {
    let mut rng = Rng(seed);
    let (w, h) = (320.0_f32, 240.0_f32);
    let mut c = Canvas::new(w as u32, h as u32, [92, 94, 100]);
    // Belt between two rails, its slats running across.
    c.rect(0.0, 26.0, w, 214.0, [50, 52, 56]);
    let pitch = rng.range(20.0, 28.0);
    let mut x = rng.range(0.0, pitch);
    while x < w {
        c.rect(x, 26.0, x + 3.0, 214.0, [62, 64, 69]);
        x += pitch;
    }
    c.rect(0.0, 18.0, w, 26.0, [152, 154, 160]);
    c.rect(0.0, 214.0, w, 222.0, [152, 154, 160]);

    let (cw, ch) = (rng.range(140.0, 180.0), rng.range(100.0, 130.0));
    let (cx, cy) = (rng.range(105.0, 215.0), rng.range(108.0, 132.0));
    let (x0, y0, x1, y1) = (cx - cw / 2.0, cy - ch / 2.0, cx + cw / 2.0, cy + ch / 2.0);
    let tone = rng.range(-12.0, 12.0);
    let card = [(196.0 + tone) as u8, (150.0 + tone * 0.8) as u8, (98.0 + tone * 0.6) as u8];
    c.rect(x0 + 5.0, y0 + 6.0, x1 + 5.0, y1 + 6.0, [30, 31, 34]);
    c.rect(x0, y0, x1, y1, card);
    let edge = shade(card, 0.8);
    for (a, b) in
        [([x0, y0], [x1, y0]), ([x1, y0], [x1, y1]), ([x1, y1], [x0, y1]), ([x0, y1], [x0, y0])]
    {
        c.line(a, b, 2.5, edge);
    }
    // The flap seam, and a printed shipping label on most cartons.
    c.line([x0, cy], [x1, cy], 2.0, shade(card, 0.72));
    if rng.unit() < 0.6 {
        let (lx, ly) = (x1 - rng.range(48.0, 58.0), y1 - rng.range(30.0, 36.0));
        c.rect(lx, ly, lx + 40.0, ly + 24.0, [236, 236, 230]);
        let mut bx = lx + 4.0;
        while bx < lx + 36.0 {
            let bw = if rng.unit() < 0.5 { 1.0 } else { 2.0 };
            c.rect(bx, ly + 4.0, bx + bw, ly + 16.0, [24, 24, 26]);
            bx += bw + rng.range(1.0, 2.5);
        }
    }
    // Packing tape over the seam, wrapping the carton's ends.
    let tape = [226, 206, 156];
    let th = ch * 0.17;
    c.rect(x0 - 6.0, cy - th / 2.0, x1 + 6.0, cy + th / 2.0, tape);
    c.rect(x0 - 6.0, cy - th / 2.0, x1 + 6.0, cy - th / 2.0 + 2.0, [240, 226, 188]);

    match defect {
        Some(Defect::TornSeal) => {
            let tx = rng.range(x0 + 0.3 * cw, x0 + 0.7 * cw);
            let gap = rng.range(0.16, 0.24) * cw;
            let (ga, gb) = (tx - gap / 2.0, tx + gap / 2.0);
            // The tape is gone over the tear; the flaps part along the seam.
            c.rect(ga, cy - th / 2.0, gb, cy + th / 2.0, card);
            let open = th * rng.range(0.5, 0.8);
            c.polygon(
                &[[ga - gap * 0.3, cy], [tx, cy - open], [gb + gap * 0.3, cy], [tx, cy + open]],
                [38, 28, 20],
            );
            // Ragged tape ends.
            for (end, dir) in [(ga, 1.0_f32), (gb, -1.0_f32)] {
                let teeth = 4;
                let mut points = vec![[end - dir * 2.0, cy - th / 2.0]];
                for t in 0..=teeth {
                    let y = cy - th / 2.0 + th * t as f32 / teeth as f32;
                    let reach = if t % 2 == 0 { rng.range(4.0, 9.0) } else { rng.range(0.0, 2.0) };
                    points.push([end + dir * reach, y]);
                }
                points.push([end - dir * 2.0, cy + th / 2.0]);
                c.polygon(&points, tape);
            }
            // A curl of torn tape standing up from one end, with its shadow.
            let up = rng.range(18.0, 28.0);
            let lean = rng.range(-10.0, 10.0);
            c.polygon(
                &[
                    [ga + 2.0, cy - th / 2.0 + 4.0],
                    [ga + 12.0, cy - th / 2.0 + 4.0],
                    [ga + 8.0 + lean + 4.0, cy - th / 2.0 - up + 6.0],
                ],
                [40, 34, 28],
            );
            c.polygon(
                &[
                    [ga - 2.0, cy - th / 2.0],
                    [ga + 9.0, cy - th / 2.0],
                    [ga + 4.0 + lean, cy - th / 2.0 - up],
                ],
                [244, 232, 196],
            );
        }
        Some(Defect::DentedCorner) => {
            let corner = (rng.next() % 4) as usize;
            let (sx, sy) = ([1.0, -1.0, -1.0, 1.0][corner], [1.0, 1.0, -1.0, -1.0][corner]);
            let (ax, ay) = ([x0, x1, x1, x0][corner], [y0, y0, y1, y1][corner]);
            let size = rng.range(26.0, 36.0);
            c.polygon(
                &[
                    [ax, ay],
                    [ax + sx * size, ay],
                    [ax + sx * size * 0.55, ay + sy * size * 0.5],
                    [ax, ay + sy * size],
                ],
                shade(card, 0.62),
            );
            for k in 0..3 {
                let t = 0.25 + 0.2 * k as f32;
                c.line(
                    [ax + sx * size * t, ay + sy * 2.0],
                    [ax + sx * 3.0, ay + sy * size * t],
                    1.5,
                    shade(card, 0.45),
                );
            }
        }
        None => {}
    }
    let gain = rng.range(0.88, 1.1);
    c.finish(&mut rng, gain)
}

/// A scanned page: a header band, text lines, and the kind's own block.
fn draw_page(kind: PageKind, seed: u64) -> DecodedImage {
    let mut rng = Rng(seed);
    let (w, h) = (240.0_f32, 320.0_f32);
    let mut c = Canvas::new(w as u32, h as u32, [246, 246, 241]);
    let ink = [40, 42, 48];
    let header = match kind {
        PageKind::DeliveryNote => [36, 72, 128],
        PageKind::InspectionReport => [32, 110, 82],
        PageKind::Invoice => [120, 44, 52],
    };
    c.rect(14.0, 14.0, w - 14.0, 42.0, header);
    c.rect(22.0, 22.0, 22.0 + rng.range(70.0, 110.0), 34.0, [240, 240, 236]);
    let mut y = 54.0;
    for _ in 0..4 {
        c.rect(22.0, y, 22.0 + rng.range(80.0, 180.0), y + 4.0, [120, 122, 128]);
        y += 10.0;
    }
    match kind {
        PageKind::DeliveryNote | PageKind::Invoice => {
            // A table of order lines.
            let (tx0, tx1, ty0) = (20.0, w - 20.0, y + 8.0);
            let rows = 6;
            for r in 0..=rows {
                let ry = ty0 + r as f32 * 18.0;
                c.line([tx0, ry], [tx1, ry], 1.2, ink);
            }
            for col in [0.0, 0.5, 0.7, 1.0] {
                let cx = tx0 + (tx1 - tx0) * col;
                c.line([cx, ty0], [cx, ty0 + rows as f32 * 18.0], 1.2, ink);
            }
            for r in 0..rows {
                let ry = ty0 + r as f32 * 18.0 + 7.0;
                c.rect(tx0 + 4.0, ry, tx0 + 4.0 + rng.range(30.0, 80.0), ry + 4.0, [90, 92, 98]);
                c.rect(tx1 - 30.0, ry, tx1 - 30.0 + rng.range(10.0, 24.0), ry + 4.0, [90, 92, 98]);
            }
            y = ty0 + rows as f32 * 18.0 + 14.0;
            if kind == PageKind::Invoice {
                c.rect(w - 100.0, y, w - 20.0, y + 30.0, [232, 230, 224]);
                c.rect(w - 92.0, y + 12.0, w - 40.0, y + 18.0, ink);
            } else {
                let (sx, sy) = (w - 62.0 + rng.range(-8.0, 8.0), y + 34.0);
                c.disc(sx, sy, 26.0, [176, 40, 44]);
                c.disc(sx, sy, 21.0, [246, 246, 241]);
                c.rect(sx - 14.0, sy - 3.0, sx + 14.0, sy + 3.0, [176, 40, 44]);
            }
        }
        PageKind::InspectionReport => {
            for _ in 0..9 {
                y += 16.0;
                c.rect(22.0, y, 32.0, y + 10.0, ink);
                c.rect(24.0, y + 2.0, 30.0, y + 8.0, [246, 246, 241]);
                if rng.unit() < 0.7 {
                    c.line([24.0, y + 5.0], [27.0, y + 8.0], 1.5, ink);
                    c.line([27.0, y + 8.0], [31.0, y + 1.0], 1.5, ink);
                }
                c.rect(40.0, y + 3.0, 40.0 + rng.range(60.0, 150.0), y + 7.0, [120, 122, 128]);
            }
        }
    }
    // A signature.
    let (mut sx, sy) = (30.0, h - 34.0);
    for _ in 0..8 {
        let nx = sx + rng.range(6.0, 12.0);
        c.line([sx, sy + rng.range(-6.0, 6.0)], [nx, sy + rng.range(-6.0, 6.0)], 1.6, ink);
        sx = nx;
    }
    let gain = rng.range(0.94, 1.04);
    c.finish(&mut rng, gain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_week_has_its_seeded_defects_on_their_days() {
        let corpus = WeekCorpus::new(WeekSpec::default());
        let per = |defect, day| corpus.relevant(defect, corpus.day(day)).len();
        assert_eq!(per(Defect::TornSeal, Weekday::Tue), 3);
        assert_eq!(per(Defect::TornSeal, Weekday::Thu), 2);
        assert_eq!(per(Defect::TornSeal, Weekday::Sat), 1);
        assert_eq!(per(Defect::TornSeal, Weekday::Mon), 0);
        assert_eq!(per(Defect::DentedCorner, Weekday::Tue), 2);
        let ids: std::collections::BTreeSet<_> = corpus.items().iter().map(|i| &i.id).collect();
        assert_eq!(ids.len(), corpus.items().len());
        assert!(corpus.items().iter().all(|i| i.captured_at.date_naive() >= corpus.spec().monday));
    }

    #[test]
    fn pictures_are_deterministic_and_differ_by_defect() {
        let corpus = WeekCorpus::new(WeekSpec::default());
        let item = &corpus.items()[0];
        assert_eq!(corpus.render(item).rgba(), corpus.render(item).rgba());
        let torn = draw_carton(Some(Defect::TornSeal), 7);
        let intact = draw_carton(None, 7);
        let differing = torn.rgba().chunks(4).zip(intact.rgba().chunks(4)).filter(|(a, b)| a != b);
        assert!(differing.count() > 200, "the tear changes the picture");
        let page = corpus.items().iter().find(|i| i.kind == VisualKind::Page).unwrap();
        assert_eq!(corpus.render(page).size(), (240, 320));
    }
}
