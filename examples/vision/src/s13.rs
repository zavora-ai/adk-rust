//! S13, native video two ways: "When does the forklift enter the aisle?" over twenty minutes
//! of the warehouse aisle (`fixtures/video/aisle-forklift-20min.mp4`, entry at 12:45).
//!
//! | Path | How |
//! |---|---|
//! | Gemini native video | The clip leaves through the egress gate, is uploaded once through the File API and asked about as a file part, at 1 fps and low media resolution. The fixture is 227 KB, so the inline limit is set to 0 to take the File API path a real 20-minute camera clip (well over 20 MB) takes. Gemini answers from its recorded exchange, served to the real client from a local server |
//! | Qwen3-VL-4B over sampled frames | One frame every five seconds — decoded from the clip by a user-installed ffmpeg, or drawn from the clip's own sprites at the same times where there is none — through `FramesAnalyzer` with a `VisibilityPass`, on the device. The model's answers replay a transcript (see `fixtures/recorded/README.md` for its provenance) |

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use adk_core::egress::EgressPolicy;
use adk_core::{
    BudgetTracker, Content, DataLocality, InMemorySpendLedger, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, ModelCallScope, Part, RunBudget, SpendLimits, UsageMetadata, UsageUnit,
};
use adk_model::GeminiModel;
use adk_vision::stream::{FfmpegBinary, FileSource, FrameSource};
use adk_vision::{
    DecodeLimits, DecodedImage, Digest, EgressAction, EgressConfig, EgressGate, FrameMeta,
    FramePixels, FrameSourceId, FramesAnalyzer, GeminiVideoAnalyzer, InMemoryEgressRecorder,
    LlmDescriber, MediaResolution, Sampling, SourceClip, SourceImage, VideoRequest, VideoSource,
    VisibilityPass, VisionCx, VisionRuntime,
};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde_json::Value;
use wiremock::matchers::{header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::Checks;
use crate::fixtures::{fixtures, read_json};
use crate::replay::{live_key, recording_proxy};

const QUESTION: &str = "When does the forklift enter the aisle?";
/// One frame every five seconds for the local path.
const EVERY_MS: u64 = 5_000;

fn video(name: &str) -> PathBuf {
    fixtures().join("video").join(name)
}

struct Label {
    start_ms: u64,
    tolerance_ms: u64,
    duration_ms: u64,
}

fn label() -> Result<Label> {
    let labels = read_json(&video("labels.json"))?;
    let clip = &labels["aisle-forklift-20min.mp4"];
    let ms = |key: &str| clip[key].as_u64().with_context(|| format!("labels.json has {key}"));
    Ok(Label {
        start_ms: ms("startMs")?,
        tolerance_ms: ms("toleranceMs")?,
        duration_ms: ms("durationMs")?,
    })
}

fn metered() -> (VisionCx, Arc<BudgetTracker>) {
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let cx = VisionCx::new().with_scope(
        ModelCallScope::new().with_budget(budget.clone()).with_spend_ledger(ledger, "aisle"),
    );
    (cx, budget)
}

fn gate(recorder: &Arc<InMemoryEgressRecorder>) -> EgressGate {
    EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        recorder.clone(),
    )
}

/// Gemini's recorded exchange — the File API upload, the processing polls and the answer —
/// served at the paths the client asks for.
async fn gemini_server(exchanges: &[Value]) -> Result<MockServer> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload/v1beta/files"))
        .and(header("x-goog-upload-command", "start"))
        .respond_with(ResponseTemplate::new(200).insert_header(
            "x-goog-upload-url",
            format!("{}/upload/v1beta/files?upload_id=u1&upload_protocol=resumable", server.uri()),
        ))
        .mount(&server)
        .await;
    let uploaded = exchanges
        .iter()
        .find(|e| e["path"].as_str().is_some_and(|p| p.contains("upload_id")))
        .context("the recording holds the upload")?;
    Mock::given(method("POST"))
        .and(path("/upload/v1beta/files"))
        .and(query_param("upload_id", "u1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(uploaded["response"].clone()))
        .mount(&server)
        .await;
    let polls: Vec<&Value> = exchanges.iter().filter(|e| e["method"] == "GET").collect();
    for (i, poll) in polls.iter().enumerate() {
        let mock = Mock::given(method("GET"))
            .and(path(
                poll["path"].as_str().unwrap_or_default().split('?').next().unwrap_or_default(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(poll["response"].clone()))
            .with_priority(u8::try_from(i + 1).unwrap_or(u8::MAX));
        if i + 1 < polls.len() {
            mock.up_to_n_times(1).mount(&server).await;
        } else {
            mock.mount(&server).await;
        }
    }
    let answer = exchanges
        .iter()
        .rfind(|e| e["path"].as_str().is_some_and(|p| p.ends_with("generateContent")))
        .context("the recording holds the question")?;
    Mock::given(method("POST"))
        .and(path_regex(r"generateContent$"))
        .respond_with(
            ResponseTemplate::new(answer["status"].as_u64().unwrap_or(200) as u16)
                .set_body_json(answer["response"].clone()),
        )
        .mount(&server)
        .await;
    Ok(server)
}

/// A scripted Qwen3-VL on this machine: it answers each request with the transcript's next
/// response after checking the request is the one the transcript holds — the same kind, and
/// for a question the same frame times.
struct Transcript {
    exchanges: Vec<Value>,
    next: Mutex<usize>,
    mismatches: Mutex<Vec<String>>,
}

fn kind_of(request: &LlmRequest) -> (&'static str, String) {
    let text: String = request
        .contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter_map(Part::text)
        .collect::<Vec<_>>()
        .join("\n");
    let kind = if text.contains("\"target\"") {
        "target"
    } else if text.contains("\"visible\"") {
        "check"
    } else {
        "ask"
    };
    (kind, text)
}

#[async_trait::async_trait]
impl Llm for Transcript {
    fn name(&self) -> &str {
        "qwen3-vl-4b-instruct"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let at = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            *next += 1;
            *next - 1
        };
        let (kind, text) = kind_of(&request);
        let images = request
            .contents
            .iter()
            .flat_map(|c| c.parts.iter())
            .filter(|p| matches!(p, Part::InlineData { .. }))
            .count();
        let Some(exchange) = self.exchanges.get(at) else {
            return Err(adk_core::AdkError::model(format!(
                "the transcript ends before request {at}"
            )));
        };
        let stamps_ok = exchange["stamps"].as_array().is_none_or(|stamps| {
            stamps.iter().all(|s| s.as_str().is_some_and(|s| text.contains(s)))
        });
        if exchange["kind"] != kind
            || exchange["images"].as_u64() != Some(images as u64)
            || !stamps_ok
        {
            self.mismatches.lock().unwrap_or_else(|e| e.into_inner()).push(format!(
                "request {at}: {kind} with {images} images, transcript has {}",
                exchange["kind"]
            ));
        }
        let mut response = LlmResponse::new(
            Content::new("model").with_text(exchange["response"].as_str().unwrap_or_default()),
        );
        let tokens = |key: &str| exchange["usage"][key].as_i64().unwrap_or_default() as i32;
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: tokens("input"),
            candidates_token_count: tokens("output"),
            total_token_count: tokens("input") + tokens("output"),
            ..Default::default()
        });
        response.turn_complete = true;
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }

    fn data_locality(&self) -> DataLocality {
        DataLocality::InProcess
    }
}

/// One frame every five seconds: from the clip through ffmpeg when there is one, else drawn
/// from the clip's sprites at the same times, as `generate.sh` overlays them.
async fn sampled(label: &Label) -> Result<(Vec<(FrameMeta, SourceImage)>, &'static str)> {
    let ffmpeg = FfmpegBinary::from_env();
    let id = FrameSourceId("aisle-3".into());
    if ffmpeg.check().is_ok() {
        let source = FileSource::new(id, video("aisle-forklift-20min.mp4")).with_ffmpeg(ffmpeg);
        let mut stream =
            source.frames(Sampling { fps: 1000.0 / EVERY_MS as f32, max_long_edge: None }).await?;
        let mut frames = Vec::new();
        while let Some(frame) = stream.next().await {
            let frame = frame?;
            let FramePixels::Ephemeral(pixels) = &frame.pixels else { bail!("a decoded frame") };
            frames.push((frame.meta(), SourceImage::from_pixels(DecodedImage::clone(pixels))?));
        }
        return Ok((frames, "decoded from the clip by ffmpeg"));
    }
    let load = |name: &str| -> Result<DecodedImage> {
        let bytes = std::fs::read(video("sprites").join(name))?;
        Ok(DecodedImage::decode(&bytes, &DecodeLimits::default())?.0)
    };
    let (background, forklift) = (load("background.png")?, load("forklift.png")?);
    let (w, h) = background.size();
    let mut frames = Vec::new();
    for (index, pts_ms) in (0..label.duration_ms).step_by(EVERY_MS as usize).enumerate() {
        let t = pts_ms as f64 / 1000.0;
        let x = if t < 760.0 { -400.0 } else { -200.0 + 40.0 * (t - 760.0) };
        let mut rgba = background.rgba().to_vec();
        let (fw, fh) = forklift.size();
        for fy in 0..fh {
            for fx in 0..fw {
                let (px, py) = (x.floor() as i64 + i64::from(fx), 135 + i64::from(fy));
                if px < 0 || py < 0 || px >= i64::from(w) || py >= i64::from(h) {
                    continue;
                }
                let [r, g, b, a] = forklift.pixel(fx, fy);
                let i = ((py as u32 * w + px as u32) * 4) as usize;
                let alpha = f32::from(a) / 255.0;
                for (c, v) in [r, g, b].into_iter().enumerate() {
                    rgba[i + c] = (f32::from(v) * alpha + f32::from(rgba[i + c]) * (1.0 - alpha))
                        .round() as u8;
                }
            }
        }
        let meta = FrameMeta {
            source: id.clone(),
            index: index as u64,
            pts_ms,
            captured_at: chrono::Utc::now(),
        };
        frames.push((meta, SourceImage::from_pixels(DecodedImage::from_rgba(w, h, rgba)?)?));
    }
    Ok((frames, "drawn from the clip's sprites (no ffmpeg on this machine)"))
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    let label = label()?;
    let within = |ms: u64| ms.abs_diff(label.start_ms) <= label.tolerance_ms;
    let clip = SourceClip::decode(std::fs::read(video("aisle-forklift-20min.mp4"))?)?;
    let ask = VideoRequest::Ask { question: QUESTION.into() };

    // Gemini native video through the File API.
    let live = recording_proxy("GEMINI");
    let (model_id, server) = match &live {
        Some(_) => ("gemini-3.8-flash".to_string(), MockServer::start().await),
        None => {
            let recording = read_json(&fixtures().join("recorded/gemini/s13-aisle-20min.json"))?;
            let exchanges = recording["exchanges"].as_array().cloned().unwrap_or_default();
            let model = recording["model"].as_str().unwrap_or("gemini-3.8-flash").to_string();
            (model, gemini_server(&exchanges).await?)
        }
    };
    let (base, key) = match &live {
        Some(proxy) => (format!("http://{proxy}/v1beta/"), live_key("GEMINI_API_KEY")),
        None => (format!("{}/v1beta/", server.uri()), "recorded".to_string()),
    };
    let model = std::env::var("VISION_GEMINI_MODEL").unwrap_or(model_id);
    let gemini = GeminiModel::new_with_base_url(key, &model, base)?
        .with_retry_config(adk_model::RetryConfig::disabled());
    let analyzer = GeminiVideoAnalyzer::new(Arc::new(gemini))
        .with_media_resolution(MediaResolution::Low)
        .with_inline_limit(0)
        .with_processing_wait(
            std::time::Duration::from_millis(200),
            std::time::Duration::from_secs(300),
        );
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(analyzer))
        .egress_gate(gate(&recorder))
        .build()?;
    let (cx, budget) = metered();
    checks.note(format!(
        "Gemini `{model}` {}; Qwen3-VL-4B on a scripted transcript",
        if live.is_some() {
            "live through the recording proxy"
        } else {
            "on its recorded exchange"
        }
    ));
    let gemini_out =
        runtime.analyze_video(&cx, VideoSource::clip(&clip, Sampling::at(1.0)), &ask).await;
    match &gemini_out {
        Ok(out) => {
            let start = out.value.events.first().map(|e| e.start_ms);
            checks
                .note(format!("Gemini answered: {}", out.value.answer.clone().unwrap_or_default()));
            checks.check(
                &format!("Gemini's answer falls within ±5 s of 12:45 ({start:?} ms)"),
                start.is_some_and(within),
            );
            let usage = &out.value.usage;
            checks.check(
                &format!(
                    "Gemini's tokens and cost are reported: {:?} in, {:?} out, {:?} µUSD for {} frames",
                    usage.input_tokens, usage.output_tokens, usage.cost_micro_usd, usage.frames
                ),
                usage.input_tokens.is_some_and(|t| t > 0)
                    && usage.output_tokens.is_some_and(|t| t > 0)
                    && usage.cost_micro_usd.is_some_and(|c| c > 0)
                    && usage.frames == 1_200
                    && budget.units().get(UsageUnit::Frames) == 1_200,
            );
        }
        Err(error) => checks.check(&format!("Gemini answered ({error})"), false),
    }
    if live.is_none() {
        let requests = server.received_requests().await.unwrap_or_default();
        let uploads: Vec<_> = requests
            .iter()
            .filter(|r| r.url.query().is_some_and(|q| q.contains("upload_id")))
            .collect();
        let asked: Vec<Value> = requests
            .iter()
            .filter(|r| r.url.path().ends_with("generateContent"))
            .filter_map(|r| serde_json::from_slice(&r.body).ok())
            .collect();
        let file_part = asked
            .first()
            .is_some_and(|body| body["contents"][0]["parts"][0]["fileData"]["fileUri"].is_string());
        let inline =
            requests.iter().any(|r| String::from_utf8_lossy(&r.body).contains("inlineData"));
        checks.check(
            "the Gemini path uploads the clip once and sends a file part, not inline bytes",
            uploads.len() == 1
                && Digest::of(&uploads[0].body) == clip.digest()
                && asked.len() == 1
                && file_part
                && !inline
                && recorder.released().iter().map(|r| r.egress).collect::<Vec<_>>()
                    == vec![clip.digest()],
        );
    }

    // Qwen3-VL over sampled frames, after a visibility pass, on the device.
    let transcript = read_json(&fixtures().join("recorded/qwen3-vl-4b/s13-aisle-20min.json"))?;
    let qwen = Arc::new(Transcript {
        exchanges: transcript["exchanges"].as_array().cloned().unwrap_or_default(),
        next: Mutex::new(0),
        mismatches: Mutex::default(),
    });
    let describer = LlmDescriber::local(qwen.clone())?;
    let analyzer = FramesAnalyzer::local(&describer)
        .with_frames_per_call(8)
        .with_visibility_pass(VisibilityPass::new().with_stride(5).with_frames_per_check(4));
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .video_analyzer(Arc::new(analyzer))
        .egress_gate(gate(&recorder))
        .build()?;
    let (frames, how) = sampled(&label).await?;
    checks.note(format!("{} frames, one every 5 s, {how}", frames.len()));
    let (cx, budget) = metered();
    let out = runtime
        .analyze_video(
            &cx,
            VideoSource::frames(&frames, Sampling::at(1000.0 / EVERY_MS as f32)),
            &ask,
        )
        .await?;
    let start = out.value.events.first().map(|e| e.start_ms);
    let asked = *qwen.next.lock().unwrap_or_else(|e| e.into_inner());
    let mismatches = qwen.mismatches.lock().unwrap_or_else(|e| e.into_inner()).clone();
    checks.note(format!("Qwen3-VL answered: {}", out.value.answer.clone().unwrap_or_default()));
    checks.check(
        &format!(
            "the local answer falls within ±5 s of 12:45 ({start:?} ms), after {asked} model calls for {} frames",
            frames.len()
        ),
        start.is_some_and(within) && mismatches.is_empty() && asked == qwen.exchanges.len(),
    );
    for mismatch in &mismatches {
        checks.note(mismatch.clone());
    }
    let usage = &out.value.usage;
    checks.check(
        &format!(
            "the local path's tokens and cost are reported: {:?} in, {:?} out, {:?} µUSD; nothing left the machine",
            usage.input_tokens, usage.output_tokens, usage.cost_micro_usd
        ),
        usage.input_tokens.is_some_and(|t| t > 0)
            && usage.cost_micro_usd == Some(0)
            && recorder.is_empty()
            && budget.units().get(UsageUnit::Frames) == frames.len() as u64,
    );
    Ok(checks)
}
