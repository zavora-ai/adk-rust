//! Video analysis through the runtime (spec 3.7 §7.13, S11, S13): clips leave only through
//! the egress gate, `sam-3.1` video tracks with stable identifiers and bills its 50-frame
//! minimum per request, Gemini native video uploads a large clip once and sends a file part,
//! questions over sampled frames name events by frame, and a visibility pass finds the frames
//! that show the question's subject before asking. Every provider runs as its real client
//! against a local server, and every analyzer runs `video_analyzer_conformance_tests!`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adk_core::egress::EgressPolicy;
use adk_core::{
    BudgetTracker, Content, DataLocality, InMemorySpendLedger, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, ModelCallScope, RunBudget, SpendKey, SpendLedger, SpendPeriod,
    UsageMetadata, UsageUnit,
};
use adk_model::GeminiModel;
use adk_vision::conformance::{VideoAnalyzerTarget, VideoFixture};
use adk_vision::sam::{SamConfig, SamVideoTracker};
use adk_vision::{
    CallOptions, DecodeLimits, DecodedImage, Digest, EgressAction, EgressConfig, EgressGate,
    EgressRecord, FrameMeta, FrameSourceId, FramesAnalyzer, GeminiVideoAnalyzer,
    InMemoryEgressRecorder, LlmDescriber, MediaResolution, RedactionClass, Sampling, SegmentPrompt,
    SourceClip, SourceImage, TrackId, VideoCodec, VideoRequest, VideoSource, VisibilityPass,
    VisionCx, VisionError, VisionRuntime,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video").join(name)
}

fn clip(name: &str) -> SourceClip {
    SourceClip::decode(std::fs::read(fixture(name)).unwrap()).unwrap()
}

fn gate(config: EgressConfig, recorder: &Arc<InMemoryEgressRecorder>) -> EgressGate {
    EgressGate::new(EgressPolicy::public_only(), config, recorder.clone())
}

fn allow() -> EgressConfig {
    EgressConfig::default().with_default(EgressAction::Allow)
}

fn metered() -> (VisionCx, Arc<BudgetTracker>, Arc<InMemorySpendLedger>) {
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let cx = VisionCx::new().with_scope(
        ModelCallScope::new().with_budget(budget.clone()).with_spend_ledger(ledger.clone(), "acme"),
    );
    (cx, budget, ledger)
}

async fn spent(ledger: &InMemorySpendLedger) -> u64 {
    ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap()
}

/// A `sam-3.1` stream answering `text` on the output lane.
fn sam_stream(text: &str) -> ResponseTemplate {
    let event = |kind: &str, data: Value| format!("event: {kind}\ndata: {data}\n\n");
    let mut out = event("response.created", json!({"type": "response.created"}));
    for line in text.split_inclusive('\n') {
        out += &event(
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "delta": line}),
        );
    }
    out += &event(
        "response.output_text.done",
        json!({"type": "response.output_text.done", "text": text}),
    );
    out += &event(
        "response.completed",
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    );
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(out + "data: [DONE]\n\n")
}

/// Two objects on each of frames 0–2 of a 640×360 clip, the first moving right.
fn two_objects() -> String {
    (0..3)
        .map(|f| {
            let x = 10 + 4 * f;
            format!(
                "<{f}f>0<|box;x1={x};y1=175;x2={};y2=260;w=640;h=360|><|mask;x=0;y=0;data=2,2,~!!|>,\
                 1<|box;x1=400;y1=20;x2=470;y2=60;w=640;h=360|><|mask;x=0;y=0;data=2,2,~!!|>\n",
                x + 140
            )
        })
        .collect()
}

fn sam(server: &MockServer) -> SamVideoTracker {
    // The local server stands in for Meta: the gate checks Meta's host.
    SamVideoTracker::new(SamConfig::new("test-key").with_base_url(format!("{}/v1", server.uri())))
        .unwrap()
        .with_hosts(["api.meta.ai"])
}

async fn sam_runtime(
    server: &MockServer,
    config: EgressConfig,
) -> (VisionRuntime, Arc<InMemoryEgressRecorder>) {
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(sam(server)))
        .egress_gate(gate(config, &recorder))
        .build()
        .unwrap();
    (runtime, recorder)
}

fn track(concepts: &[&str]) -> VideoRequest {
    VideoRequest::Track {
        prompts: concepts.iter().map(|c| SegmentPrompt::Concept { text: (*c).into() }).collect(),
    }
}

#[test]
fn clips_are_read_from_their_container() {
    let aisle = clip("aisle-forklift-120s.mp4");
    let info = aisle.info();
    assert_eq!(
        (info.width, info.height, info.frames, info.duration_ms),
        (640, 360, 1_200, 120_000)
    );
    assert_eq!((info.codec, info.mime_type.as_str()), (VideoCodec::H264, "video/mp4"));
    assert!((info.frame_rate() - 10.0).abs() < 1e-9);
    // The media duration counts x264's B-frame delay (3.1 s); the edit list states 3.0 s.
    let crossing = clip("forklift-crossing-3s.mp4");
    assert_eq!((crossing.info().frames, crossing.info().duration_ms), (30, 3_000));
    assert_eq!(
        aisle.digest(),
        Digest::of(&std::fs::read(fixture("aisle-forklift-120s.mp4")).unwrap())
    );
    let png = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cloud/shapes-960x540.png"),
    )
    .unwrap();
    assert!(
        matches!(SourceClip::decode(png), Err(VisionError::InvalidInput(m)) if m.contains("MP4"))
    );
}

#[tokio::test]
async fn a_clip_leaves_only_through_the_gate() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(sam_stream(&two_objects())).mount(&server).await;
    let crossing = clip("forklift-crossing-3s.mp4");
    let video = VideoSource::clip(&crossing, Sampling::at(10.0));
    let request = track(&["orange forklift"]);

    // Redaction required: a clip cannot be redacted on the device, so it never leaves.
    let (runtime, recorder) =
        sam_runtime(&server, allow().redacting(RedactionClass::faces())).await;
    let err = runtime.analyze_video(&VisionCx::new(), video, &request).await.unwrap_err();
    assert!(
        matches!(&err, VisionError::EgressRefused { reason, .. } if reason.contains("faces")),
        "{err}"
    );
    let Some(EgressRecord::Refused(refusal)) = recorder.records().pop() else {
        panic!("no refusal")
    };
    assert_eq!((refusal.step.as_str(), refusal.source), ("redact", crossing.digest()));

    // A rule that asks holds the clip by its digest; approving that digest releases it.
    let (runtime, recorder) = sam_runtime(&server, EgressConfig::default()).await;
    let held = runtime.analyze_video(&VisionCx::new(), video, &request).await.unwrap_err();
    let VisionError::EgressHeld { egress, data_class, .. } = held else { panic!("{held}") };
    assert_eq!((egress, data_class.as_str()), (crossing.digest(), "video"));
    let approved = CallOptions::new().approving(egress);
    let out =
        runtime.analyze_video_with(&VisionCx::new(), video, &request, &approved).await.unwrap();
    assert_eq!(out.calls[0].egress, vec![crossing.digest()]);
    assert!(
        matches!(recorder.records().last(), Some(EgressRecord::Released(r)) if r.egress == crossing.digest())
    );

    // No gate: refused before any request.
    let before = server.received_requests().await.unwrap().len();
    let ungated =
        VisionRuntime::builder().remote_video_analyzer(Arc::new(sam(&server))).build().unwrap();
    let err = ungated.analyze_video(&VisionCx::new(), video, &request).await.unwrap_err();
    assert!(matches!(err, VisionError::NotReleased { .. }), "{err}");
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn sam_video_tracks_with_stable_ids_and_bills_fifty_frames_a_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(sam_stream(&two_objects()))
        .mount(&server)
        .await;
    let (runtime, recorder) = sam_runtime(&server, allow()).await;
    let crossing = clip("forklift-crossing-3s.mp4");
    let video = VideoSource::clip(&crossing, Sampling::at(10.0));
    let (cx, budget, ledger) = metered();

    let out = runtime.analyze_video(&cx, video, &track(&["orange forklift"])).await.unwrap();
    let frames = &out.value.frames;
    assert_eq!(frames.iter().map(|f| f.index).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(
        frames.iter().map(|f| f.pts_ms).collect::<Vec<_>>(),
        vec![Some(0), Some(100), Some(200)]
    );
    for frame in frames {
        let ids: Vec<Option<TrackId>> = frame.regions.iter().map(|r| r.track).collect();
        assert_eq!(ids, vec![Some(TrackId(0)), Some(TrackId(1))]);
        assert_eq!(frame.regions[0].image, crossing.digest());
        assert_eq!(frame.regions[0].label.as_deref(), Some("orange forklift"));
    }
    assert_eq!(frames[2].regions[0].shape.bbox().unwrap().x0, 18.0);
    // Thirty frames bill the 50-frame minimum: $0.01.
    assert_eq!(spent(&ledger).await, 10_000);
    assert_eq!(budget.units().get(UsageUnit::Frames), 30);
    assert_eq!(
        (out.value.usage.frames, out.value.usage.cost_micro_usd, out.value.usage.requests),
        (30, Some(10_000), 1)
    );
    let Some(EgressRecord::Released(receipt)) = recorder.records().pop() else { panic!() };
    assert_eq!(receipt.estimated_cost_micro_usd, Some(10_000));
    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    let content = &body["input"][0]["content"];
    assert_eq!(content[0], json!({"type": "input_text", "text": "orange forklift"}));
    assert_eq!(content[1]["type"], "input_video");
    assert!(content[1]["video_url"].as_str().unwrap().starts_with("data:video/mp4;base64,AAAA"));

    // Two concepts are two requests, each billed its own minimum; their objects never share
    // an identifier.
    let out =
        runtime.analyze_video(&cx, video, &track(&["orange forklift", "shelf"])).await.unwrap();
    assert_eq!(spent(&ledger).await, 30_000);
    let ids: std::collections::BTreeSet<_> =
        out.value.frames[0].regions.iter().filter_map(|r| r.track).collect();
    assert_eq!(
        ids.into_iter().collect::<Vec<_>>(),
        vec![TrackId(0), TrackId(1), TrackId(2), TrackId(3)]
    );
    assert_eq!(out.calls[0].units.get(UsageUnit::Frames), 60);
}

#[tokio::test]
async fn a_large_clip_is_uploaded_to_meta_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/files"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "file-842549258569145", "status": "uploaded"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(sam_stream(&two_objects()))
        .mount(&server)
        .await;
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(sam(&server).with_inline_limit(0)))
        .egress_gate(gate(allow(), &recorder))
        .build()
        .unwrap();
    let crossing = clip("forklift-crossing-3s.mp4");
    let video = VideoSource::clip(&crossing, Sampling::at(10.0));
    for _ in 0..2 {
        runtime.analyze_video(&VisionCx::new(), video, &track(&["orange forklift"])).await.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    let upload = requests.iter().find(|r| r.url.path() == "/v1/files").unwrap();
    let form = String::from_utf8_lossy(&upload.body);
    assert!(
        form.contains("name=\"purpose\"")
            && form.contains("user_data")
            && form.contains("filename=\"clip.mp4\"")
    );
    let answer = requests.iter().rfind(|r| r.url.path() == "/v1/responses").unwrap();
    let body: Value = serde_json::from_slice(&answer.body).unwrap();
    assert_eq!(
        body["input"][0]["content"][1],
        json!({"type": "input_video", "file_id": "file-842549258569145"})
    );
}

/// Gemini's File API and `generateContent` on a local server: the upload session, the
/// file processing then active, and an answer naming the forklift at 0:46.0.
async fn gemini_server(answer: &str) -> MockServer {
    let server = MockServer::start().await;
    let file = |state: &str| {
        json!({"name": "files/aisle1", "uri": "https://generativelanguage.googleapis.com/v1beta/files/aisle1",
               "mimeType": "video/mp4", "sizeBytes": "76391", "state": state})
    };
    Mock::given(method("POST"))
        .and(path("/upload/v1beta/files"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-goog-upload-url", format!("{}/upload-session", server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload-session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"file": file("PROCESSING")})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1beta/files/aisle1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file("ACTIVE")))
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path_regex(r"generateContent$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": answer}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 11_874, "candidatesTokenCount": 64, "totalTokenCount": 11_938}
        })))
        .mount(&server)
        .await;
    server
}

fn gemini(server: &MockServer) -> GeminiVideoAnalyzer {
    let model = GeminiModel::new_with_base_url(
        "test-key",
        "gemini-3.8-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap();
    GeminiVideoAnalyzer::new(Arc::new(model))
        .with_media_resolution(MediaResolution::Low)
        .with_processing_wait(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_secs(5),
        )
}

#[tokio::test]
async fn gemini_native_video_uploads_once_and_sends_a_file_part() {
    let answer = r#"{"answer": "The forklift enters aisle 3 at about 0:46.", "events": [{"start": "0:46.0", "end": "0:50.0", "text": "the forklift crosses the yellow line"}]}"#;
    let server = gemini_server(answer).await;
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(gemini(&server).with_inline_limit(0)))
        .egress_gate(gate(allow(), &recorder))
        .build()
        .unwrap();
    let aisle = clip("aisle-forklift-120s.mp4");
    let ask = VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() };
    let (cx, budget, ledger) = metered();
    let out = runtime
        .analyze_video(&cx, VideoSource::clip(&aisle, Sampling::at(1.0)), &ask)
        .await
        .unwrap();
    // Asked twice, uploaded once.
    runtime.analyze_video(&cx, VideoSource::clip(&aisle, Sampling::at(1.0)), &ask).await.unwrap();

    let event = &out.value.events[0];
    assert_eq!((event.start_ms, event.end_ms), (46_000, 50_000));
    assert!(event.start_ms.abs_diff(45_000) <= 5_000);
    let usage = &out.value.usage;
    assert_eq!(
        (usage.frames, usage.input_tokens, usage.output_tokens),
        (120, Some(11_874), Some(64))
    );
    // gemini-3.8-flash: $0.75 per million input tokens and $3.75 per million output tokens.
    assert_eq!(usage.cost_micro_usd, Some(9_146));
    assert_eq!(out.calls[0].units.get(UsageUnit::Frames), 120);
    assert_eq!(budget.units().get(UsageUnit::Frames), 240);
    assert!(spent(&ledger).await > 0, "the model call committed its tokens");
    assert_eq!(
        recorder.released().iter().map(|r| r.egress).collect::<Vec<_>>(),
        vec![aisle.digest(); 2]
    );

    let requests = server.received_requests().await.unwrap();
    let generate: Vec<Value> = requests
        .iter()
        .filter(|r| r.url.path().ends_with("generateContent"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(generate.len(), 2);
    let parts = &generate[0]["contents"][0]["parts"];
    assert_eq!(
        parts[0],
        json!({"fileData": {"mimeType": "video/mp4", "fileUri": "https://generativelanguage.googleapis.com/v1beta/files/aisle1"},
               "videoMetadata": {"fps": 1.0}})
    );
    assert!(parts[1]["text"].as_str().unwrap().contains("When does the forklift enter the aisle?"));
    assert_eq!(generate[0]["generationConfig"]["mediaResolution"], "MEDIA_RESOLUTION_LOW");
    assert!(!requests.iter().any(|r| String::from_utf8_lossy(&r.body).contains("inlineData")));
}

#[tokio::test]
async fn a_small_clip_goes_to_gemini_inline() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path_regex(r"generateContent$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "{\"answer\": \"No forklift.\", \"events\": []}"}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 400, "candidatesTokenCount": 9, "totalTokenCount": 409}
        })))
        .mount(&server)
        .await;
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(gemini(&server)))
        .egress_gate(gate(allow(), &recorder))
        .build()
        .unwrap();
    let crossing = clip("forklift-crossing-3s.mp4");
    let ask = VideoRequest::Ask { question: "Is there a person?".into() };
    let out = runtime
        .analyze_video(&VisionCx::new(), VideoSource::clip(&crossing, Sampling::at(2.0)), &ask)
        .await
        .unwrap();
    assert_eq!(out.value.answer.as_deref(), Some("No forklift."));
    assert_eq!(out.value.usage.frames, 6);
    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    let part = &body["contents"][0]["parts"][0];
    assert_eq!(part["inlineData"]["mimeType"], "video/mp4");
    assert_eq!(part["videoMetadata"], json!({"fps": 2.0}));
}

/// A model on this machine that answers one scripted text and records what it was sent.
struct Scripted {
    answer: String,
    locality: DataLocality,
    seen: Mutex<Vec<LlmRequest>>,
}

#[async_trait]
impl Llm for Scripted {
    fn name(&self) -> &str {
        "qwen3-vl-2b-instruct"
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.seen.lock().unwrap().push(req);
        let mut response = LlmResponse::new(Content::new("model").with_text(&self.answer));
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 900,
            candidates_token_count: 40,
            total_token_count: 940,
            ..Default::default()
        });
        response.turn_complete = true;
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }

    fn data_locality(&self) -> DataLocality {
        self.locality
    }
}

fn frames(n: u64, every_ms: u64) -> Vec<(FrameMeta, SourceImage)> {
    (0..n)
        .map(|i| {
            let meta = FrameMeta {
                source: FrameSourceId("aisle".into()),
                index: i,
                pts_ms: i * every_ms,
                captured_at: chrono::Utc::now(),
            };
            let shade = (i * 40 % 255) as u8;
            (
                meta,
                SourceImage::from_pixels(DecodedImage::filled(64, 36, [shade, shade, shade, 255]))
                    .unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn questions_over_sampled_frames_name_events_by_frame() {
    let qwen = Arc::new(Scripted {
        answer: r#"{"answer": "It enters in the second image.", "events": [{"start": 2, "end": 3, "text": "the forklift crosses the line"}]}"#.into(),
        locality: DataLocality::InProcess,
        seen: Mutex::new(Vec::new()),
    });
    let describer = LlmDescriber::local(qwen.clone()).unwrap();
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .video_analyzer(Arc::new(FramesAnalyzer::local(&describer).with_frames_per_call(4)))
        .egress_gate(gate(allow(), &recorder))
        .build()
        .unwrap();
    let sampled = frames(8, 5_000);
    let ask = VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() };
    let (cx, budget, _) = metered();
    let out = runtime
        .analyze_video(&cx, VideoSource::frames(&sampled, Sampling::at(0.2)), &ask)
        .await
        .unwrap();

    // Two calls of four frames; each names images 2–3 of its batch.
    let starts: Vec<(u64, u64)> = out.value.events.iter().map(|e| (e.start_ms, e.end_ms)).collect();
    assert_eq!(starts, vec![(5_000, 10_000), (25_000, 30_000)]);
    let usage = &out.value.usage;
    assert_eq!(
        (usage.requests, usage.input_tokens, usage.output_tokens, usage.cost_micro_usd),
        (2, Some(1_800), Some(80), Some(0))
    );
    assert_eq!(budget.units().get(UsageUnit::Frames), 8);
    assert!(recorder.is_empty(), "an on-device analyzer sends nothing off the machine");
    {
        let seen = qwen.seen.lock().unwrap();
        let prompt = seen[1].contents[0].parts.last().unwrap().text().unwrap().to_string();
        assert!(
            prompt.contains("image 1 at 0:20.0") && prompt.contains("image 4 at 0:35.0"),
            "{prompt}"
        );
        assert_eq!(
            seen[0].contents[0]
                .parts
                .iter()
                .filter(|p| matches!(p, adk_core::Part::InlineData { .. }))
                .count(),
            4
        );
    }

    // A track request is refused: this analyzer only answers questions.
    let err = runtime
        .analyze_video(&cx, VideoSource::frames(&sampled, Sampling::at(0.2)), &track(&["forklift"]))
        .await
        .unwrap_err();
    assert!(matches!(err, VisionError::Unsupported(_)), "{err}");
}

#[tokio::test]
async fn a_remote_model_sees_only_released_frames() {
    let claude = Arc::new(Scripted {
        answer: r#"{"answer": "Not seen.", "events": []}"#.into(),
        locality: DataLocality::Remote,
        seen: Mutex::new(Vec::new()),
    });
    let describer = LlmDescriber::remote(claude.clone(), ["api.anthropic.com"]).unwrap();
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(FramesAnalyzer::remote(&describer)))
        .egress_gate(gate(allow(), &recorder))
        .build()
        .unwrap();
    let sampled = frames(3, 1_000);
    let ask = VideoRequest::Ask { question: "Is anyone there?".into() };
    let out = runtime
        .analyze_video(&VisionCx::new(), VideoSource::frames(&sampled, Sampling::at(1.0)), &ask)
        .await
        .unwrap();
    assert_eq!(out.value.answer.as_deref(), Some("Not seen."));
    assert_eq!(recorder.released().len(), 3, "every frame passed the gate");
    assert_eq!(out.calls[0].egress.len(), 3);
}

/// The live `sam-3.1` video answer for the crossing clip, recorded on 2026-11-21: three
/// orange rectangles — the forklift's body and two shelf boxes — on all 30 frames.
#[tokio::test]
async fn the_recorded_sam_video_answer_tracks_the_forklift() {
    let recording: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture(
            "recorded/sam-3-1.forklift-crossing-orange-rectangle.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(recording["response"].as_str().unwrap().to_string()),
        )
        .mount(&server)
        .await;
    let (runtime, _) = sam_runtime(&server, allow()).await;
    let crossing = clip("forklift-crossing-3s.mp4");
    let (cx, _, ledger) = metered();
    let out = runtime
        .analyze_video(
            &cx,
            VideoSource::clip(&crossing, Sampling::at(10.0)),
            &track(&["orange rectangle"]),
        )
        .await
        .unwrap();
    let frames = &out.value.frames;
    assert_eq!(frames.len(), 30);
    let body = |index: u64| {
        let left = -40.0 + 4.0 * index as f32;
        adk_vision::BBox::new(left.max(0.0), 175.0, left + 141.0, 261.0)
    };
    let forklift = frames[0]
        .regions
        .iter()
        .find(|r| r.shape.bbox().is_some_and(|b| b.iou(&body(0)) > 0.8))
        .and_then(|r| r.track)
        .unwrap();
    for frame in frames {
        assert_eq!(frame.regions.len(), 3);
        let b =
            frame.regions.iter().find(|r| r.track == Some(forklift)).unwrap().shape.bbox().unwrap();
        assert!((b.x0 - body(frame.index).x0).abs() <= 2.0, "frame {}: {b:?}", frame.index);
    }
    assert_eq!(spent(&ledger).await, 10_000);
    // The request the live call sent: the phrase and the clip as a data URL of these bytes.
    let sent: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(sent["input"][0]["content"][0], recording["request"]["input"][0]["content"][0]);
    let recorded_media =
        recording["request"]["input"][0]["content"][1]["video_url"].as_str().unwrap();
    assert!(recorded_media.contains(&crossing.digest().to_string()), "{recorded_media}");
}

/// Gemini's File API and `generateContent` as they answered live on 2026-11-21: the upload,
/// two polls from `PROCESSING` to `ACTIVE`, then `503` for high demand. The client's
/// requests match the recorded ones byte for byte where it matters: the uploaded bytes'
/// digest and the `generateContent` body with its `fileData` part, `videoMetadata` and
/// `mediaResolution`.
#[tokio::test]
async fn the_recorded_gemini_exchange_replays_and_the_overload_is_reported() {
    use wiremock::matchers::{header, query_param};
    let recording: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("recorded/gemini-file-api.aisle-forklift.json")).unwrap(),
    )
    .unwrap();
    let ex = recording["exchanges"].as_array().unwrap();
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
    Mock::given(method("POST"))
        .and(path("/upload/v1beta/files"))
        .and(query_param("upload_id", "u1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ex[1]["response"].clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1beta/files/c79io5wyjotq"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ex[2]["response"].clone()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1beta/files/c79io5wyjotq"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ex[3]["response"].clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"generateContent$"))
        .respond_with(ResponseTemplate::new(503).set_body_json(ex[4]["response"].clone()))
        .mount(&server)
        .await;
    let model = GeminiModel::new_with_base_url(
        "test-key",
        "gemini-3.8-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap()
    .with_retry_config(adk_model::RetryConfig::disabled());
    let analyzer = GeminiVideoAnalyzer::new(Arc::new(model))
        .with_media_resolution(MediaResolution::Low)
        .with_inline_limit(0)
        .with_processing_wait(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_secs(5),
        );
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(analyzer))
        .egress_gate(gate(allow(), &recorder))
        .build()
        .unwrap();
    let aisle = clip("aisle-forklift-120s.mp4");
    let ask = VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() };
    let (cx, budget, _) = metered();
    let err = runtime
        .analyze_video(&cx, VideoSource::clip(&aisle, Sampling::at(1.0)), &ask)
        .await
        .unwrap_err();
    let adk: adk_core::AdkError = err.into();
    assert!(adk.is_retryable() && adk.message.contains("high demand"), "{adk}");
    // The failed call returned its frames to the budget; the release stays on record.
    assert_eq!(budget.units().get(UsageUnit::Frames), 0);
    assert_eq!(recorder.released().len(), 1);

    let requests = server.received_requests().await.unwrap();
    let upload =
        requests.iter().find(|r| r.url.query().is_some_and(|q| q.contains("upload_id"))).unwrap();
    let digest = format!("{{{{media:binary:{}:{}}}}}", Digest::of(&upload.body), upload.body.len());
    assert_eq!(Value::String(digest), ex[1]["request"]);
    let sent: Value = serde_json::from_slice(
        &requests.iter().find(|r| r.url.path().ends_with("generateContent")).unwrap().body,
    )
    .unwrap();
    assert_eq!(sent, ex[4]["request"]);
}

/// What the judge was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    Target,
    Check(Vec<u64>),
    Question(Vec<u64>),
}

/// A vision-language model that answers from the frames it is shown, recognising each by its
/// width (`40 + index`, see [`numbered_frames`]): the orange forklift is visible in `seen` and
/// enters the aisle at frame `enters`; a frame in `false_alarm` looks like it to a visibility
/// check but not to the question.
struct Judge {
    seen: std::ops::RangeInclusive<u64>,
    enters: u64,
    false_alarm: Vec<u64>,
    locality: DataLocality,
    asked: Mutex<Vec<Asked>>,
}

impl Judge {
    fn new(seen: std::ops::RangeInclusive<u64>, enters: u64) -> Self {
        Self {
            seen,
            enters,
            false_alarm: Vec::new(),
            locality: DataLocality::InProcess,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn looks_visible(&self, index: u64) -> bool {
        self.seen.contains(&index) || self.false_alarm.contains(&index)
    }

    fn answer(&self, prompt: &str, frames: &[u64]) -> (Asked, Value) {
        if prompt.contains("What must be visible") {
            return (Asked::Target, json!({"target": "the orange forklift"}));
        }
        if prompt.contains("Is the orange forklift visible in this image?") {
            return (
                Asked::Check(frames.to_vec()),
                json!({"visible": self.looks_visible(frames[0])}),
            );
        }
        if prompt.contains("In which of them is the orange forklift visible?") {
            let visible: Vec<usize> =
                (1..=frames.len()).filter(|&n| self.looks_visible(frames[n - 1])).collect();
            return (Asked::Check(frames.to_vec()), json!({"visible": visible}));
        }
        assert!(prompt.contains("Question:"), "an unexpected prompt: {prompt}");
        let inside = |i: &u64| self.seen.contains(i) && *i >= self.enters;
        let answer = match frames.iter().position(inside) {
            Some(first) => {
                let last = frames.iter().rposition(|i| self.seen.contains(i)).unwrap();
                json!({"answer": "The forklift crosses the yellow line.",
                       "events": [{"start": first + 1, "end": last + 1, "text": "the forklift enters aisle 3"}]})
            }
            None => json!({"answer": "No forklift enters in these images.", "events": []}),
        };
        (Asked::Question(frames.to_vec()), answer)
    }
}

#[async_trait]
impl Llm for Judge {
    fn name(&self) -> &str {
        "qwen3-vl-4b-instruct"
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let parts = &req.contents[0].parts;
        let frames: Vec<u64> = parts
            .iter()
            .filter_map(|p| match p {
                adk_core::Part::InlineData { data, .. } => {
                    let image = SourceImage::decode(data, &DecodeLimits::default()).unwrap();
                    Some(u64::from(image.size().0) - 40)
                }
                _ => None,
            })
            .collect();
        let prompt = parts.last().and_then(adk_core::Part::text).unwrap_or_default().to_string();
        let (asked, answer) = self.answer(&prompt, &frames);
        self.asked.lock().unwrap().push(asked);
        let mut response = LlmResponse::new(Content::new("model").with_text(answer.to_string()));
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 100 * frames.len() as i32 + 50,
            candidates_token_count: 10,
            total_token_count: 100 * frames.len() as i32 + 60,
            ..Default::default()
        });
        response.turn_complete = true;
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }

    fn data_locality(&self) -> DataLocality {
        self.locality
    }
}

/// `n` frames `every_ms` apart, frame `i` being `40 + i` pixels wide so the judge knows it.
fn numbered_frames(n: u64, every_ms: u64) -> Vec<(FrameMeta, SourceImage)> {
    (0..n)
        .map(|i| {
            let meta = FrameMeta {
                source: FrameSourceId("aisle".into()),
                index: i,
                pts_ms: i * every_ms,
                captured_at: chrono::Utc::now(),
            };
            let image = DecodedImage::filled(40 + i as u32, 24, [90, 90, 90, 255]);
            (meta, SourceImage::from_pixels(image).unwrap())
        })
        .collect()
}

fn ask_when_it_enters() -> VideoRequest {
    VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() }
}

fn local_runtime(analyzer: FramesAnalyzer) -> (VisionRuntime, Arc<InMemoryEgressRecorder>) {
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .video_analyzer(Arc::new(analyzer))
        .egress_gate(gate(allow(), &recorder))
        .build()
        .unwrap();
    (runtime, recorder)
}

#[tokio::test]
async fn a_visibility_pass_asks_only_around_the_first_frame_that_shows_the_subject() {
    // 24 frames 5 s apart: the forklift is in frames 9–12 (0:45–1:00) and frame 3 fools a
    // check. The question over all 24 at once is what a small model gets wrong.
    let judge = Arc::new(Judge { false_alarm: vec![3], ..Judge::new(9..=12, 9) });
    let describer = LlmDescriber::local(judge.clone()).unwrap();
    let analyzer = FramesAnalyzer::local(&describer).with_visibility_pass(VisibilityPass::new());
    let (runtime, recorder) = local_runtime(analyzer);
    let sampled = numbered_frames(24, 5_000);
    let (cx, budget, _) = metered();
    let out = runtime
        .analyze_video(&cx, VideoSource::frames(&sampled, Sampling::at(0.2)), &ask_when_it_enters())
        .await
        .unwrap();

    let events: Vec<(u64, u64)> = out.value.events.iter().map(|e| (e.start_ms, e.end_ms)).collect();
    assert_eq!(events, vec![(45_000, 50_000)]);
    assert_eq!(out.value.answer.as_deref(), Some("The forklift crosses the yellow line."));
    // The target, frames 0–3 checked one by one, the false alarm refined over 2–4 and
    // dismissed, frames 4–9 checked, then the question over 8–10.
    let check = |i: u64| Asked::Check(vec![i]);
    let mut expected = vec![Asked::Target];
    expected.extend((0..=3).map(check));
    expected.push(Asked::Question(vec![2, 3, 4]));
    expected.extend((4..=9).map(check));
    expected.push(Asked::Question(vec![8, 9, 10]));
    assert_eq!(*judge.asked.lock().unwrap(), expected);
    let usage = &out.value.usage;
    // Thirteen calls: one text-only, ten of one image, two of three.
    assert_eq!(
        (usage.requests, usage.input_tokens, usage.output_tokens, usage.cost_micro_usd),
        (13, Some(13 * 50 + 16 * 100), Some(130), Some(0))
    );
    assert_eq!((usage.frames, budget.units().get(UsageUnit::Frames)), (24, 24));
    assert!(recorder.is_empty(), "an on-device analyzer sends nothing off the machine");
}

#[tokio::test]
async fn a_strided_visibility_pass_checks_in_batches_and_refines_every_frame() {
    // Two minutes at 1 fps; the forklift is on screen from 0:41 to 1:01 and enters at 0:45.
    let judge = Arc::new(Judge::new(41..=61, 45));
    let describer = LlmDescriber::local(judge.clone()).unwrap();
    let pass = VisibilityPass::new()
        .with_target("the orange forklift")
        .with_stride(5)
        .with_frames_per_check(4);
    let analyzer =
        FramesAnalyzer::local(&describer).with_frames_per_call(16).with_visibility_pass(pass);
    let (runtime, _) = local_runtime(analyzer);
    let sampled = numbered_frames(120, 1_000);
    let out = runtime
        .analyze_video(
            &VisionCx::new(),
            VideoSource::frames(&sampled, Sampling::at(1.0)),
            &ask_when_it_enters(),
        )
        .await
        .unwrap();

    assert_eq!(out.value.events[0].start_ms, 45_000);
    // Every fifth frame, four a call, until 0:45; then every frame from 0:40 to 0:50.
    assert_eq!(
        *judge.asked.lock().unwrap(),
        vec![
            Asked::Check(vec![0, 5, 10, 15]),
            Asked::Check(vec![20, 25, 30, 35]),
            Asked::Check(vec![40, 45, 50, 55]),
            Asked::Question((40..=50).collect()),
        ]
    );
    assert_eq!(out.value.usage.requests, 4);

    // Capped at four frames a call, the refine step reads every third frame of the window.
    let judge = Arc::new(Judge::new(41..=61, 45));
    let describer = LlmDescriber::local(judge.clone()).unwrap();
    let pass = VisibilityPass::new().with_target("the orange forklift").with_stride(5);
    let (runtime, _) = local_runtime(
        FramesAnalyzer::local(&describer).with_frames_per_call(4).with_visibility_pass(pass),
    );
    let out = runtime
        .analyze_video(
            &VisionCx::new(),
            VideoSource::frames(&sampled, Sampling::at(1.0)),
            &ask_when_it_enters(),
        )
        .await
        .unwrap();
    assert_eq!(judge.asked.lock().unwrap().last(), Some(&Asked::Question(vec![40, 43, 46, 49])));
    assert_eq!(out.value.events[0].start_ms, 46_000);
}

#[tokio::test]
async fn a_subject_no_frame_shows_names_no_event() {
    // The forklift is only in frame 99, past the six sampled.
    let judge = Arc::new(Judge::new(99..=99, 99));
    let describer = LlmDescriber::local(judge.clone()).unwrap();
    let pass = VisibilityPass::new().with_target("the orange forklift");
    let (runtime, _) = local_runtime(FramesAnalyzer::local(&describer).with_visibility_pass(pass));
    let sampled = numbered_frames(6, 5_000);
    let out = runtime
        .analyze_video(
            &VisionCx::new(),
            VideoSource::frames(&sampled, Sampling::at(0.2)),
            &ask_when_it_enters(),
        )
        .await
        .unwrap();
    assert!(out.value.events.is_empty());
    assert_eq!(
        out.value.answer.as_deref(),
        Some("the orange forklift is not seen in any of the 6 frames checked.")
    );
    assert_eq!(judge.asked.lock().unwrap().len(), 6);

    // Seen, but never entering: each positive is refined and dismissed.
    let judge = Arc::new(Judge::new(2..=3, 99));
    let describer = LlmDescriber::local(judge.clone()).unwrap();
    let pass = VisibilityPass::new().with_target("the orange forklift");
    let (runtime, _) = local_runtime(FramesAnalyzer::local(&describer).with_visibility_pass(pass));
    let out = runtime
        .analyze_video(
            &VisionCx::new(),
            VideoSource::frames(&sampled, Sampling::at(0.2)),
            &ask_when_it_enters(),
        )
        .await
        .unwrap();
    assert!(out.value.events.is_empty());
    assert_eq!(
        out.value.answer.as_deref(),
        Some(
            "the orange forklift is seen in 2 of the 6 frames checked, but the frames around \
             them do not answer the question."
        )
    );
    assert_eq!(out.value.usage.requests, 8, "six checks and two refines");
}

/// A Gemini analyzer against a local server that answers `generateContent` with the
/// forklift entering at 0:46, kept alive for the rest of the test process.
async fn gemini_answering() -> GeminiVideoAnalyzer {
    let answer = r#"{"answer": "The forklift enters aisle 3 at about 0:46.", "events": [{"start": "0:46.0", "end": "0:50.0", "text": "the forklift crosses the yellow line"}]}"#;
    let server: &'static MockServer = Box::leak(Box::new(gemini_server(answer).await));
    gemini(server)
}

/// A `sam-3.1` video tracker against a local server replaying the live answer for the
/// crossing clip, kept alive for the rest of the test process.
async fn sam_replaying() -> SamVideoTracker {
    let recording: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture(
            "recorded/sam-3-1.forklift-crossing-orange-rectangle.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(recording["response"].as_str().unwrap().to_string()),
        )
        .mount(&server)
        .await;
    sam(Box::leak(Box::new(server)))
}

/// The judge with the forklift in frames 9–12, entering at frame 9.
fn judge(locality: DataLocality) -> Arc<dyn Llm> {
    Arc::new(Judge { locality, ..Judge::new(9..=12, 9) })
}

/// Frames 5 s apart, the forklift entering at 0:45.
fn entering_frames() -> VideoFixture {
    VideoFixture::frames(numbered_frames(24, 5_000), Sampling::at(0.2), ask_when_it_enters())
        .with_expected_event(45_000, 5_000)
}

mod frames_local_conformance {
    use super::*;

    adk_vision::video_analyzer_conformance_tests! {
        backend = VideoAnalyzerTarget::Local(Arc::new(FramesAnalyzer::local(
            &LlmDescriber::local(judge(DataLocality::InProcess)).unwrap(),
        ))),
        fixture = entering_frames(),
    }
}

mod frames_visibility_conformance {
    use super::*;

    adk_vision::video_analyzer_conformance_tests! {
        backend = VideoAnalyzerTarget::Local(Arc::new(
            FramesAnalyzer::local(
                &LlmDescriber::local(judge(DataLocality::InProcess)).unwrap(),
            )
            .with_visibility_pass(VisibilityPass::new()),
        )),
        fixture = entering_frames(),
    }
}

mod frames_remote_conformance {
    use super::*;

    adk_vision::video_analyzer_conformance_tests! {
        backend = VideoAnalyzerTarget::Remote(Arc::new(
            FramesAnalyzer::remote(
                &LlmDescriber::remote(judge(DataLocality::Remote), ["api.anthropic.com"])
                    .unwrap(),
            )
            .with_visibility_pass(VisibilityPass::new().with_frames_per_check(8)),
        )),
        fixture = entering_frames(),
    }
}

mod gemini_conformance {
    use super::*;

    adk_vision::video_analyzer_conformance_tests! {
        backend = VideoAnalyzerTarget::Remote(Arc::new(gemini_answering().await)),
        fixture = VideoFixture::clip(clip("aisle-forklift-120s.mp4"), Sampling::at(1.0), ask_when_it_enters())
            .with_expected_event(45_000, 5_000),
    }
}

mod sam_conformance {
    use super::*;

    adk_vision::video_analyzer_conformance_tests! {
        backend = VideoAnalyzerTarget::Remote(Arc::new(sam_replaying().await)),
        fixture = VideoFixture::clip(
            clip("forklift-crossing-3s.mp4"),
            Sampling::at(10.0),
            track(&["orange rectangle"]),
        ),
    }
}
