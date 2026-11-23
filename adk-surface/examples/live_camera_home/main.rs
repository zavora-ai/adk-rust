//! The live camera surface on the real frame pipeline (spec 3.7 §7.13, §8.2, S12).
//!
//! The generated conveyor clip is served over RTSP by adk-vision's in-process test server and
//! read by `RtspSource` for two cameras. Each runs through `StreamPipeline` — sampler, frame
//! governor, privacy hook, detector, tracker — and `PipelineCameras` reports what it measured to
//! the live camera surface: the picture with its overlays, the tracks, the governor's rate,
//! quota and steps, the watch condition and its zone.
//!
//! | Camera | Budget | Watches |
//! |---|---|---|
//! | Line 2 | 5 a second, 500,000 frames a day | A crack inside the inspection window, 3 frames in a row |
//! | Line 3 | 5 a second, `--line-3-quota` frames a day (2,000): the governor steps down at once | — |
//! | This computer's camera | — | The web home's own camera through `SurfaceCameraSource`, off: the voice screen's, or this browser's from the camera page |
//!
//! The detector here is the belt detector below: a few lines of colour segmentation that find
//! the clip's printed parts and the cracks across them, so the example needs no model weights.
//! Any `Detector` drops in through [`home::Detection`]; the RF-DETR run of the proof uses this
//! same home with `adk_vision::onnx::RfDetr` and a watch on the clip's wall clock.
//!
//! Raising a card from a frame keeps that frame as evidence and sends it to the Inspector,
//! whose stand-in model asks to stop line 2 with the frame as the call's evidence: the call is
//! held for the person, on the web home beside the camera.
//!
//! ```sh
//! cargo run -p adk-surface --features web,vision-camera --example live_camera_home -- 8814
//! ```
//!
//! Then open `http://127.0.0.1:8814/ui/cameras/line-2`; `/ui/cameras/this-computer` takes this
//! browser's camera through **Use this browser's camera**. `GET /e2e/cameras` returns what each
//! pipeline did with its frames and whether its source is open.

mod home;

use std::sync::Arc;

use adk_surface::camera::{LiveWatch, ZoneFacts};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DetectQuery, Detector, Locality, RawRegion, SentImage,
    VisionCx, VisionRuntime, WatchCondition,
};
use async_trait::async_trait;

/// Finds the clip's parts — orange blocks on the belt — and the dark cracks across them.
struct BeltDetector(BackendDescriptor);

/// Connected areas of the pixels `keep` accepts, on a grid of `step` pixels, as boxes.
fn blobs(
    image: &adk_vision::DecodedImage,
    area: [u32; 4],
    step: u32,
    keep: impl Fn([u8; 4]) -> bool,
) -> Vec<([u32; 4], u32)> {
    let [ax0, ay0, ax1, ay1] = area;
    let (cols, rows) = ((ax1 - ax0) / step, (ay1 - ay0) / step);
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

#[async_trait]
impl Detector for BeltDetector {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.0
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        _query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        let pixels = image.pixels();
        let (width, height) = pixels.size();
        let orange = |[r, g, b, _]: [u8; 4]| r > 170 && (70..170).contains(&g) && b < 110;
        // Darker than the part's two holes, which are a mid grey.
        let crack = |[r, g, b, _]: [u8; 4]| {
            0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b) < 36.0
        };
        let mut regions = Vec::new();
        for ([x0, y0, x1, y1], cells) in blobs(pixels, [0, 0, width, height], 2, orange) {
            // A part is a block about 80 by 50 pixels; anything much smaller is a speck.
            if cells < 120 {
                continue;
            }
            regions.push(RawRegion::boxed(
                BBox::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32),
                "part",
                0.96,
            ));
            // A crack is a dark line on the part's face, clear of its edges and its shadow.
            let inner = [x0 + 5, y0 + 5, x1.saturating_sub(5), y1.saturating_sub(5)];
            if inner[2] <= inner[0] + 4 || inner[3] <= inner[1] + 4 {
                continue;
            }
            let cracks = blobs(pixels, inner, 1, crack);
            let Some(([cx0, cy0, cx1, cy1], cells)) =
                cracks.into_iter().filter(|(_, cells)| *cells >= 6).max_by_key(|(_, cells)| *cells)
            else {
                continue;
            };
            // Padded to a box a tracker follows from frame to frame as the belt moves it.
            let (mx, my) = ((cx0 + cx1) as f32 / 2.0, (cy0 + cy1) as f32 / 2.0);
            let (hw, hh) =
                (((cx1 - cx0) as f32 / 2.0).max(28.0), ((cy1 - cy0) as f32 / 2.0).max(18.0));
            let score = (0.7 + cells as f32 / 200.0).min(0.97);
            regions.push(RawRegion::boxed(
                BBox::new(
                    (mx - hw).max(x0 as f32),
                    (my - hh).max(y0 as f32),
                    (mx + hw).min(x1 as f32),
                    (my + hh).min(y1 as f32),
                ),
                "crack",
                score,
            ));
        }
        Ok(regions)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8814), |port| port.parse())?;
    let mut line_3_quota = 2_000;
    let rest: Vec<String> = args.collect();
    if let Some(at) = rest.iter().position(|arg| arg == "--line-3-quota") {
        line_3_quota = rest.get(at + 1).ok_or("--line-3-quota needs a number")?.parse()?;
    }
    let detector = Arc::new(BeltDetector(BackendDescriptor::new(
        "belt",
        Locality::InProcess,
        CoordFrame::SentPixels,
    )));
    let runtime = Arc::new(VisionRuntime::builder().detector(detector).build()?);
    let detection = home::Detection {
        runtime,
        query: DetectQuery::Classes(vec!["part".into(), "crack".into()]),
        watch: LiveWatch::new(
            "crack",
            "Crack",
            WatchCondition::new("crack", ["crack"]).with_min_score(0.6).with_min_frames(3),
        )
        .in_zone("inspection-window")
        .wakes_for("a defect"),
        zone: ZoneFacts {
            id: "inspection-window".into(),
            name: "Inspection window".into(),
            corners: [230.0, 140.0, 410.0, 282.0],
        },
    };
    home::serve(port, detection, line_3_quota, 5.0).await
}
