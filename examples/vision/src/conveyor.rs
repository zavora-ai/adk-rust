//! What the phase B scenarios share: the conveyor clip, the Inspector's defect detector, the
//! clip served over RTSP or replayed at its own capture times, and sinks that keep what a
//! stream did.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_vision::stream::fixtures::{
    ConveyorClip, ConveyorSpec, RtspTestOptions, RtspTestServer, TestVideo,
};
use adk_vision::stream::{
    FrameSource, FrameStream, Observation, ObservationSink, Sampler, SourceEvent,
};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodedImage, DetectQuery, Detector, Frame, FramePixels,
    FrameSourceId, Locality, RawRegion, Sampling, SentImage, VisionCx, WatchEvent, WatchSink,
};
use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use futures::StreamExt;

/// The Inspector's clip (S9, S10): the scenario clip's 200 parts and twelve seeded cracks,
/// drawn at 1280×720 and 6 px/mm so a crack of 18–24 mm spans 108–144 px, on a belt slowed to
/// 24 mm/s so a part moves 29 px between frames sampled at 5 a second. The fiducial is 20 mm,
/// which fits below the belt at this scale.
pub fn inspector_spec() -> ConveyorSpec {
    ConveyorSpec {
        width: 1280,
        height: 720,
        px_per_mm: 6.0,
        belt_mm_per_s: 24.0,
        fiducial_mm: 20.0,
        defect_mm: [18.0, 24.0],
        ..ConveyorSpec::default()
    }
}

/// The defective parts' seeded lengths in millimetres, by part number.
pub fn seeded(clip: &ConveyorClip) -> Vec<(usize, f32)> {
    clip.truth()
        .parts
        .iter()
        .filter_map(|p| p.defect.as_ref().map(|d| (p.index, d.length_mm)))
        .collect()
}

/// The clip frame shown at `pts_ms` into a stream that plays the clip from `from_frame`.
pub fn clip_frame(clip: &ConveyorClip, from_frame: u64, pts_ms: u64) -> u64 {
    from_frame + (pts_ms * u64::from(clip.truth().spec.fps) + 500) / 1000
}

/// The defective part whose crack lies inside `bbox` in clip frame `index`, if any.
pub fn defect_in(clip: &ConveyorClip, index: u64, bbox: &BBox) -> Option<usize> {
    clip.visible(index).into_iter().find_map(|part| {
        let [ax, ay, bx, by] = part.defect?;
        let (mx, my) = ((ax + bx) / 2.0, (ay + by) / 2.0);
        (mx >= bbox.x0 && mx <= bbox.x1 && my >= bbox.y0 && my <= bbox.y1).then_some(part.index)
    })
}

/// Serves `frames` frames of `clip` over the in-process RTSP server at `fps` a second, frame
/// `i` of the stream being the clip's frame `from + i * step`, `speed` times real time.
pub async fn serve(
    clip: &Arc<ConveyorClip>,
    from: u64,
    step: u64,
    frames: u64,
    fps: u32,
    speed: f64,
) -> Result<RtspTestServer> {
    let render = Arc::clone(clip);
    Ok(RtspTestServer::start_with(
        TestVideo::Mjpeg {
            render: Arc::new(move |i| render.render(from + i * step)),
            frames,
            fps,
            quality: 92,
        },
        RtspTestOptions { speed, ..RtspTestOptions::default() },
    )
    .await?)
}

/// How fast the RTSP scenarios play the clip: `VISION_STREAM_SPEED`, else twice real time,
/// ten 1280×720 frames a second, which a CI runner keeps up with.
pub fn stream_speed() -> f64 {
    std::env::var("VISION_STREAM_SPEED").ok().and_then(|v| v.parse().ok()).unwrap_or(2.0)
}

/// Connected areas of the pixels `keep` accepts inside `area`, on a grid of `step` pixels:
/// each as its box in pixels and its count of grid cells.
fn blobs(
    image: &DecodedImage,
    area: [u32; 4],
    step: u32,
    keep: impl Fn([u8; 4]) -> bool,
) -> Vec<([u32; 4], u32)> {
    let [ax0, ay0, ax1, ay1] = area;
    let (cols, rows) = ((ax1.saturating_sub(ax0)) / step, (ay1.saturating_sub(ay0)) / step);
    let mut seen = vec![false; (cols * rows) as usize];
    let hit = |c: u32, r: u32| keep(image.pixel(ax0 + c * step, ay0 + r * step));
    let mut found = Vec::new();
    for start in 0..cols * rows {
        let (c, r) = (start % cols, start / cols);
        if seen[start as usize] || !hit(c, r) {
            continue;
        }
        let mut stack = vec![(c, r)];
        seen[start as usize] = true;
        let (mut x0, mut y0, mut x1, mut y1, mut count) = (c, r, c, r, 0u32);
        while let Some((c, r)) = stack.pop() {
            count += 1;
            (x0, y0, x1, y1) = (x0.min(c), y0.min(r), x1.max(c), y1.max(r));
            let around = [(c.wrapping_sub(1), r), (c + 1, r), (c, r.wrapping_sub(1)), (c, r + 1)];
            for (nc, nr) in around {
                if nc < cols && nr < rows && !seen[(nr * cols + nc) as usize] && hit(nc, nr) {
                    seen[(nr * cols + nc) as usize] = true;
                    stack.push((nc, nr));
                }
            }
        }
        found.push((
            [ax0 + x0 * step, ay0 + y0 * step, ax0 + (x1 + 1) * step, ay0 + (y1 + 1) * step],
            count,
        ));
    }
    found
}

fn luma([r, g, b, _]: [u8; 4]) -> f32 {
    0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)
}

/// A crack the detector found: its tight box and its length along the box's diagonal.
#[derive(Clone, Copy, Debug)]
pub struct Crack {
    pub bbox: BBox,
    pub score: f32,
}

/// The Inspector's defect detector, standing in for the RF-DETR fine-tune a user would
/// register: colour segmentation that finds the printed parts, the dark cracks across them
/// and the fiducial below the belt, on the device. A part or crack is reported only while
/// the part is wholly in view, as an inspection station looks at it.
///
/// A crack's score grows with its length against half the part's: a short crack is the
/// uncertain one S11 escalates.
pub struct BeltDetector {
    descriptor: BackendDescriptor,
}

impl BeltDetector {
    pub fn new() -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "inspector-defects",
                Locality::InProcess,
                CoordFrame::SentPixels,
            ),
        }
    }

    /// Parts, as boxes, and the crack on each.
    pub fn find(image: &DecodedImage) -> Vec<(BBox, Option<Crack>)> {
        let (width, height) = image.size();
        let orange = |[r, g, b, _]: [u8; 4]| r > 170 && (70..175).contains(&g) && b < 110;
        // Darker than the part's holes (a mid grey) and its print layers.
        let dark = |p: [u8; 4]| luma(p) < 40.0;
        let belt = [0, (0.38 * height as f32) as u32, width, (0.80 * height as f32) as u32];
        let mut out = Vec::new();
        for ([x0, y0, x1, y1], cells) in blobs(image, belt, 4, orange) {
            let (w, h) = (x1 - x0, y1 - y0);
            // A part is a block of 32 × 20 mm; specks and slivers at the frame's edge are not.
            if cells < 40 || x0 <= 4 || x1 + 4 >= width || w < h {
                continue;
            }
            let part = BBox::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32);
            let inner = [x0 + 6, y0 + 6, x1.saturating_sub(6), y1.saturating_sub(6)];
            // A crack lies on the part's face; anything dark reaching the edge of the search
            // is the belt or the shadow showing past a rounded corner.
            let clear = |[bx0, by0, bx1, by1]: [u32; 4]| {
                bx0 > inner[0] && by0 > inner[1] && bx1 < inner[2] && by1 < inner[3]
            };
            let crack = blobs(image, inner, 1, dark)
                .into_iter()
                .filter(|(bbox, cells)| *cells >= 8 && clear(*bbox))
                .max_by_key(|(_, cells)| *cells)
                .map(|([cx0, cy0, cx1, cy1], _)| {
                    let bbox = BBox::new(cx0 as f32, cy0 as f32, cx1 as f32, cy1 as f32);
                    let length = (bbox.width().powi(2) + bbox.height().powi(2)).sqrt();
                    let score = (0.55 + 0.45 * length / (0.5 * w as f32)).min(0.97);
                    Crack { bbox, score }
                });
            out.push((part, crack));
        }
        out
    }

    /// The fiducial: the near-black square below the belt.
    pub fn fiducial(image: &DecodedImage) -> Option<BBox> {
        let (width, height) = image.size();
        let below = [0, (0.80 * height as f32) as u32, width / 3, height];
        blobs(image, below, 2, |p| luma(p) < 25.0)
            .into_iter()
            .max_by_key(|(_, cells)| *cells)
            .filter(|(_, cells)| *cells >= 25)
            .map(|([x0, y0, x1, y1], _)| BBox::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32))
    }
}

#[async_trait::async_trait]
impl Detector for BeltDetector {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        let wants = |label: &str| match query {
            DetectQuery::Classes(classes) => classes.iter().any(|c| c == label),
            _ => true,
        };
        let pixels = image.pixels();
        let mut regions = Vec::new();
        for (part, crack) in Self::find(pixels) {
            if wants("part") {
                regions.push(RawRegion::boxed(part, "part", 0.96));
            }
            if let Some(crack) = crack.filter(|_| wants("crack")) {
                regions.push(RawRegion::boxed(crack.bbox, "crack", crack.score));
            }
        }
        if wants("fiducial")
            && let Some(fiducial) = Self::fiducial(pixels)
        {
            regions.push(RawRegion::boxed(fiducial, "fiducial", 0.99));
        }
        Ok(regions)
    }
}

/// The conveyor clip looped `loops` times as a frame source whose frames carry the capture
/// times a camera started at `start` would give them, rendered only when the sampler keeps
/// them and only as fast as the pipeline reads them, so nothing is dropped.
pub struct ClipSource {
    id: FrameSourceId,
    clip: Arc<ConveyorClip>,
    loops: u64,
    start: DateTime<Utc>,
}

impl ClipSource {
    pub fn new(id: &str, clip: Arc<ConveyorClip>, loops: u64, start: DateTime<Utc>) -> Self {
        Self { id: FrameSourceId(id.into()), clip, loops, start }
    }
}

#[async_trait::async_trait]
impl FrameSource for ClipSource {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> adk_vision::Result<FrameStream> {
        let mut sampler = Sampler::new(sampling)?;
        let (id, clip, start) = (self.id.clone(), Arc::clone(&self.clip), self.start);
        let per_loop = clip.frame_count();
        let fps = u64::from(clip.truth().spec.fps);
        let frames = (0..per_loop * self.loops).filter_map(move |index| {
            let pts = Duration::from_millis(index * 1000 / fps);
            sampler.admit(pts).then_some((index, pts))
        });
        Ok(futures::stream::iter(frames)
            .map(move |(index, pts)| {
                let pixels = clip.render(index % per_loop);
                Ok(Frame {
                    source: id.clone(),
                    index,
                    pts,
                    captured_at: start
                        + TimeDelta::from_std(pts).unwrap_or_else(|_| TimeDelta::zero()),
                    pixels: FramePixels::Ephemeral(Arc::new(pixels)),
                })
            })
            .boxed())
    }
}

/// One crack seen on one frame: the frame's presentation time, the region's box and track.
#[derive(Clone, Debug)]
pub struct Seen {
    pub pts_ms: u64,
    pub bbox: BBox,
    pub track: Option<u64>,
}

/// Keeps every crack an observation held, the frames observed, and the stream's events.
#[derive(Default)]
pub struct StreamLog {
    pub cracks: Mutex<Vec<Seen>>,
    pub observed: Mutex<u64>,
    pub events: Mutex<Vec<SourceEvent>>,
}

impl StreamLog {
    pub fn cracks(&self) -> Vec<Seen> {
        self.cracks.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn observed(&self) -> u64 {
        *self.observed.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn events(&self) -> Vec<SourceEvent> {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn record(&self, observation: &Observation) {
        *self.observed.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        let mut cracks = self.cracks.lock().unwrap_or_else(|e| e.into_inner());
        for region in &observation.regions {
            if region.label.as_deref() == Some("crack")
                && let Some(bbox) = region.shape.bbox()
            {
                cracks.push(Seen {
                    pts_ms: observation.frame.pts_ms,
                    bbox,
                    track: region.track.map(|t| t.0),
                });
            }
        }
    }
}

#[async_trait::async_trait]
impl ObservationSink for StreamLog {
    async fn observe(&self, _cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        self.record(&observation);
        Ok(())
    }

    async fn source_event(&self, _cx: &VisionCx, event: &SourceEvent) -> adk_vision::Result<()> {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).push(event.clone());
        Ok(())
    }
}

/// The wakes a watch delivered.
#[derive(Default)]
pub struct WakeLog(Mutex<Vec<WatchEvent>>);

impl WakeLog {
    pub fn events(&self) -> Vec<WatchEvent> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl WatchSink for WakeLog {
    async fn deliver(&self, event: &WatchEvent) -> adk_vision::Result<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event.clone());
        Ok(())
    }
}
