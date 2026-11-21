//! S13's local path: Qwen3-VL-2B in this process (feature `mistralrs`) answers "When does
//! the forklift enter the aisle?" over frames sampled from `aisle-forklift-120s.mp4`.
//!
//! `#[ignore]`d: it loads `Qwen/Qwen3-VL-2B-Instruct` (4.26 GB) through the fetcher and
//! samples the clip with a user-installed `ffmpeg`, as a frame source would.
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo test -p adk-vision --features mistralrs \
//!     --test video_local_live_tests -- --ignored --nocapture
//! ```

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use adk_core::{BudgetTracker, ModelCallScope, RunBudget, UsageUnit};
use adk_mistralrs::MistralRsConfig;
use adk_vision::mistralrs::QWEN3_VL_2B;
use adk_vision::{
    DecodeLimits, EgressAction, EgressConfig, EgressGate, FetchConfig, FrameMeta, FrameSourceId,
    FramesAnalyzer, InMemoryAcceptanceStore, InMemoryEgressRecorder, LlmDescriber, ManifestSet,
    ModelFetcher, Sampling, SourceImage, VideoRequest, VideoSource, VisionCx, VisionRuntime,
};

/// One frame every `every_s` seconds, `width` pixels wide, decoded by `ffmpeg`.
fn sample(clip: &Path, every_s: u64, width: u32) -> Vec<(FrameMeta, SourceImage)> {
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i"])
        .arg(clip)
        .args(["-vf", &format!("fps=1/{every_s},scale={width}:-2")])
        .arg(dir.path().join("f%03d.png"))
        .status()
        .expect("ffmpeg is installed");
    assert!(status.success());
    let mut files: Vec<_> =
        std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let meta = FrameMeta {
                source: FrameSourceId("aisle".into()),
                index: i as u64,
                pts_ms: i as u64 * every_s * 1_000,
                captured_at: chrono::Utc::now(),
            };
            let image =
                SourceImage::decode(&std::fs::read(path).unwrap(), &DecodeLimits::default())
                    .unwrap();
            (meta, image)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "loads Qwen/Qwen3-VL-2B-Instruct (4.26 GB) and needs ffmpeg; see the module docs"]
async fn qwen3_vl_answers_when_the_forklift_enters_over_sampled_frames() {
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let model =
        fetcher.fetch(ManifestSet::builtin().unwrap().get(QWEN3_VL_2B).unwrap()).await.unwrap();
    let loaded = Instant::now();
    let qwen = LlmDescriber::mistralrs(&model, MistralRsConfig::default()).await.unwrap();
    println!("loaded {QWEN3_VL_2B} in {:?}", loaded.elapsed());

    let clip =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video/aisle-forklift-120s.mp4");
    let frames = sample(&clip, 5, 448);
    assert_eq!(frames.len(), 24);
    // Qwen3-VL-2B misses S13's 5 s window both ways (recorded 2026-11-21): 8 frames a call
    // (the default) puts the right batch's event at 0:45.0 but names events in batches without
    // the forklift too; all 24 in one call answers 0:35.0. The spec's local default is 4B or 8B.
    let per_call =
        std::env::var("ADK_VISION_FRAMES_PER_CALL").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .video_analyzer(Arc::new(FramesAnalyzer::local(&qwen).with_frames_per_call(per_call)))
        .egress_gate(EgressGate::new(
            adk_core::egress::EgressPolicy::public_only(),
            EgressConfig::default().with_default(EgressAction::Allow),
            recorder.clone(),
        ))
        .build()
        .unwrap();
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let cx = VisionCx::new().with_scope(ModelCallScope::new().with_budget(budget.clone()));
    let ask = VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() };
    let started = Instant::now();
    let out = runtime
        .analyze_video(&cx, VideoSource::frames(&frames, Sampling::at(0.2)), &ask)
        .await
        .unwrap();
    println!("answered in {:?}", started.elapsed());
    println!("answer: {:?}", out.value.answer);
    println!("events: {:?}", out.value.events);
    println!("usage: {:?}", out.value.usage);
    assert!(recorder.is_empty(), "nothing left the machine");
    assert_eq!(out.value.usage.cost_micro_usd, Some(0));
    assert_eq!(budget.units().get(UsageUnit::Frames), 24);
    let first = out.value.events.first().expect("an event");
    assert!(first.start_ms.abs_diff(45_000) <= 5_000, "{first:?} is not within 5 s of 0:45");
}
