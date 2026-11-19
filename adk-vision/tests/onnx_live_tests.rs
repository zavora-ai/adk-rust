//! Live local inference on the `ort` backends (spec 3.7 §7.6, §7.18). Ignored by default:
//! the models come from the Hugging Face cache (and are fetched into it when missing), and
//! the photos from the COCO 2017 validation set over plain HTTP, each pinned by SHA-256.
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-vision --features onnx \
//!     --test onnx_live_tests --run-ignored only --no-capture
//! ```
//!
//! | Backend | Manifest | Proof |
//! |---------|----------|-------|
//! | `RfDetr` | `rf-detr-n` (threshold 0.5), `rf-detr-n-q4f16` (0.3) | every COCO ground-truth object found with IoU ≥ 0.75 |
//! | `RfDetr` (instance masks) | `rf-detr-seg-preview` | every object's mask, its bounding box at IoU ≥ 0.75 |
//! | `EfficientSam` | `efficientsam-ti` | drawn shapes segmented with mask IoU ≥ 0.9 |
//! | `Sam2` | `sam2.1-hiera-tiny-q4` | the same |
//! | `SigLip2` | `siglip2-base-patch16-256-quantized` | each caption ranks its photo first, and each photo its caption |
//!
//! Each backend also runs its trait's conformance suite on these models.
//!
//! With `--features onnx-dynamic` and `ORT_DYLIB_PATH` set, the same tests run against an
//! onnxruntime library loaded at run time.

#![cfg(any(feature = "onnx", feature = "onnx-dynamic"))]

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use adk_vision::onnx::{EfficientSam, OnnxExecutionProvider, OnnxOptions, RfDetr, Sam2, SigLip2};
use adk_vision::{
    BBox, DecodeLimits, DecodedImage, DetectQuery, FetchConfig, InMemoryAcceptanceStore,
    ManifestSet, Mask, ModelFetcher, SegmentPrompt, Shape, SourceImage, VerifiedModel, VisionCx,
    VisionRuntime,
};

/// A COCO 2017 validation photo with its ground-truth boxes (`[x0, y0, x1, y1]`, pixels).
struct Photo {
    id: &'static str,
    sha256: &'static str,
    objects: &'static [(&'static str, [f32; 4])],
}

const BEAR: Photo = Photo {
    id: "000000000285",
    sha256: "f3a2974ce3686332609124c70e3e6a2e3aca43fccf1cd1bd7c5c03820977f57d",
    objects: &[("bear", [1.4, 68.8, 586.0, 632.8])],
};

const CAT: Photo = Photo {
    id: "000000004795",
    sha256: "6f8994bac6aef0602d4aa4397ecd4b7d579f5b18f66058a2df021352dd22bdc6",
    objects: &[
        ("cat", [152.8, 113.5, 548.1, 477.9]),
        ("laptop", [0.0, 192.0, 174.7, 474.6]),
        ("tv", [19.9, 2.5, 640.0, 351.6]),
    ],
};

const SHEEP: Photo = Photo {
    id: "000000005992",
    sha256: "c33842fbbd9626843b93ede50dd4908221cb0c9bf9837f58861354eec2a4ec56",
    objects: &[
        ("sheep", [204.9, 158.8, 504.5, 423.2]),
        ("sheep", [425.7, 124.5, 580.8, 411.9]),
        ("sheep", [25.8, 125.2, 235.6, 368.2]),
    ],
};

/// The box IoU every ground-truth object must reach.
const MIN_BOX_IOU: f32 = 0.75;
/// The mask IoU every drawn shape must reach.
const MIN_MASK_IOU: f32 = 0.9;

/// The execution provider from `ADK_VISION_ONNX_EP` (`cpu`, `coreml`, `cuda`), the CPU when
/// unset.
fn options() -> OnnxOptions {
    let provider = match std::env::var("ADK_VISION_ONNX_EP").as_deref() {
        Ok("coreml") => OnnxExecutionProvider::CoreMl,
        Ok("cuda") => OnnxExecutionProvider::Cuda,
        _ => OnnxExecutionProvider::Cpu,
    };
    OnnxOptions::default().with_execution_provider(provider)
}

fn models() -> ManifestSet {
    ManifestSet::builtin().unwrap()
}

fn fetch(id: &str) -> VerifiedModel {
    let manifest = models().get(id).cloned().unwrap_or_else(|| panic!("`{id}` is not built in"));
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let started = Instant::now();
    let model = fetcher.fetch_blocking(&manifest).unwrap();
    eprintln!("{id}: fetched and verified in {:?}", started.elapsed());
    model
}

/// Loads a COCO photo, downloading it once into the system temp dir and checking its digest.
fn photo(photo: &Photo) -> SourceImage {
    let dir = std::env::var_os("ADK_VISION_COCO_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("adk-vision-coco"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.jpg", photo.id));
    if !path.exists() {
        let url = format!("http://images.cocodataset.org/val2017/{}.jpg", photo.id);
        let bytes = ureq::get(&url).call().unwrap().into_body().read_to_vec().unwrap();
        std::fs::write(&path, bytes).unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    let digest = adk_vision::Digest::of(&bytes).to_string();
    assert_eq!(digest, format!("sha256:{}", photo.sha256), "{} changed", path.display());
    SourceImage::decode(&bytes, &DecodeLimits::default()).unwrap()
}

/// p50 and p95 of `runs` timings.
fn percentiles(mut runs: Vec<Duration>) -> (Duration, Duration) {
    runs.sort();
    let at = |q: f64| runs[((runs.len() - 1) as f64 * q).round() as usize];
    (at(0.5), at(0.95))
}

/// Detectors loaded in this process, by manifest id.
type Detectors = std::sync::Mutex<Vec<(String, Arc<RfDetr>)>>;

/// The RF-DETR backend of manifest `id`, reporting detections from `threshold`.
fn detector(id: &str, threshold: f32) -> Arc<RfDetr> {
    static CACHE: OnceLock<Detectors> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap();
    if let Some((_, d)) = cache.iter().find(|(k, _)| k == id) {
        return Arc::clone(d);
    }
    let model = fetch(id);
    let started = Instant::now();
    let d = Arc::new(RfDetr::new(&model, &options()).unwrap().with_threshold(threshold));
    eprintln!("{id}: session loaded in {:?}", started.elapsed());
    cache.push((id.to_string(), Arc::clone(&d)));
    d
}

async fn detects_coco_objects(id: &str, threshold: f32) {
    let segments = detector(id, threshold).segments();
    let runtime = VisionRuntime::builder().detector(detector(id, threshold)).build().unwrap();
    let cx = VisionCx::new();
    let mut timings = Vec::new();
    for p in [&BEAR, &CAT, &SHEEP] {
        let image = photo(p);
        let mut labels: Vec<String> = p.objects.iter().map(|(l, _)| (*l).to_string()).collect();
        labels.dedup();
        let query = DetectQuery::Classes(labels);
        let started = Instant::now();
        let found = runtime.detect(&cx, &image, &query).await.unwrap().value;
        timings.push(started.elapsed());
        for (label, [x0, y0, x1, y1]) in p.objects {
            let truth = BBox::new(*x0, *y0, *x1, *y1);
            let best = found
                .iter()
                .filter(|r| r.label.as_deref() == Some(label))
                .filter_map(|r| r.shape.bbox().map(|b| (b.iou(&truth), b, r.score)))
                .max_by(|a, b| a.0.total_cmp(&b.0));
            let Some((iou, b, score)) = best else {
                panic!("{id} found no {label} in {}: {found:?}", p.id);
            };
            if segments {
                assert!(
                    found.iter().all(|r| matches!(r.shape, Shape::Mask(_))),
                    "{id} is a segmenting export and answered without masks"
                );
            }
            eprintln!(
                "{id} | {} | {label} | truth [{x0:.1}, {y0:.1}, {x1:.1}, {y1:.1}] | found [{:.1}, {:.1}, {:.1}, {:.1}] | IoU {iou:.3} | score {:.3}",
                p.id,
                b.x0,
                b.y0,
                b.x1,
                b.y1,
                score.unwrap_or_default()
            );
            assert!(iou >= MIN_BOX_IOU, "{id}: {label} in {} at IoU {iou:.3}", p.id);
        }
    }
    let image = photo(&CAT);
    let query = DetectQuery::Classes(Vec::new());
    for _ in 0..10 {
        let started = Instant::now();
        runtime.detect(&cx, &image, &query).await.unwrap();
        timings.push(started.elapsed());
    }
    let (p50, p95) = percentiles(timings);
    eprintln!("{id}: per image p50 {p50:?}, p95 {p95:?} ({})", options().execution_provider);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs RF-DETR Nano (108 MB) and three COCO photos"]
async fn live_rf_detr_nano_finds_coco_ground_truth() {
    detects_coco_objects("rf-detr-n", 0.5).await;
}

/// RF-DETR-Seg's preview checkpoint: every detection is an instance mask, whose bounding box
/// is held to the same IoU as a detector's box.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs RF-DETR-Seg preview (134 MB) and three COCO photos"]
async fn live_rf_detr_seg_preview_finds_coco_ground_truth() {
    detects_coco_objects("rf-detr-seg-preview", 0.5).await;
}

/// The 4-bit export scores the same objects lower (the bear at 0.43 against 0.97), so it
/// reports from 0.3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs RF-DETR Nano q4f16 (19 MB) and three COCO photos"]
async fn live_rf_detr_nano_q4f16_finds_coco_ground_truth() {
    detects_coco_objects("rf-detr-n-q4f16", 0.3).await;
}

/// A white 640×480 canvas with a red disc and a blue rectangle, and their exact bitmaps.
fn shapes() -> (SourceImage, Vec<(BBox, Vec<bool>)>) {
    let (w, h) = (640u32, 480u32);
    let disc = |x: u32, y: u32| {
        let (dx, dy) = (x as f32 + 0.5 - 200.0, y as f32 + 0.5 - 200.0);
        dx * dx + dy * dy <= 110.0 * 110.0
    };
    let rect = BBox::new(380.0, 260.0, 580.0, 420.0);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    let mut disc_bits = Vec::with_capacity((w * h) as usize);
    let mut rect_bits = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let (d, r) = (disc(x, y), rect.contains([x as f32 + 0.5, y as f32 + 0.5]));
            disc_bits.push(d);
            rect_bits.push(r);
            rgba.extend_from_slice(match (d, r) {
                (true, _) => &[220, 30, 30, 255],
                (_, true) => &[30, 60, 220, 255],
                _ => &[255, 255, 255, 255],
            });
        }
    }
    let image = SourceImage::from_pixels(DecodedImage::from_rgba(w, h, rgba).unwrap()).unwrap();
    (image, vec![(BBox::new(90.0, 90.0, 310.0, 310.0), disc_bits), (rect, rect_bits)])
}

fn mask_iou(mask: &Mask, truth: &[bool]) -> f32 {
    let bits = mask.decode();
    let inter = bits.iter().zip(truth).filter(|(a, b)| **a && **b).count();
    let union = bits.iter().zip(truth).filter(|(a, b)| **a || **b).count();
    inter as f32 / union.max(1) as f32
}

async fn segments_shapes(runtime: &VisionRuntime, id: &str) {
    let (image, truths) = shapes();
    let cx = VisionCx::new();
    let cases: Vec<(&str, Vec<SegmentPrompt>)> = vec![
        (
            "boxes",
            truths.iter().map(|(b, _)| SegmentPrompt::Box { bbox: b.expanded(6.0) }).collect(),
        ),
        ("disc point", vec![SegmentPrompt::Point { at: [200.0, 200.0], positive: true }]),
    ];
    let mut timings = Vec::new();
    for (name, prompts) in cases {
        let started = Instant::now();
        let found = runtime.segment(&cx, &image, &prompts).await.unwrap().value;
        timings.push(started.elapsed());
        let expected = if name == "boxes" { truths.len() } else { 1 };
        assert_eq!(found.len(), expected, "{id} {name}: {found:?}");
        for (region, (_, truth)) in found.iter().zip(&truths) {
            let Shape::Mask(mask) = &region.shape else {
                panic!("{id} returned {:?}", region.shape)
            };
            assert_eq!((mask.width, mask.height), (640, 480));
            let iou = mask_iou(mask, truth);
            eprintln!(
                "{id} | {name} | mask IoU {iou:.4} | area {} | predicted IoU {:.3}",
                mask.area(),
                region.score.unwrap_or_default()
            );
            assert!(iou >= MIN_MASK_IOU, "{id} {name}: mask IoU {iou:.4}");
        }
    }
    // Later prompts on the same image reuse the encoder's embedding.
    for _ in 0..5 {
        let started = Instant::now();
        runtime
            .segment(&cx, &image, &[SegmentPrompt::Point { at: [480.0, 340.0], positive: true }])
            .await
            .unwrap();
        timings.push(started.elapsed());
    }
    eprintln!(
        "{id}: first call (encoder and decoder) {:?}; cached-image calls p50 {:?} ({})",
        timings[0],
        percentiles(timings[2..].to_vec()).0,
        options().execution_provider
    );
}

fn efficient_sam() -> Arc<EfficientSam> {
    static CACHE: OnceLock<Arc<EfficientSam>> = OnceLock::new();
    Arc::clone(CACHE.get_or_init(|| {
        let model = fetch("efficientsam-ti");
        let started = Instant::now();
        let s = Arc::new(EfficientSam::new(&model, &options()).unwrap());
        eprintln!("efficientsam-ti: sessions loaded in {:?}", started.elapsed());
        s
    }))
}

fn sam2() -> Arc<Sam2> {
    static CACHE: OnceLock<Arc<Sam2>> = OnceLock::new();
    Arc::clone(CACHE.get_or_init(|| {
        let model = fetch("sam2.1-hiera-tiny-q4");
        let started = Instant::now();
        let s = Arc::new(Sam2::new(&model, &options()).unwrap());
        eprintln!("sam2.1-hiera-tiny-q4: sessions loaded in {:?}", started.elapsed());
        s
    }))
}

fn siglip() -> Arc<SigLip2> {
    static CACHE: OnceLock<Arc<SigLip2>> = OnceLock::new();
    Arc::clone(CACHE.get_or_init(|| {
        let model = fetch("siglip2-base-patch16-256-quantized");
        let started = Instant::now();
        let s = Arc::new(SigLip2::new(&model, &options()).unwrap());
        eprintln!("siglip2-base-patch16-256-quantized: sessions loaded in {:?}", started.elapsed());
        s
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs EfficientSAM-Ti (41 MB)"]
async fn live_efficientsam_segments_drawn_shapes() {
    let runtime = VisionRuntime::builder().segmenter(efficient_sam()).build().unwrap();
    segments_shapes(&runtime, "efficientsam-ti").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs SAM 2.1 Tiny q4 (51 MB)"]
async fn live_sam2_segments_drawn_shapes() {
    let runtime = VisionRuntime::builder().segmenter(sam2()).build().unwrap();
    segments_shapes(&runtime, "sam2.1-hiera-tiny-q4").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs SigLIP 2 base quantized (381 MB) and three COCO photos"]
async fn live_siglip2_ranks_the_right_photo_first() {
    use adk_vision::CallOptions;
    let runtime = VisionRuntime::builder().embedder(siglip()).build().unwrap();
    let cx = VisionCx::new();
    let images = [photo(&BEAR), photo(&CAT), photo(&SHEEP)];
    let captions = ["a photo of a bear", "a photo of a cat", "a photo of sheep in a field"];
    let started = Instant::now();
    let image_vectors = runtime
        .embed_images(&cx, &images.iter().collect::<Vec<_>>(), &CallOptions::default())
        .await
        .unwrap()
        .value;
    let image_time = started.elapsed();
    let started = Instant::now();
    let text_vectors =
        runtime.embed_texts(&cx, &captions, &CallOptions::default()).await.unwrap().value;
    let text_time = started.elapsed();
    for v in image_vectors.iter().chain(&text_vectors) {
        assert_eq!(v.len(), 768);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "a vector of length {norm}");
    }
    let cosine = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    for (t, caption) in captions.iter().enumerate() {
        let scores: Vec<f32> = image_vectors.iter().map(|i| cosine(&text_vectors[t], i)).collect();
        eprintln!("siglip2 | {caption} | cosine to bear, cat, sheep: {scores:.4?}");
        let best = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert_eq!(best, t, "`{caption}` ranked photo {best} first");
    }
    for (i, image) in image_vectors.iter().enumerate() {
        let best = text_vectors
            .iter()
            .enumerate()
            .max_by(|a, b| cosine(image, a.1).total_cmp(&cosine(image, b.1)))
            .unwrap()
            .0;
        assert_eq!(best, i, "photo {i} ranked caption {best} first");
    }
    eprintln!(
        "siglip2: 3 images in {image_time:?}, 3 texts in {text_time:?} ({}, one batch each)",
        options().execution_provider
    );
}

mod rf_detr_conformance {
    use adk_vision::conformance::{DetectorTarget, Expected, Fixture};

    use super::*;

    fn bear() -> Fixture {
        let mut fixture = Fixture::shapes();
        fixture.image = photo(&BEAR);
        let [x0, y0, x1, y1] = BEAR.objects[0].1;
        fixture.objects =
            vec![Expected { label: Some("bear".into()), bbox: BBox::new(x0, y0, x1, y1) }];
        // RF-DETR Nano's box edges land within 1.4 px of COCO's; 8 px leaves room for
        // onnxruntime builds that round differently.
        fixture.tolerance_px = 8.0;
        fixture.query = DetectQuery::Classes(vec!["bear".into()]);
        fixture
    }

    adk_vision::detector_conformance_tests! {
        #[ignore = "needs RF-DETR Nano and a COCO photo"]
        backend = DetectorTarget::Local(detector("rf-detr-n", 0.5)),
        fixture = bear(),
    }
}

/// The conformance shapes with a box prompt around each, in place of the concept prompts
/// a point-and-box segmenter does not take; masks within 4 px of each box.
fn boxed_shapes() -> adk_vision::conformance::Fixture {
    let mut fixture = adk_vision::conformance::Fixture::shapes();
    fixture.prompts =
        fixture.objects.iter().map(|o| SegmentPrompt::Box { bbox: o.bbox.expanded(6.0) }).collect();
    fixture.tolerance_px = 4.0;
    fixture
}

mod efficientsam_conformance {
    use adk_vision::conformance::SegmenterTarget;

    use super::*;

    adk_vision::segmenter_conformance_tests! {
        #[ignore = "needs EfficientSAM-Ti"]
        backend = SegmenterTarget::Local(efficient_sam()),
        fixture = boxed_shapes(),
    }
}

mod sam2_conformance {
    use adk_vision::conformance::SegmenterTarget;

    use super::*;

    adk_vision::segmenter_conformance_tests! {
        #[ignore = "needs SAM 2.1 Tiny q4"]
        backend = SegmenterTarget::Local(sam2()),
        fixture = boxed_shapes(),
    }
}

mod siglip2_conformance {
    use adk_vision::conformance::EmbedderTarget;

    use super::*;

    adk_vision::embedder_conformance_tests! {
        #[ignore = "needs SigLIP 2 base quantized"]
        backend = EmbedderTarget::Local(siglip()),
    }
}
