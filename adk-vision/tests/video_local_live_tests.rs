//! S13's local path: Qwen3-VL in this process (feature `mistralrs`) answers "When does the
//! forklift enter the aisle?" over frames sampled from `aisle-forklift-120s.mp4`, whose
//! `labels.json` puts the entry at 0:45.
//!
//! `#[ignore]`d: each loads a Qwen3-VL Instruct model through the fetcher — 4B (8.88 GB) by
//! default, `ADK_VISION_QWEN=qwen3-vl-2b-instruct` (4.26 GB) or `qwen3-vl-8b-instruct`
//! (17.5 GB) — and samples the clip with a user-installed `ffmpeg`, as a frame source
//! would.
//!
//! | Test | Mode |
//! |------|------|
//! | `qwen3_vl_finds_the_entry_after_a_visibility_pass` | a `VisibilityPass` over frames 5 s apart (`ADK_VISION_EVERY_S`), then the question around the first frame with the forklift |
//! | `qwen3_vl_answers_in_batches_over_every_frame` | the question over every frame, `ADK_VISION_FRAMES_PER_CALL` (8) a call |
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo test -p adk-vision --features mistralrs \
//!     --test video_local_live_tests -- --ignored --nocapture --test-threads 1
//! ```
//!
//! `ADK_VISION_DEVICE` (`cpu`, `metal`, or automatic) picks the device. `ADK_VISION_CLIP` and
//! `ADK_VISION_ENTRY_MS` run the same question over another clip of the
//! scene, such as S13's twenty minutes with the entry at 12:45 (765,000 ms), made from the
//! fixtures' sprites with the overlay of `generate.sh`, `-t 1200`, and the forklift's left
//! edge at `if(lt(t,760),-400,-200+40*(t-760))`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use adk_core::{BudgetTracker, ModelCallScope, RunBudget, UsageUnit};
use adk_mistralrs::{Device, DeviceConfig, MistralRsConfig};
use adk_vision::mistralrs::QWEN3_VL_4B;
use adk_vision::{
    DecodeLimits, EgressAction, EgressConfig, EgressGate, FetchConfig, FrameMeta, FrameSourceId,
    FramesAnalyzer, InMemoryAcceptanceStore, InMemoryEgressRecorder, LlmDescriber, ManifestSet,
    ModelFetcher, Sampling, SentImage, SourceImage, VideoRequest, VideoSource, VisibilityPass,
    VisionCx, VisionRuntime,
};

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

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

/// The model `ADK_VISION_QWEN` names (Qwen3-VL-4B by default), fetched and verified.
async fn qwen() -> LlmDescriber<SentImage> {
    let id = std::env::var("ADK_VISION_QWEN").unwrap_or_else(|_| QWEN3_VL_4B.into());
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let model = fetcher.fetch(ManifestSet::builtin().unwrap().get(&id).unwrap()).await.unwrap();
    let loaded = Instant::now();
    // `ADK_VISION_DEVICE=cpu` skips mistral.rs's automatic device map, which refuses a model
    // larger than the memory the system reports free at load time.
    let device = match std::env::var("ADK_VISION_DEVICE").as_deref() {
        Ok("cpu") => Device::Cpu,
        Ok("metal") => Device::Metal,
        _ => Device::Auto,
    };
    let config = MistralRsConfig {
        device: DeviceConfig { device, ..DeviceConfig::default() },
        ..MistralRsConfig::default()
    };
    let qwen = LlmDescriber::mistralrs(&model, config).await.unwrap();
    println!("loaded {id} in {:?}", loaded.elapsed());
    qwen
}

/// Asks over the clip's frames sampled every `every_s` seconds; nothing leaves the machine
/// and every frame is metered at zero cost. Returns the first event's start.
async fn ask(analyzer: FramesAnalyzer, every_s: u64) -> Option<u64> {
    let clip = std::env::var("ADK_VISION_CLIP").map(PathBuf::from).unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video/aisle-forklift-120s.mp4")
    });
    let frames = sample(&clip, every_s, 448);
    println!("{} frames, one every {every_s} s; {analyzer:?}", frames.len());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .video_analyzer(Arc::new(analyzer))
        .egress_gate(EgressGate::new(
            adk_core::egress::EgressPolicy::public_only(),
            EgressConfig::default().with_default(EgressAction::Allow),
            recorder.clone(),
        ))
        .build()
        .unwrap();
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let cx = VisionCx::new().with_scope(ModelCallScope::new().with_budget(budget.clone()));
    let question = VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() };
    let started = Instant::now();
    let out = runtime
        .analyze_video(
            &cx,
            VideoSource::frames(&frames, Sampling::at(1.0 / every_s as f32)),
            &question,
        )
        .await
        .unwrap();
    println!("answered in {:?}", started.elapsed());
    println!("answer: {:?}", out.value.answer);
    println!("events: {:?}", out.value.events);
    println!("usage: {:?}", out.value.usage);
    assert!(recorder.is_empty(), "nothing left the machine");
    assert_eq!(out.value.usage.cost_micro_usd, Some(0));
    assert_eq!(budget.units().get(UsageUnit::Frames), frames.len() as u64);
    out.value.events.first().map(|e| e.start_ms)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "loads Qwen3-VL (4B: 8.88 GB) and needs ffmpeg; see the module docs"]
async fn qwen3_vl_finds_the_entry_after_a_visibility_pass() {
    let qwen = qwen().await;
    let mut pass = VisibilityPass::new()
        .with_stride(env_or("ADK_VISION_STRIDE", 1))
        .with_frames_per_check(env_or("ADK_VISION_FRAMES_PER_CHECK", 1));
    if let Ok(target) = std::env::var("ADK_VISION_TARGET") {
        pass = pass.with_target(target);
    }
    let analyzer = FramesAnalyzer::local(&qwen)
        .with_frames_per_call(env_or("ADK_VISION_FRAMES_PER_CALL", 8))
        .with_visibility_pass(pass);
    let start = ask(analyzer, env_or("ADK_VISION_EVERY_S", 5)).await.expect("an event");
    let entry = env_or("ADK_VISION_ENTRY_MS", 45_000u64);
    assert!(start.abs_diff(entry) <= 5_000, "{start} ms is not within 5 s of {entry} ms");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "loads Qwen3-VL (4B: 8.88 GB) and needs ffmpeg; see the module docs"]
async fn qwen3_vl_answers_in_batches_over_every_frame() {
    // Qwen3-VL-2B misses S13's 5 s window this way (recorded 2026-11-21): 8 frames a call
    // puts the right batch's event at 0:45.0 but names events in batches without the forklift
    // too; all 24 in one call answers 0:35.0.
    let qwen = qwen().await;
    let analyzer =
        FramesAnalyzer::local(&qwen).with_frames_per_call(env_or("ADK_VISION_FRAMES_PER_CALL", 8));
    let start = ask(analyzer, env_or("ADK_VISION_EVERY_S", 5)).await.expect("an event");
    let entry = env_or("ADK_VISION_ENTRY_MS", 45_000u64);
    assert!(start.abs_diff(entry) <= 5_000, "{start} ms is not within 5 s of {entry} ms");
}
