//! Contract tests for [`SamSegmenter`] on recorded `sam-3.1` streams, so the PR tier runs
//! without a key (spec 3.7 §7.7, §7.9, §11, S5, S7).
//!
//! `tests/fixtures/cloud/recorded/sam-3-1.*.json` were recorded on 2026-11-19 through a
//! proxy that kept each request body (the image reduced to its SHA-256 and size) and the
//! server-sent events, never a header. Each test serves a recording from a local server
//! to the real client, so request building, the stream parser and the mapping to source
//! pixels run as they did live. The S5 recording drops the three repeated copies of its
//! 80 KB answer text that the parser never reads.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::egress::EgressPolicy;
use adk_core::{
    AdkError, BudgetTracker, ErrorCategory, InMemorySpendLedger, ModelCallScope, RunBudget,
    SpendKey, SpendLedger, SpendPeriod, UsageUnit,
};
use adk_vision::conformance::{DetectorTarget, SegmenterTarget};
use adk_vision::sam::{SamMask, SamMaskDecoder};
use adk_vision::{
    BBox, CallOptions, DecodeLimits, DetectQuery, EgressAction, EgressConfig, EgressGate,
    InMemoryEgressRecorder, Route, SamConfig, SamSegmenter, SegmentPrompt, Shape, SourceImage,
    VisionCx, VisionError, VisionRuntime,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn recording(name: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join("cloud/recorded").join(name)).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn load(path: &str) -> SourceImage {
    SourceImage::decode(&std::fs::read(fixtures().join(path)).unwrap(), &DecodeLimits::default())
        .unwrap()
}

fn config(server: &MockServer) -> SamConfig {
    SamConfig::new("test-key").with_base_url(format!("{}/v1", server.uri()))
}

fn sse(response: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(response.as_str().unwrap().to_string())
}

/// A stream in Meta's format answering `text` on the output lane.
fn stream(text: &str) -> String {
    let event = |kind: &str, data: Value| format!("event: {kind}\ndata: {data}\n\n");
    let mut out = event(
        "response.created",
        json!({"type": "response.created", "response": {"status": "in_progress"}}),
    );
    if !text.is_empty() {
        out += &event(
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "item_id": "m", "output_index": 0, "content_index": 0, "delta": text}),
        );
    }
    out += &event(
        "response.output_text.done",
        json!({"type": "response.output_text.done", "item_id": "m", "output_index": 0, "content_index": 0, "text": text}),
    );
    out += &event(
        "response.completed",
        json!({"type": "response.completed", "response": {"status": "completed", "usage": {"images_processed": 1}}}),
    );
    out + "data: [DONE]\n\n"
}

struct Harness {
    runtime: VisionRuntime,
    budget: Arc<BudgetTracker>,
    ledger: Arc<InMemorySpendLedger>,
    recorder: Arc<InMemoryEgressRecorder>,
}

impl Harness {
    fn new(sam: SamSegmenter, budget: RunBudget) -> Self {
        let sam = Arc::new(sam);
        let recorder = Arc::new(InMemoryEgressRecorder::new());
        let gate = EgressGate::new(
            EgressPolicy::allow_private_networks(),
            EgressConfig::default().with_default(EgressAction::Allow),
            recorder.clone(),
        );
        let runtime = VisionRuntime::builder()
            .remote_segmenter(sam.clone())
            .remote_detector(sam)
            .egress_gate(gate)
            .build()
            .unwrap();
        Self {
            runtime,
            budget: Arc::new(BudgetTracker::new(budget)),
            ledger: Arc::new(InMemorySpendLedger::default()),
            recorder,
        }
    }

    fn cx(&self) -> VisionCx {
        VisionCx::new().with_scope(
            ModelCallScope::new()
                .with_budget(self.budget.clone())
                .with_spend_ledger(self.ledger.clone(), "acme"),
        )
    }

    async fn spent(&self) -> u64 {
        self.ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap()
    }
}

fn edge_error(got: BBox, want: BBox) -> f32 {
    [got.x0 - want.x0, got.y0 - want.y0, got.x1 - want.x1, got.y1 - want.y1]
        .into_iter()
        .map(f32::abs)
        .fold(0f32, f32::max)
}

/// Serves each concept's recording by the phrase in its request.
struct ByConcept(Vec<(String, Value)>);

impl Respond for ByConcept {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        let text = body["input"][0]["content"][0]["text"].as_str().unwrap_or_default();
        match self.0.iter().find(|(c, _)| c == text) {
            Some((_, response)) => sse(response),
            None => ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream("")),
        }
    }
}

#[tokio::test]
async fn s5_sam_boxes_land_within_two_pixels_of_the_truth() {
    let fixture = recording("sam-3-1.s5-screen-rectangle.json");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(sse(&fixture["response"]))
        .mount(&server)
        .await;
    let harness = Harness::new(SamSegmenter::new(config(&server)).unwrap(), RunBudget::new());
    let image = load("s5/s5-screen.png");
    let out = harness
        .runtime
        .segment(&harness.cx(), &image, &[SegmentPrompt::Concept { text: "rectangle".into() }])
        .await
        .unwrap();
    let truth: Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("s5/s5.json")).unwrap())
            .unwrap();
    assert_eq!(out.value.len(), 12);
    for target in truth["targets"].as_array().unwrap() {
        let b: Vec<f32> =
            target["box"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
        let want = BBox::new(b[0], b[1], b[2], b[3]);
        let got = out
            .value
            .iter()
            .filter_map(|r| r.shape.bbox())
            .max_by(|a, b| a.iou(&want).total_cmp(&b.iou(&want)))
            .unwrap();
        let err = edge_error(got, want);
        assert!(err <= 2.0, "{}: {got:?} is {err} px from {want:?}", target["name"]);
    }
    // sam-3.1 answers in pixels of the image as sent: no transform, labels from the concept.
    assert!(out.value.iter().all(|r| r.origin.chain.is_identity()));
    assert!(out.value.iter().all(|r| r.label.as_deref() == Some("rectangle")));
    // One request: one image unit at $2.50 per 1,000.
    assert_eq!(harness.spent().await, 2_500);
    assert_eq!(harness.budget.units().get(UsageUnit::Images), 1);
    // The request carried the released bytes in Meta's request shape.
    let request: Value = server.received_requests().await.unwrap()[0].body_json().unwrap();
    assert_eq!(request["model"], "sam-3.1");
    assert_eq!(request["stream"], true);
    let url = request["input"][0]["content"][1]["image_url"].as_str().unwrap();
    let released = harness.recorder.released();
    let data = url.strip_prefix("data:image/png;base64,").unwrap();
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).unwrap();
    assert_eq!(adk_vision::Digest::of(&bytes), released[0].egress);
    let recorded = fixture["request"].to_string();
    assert!(recorded.contains(&released[0].egress.to_string()["sha256:".len()..]));
}

#[tokio::test]
async fn three_concepts_are_three_requests_billed_one_image_each() {
    let concepts = ["red rectangle", "blue circle", "green triangle"];
    let recorded: Vec<(String, Value)> = concepts
        .iter()
        .map(|c| {
            let name = format!("sam-3-1.shapes-{}.json", c.replace(' ', "-"));
            ((*c).to_string(), recording(&name)["response"].clone())
        })
        .collect();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ByConcept(recorded))
        .mount(&server)
        .await;
    let harness = Harness::new(SamSegmenter::new(config(&server)).unwrap(), RunBudget::new());
    let image = load("cloud/shapes-960x540.png");
    let prompts: Vec<SegmentPrompt> =
        concepts.iter().map(|c| SegmentPrompt::Concept { text: (*c).into() }).collect();
    let out = harness.runtime.segment(&harness.cx(), &image, &prompts).await.unwrap();
    let truth = [
        ("red rectangle", BBox::new(80.0, 60.0, 320.0, 220.0)),
        ("blue circle", BBox::new(550.0, 60.0, 730.0, 240.0)),
        ("green triangle", BBox::new(140.0, 330.0, 301.0, 481.0)),
    ];
    for (name, want) in truth {
        let region = out.value.iter().find(|r| r.label.as_deref() == Some(name)).unwrap();
        let got = region.shape.bbox().unwrap();
        assert!(edge_error(got, want) <= 1.0, "{name}: {got:?}");
        assert!(matches!(region.shape, Shape::Box(_)), "no decoder: the object is its box");
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0].units.get(UsageUnit::Images), 3);
    assert_eq!(out.calls[0].cost_micro_usd, Some(7_500));
    assert_eq!(harness.spent().await, 7_500);
    assert_eq!(harness.budget.units().get(UsageUnit::Images), 3);
}

/// Every mask is solid, so a placed mask is exactly its box.
struct Solid(AtomicUsize);

#[async_trait]
impl SamMaskDecoder for Solid {
    async fn decode(&self, masks: &[SamMask]) -> adk_vision::Result<Vec<Vec<bool>>> {
        self.0.fetch_add(masks.len(), Ordering::SeqCst);
        Ok(masks.iter().map(|m| vec![true; (m.width * m.height) as usize]).collect())
    }
}

#[tokio::test]
async fn decoded_rasters_are_stretched_over_their_boxes_in_source_pixels() {
    // The recording holds a 1024×240 raster for the 1699×398 notes widget, and box-sized
    // rasters for the rest.
    let fixture = recording("sam-3-1.s5-screen-rectangle.json");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(sse(&fixture["response"]))
        .mount(&server)
        .await;
    let decoder = Arc::new(Solid(AtomicUsize::new(0)));
    let sam = SamSegmenter::new(config(&server)).unwrap().with_mask_decoder(decoder.clone());
    let harness = Harness::new(sam, RunBudget::new());
    let image = load("s5/s5-screen.png");
    let out = harness
        .runtime
        .segment(&harness.cx(), &image, &[SegmentPrompt::Concept { text: "rectangle".into() }])
        .await
        .unwrap();
    assert_eq!(decoder.0.load(Ordering::SeqCst), 12);
    for region in &out.value {
        let Shape::Mask(mask) = &region.shape else { panic!("not a mask") };
        assert_eq!((mask.width, mask.height), (3024, 1964));
        let b = mask.bbox().unwrap();
        assert_eq!(mask.area() as f32, b.area(), "a solid raster fills its box exactly");
    }
    let notes =
        out.value.iter().filter_map(|r| r.shape.bbox()).find(|b| b.width() > 1600.0).unwrap();
    assert_eq!(notes, BBox::new(400.0, 701.0, 2099.0, 1099.0));
}

#[tokio::test]
async fn no_match_is_an_empty_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream("")),
        )
        .mount(&server)
        .await;
    let harness = Harness::new(SamSegmenter::new(config(&server)).unwrap(), RunBudget::new());
    let image = load("cloud/shapes-960x540.png");
    let out = harness
        .runtime
        .detect(&harness.cx(), &image, &DetectQuery::Concepts(vec!["giraffe".into()]))
        .await
        .unwrap();
    assert!(out.value.is_empty());
    assert_eq!(harness.spent().await, 2_500, "a request that matched nothing is still billed");
}

#[tokio::test]
async fn object_prompts_go_in_one_request_in_meta_s_grammar() {
    let server = MockServer::start().await;
    let line =
        "<0f>1<|box;x1=80;y1=60;x2=319;y2=219;w=960;h=540|><|mask;x=0;y=0;data=160,240,~!|>\n";
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream(line)),
        )
        .mount(&server)
        .await;
    let harness = Harness::new(SamSegmenter::new(config(&server)).unwrap(), RunBudget::new());
    let image = load("cloud/shapes-960x540.png");
    let prompts = [
        SegmentPrompt::Box { bbox: BBox::new(78.0, 58.0, 322.0, 222.0) },
        SegmentPrompt::Point { at: [200.0, 140.0], positive: true },
    ];
    let out = harness.runtime.segment(&harness.cx(), &image, &prompts).await.unwrap();
    assert_eq!(out.value[0].shape.bbox(), Some(BBox::new(80.0, 60.0, 320.0, 220.0)));
    assert_eq!(out.value[0].label.as_deref(), Some("object 1"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().unwrap();
    assert_eq!(
        body["input"][0]["content"][0]["text"],
        "<0f>1<|box;x1=78;y1=58;x2=321;y2=221;w=960;h=540|><|point;x=200;y=140;w=960;h=540|>"
    );
}

#[tokio::test]
async fn a_rate_limit_is_retried_after_its_hint_then_reported() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    struct LimitedOnce(Arc<AtomicUsize>);
    impl Respond for LimitedOnce {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(stream(""))
            }
        }
    }
    Mock::given(method("POST")).respond_with(LimitedOnce(calls.clone())).mount(&server).await;
    let harness = Harness::new(SamSegmenter::new(config(&server)).unwrap(), RunBudget::new());
    let image = load("cloud/shapes-960x540.png");
    let query = DetectQuery::Concepts(vec!["red rectangle".into()]);
    harness.runtime.detect(&harness.cx(), &image, &query).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(harness.spent().await, 2_500, "the refused attempt is not billed twice");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
        .mount(&server)
        .await;
    let sam = SamSegmenter::new(config(&server).with_max_retries(0)).unwrap();
    let harness = Harness::new(sam, RunBudget::new());
    let err = harness.runtime.detect(&harness.cx(), &image, &query).await.unwrap_err();
    let adk: AdkError = err.into();
    assert_eq!(adk.category, ErrorCategory::RateLimited);
    assert_eq!(adk.retry.retry_after(), Some(std::time::Duration::from_secs(7)));
    assert_eq!(harness.spent().await, 0, "a refused call commits nothing");
}

#[tokio::test]
async fn provider_refusals_map_to_their_categories() {
    let image = load("cloud/shapes-960x540.png");
    let query = DetectQuery::Concepts(vec!["red rectangle".into()]);
    for (status, body, category) in [
        (401, json!({"error": {"message": "bad key"}}), ErrorCategory::Unauthorized),
        (
            400,
            json!({"error": {"message": "metadata[\"mask_encoding\"] must be one of ['lossless', 'one_bit']"}}),
            ErrorCategory::InvalidInput,
        ),
        (503, json!({"error": {"message": "overloaded"}}), ErrorCategory::Unavailable),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        let harness = Harness::new(SamSegmenter::new(config(&server)).unwrap(), RunBudget::new());
        let err = harness.runtime.detect(&harness.cx(), &image, &query).await.unwrap_err();
        assert_eq!(AdkError::from(err).category, category, "{status}");
    }
    // A stream cut before `response.completed` is not an answer.
    let server = MockServer::start().await;
    let cut = stream("").replace("response.completed", "response.in_progress");
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(cut),
        )
        .mount(&server)
        .await;
    let harness = Harness::new(SamSegmenter::new(config(&server)).unwrap(), RunBudget::new());
    let err = harness.runtime.detect(&harness.cx(), &image, &query).await.unwrap_err();
    assert!(matches!(err, VisionError::Unavailable { .. }), "{err}");
}

#[tokio::test]
async fn s7_a_run_capped_at_twenty_images_refuses_the_twenty_first_request_before_it_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream("")),
        )
        .mount(&server)
        .await;
    let harness = Harness::new(
        SamSegmenter::new(config(&server)).unwrap(),
        RunBudget::new().max_units(UsageUnit::Images, 20),
    );
    let image = load("cloud/shapes-960x540.png");
    let query = DetectQuery::Concepts(vec!["red rectangle".into()]);
    let cx = harness.cx();
    let mut ok = 0;
    let mut refusal = None;
    for _ in 0..30 {
        match harness.runtime.detect(&cx, &image, &query).await {
            Ok(_) => ok += 1,
            Err(err) => {
                refusal.get_or_insert(err);
            }
        }
    }
    assert_eq!(ok, 20);
    assert_eq!(server.received_requests().await.unwrap().len(), 20);
    assert_eq!(harness.spent().await, 50_000);
    let adk: AdkError = refusal.unwrap().into();
    assert_eq!(adk.code, "budget.units");
    assert!(adk.message.contains("images"), "{}", adk.message);

    // A five-concept query a budget cannot cover is refused before its first request.
    let harness = Harness::new(
        SamSegmenter::new(config(&server)).unwrap(),
        RunBudget::new().max_units(UsageUnit::Images, 3),
    );
    let five = DetectQuery::Concepts(["a", "b", "c", "d", "e"].map(String::from).to_vec());
    let before = server.received_requests().await.unwrap().len();
    assert!(harness.runtime.detect(&harness.cx(), &image, &five).await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

/// Answers the conformance fixture (red `(100, 80)–(260, 200)`, blue `(380, 260)–(560,
/// 420)` on 640×480) in Meta's format, by the concept asked for.
struct Colours;

impl Respond for Colours {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        let text = body["input"][0]["content"][0]["text"].as_str().unwrap_or_default();
        let url = body["input"][0]["content"][1]["image_url"].as_str().unwrap_or_default();
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(url.split_once(",").map_or("", |(_, d)| d))
            .unwrap_or_default();
        let (w, h) =
            image::load_from_memory(&bytes).map(|i| (i.width(), i.height())).unwrap_or((640, 480));
        // Boxes scale with the image sent, as the model's would.
        let s = |v: f32, side: u32, of: f32| (v * side as f32 / of).round() as u32;
        let line = match text {
            "red" => Some((100.0, 80.0, 260.0, 200.0)),
            "blue" => Some((380.0, 260.0, 560.0, 420.0)),
            _ => None,
        }
        .map(|(x0, y0, x1, y1)| {
            let (x1, y1, x2, y2) = (s(x0, w, 640.0), s(y0, h, 480.0), s(x1, w, 640.0) - 1, s(y1, h, 480.0) - 1);
            format!("<0f>0<|box;x1={x1};y1={y1};x2={x2};y2={y2};w={w};h={h}|><|mask;x=0;y=0;data=1,1,~!|>\n")
        })
        .unwrap_or_default();
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(stream(&line))
    }
}

/// A server answering every request with a rate limit and no retry.
fn limited() -> SamSegmenter {
    let server = futures::executor::block_on(MockServer::start());
    futures::executor::block_on(
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
            .mount(&server),
    );
    let sam =
        SamSegmenter::new(config(&server).with_max_retries(0)).unwrap().with_hosts(["api.meta.ai"]);
    std::mem::forget(server);
    sam
}

fn colours() -> SamSegmenter {
    let server = futures::executor::block_on(MockServer::start());
    futures::executor::block_on(Mock::given(method("POST")).respond_with(Colours).mount(&server));
    let sam = SamSegmenter::new(config(&server)).unwrap().with_hosts(["api.meta.ai"]);
    std::mem::forget(server);
    sam
}

mod segmenter_conformance {
    use super::*;

    adk_vision::segmenter_conformance_tests! {
        backend = SegmenterTarget::Remote(Arc::new(colours())),
        rate_limited = SegmenterTarget::Remote(Arc::new(limited())),
    }
}

mod detector_conformance {
    use super::*;

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Remote(Arc::new(colours())),
        rate_limited = DetectorTarget::Remote(Arc::new(limited())),
    }
}

#[tokio::test]
async fn sam_is_reachable_on_a_detection_route() {
    let sam = Arc::new(colours());
    let gate = EgressGate::new(
        EgressPolicy::allow_private_networks(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    let runtime = VisionRuntime::builder().remote_detector(sam).egress_gate(gate).build().unwrap();
    let image = adk_vision::conformance::Fixture::shapes().image;
    let out = runtime
        .detect_with(
            &VisionCx::new(),
            &image,
            &DetectQuery::Concepts(vec!["red".into(), "blue".into()]),
            &CallOptions::new().with_route(Route::to("sam-3.1")),
        )
        .await
        .unwrap();
    assert_eq!(out.value.len(), 2);
    assert_eq!(out.calls[0].units.get(UsageUnit::Images), 2);
}

/// A user-installed decoder (`MaskDecoderChoice::External`), played by stub shell scripts.
#[cfg(unix)]
mod external_decoder {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use adk_vision::MaskDecoderChoice;
    use adk_vision::sam::{ExternalMaskDecoder, SamMaskEncoding};

    use super::*;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn mask() -> SamMask {
        SamMask {
            height: 2,
            width: 2,
            encoding: SamMaskEncoding::Lossless,
            payload: "!!!!%abcde".into(),
        }
    }

    async fn refusal(body: &str, timeout: Duration) -> String {
        let dir = tempfile::tempdir().unwrap();
        let decoder =
            ExternalMaskDecoder::new(script(dir.path(), "decoder", body)).with_timeout(timeout);
        decoder.decode(&[mask()]).await.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn a_configured_program_decodes_the_masks_of_an_answer() {
        let dir = tempfile::tempdir().unwrap();
        // The stub keeps what it was given and answers a 2x2 raster with its diagonal set.
        let input = dir.path().join("input.json");
        let program = script(
            dir.path(),
            "decoder",
            r#"[ "$1" = "--json" ] || exit 3
cat > "$2"
printf '["AQAAAQ=="]'"#,
        );
        let line = "<0f>0<|box;x1=100;y1=80;x2=259;y2=199;w=960;h=540|><|mask;x=0;y=0;data=2,2,~!!!!%abcde|>\n";
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(stream(line)),
            )
            .mount(&server)
            .await;
        let choice = MaskDecoderChoice::external(
            &program,
            ["--json".to_string(), input.to_string_lossy().into_owned()],
        );
        let sam = SamSegmenter::new(config(&server).with_mask_decoder(choice)).unwrap();
        let harness = Harness::new(sam, RunBudget::new());
        let out = harness
            .runtime
            .segment(
                &harness.cx(),
                &load("cloud/shapes-960x540.png"),
                &[SegmentPrompt::Concept { text: "red rectangle".into() }],
            )
            .await
            .unwrap();
        let Shape::Mask(placed) = &out.value[0].shape else { panic!("not a mask") };
        // Two of the box's four 80x60 quadrants.
        assert_eq!(placed.area(), 2 * 80 * 60);
        assert_eq!(placed.bbox().unwrap(), BBox::new(100.0, 80.0, 260.0, 200.0));
        let sent: Value = serde_json::from_str(&std::fs::read_to_string(input).unwrap()).unwrap();
        assert_eq!(
            sent,
            json!([{"encoding": "lossless", "width": 2, "height": 2, "payload": "~!!!!%abcde"}])
        );
    }

    #[tokio::test]
    async fn a_missing_program_is_refused_when_the_segmenter_is_built() {
        for (program, says) in [
            ("/nonexistent/sam-mask-decode", "`/nonexistent/sam-mask-decode` was not found;"),
            ("no-such-sam-mask-decoder", "`no-such-sam-mask-decoder` was not found on PATH"),
        ] {
            let choice = MaskDecoderChoice::external(program, Vec::<String>::new());
            let err = SamSegmenter::new(SamConfig::new("k").with_mask_decoder(choice)).unwrap_err();
            assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
            assert!(err.to_string().contains(says), "{err}");
            assert!(err.to_string().contains("decoder = \"boxes\""), "{err}");
        }
        // A program removed after the segmenter was built fails the answer, not a guess.
        let err = ExternalMaskDecoder::new("/nonexistent/sam-mask-decode")
            .decode(&[mask()])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("was not found"), "{err}");
    }

    #[tokio::test]
    async fn malformed_answers_are_refused_with_what_went_wrong() {
        let second = Duration::from_secs(10);
        let cases = [
            ("echo not json", "answered unreadable output"),
            ("echo '[]'", "answered 0 rasters for 1 masks"),
            ("echo '[\"AQID\"]'", "raster 0 has 3 bytes for a 2x2 mask"),
            ("echo '[\"%%%\"]'", "raster 0 is not base64"),
            ("echo 'parser missing' >&2; exit 4", "parser missing"),
            ("head -c 100000 /dev/zero | tr '\\0' 'A'", "wrote more than"),
        ];
        for (body, says) in cases {
            let message = refusal(body, second).await;
            assert!(message.contains(says), "{body}: {message}");
            assert!(message.contains("sam-3.1 mask decoder"), "{message}");
        }
    }

    #[tokio::test]
    async fn a_slow_program_is_stopped_at_its_timeout() {
        let started = Instant::now();
        let message = refusal("sleep 30", Duration::from_millis(300)).await;
        assert!(message.contains("took longer than 300ms and was stopped"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn metas_parser_runs_in_node_with_the_package_path() {
        let decoder = ExternalMaskDecoder::meta_parser("/opt/sam/node_modules/@meta-sam/parser");
        assert_eq!(decoder.program(), Path::new("node"));
        assert!(format!("{decoder:?}").contains("/opt/sam/node_modules/@meta-sam/parser"));
        let other = decoder.with_program("/usr/local/bin/node");
        assert_eq!(other.program(), Path::new("/usr/local/bin/node"));
    }
}

#[test]
fn the_licensed_decoder_without_its_feature_says_how_to_get_it() {
    let config = SamConfig::new("k").with_mask_decoder(adk_vision::MaskDecoderChoice::Licensed);
    let result = SamSegmenter::new(config);
    if cfg!(feature = "sam-codec") {
        let err = result.unwrap_err();
        assert!(matches!(err, VisionError::LicenceAcceptanceRequired { .. }), "{err}");
    } else {
        let err = result.unwrap_err();
        assert!(matches!(err, VisionError::Unsupported(_)), "{err}");
        assert!(err.to_string().contains("`sam-codec` feature"), "{err}");
    }
}
