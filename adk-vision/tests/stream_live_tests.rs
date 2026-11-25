//! One real run (spec 3.7 §11): the generated conveyor clip served over RTSP by the
//! in-process server, read by `RtspSource`, and RF-DETR Nano detecting on the sampled frames
//! through the stream pipeline. Ignored by default: it fetches RF-DETR Nano into the Hugging
//! Face cache (verified against the built-in manifest) and runs it on ONNX Runtime.
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-vision \
//!     --features rtsp,onnx,stream-fixtures --test stream_live_tests --run-ignored only --no-capture
//! ```
//!
//! RF-DETR is a COCO detector, so it cannot see a printed part's defect; the clip's wall
//! clock is the COCO object it is scored on. Defect detection is S9's, with a fine-tuned
//! detector.

#![cfg(all(feature = "rtsp", feature = "onnx", feature = "stream-fixtures"))]

use std::sync::{Arc, Mutex};
use std::time::Instant;

use adk_artifact::InMemoryArtifactService;
use adk_vision::onnx::{OnnxOptions, RfDetr};
use adk_vision::stream::fixtures::{
    ConveyorClip, ConveyorSpec, RtspTestOptions, RtspTestServer, TestVideo,
};
use adk_vision::stream::{
    FrameBudget, FrameKeeper, KeepReason, Observation, ObservationSink, RtspSource, StreamPipeline,
};
use adk_vision::{
    BBox, DetectQuery, FetchConfig, FrameSourceId, GreedyIouTracker, InMemoryAcceptanceStore,
    ManifestSet, MediaScope, MediaStore, ModelFetcher, Sampling, VisionCx, VisionRuntime,
};

#[derive(Default)]
struct Seen(Mutex<Vec<Observation>>);

#[async_trait::async_trait]
impl ObservationSink for Seen {
    async fn observe(&self, _cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        self.0.lock().unwrap().push(observation);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "fetches and runs RF-DETR Nano; see the module docs"]
async fn rf_detr_detects_on_the_conveyor_clip_read_over_rtsp() {
    let manifest = ManifestSet::builtin().unwrap().get("rf-detr-n").cloned().unwrap();
    let model = tokio::task::spawn_blocking(move || {
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()))
            .fetch_blocking(&manifest)
    })
    .await
    .unwrap()
    .unwrap();
    let detector =
        Arc::new(RfDetr::new(&model, &OnnxOptions::default()).unwrap().with_threshold(0.3));
    let runtime = Arc::new(VisionRuntime::builder().detector(detector).build().unwrap());

    // The first minute of the clip, played at twice real time.
    let clip = Arc::new(ConveyorClip::new(ConveyorSpec::default()));
    let render = Arc::clone(&clip);
    let server = RtspTestServer::start_with(
        TestVideo::Mjpeg {
            render: Arc::new(move |i| render.render(i)),
            frames: 600,
            fps: 10,
            quality: 85,
        },
        RtspTestOptions { speed: 2.0, ..RtspTestOptions::default() },
    )
    .await
    .unwrap();
    let id = FrameSourceId("conveyor".into());
    let source = Arc::new(RtspSource::new(id.clone(), &server.url()).unwrap());
    let seen = Arc::new(Seen::default());
    let started = Instant::now();
    let summary = StreamPipeline::builder(source)
        .sampling(Sampling { fps: 2.0, max_long_edge: None })
        .budget(FrameBudget::new(2.0).with_min_local_fps(0.5))
        .detector(runtime, DetectQuery::Classes(vec!["clock".into(), "person".into()]))
        .tracker(Box::new(GreedyIouTracker::new(0.3, 4)))
        .sink(seen.clone())
        .build()
        .unwrap()
        .run(&VisionCx::new())
        .await
        .unwrap();
    let wall = started.elapsed();
    server.shutdown().await;

    let observations = seen.0.lock().unwrap().clone();
    let [x0, y0, x1, y1] = clip.truth().clock;
    let truth = BBox::new(x0, y0, x1, y1);
    let mut hits = 0;
    let mut ious = Vec::new();
    let mut latencies: Vec<u64> = Vec::new();
    let mut tracks = std::collections::BTreeSet::new();
    let mut others = std::collections::BTreeMap::<String, usize>::new();
    for observation in observations.iter() {
        latencies.extend(observation.calls.iter().map(|c| c.duration_ms));
        let best = observation
            .regions
            .iter()
            .filter(|r| r.label.as_deref() == Some("clock"))
            .map(|r| (r.shape.bbox().unwrap().iou(&truth), r.track))
            .max_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((iou, track)) = best.filter(|(iou, _)| *iou >= 0.5) {
            hits += 1;
            ious.push(iou);
            tracks.extend(track.map(|t| t.0));
        }
        for region in observation.regions.iter().filter(|r| r.label.as_deref() != Some("clock")) {
            *others.entry(region.label.clone().unwrap_or_default()).or_default() += 1;
        }
    }
    latencies.sort_unstable();
    let p50 = latencies.get(latencies.len() / 2).copied().unwrap_or_default();
    let mean_iou = ious.iter().sum::<f32>() / ious.len().max(1) as f32;
    eprintln!(
        "rtsp conveyor + rf-detr-n: {} received, {} observed, {} thinned in {:?}; clock found on {hits}/{} frames (mean IoU {mean_iou:.3}, tracks {tracks:?}); detect p50 {p50} ms; other labels {others:?}",
        summary.received,
        summary.observations,
        summary.thinned,
        wall,
        observations.len()
    );
    assert!(summary.observations >= 50, "{summary:?}");
    assert!(
        hits * 10 >= observations.len() * 9,
        "the clock on at least 90% of frames: {hits}/{}",
        observations.len()
    );
    assert_eq!(tracks.len(), 1, "one anonymous track for one clock");
    assert!(!others.contains_key("person"), "no person in the clip: {others:?}");

    // A frame kept as card evidence is stored by its digest, and loads back.
    let observation = observations.last().unwrap();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let keeper =
        FrameKeeper::new(artifacts.clone(), MediaScope::new("inspector", "u1", "live"), id);
    let kept =
        keeper.keep(&VisionCx::new(), &observation.image, KeepReason::CardEvidence).await.unwrap();
    assert_eq!(kept.image.digest, observation.digest);
    assert_eq!(
        MediaStore::new(artifacts).load(&kept.image).await.unwrap().digest(),
        observation.digest
    );
}
