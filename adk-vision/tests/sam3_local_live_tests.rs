//! Live SAM 3 on the device, beside SAM 2.1 Tiny: the drawn shapes of the 960×540 image
//! `sam_live_tests` sends to `sam-3.1` (concepts, mask IoU and box error against the drawn
//! truth), the same shapes from box prompts, and the twelve S5 widgets, with latency.
//!
//! `#[ignore]`d: it needs SAM 3 weights a person exported and a manifest they wrote
//! (`ADK_VISION_SAM3_MANIFEST`, and `ADK_VISION_SAM3_TRACKER_MANIFEST` for the tracker), and
//! their acceptance of the SAM License recorded in the project
//! (`ADK_VISION_SAM3_PROJECT`, `adk-rust vision models accept <id>`). The test reads that
//! acceptance and records none. SAM 2.1 Tiny q4 comes from the built-in manifest through the
//! Hugging Face cache.
//!
//! ```text
//! ADK_VISION_SAM3_MANIFEST=sam3-local.json ADK_VISION_SAM3_PROJECT=. \
//!     cargo nextest run -p adk-vision --features sam3-local --test sam3_local_live_tests \
//!     --run-ignored only --no-capture
//! ```

#![cfg(feature = "sam3-local")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_vision::onnx::{OnnxExecutionProvider, OnnxOptions, Sam2, Sam3, Sam3Tracker};
use adk_vision::{
    BBox, DecodeLimits, FetchConfig, FileAcceptanceStore, InMemoryAcceptanceStore, ManifestSet,
    Mask, ModelFetcher, ModelManifest, SegmentPrompt, Segmenter, Shape, SourceImage, VerifiedModel,
    VisionCx, VisionRuntime,
};
use serde_json::Value;

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(path)
}

fn load(path: &str) -> SourceImage {
    SourceImage::decode(&std::fs::read(fixture(path)).unwrap(), &DecodeLimits::default()).unwrap()
}

/// `ADK_VISION_ONNX_EP` (`cpu`, `coreml`, `cuda`), the CPU when unset.
fn options() -> OnnxOptions {
    let provider = match std::env::var("ADK_VISION_ONNX_EP").as_deref() {
        Ok("coreml") => OnnxExecutionProvider::CoreMl,
        Ok("cuda") => OnnxExecutionProvider::Cuda,
        _ => OnnxExecutionProvider::Cpu,
    };
    OnnxOptions::default().with_execution_provider(provider)
}

/// The person's own manifest, verified against the acceptances recorded in the project.
fn verified(variable: &str) -> Option<VerifiedModel> {
    let path = std::env::var_os(variable)?;
    let text = std::fs::read_to_string(&path).unwrap();
    let manifest = ModelManifest::from_json(&text).unwrap();
    let project = std::env::var_os("ADK_VISION_SAM3_PROJECT")
        .map_or_else(|| std::env::current_dir().unwrap(), PathBuf::from);
    let acceptances = Arc::new(FileAcceptanceStore::in_project(&project));
    let started = Instant::now();
    let model = ModelFetcher::new(FetchConfig::from_env(), acceptances)
        .fetch_blocking(&manifest)
        .unwrap_or_else(|err| panic!("{}: {err}", manifest.id));
    println!("LIVE sam3-local {}: verified in {:?}", manifest.id, started.elapsed());
    Some(model)
}

/// The drawn shapes of `shapes-960x540.png` as masks, pixel centres inside each shape.
fn truth_masks() -> Vec<(&'static str, Mask)> {
    let (w, h) = (960u32, 540u32);
    let raster = |inside: &dyn Fn(f32, f32) -> bool| {
        let mut bits = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                bits[(y * w + x) as usize] = inside(x as f32 + 0.5, y as f32 + 0.5);
            }
        }
        Mask::encode(w, h, &bits)
    };
    let tri = [[140.0f32, 480.0], [300.0, 480.0], [220.0, 330.0]];
    let in_tri = |x: f32, y: f32| {
        let sign =
            |a: [f32; 2], b: [f32; 2]| (x - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (y - b[1]);
        let (d1, d2, d3) = (sign(tri[0], tri[1]), sign(tri[1], tri[2]), sign(tri[2], tri[0]));
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    vec![
        ("red rectangle", raster(&|x, y| (80.0..320.0).contains(&x) && (60.0..220.0).contains(&y))),
        ("blue circle", raster(&|x, y| (x - 640.0).powi(2) + (y - 150.0).powi(2) <= 90.0 * 90.0)),
        ("green triangle", raster(&in_tri)),
    ]
}

fn truth_boxes() -> Vec<(String, BBox)> {
    let truth: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("cloud/shapes-960x540.groundtruth.json")).unwrap(),
    )
    .unwrap();
    truth["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let b: Vec<f32> =
                o["bbox"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
            (o["label"].as_str().unwrap().to_string(), BBox::new(b[0], b[1], b[2], b[3]))
        })
        .collect()
}

fn iou(a: &Mask, b: &Mask) -> f32 {
    let (a, b) = (a.decode(), b.decode());
    let inter = a.iter().zip(&b).filter(|(x, y)| **x && **y).count();
    let union = a.iter().zip(&b).filter(|(x, y)| **x || **y).count();
    if union == 0 { 0.0 } else { inter as f32 / union as f32 }
}

fn box_error(got: BBox, want: BBox) -> f32 {
    [got.x0 - want.x0, got.y0 - want.y0, got.x1 - want.x1, got.y1 - want.y1]
        .into_iter()
        .map(f32::abs)
        .fold(0f32, f32::max)
}

fn p50(mut runs: Vec<Duration>) -> Duration {
    runs.sort();
    runs[runs.len() / 2]
}

/// Box prompts around each drawn shape: mask IoU against the drawn truth.
async fn boxes_on_shapes(name: &str, segmenter: Arc<dyn Segmenter>) {
    let runtime = VisionRuntime::builder().segmenter(segmenter).build().unwrap();
    let cx = VisionCx::new();
    let shapes = load("cloud/shapes-960x540.png");
    let masks = truth_masks();
    for (label, bbox) in truth_boxes() {
        let truth = &masks.iter().find(|(l, _)| *l == label).unwrap().1;
        let started = Instant::now();
        let found = runtime
            .segment(&cx, &shapes, &[SegmentPrompt::Box { bbox: bbox.expanded(6.0) }])
            .await
            .unwrap()
            .value;
        let elapsed = started.elapsed();
        let Some(Shape::Mask(mask)) = found.first().map(|r| &r.shape) else {
            panic!("{name} {label}: no mask in {found:?}")
        };
        let mask_iou = iou(mask, truth);
        println!("LIVE {name} box {label}: mask_iou {mask_iou:.4} in {elapsed:?}");
        assert!(mask_iou >= 0.9, "{name} {label}: mask IoU {mask_iou:.4}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs SAM 3 weights you exported, your manifest and your recorded acceptance"]
async fn sam3_segments_the_drawn_shapes_and_the_s5_widgets() {
    let Some(model) = verified("ADK_VISION_SAM3_MANIFEST") else {
        panic!("set ADK_VISION_SAM3_MANIFEST to your SAM 3 manifest")
    };
    let started = Instant::now();
    let sam3 = Arc::new(Sam3::new(&model, &options()).unwrap());
    println!(
        "LIVE sam3-local: sessions loaded in {:?} ({})",
        started.elapsed(),
        options().execution_provider
    );
    let runtime = VisionRuntime::builder().segmenter(sam3).build().unwrap();
    let cx = VisionCx::new();

    // Shapes: each concept alone, the first call paying for the image encoder.
    let shapes = load("cloud/shapes-960x540.png");
    let boxes = truth_boxes();
    let mut timings = Vec::new();
    for (name, truth) in &truth_masks() {
        let started = Instant::now();
        let found = runtime
            .segment(&cx, &shapes, &[SegmentPrompt::Concept { text: (*name).to_string() }])
            .await
            .unwrap()
            .value;
        let elapsed = started.elapsed();
        timings.push(elapsed);
        let want = boxes.iter().find(|(l, _)| l == name).unwrap().1;
        let best = found
            .iter()
            .filter_map(|r| match &r.shape {
                Shape::Mask(m) => Some((iou(m, truth), r)),
                _ => None,
            })
            .max_by(|a, b| a.0.total_cmp(&b.0));
        let Some((mask_iou, region)) = best else {
            panic!("sam3-local {name}: no mask among {} regions", found.len())
        };
        let err = box_error(region.shape.bbox().unwrap(), want);
        println!(
            "LIVE sam3-local shapes {name}: mask_iou {mask_iou:.4} box_err_px {err:.1} score {:.3} \
             regions {} in {elapsed:?}",
            region.score.unwrap_or_default(),
            found.len()
        );
        assert!(mask_iou >= 0.9, "{name}: mask IoU {mask_iou:.4}");
    }
    println!(
        "LIVE sam3-local: first call (both encoders and the decoder) {:?}; cached-image calls p50 {:?}",
        timings[0],
        p50(timings[1..].to_vec())
    );

    // S5: one concept for every widget; each target takes the region overlapping it most.
    let s5 = load("s5/s5-screen.png");
    let s5_json: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("s5/s5.json")).unwrap()).unwrap();
    let started = Instant::now();
    let found = runtime
        .segment(&cx, &s5, &[SegmentPrompt::Concept { text: "rectangle".into() }])
        .await
        .unwrap()
        .value;
    let elapsed = started.elapsed();
    let mut errors = Vec::new();
    for target in s5_json["targets"].as_array().unwrap() {
        let b: Vec<f32> =
            target["box"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
        let want = BBox::new(b[0], b[1], b[2], b[3]);
        let err = found
            .iter()
            .filter_map(|r| r.shape.bbox())
            .max_by(|a, b| a.iou(&want).total_cmp(&b.iou(&want)))
            .filter(|b| b.iou(&want) > 0.5)
            .map_or(f32::NAN, |got| box_error(got, want));
        errors.push((target["name"].as_str().unwrap().to_string(), err));
    }
    println!(
        "LIVE sam3-local s5: {} regions for `rectangle` in {elapsed:?}: {errors:?}",
        found.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs SAM 3 tracker weights you exported, your manifest and your recorded acceptance"]
async fn sam3_tracker_segments_the_drawn_shapes_from_boxes() {
    let Some(model) = verified("ADK_VISION_SAM3_TRACKER_MANIFEST") else {
        panic!("set ADK_VISION_SAM3_TRACKER_MANIFEST to your SAM 3 tracker manifest")
    };
    let tracker = Arc::new(Sam3Tracker::new(&model, &options()).unwrap());
    boxes_on_shapes("sam3-tracker-local", tracker).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs SAM 2.1 Tiny q4 (51 MB)"]
async fn sam2_segments_the_same_shapes_from_boxes() {
    let manifest = ManifestSet::builtin().unwrap().get("sam2.1-hiera-tiny-q4").cloned().unwrap();
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let model = fetcher.fetch_blocking(&manifest).unwrap();
    boxes_on_shapes("sam2.1-hiera-tiny-q4", Arc::new(Sam2::new(&model, &options()).unwrap())).await;
}
