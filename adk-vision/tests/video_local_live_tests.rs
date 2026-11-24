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
//! edge at `if(lt(t,760),-400,-200+40*(t-760))`. `ADK_VISION_WIDTH` (448) is the width the
//! frames are scaled to.
//!
//! With `ADK_VISION_RECORD` naming a file, the run is written there as the transcript
//! `examples/vision` S13 replays: each call's kind, the SHA-256 and size of each image, the
//! frame times a question names, the model's answer, its token counts and its wall time.
//! S13's own settings, on its clip at its full width, from the repository root (the test runs
//! in `adk-vision/`, so the paths are absolute):
//!
//! ```text
//! ADK_VISION_DEVICE=cpu ADK_VISION_CLIP=$PWD/examples/vision/fixtures/video/aisle-forklift-20min.mp4 \
//!     ADK_VISION_ENTRY_MS=765000 ADK_VISION_WIDTH=640 ADK_VISION_STRIDE=5 \
//!     ADK_VISION_FRAMES_PER_CHECK=4 ADK_VISION_FRAMES_PER_CALL=8 \
//!     ADK_VISION_RECORD=$PWD/examples/vision/fixtures/recorded/qwen3-vl-4b/s13-aisle-20min.json \
//!     cargo test -p adk-vision --features mistralrs --test video_local_live_tests \
//!     qwen3_vl_finds_the_entry_after_a_visibility_pass -- --ignored --nocapture
//! ```
//!
//! Recorded on an Apple M4 (10 cores, 24 GB), the `dev` profile, with no network: Qwen3-VL-4B
//! names the target, checks the 240 frames four at a time until the eighth check finds the
//! forklift, and answers 12:50 from six frames around it — 10 calls, 9,208 input and 120
//! output tokens. On the CPU the model loads in 20 s and answers in 706 s (61 s a check); with
//! `mistralrs-metal` it loads in 26 s and answers in 61 s (4.2 s a check), the same calls
//! with the same answers.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use adk_core::{
    BudgetTracker, DataLocality, Llm, LlmRequest, LlmResponse, LlmResponseStream, ModelCallScope,
    Part, RunBudget, UsageUnit,
};
use adk_mistralrs::{Device, DeviceConfig, MistralRsConfig};
use adk_vision::cloud::Normalized1000;
use adk_vision::mistralrs::{MAX_PIXELS, QWEN3_VL_4B};
use adk_vision::{
    DecodeLimits, Digest, EgressAction, EgressConfig, EgressGate, FetchConfig, FrameMeta,
    FrameSourceId, FramesAnalyzer, InMemoryAcceptanceStore, InMemoryEgressRecorder, InputLimits,
    LlmDescriber, ManifestSet, ModelFetcher, Sampling, SentImage, SourceImage, VideoRequest,
    VideoSource, VisibilityPass, VisionCx, VisionRuntime,
};
use futures::StreamExt;
use serde_json::{Value, json};

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

/// Every call to the model as S13's transcript holds it.
struct Recorder {
    inner: Arc<dyn Llm>,
    exchanges: Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl Llm for Recorder {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let parts: Vec<&Part> = req.contents.iter().flat_map(|c| c.parts.iter()).collect();
        let prompt: String = parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("\n");
        let images: Vec<String> = parts
            .iter()
            .filter_map(|p| match p {
                Part::InlineData { data, .. } => {
                    Some(format!("{}:{}", Digest::of(data), data.len()))
                }
                _ => None,
            })
            .collect();
        let kind = if prompt.contains("\"target\"") {
            "target"
        } else if prompt.contains("\"visible\"") {
            "check"
        } else {
            "ask"
        };
        // "… at 0.2 frames per second: image 1 at 12:30.0, image 2 at 12:35.0.\n\nQuestion: …"
        let stamps: Vec<String> = prompt
            .split_once("frames per second: ")
            .and_then(|(_, rest)| rest.split_once(".\n\n"))
            .map(|(list, _)| list.split(", ").map(str::to_string).collect())
            .unwrap_or_default();
        let frames: Vec<u64> = stamps
            .iter()
            .filter_map(|s| s.rsplit_once(" at "))
            .filter_map(|(_, t)| {
                // `m:ss.s` or `h:mm:ss.s`.
                t.split(':')
                    .try_fold(0.0, |total, part| Some(total * 60.0 + part.parse::<f64>().ok()?))
                    .map(|seconds| (seconds * 1_000.0).round() as u64)
            })
            .collect();
        let started = Instant::now();
        let responses: Vec<adk_core::Result<LlmResponse>> =
            self.inner.generate_content(req, stream).await?.collect().await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let mut text = String::new();
        let (mut input, mut output) = (0, 0);
        for response in responses.iter().flatten() {
            if !response.partial {
                for part in response.content.iter().flat_map(|c| c.parts.iter()) {
                    text.push_str(part.text().unwrap_or_default());
                }
            }
            if let Some(usage) = &response.usage_metadata {
                (input, output) = (usage.prompt_token_count, usage.candidates_token_count);
            }
        }
        println!("{kind} over {} images in {elapsed_ms} ms: {text}", images.len());
        let mut exchange = json!({ "kind": kind, "images": images.len(), "imageDigests": images });
        if kind == "ask" {
            exchange["frames"] = json!(frames);
            exchange["stamps"] = json!(stamps);
        }
        exchange["response"] = json!(text);
        exchange["usage"] = json!({ "input": input, "output": output });
        exchange["elapsedMs"] = json!(elapsed_ms);
        self.exchanges.lock().unwrap().push(exchange);
        Ok(Box::pin(futures::stream::iter(responses)))
    }

    fn max_cost_micro_usd(&self, req: &LlmRequest) -> Option<u64> {
        self.inner.max_cost_micro_usd(req)
    }

    fn data_locality(&self) -> DataLocality {
        self.inner.data_locality()
    }
}

/// The model `ADK_VISION_QWEN` names (Qwen3-VL-4B by default), fetched and verified, on
/// the device `ADK_VISION_DEVICE` names; with `ADK_VISION_RECORD`, its calls are recorded.
struct Qwen {
    id: String,
    device: String,
    load_ms: u64,
    describer: LlmDescriber<SentImage>,
    recorder: Option<Arc<Recorder>>,
}

async fn qwen() -> Qwen {
    let id = std::env::var("ADK_VISION_QWEN").unwrap_or_else(|_| QWEN3_VL_4B.into());
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let model = fetcher.fetch(ManifestSet::builtin().unwrap().get(&id).unwrap()).await.unwrap();
    let loaded = Instant::now();
    // `ADK_VISION_DEVICE=cpu` skips mistral.rs's automatic device map, which refuses a model
    // larger than the memory the system reports free at load time.
    let device = std::env::var("ADK_VISION_DEVICE").unwrap_or_else(|_| "auto".into());
    let config = MistralRsConfig {
        device: DeviceConfig {
            device: match device.as_str() {
                "cpu" => Device::Cpu,
                "metal" => Device::Metal,
                _ => Device::Auto,
            },
            ..DeviceConfig::default()
        },
        ..MistralRsConfig::default()
    };
    let describer = LlmDescriber::mistralrs(&model, config).await.unwrap();
    let load_ms = loaded.elapsed().as_millis() as u64;
    println!("loaded {id} on {device} in {load_ms} ms");
    if std::env::var_os("ADK_VISION_RECORD").is_none() {
        return Qwen { id, device, load_ms, describer, recorder: None };
    }
    // The same describer over the recorder: the model's id, region frame and pixel cap.
    let recorder =
        Arc::new(Recorder { inner: Arc::clone(describer.model()), exchanges: Mutex::default() });
    let describer = LlmDescriber::local(recorder.clone())
        .unwrap()
        .with_id(id.clone())
        .with_adapter(Arc::new(Normalized1000::qwen3_vl(&id)))
        .with_limits(InputLimits::unlimited().with_max_pixels(MAX_PIXELS));
    Qwen { id, device, load_ms, describer, recorder: Some(recorder) }
}

/// Asks over the clip's frames sampled every `every_s` seconds; nothing leaves the machine
/// and every frame is metered at zero cost. Returns the first event's start. `settings` names
/// the analyzer's settings in a recording.
async fn ask(qwen: &Qwen, analyzer: FramesAnalyzer, every_s: u64, settings: Value) -> Option<u64> {
    let clip = std::env::var("ADK_VISION_CLIP").map(PathBuf::from).unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video/aisle-forklift-120s.mp4")
    });
    let width = env_or("ADK_VISION_WIDTH", 448);
    let frames = sample(&clip, every_s, width);
    println!("{} frames, one every {every_s} s; {settings}", frames.len());
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
    let wall_ms = started.elapsed().as_millis() as u64;
    println!("answered in {wall_ms} ms");
    println!("answer: {:?}", out.value.answer);
    println!("events: {:?}", out.value.events);
    println!("usage: {:?}", out.value.usage);
    assert!(recorder.is_empty(), "nothing left the machine");
    assert_eq!(out.value.usage.cost_micro_usd, Some(0));
    assert_eq!(budget.units().get(UsageUnit::Frames), frames.len() as u64);
    if let (Some(calls), Ok(path)) = (&qwen.recorder, std::env::var("ADK_VISION_RECORD")) {
        let exchanges = calls.exchanges.lock().unwrap().clone();
        let recording = json!({
            "model": qwen.id,
            "recorded": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "device": qwen.device,
            "clip": clip.file_name().map(|n| n.to_string_lossy().to_string()),
            "frames": frames.len(),
            "everyMs": every_s * 1_000,
            "width": width,
            "settings": settings,
            "loadMs": qwen.load_ms,
            "wallMs": wall_ms,
            "answer": out.value.answer,
            "events": out.value.events.iter().map(|e| json!({
                "startMs": e.start_ms, "endMs": e.end_ms, "text": e.text
            })).collect::<Vec<_>>(),
            "exchanges": exchanges,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&recording).unwrap() + "\n").unwrap();
        println!(
            "recorded {} calls to {path}",
            recording["exchanges"].as_array().map_or(0, Vec::len)
        );
    }
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
    let frames_per_call = env_or("ADK_VISION_FRAMES_PER_CALL", 8);
    let settings = json!({
        "framesPerCall": frames_per_call,
        "visibilityPass": {
            "stride": env_or("ADK_VISION_STRIDE", 1),
            "framesPerCheck": env_or("ADK_VISION_FRAMES_PER_CHECK", 1),
            "target": std::env::var("ADK_VISION_TARGET").ok(),
        },
    });
    let analyzer = FramesAnalyzer::local(&qwen.describer)
        .with_frames_per_call(frames_per_call)
        .with_visibility_pass(pass);
    let every_s = env_or("ADK_VISION_EVERY_S", 5);
    let start = ask(&qwen, analyzer, every_s, settings).await.expect("an event");
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
    let frames_per_call = env_or("ADK_VISION_FRAMES_PER_CALL", 8);
    let analyzer = FramesAnalyzer::local(&qwen.describer).with_frames_per_call(frames_per_call);
    let settings = json!({ "framesPerCall": frames_per_call });
    let every_s = env_or("ADK_VISION_EVERY_S", 5);
    let start = ask(&qwen, analyzer, every_s, settings).await.expect("an event");
    let entry = env_or("ADK_VISION_ENTRY_MS", 45_000u64);
    assert!(start.abs_diff(entry) <= 5_000, "{start} ms is not within 5 s of {entry} ms");
}
