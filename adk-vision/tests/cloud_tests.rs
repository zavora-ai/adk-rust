//! Contract tests for [`LlmDescriber`] on recorded provider responses, so the PR tier runs
//! without keys (spec 3.7 §7.7, §7.18, §11, S5).
//!
//! The responses in `tests/fixtures/cloud/recorded/` were recorded on 2026-11-19 through a
//! proxy that kept each request body (inline images reduced to their SHA-256 and size)
//! and the response, never a request header. Each test serves a recording from a local
//! server to the real `adk-model` client, so the request the describer builds, the
//! provider's wire format, the region adapter and the runtime's mapping to source pixels
//! all run as they did live. Gemini answered no request on 2026-11-19 (a plain text call
//! timed out after 300 s), so its tests serve answers written in Gemini's documented
//! format, and are marked synthetic.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_core::{
    AdkError, BudgetTracker, Content, ErrorCategory, ErrorComponent, InMemorySpendLedger, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, ModelCallScope, Part, RetryHint, RunBudget,
    SpendKey, SpendLedger, SpendPeriod, UsageUnit,
};
use adk_vision::cloud::{ClaudePixels, GeminiBox2d, Normalized1000, RegionAdapter};
use adk_vision::conformance::DescriberTarget;
use adk_vision::{
    BBox, DecodeLimits, DescribeSpec, EgressAction, EgressConfig, EgressGate,
    InMemoryEgressRecorder, LlmDescriber, Region, SourceImage, Transform, VisionCx, VisionError,
    VisionRuntime,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn boxes(value: &Value, key: &str, name: &str) -> Vec<(String, BBox)> {
    value[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let b: Vec<f32> =
                t[name].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
            let label = t["name"].as_str().or(t["label"].as_str()).unwrap().to_string();
            (label, BBox::new(b[0], b[1], b[2], b[3]))
        })
        .collect()
}

/// The S5 screenshot and its twelve targets.
fn s5() -> (SourceImage, Vec<(String, BBox)>, String) {
    let truth: Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("s5/s5.json")).unwrap())
            .unwrap();
    let targets = boxes(&truth, "targets", "box");
    let names = targets.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ");
    let prompt = format!(
        "This is a screenshot of a form. Locate each of these labelled widgets: {names}. \
         Label each region with the widget's name exactly as given (for example `name-field`)."
    );
    (load("s5/s5-screen.png"), targets, prompt)
}

/// The 960×540 shapes image and its five objects.
fn shapes() -> (SourceImage, Vec<(String, BBox)>, String) {
    let truth: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("cloud/shapes-960x540.groundtruth.json")).unwrap(),
    )
    .unwrap();
    let objects = boxes(&truth, "objects", "bbox");
    let names = objects.iter().map(|(n, _)| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
    let prompt = format!("Locate these objects and label each region exactly as given: {names}.");
    (load("cloud/shapes-960x540.png"), objects, prompt)
}

/// The largest edge error, in source pixels, of the region labelled like each target, and
/// how many targets were found.
fn score(regions: &[Region], truth: &[(String, BBox)]) -> (usize, f32) {
    let mut found = 0;
    let mut worst = 0f32;
    for (name, want) in truth {
        let name = name.to_ascii_lowercase();
        let hit = regions.iter().find(|r| {
            r.label.as_deref().is_some_and(|l| {
                let l = l.to_ascii_lowercase();
                l.contains(&name) || name.contains(&l)
            })
        });
        if let Some(b) = hit.and_then(|r| r.shape.bbox()) {
            found += 1;
            let e = [b.x0 - want.x0, b.y0 - want.y0, b.x1 - want.x1, b.y1 - want.y1]
                .into_iter()
                .map(f32::abs)
                .fold(0f32, f32::max);
            worst = worst.max(e);
        }
    }
    (found, worst)
}

struct Outcome {
    regions: Vec<Region>,
    released: Vec<adk_vision::EgressReceipt>,
    cost_micro_usd: u64,
    images: u64,
    model_calls: u64,
    request: Value,
}

/// Serves `fixture` at its recorded path and describes `image` through `describer`.
async fn replay(
    server: &MockServer,
    fixture: &Value,
    describer: LlmDescriber,
    image: &SourceImage,
    prompt: &str,
) -> Outcome {
    let body = &fixture["response"];
    let template = match body {
        Value::String(text) => ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(text.clone()),
        other => ResponseTemplate::new(200).set_body_json(other),
    };
    Mock::given(method("POST"))
        .and(path(fixture["path"].as_str().unwrap()))
        .respond_with(template)
        .mount(server)
        .await;
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        recorder.clone(),
    );
    let runtime = VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(gate)
        .build()
        .unwrap();
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let cx = VisionCx::new().with_scope(
        ModelCallScope::new().with_budget(budget.clone()).with_spend_ledger(ledger.clone(), "acme"),
    );
    let out =
        runtime.describe(&cx, &[image], &DescribeSpec::new(prompt).with_regions()).await.unwrap();
    let request = server.received_requests().await.unwrap().last().unwrap().body_json().unwrap();
    Outcome {
        regions: out.value.regions,
        released: recorder.released(),
        cost_micro_usd: ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(),
        images: budget.units().get(UsageUnit::Images),
        model_calls: budget.usage().model_calls,
        request,
    }
}

/// The SHA-256 and size of every inline image in a request body, in the recorded form.
fn images_in(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => map.values().for_each(|v| images_in(v, out)),
        Value::Array(list) => list.iter().for_each(|v| images_in(v, out)),
        Value::String(s) if s.len() > 512 => {
            use base64::Engine as _;
            let (mime, data) = match s.strip_prefix("data:").and_then(|r| r.split_once(";base64,"))
            {
                Some((mime, data)) => (mime, data),
                None => ("base64", s.as_str()),
            };
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                let digest = hex::encode(Sha256::digest(&bytes));
                out.push(format!("{{{{image:{mime}:sha256:{digest}:{}}}}}", bytes.len()));
            }
        }
        _ => {}
    }
}

fn recorded_images(fixture: &Value) -> Vec<String> {
    let text = fixture["request"].to_string();
    let mut found = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("{{image:") {
        let end = rest[start..].find("}}").unwrap() + start + 2;
        found.push(rest[start..end].to_string());
        rest = &rest[end..];
    }
    found
}

fn anthropic(server: &MockServer, model: &str) -> LlmDescriber {
    let config = adk_model::anthropic::AnthropicConfig::new("test-key", model)
        .allow_insecure_http()
        .with_base_url(server.uri());
    let client: Arc<dyn Llm> = Arc::new(adk_model::AnthropicClient::new(config).unwrap());
    LlmDescriber::remote(client, ["api.anthropic.com"])
        .unwrap()
        .with_effort(adk_core::Effort::Low)
        .with_max_output_tokens(4_000)
}

fn nim(server: &MockServer, model: &str) -> LlmDescriber {
    let config = adk_vision::cloud::nim::NimConfig::new(format!("{}/v1", server.uri()), "k", model);
    let client: Arc<dyn Llm> = Arc::new(config.client().unwrap());
    LlmDescriber::remote(client, ["integrate.api.nvidia.com"])
        .unwrap()
        .with_max_output_tokens(1_500)
        .with_unpriced_hold(0)
}

fn muse(server: &MockServer) -> LlmDescriber {
    let config = adk_model::MuseConfig::new("k", "muse-spark-1.3")
        .with_base_url(format!("{}/v1", server.uri()))
        .with_max_tokens(4_000);
    let client: Arc<dyn Llm> = Arc::new(adk_model::MuseClient::new(config).unwrap());
    LlmDescriber::remote(client, ["api.meta.ai"]).unwrap()
}

// --- S5 and the drawn shapes, recorded --------------------------------------------------

#[tokio::test]
async fn s5_claude_sonnet_answers_land_within_two_pixels_of_the_truth() {
    let (image, truth, prompt) = s5();
    let fixture = recording("claude-sonnet-5-5.s5-screen.json");
    let server = MockServer::start().await;
    let out =
        replay(&server, &fixture, anthropic(&server, "claude-sonnet-5-5"), &image, &prompt).await;
    let (found, worst) = score(&out.regions, &truth);
    assert_eq!(found, 12);
    assert!(worst <= 2.0, "worst edge error {worst} px");
    // The gate downscaled the 3024×1964 screenshot until Claude would not resize it, and
    // the answer was mapped back through that resize.
    let resize = out.regions[0].origin.chain.steps.iter().find_map(|s| match s {
        Transform::Resize { width, height } => Some((*width, *height)),
        _ => None,
    });
    assert_eq!(resize, Some((2356, 1530)));
    // The bytes sent are the bytes released and recorded.
    let mut sent = Vec::new();
    images_in(&out.request, &mut sent);
    assert_eq!(sent, recorded_images(&fixture));
    assert_eq!(out.released.len(), 1);
    assert!(sent[0].contains(&out.released[0].egress.to_string()["sha256:".len()..]));
    // One model call, one image, and its tokens priced from the catalog.
    assert_eq!((out.model_calls, out.images), (1, 1));
    assert!(out.cost_micro_usd > 0);
    assert_eq!(out.request["output_config"]["effort"], "low");
    assert!(out.request["output_config"]["format"]["schema"]["properties"]["regions"].is_object());
}

#[tokio::test]
async fn s5_claude_haiku_answers_land_within_two_pixels_of_the_truth() {
    let (image, truth, prompt) = s5();
    let fixture = recording("claude-haiku-5-5.s5-screen.json");
    let server = MockServer::start().await;
    let out =
        replay(&server, &fixture, anthropic(&server, "claude-haiku-5-5"), &image, &prompt).await;
    let (found, worst) = score(&out.regions, &truth);
    assert_eq!((found, out.images), (12, 1));
    assert!(worst <= 2.0, "worst edge error {worst} px");
}

#[tokio::test]
async fn claude_finds_the_drawn_shapes_in_the_pixels_it_was_sent() {
    let (image, truth, prompt) = shapes();
    for model in ["claude-sonnet-5-5", "claude-haiku-5-5"] {
        let fixture = recording(&format!("{model}.shapes.json"));
        let server = MockServer::start().await;
        let out = replay(&server, &fixture, anthropic(&server, model), &image, &prompt).await;
        let (found, _) = score(&out.regions, &truth);
        assert_eq!(found, 5, "{model}");
        // The rectangle, circle and triangle are exact; text boxes are the model's.
        let (_, shapes_worst) = score(&out.regions, &truth[..3]);
        assert!(shapes_worst <= 1.0, "{model}: {shapes_worst} px");
        assert!(out.regions[0].origin.chain.is_identity(), "a 960×540 image is sent as it is");
    }
}

#[tokio::test]
async fn nvidia_super_vl_answers_on_the_prompt_fixed_grid() {
    for (case, max_px) in [("s5-screen", 22.0), ("shapes", 12.0)] {
        let (image, truth, prompt) = if case == "s5-screen" { s5() } else { shapes() };
        let fixture = recording(&format!("nemotron-super-vl.{case}.json"));
        let server = MockServer::start().await;
        let out = replay(
            &server,
            &fixture,
            nim(&server, "nvidia/nemotron-3.5-super-vl-120b-a12b"),
            &image,
            &prompt,
        )
        .await;
        let (found, worst) = score(&out.regions, &truth);
        assert_eq!(found, truth.len(), "{case}");
        assert!(worst <= max_px, "{case}: {worst} px");
        // Reasoning off, structured output, the image inline, nothing priced.
        assert_eq!(out.request["chat_template_kwargs"], json!({"enable_thinking": false}));
        assert_eq!(out.request["response_format"]["type"], "json_schema");
        let mut sent = Vec::new();
        images_in(&out.request, &mut sent);
        assert_eq!(sent, recorded_images(&fixture), "{case}");
        assert_eq!(out.cost_micro_usd, 0, "NVIDIA publishes no price");
        assert_eq!(out.images, 1);
    }
}

#[tokio::test]
async fn gemma_4_and_muse_answer_on_the_prompt_fixed_grid() {
    let (image, truth, prompt) = shapes();
    let server = MockServer::start().await;
    let fixture = recording("gemma-4.shapes.json");
    let out =
        replay(&server, &fixture, nim(&server, "google/gemma-4-31b-it"), &image, &prompt).await;
    let (found, worst) = score(&out.regions, &truth);
    assert_eq!(found, 5);
    assert!(worst <= 9.0, "gemma 4: {worst} px");
    assert!(out.request.get("chat_template_kwargs").is_none(), "Gemma 4 has no reasoning switch");

    let server = MockServer::start().await;
    let fixture = recording("muse-spark-1-3.s5-screen.json");
    let (image, truth, prompt) = s5();
    let out = replay(&server, &fixture, muse(&server), &image, &prompt).await;
    let (found, worst) = score(&out.regions, &truth);
    assert_eq!(found, 12);
    assert!(worst <= 40.0, "muse: {worst} px");
    assert!(out.cost_micro_usd > 0, "Muse tokens are priced");
}

// --- Gemini, synthetic --------------------------------------------------------------------

/// A `generateContent` answer in Gemini's documented format.
fn gemini_answer(text: &str) -> Value {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": text}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 1600, "candidatesTokenCount": 300, "totalTokenCount": 1900}
    })
}

#[tokio::test]
async fn gemini_box_2d_is_read_y_first_on_0_to_1000_synthetic() {
    let (image, truth, prompt) = s5();
    // Gemini's quantisation: [ymin, xmin, ymax, xmax] rounded on 0–1000 of the image sent.
    let regions: Vec<Value> = truth
        .iter()
        .map(|(name, b)| {
            let q = |v: f32, side: f32| (v / side * 1000.0).round();
            json!({"label": name, "box_2d": [q(b.y0, 1964.0), q(b.x0, 3024.0), q(b.y1, 1964.0), q(b.x1, 3024.0)]})
        })
        .collect();
    let text = json!({"text": "twelve widgets", "regions": regions}).to_string();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(".*:generateContent$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(gemini_answer(&text)))
        .mount(&server)
        .await;
    let client: Arc<dyn Llm> = Arc::new(
        adk_model::GeminiModel::new_with_base_url(
            "test-key",
            "gemini-3.8-flash",
            format!("{}/v1beta/", server.uri()),
        )
        .unwrap(),
    );
    let describer = LlmDescriber::remote(client, ["generativelanguage.googleapis.com"]).unwrap();
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    let runtime = VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(gate)
        .build()
        .unwrap();
    let out = runtime
        .describe(&VisionCx::new(), &[&image], &DescribeSpec::new(prompt).with_regions())
        .await
        .unwrap();
    let (found, worst) = score(&out.value.regions, &truth);
    assert_eq!(found, 12);
    assert!(worst <= 2.0, "worst edge error {worst} px");
    let request: Value = server.received_requests().await.unwrap()[0].body_json().unwrap();
    let text = request.to_string();
    assert!(text.contains("[ymin, xmin, ymax, xmax] normalized to 0-1000"));
}

// --- Behaviour without a provider ----------------------------------------------------------

/// A model that answers every request with `text`, or with `error`.
struct Scripted {
    name: String,
    answer: Result<String, ErrorCategory>,
    seen: std::sync::Mutex<Vec<LlmRequest>>,
}

impl Scripted {
    fn new(name: &str, text: impl Into<String>) -> Self {
        Self { name: name.into(), answer: Ok(text.into()), seen: Default::default() }
    }

    fn failing(name: &str, category: ErrorCategory) -> Self {
        Self { name: name.into(), answer: Err(category), seen: Default::default() }
    }
}

#[async_trait]
impl Llm for Scripted {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.seen.lock().unwrap().push(req);
        match &self.answer {
            Ok(text) => {
                let response = LlmResponse {
                    content: Some(Content::new("model").with_text(text.clone())),
                    turn_complete: true,
                    ..LlmResponse::default()
                };
                Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
            }
            Err(category) => Err(AdkError::new(
                ErrorComponent::Model,
                *category,
                "model.scripted",
                "scripted failure",
            )
            .with_retry(RetryHint::for_category(*category))),
        }
    }
}

/// The conformance fixture's two boxes (red `(100, 80)–(260, 200)`, blue `(380, 260)–(560,
/// 420)` on 640×480), as each adapter's provider would answer.
fn fixture_answer(adapter: &str) -> String {
    let regions = match adapter {
        "claude" => json!([
            {"label": "red", "box": [100, 80, 260, 200]},
            {"label": "blue", "box": [380, 260, 560, 420]}
        ]),
        "gemini" => json!([
            {"label": "red", "box_2d": [167, 156, 417, 406]},
            {"label": "blue", "box_2d": [542, 594, 875, 875]}
        ]),
        _ => json!([
            {"label": "red", "bbox": [156, 167, 406, 417]},
            {"label": "blue", "bbox": [594, 542, 875, 875]}
        ]),
    };
    json!({"text": "two boxes", "regions": regions}).to_string()
}

fn scripted(model: &str, adapter: &str) -> LlmDescriber {
    LlmDescriber::remote(
        Arc::new(Scripted::new(model, fixture_answer(adapter))),
        ["provider.example.com"],
    )
    .unwrap()
}

fn rate_limited(model: &str) -> LlmDescriber {
    LlmDescriber::remote(
        Arc::new(Scripted::failing(model, ErrorCategory::RateLimited)),
        ["provider.example.com"],
    )
    .unwrap()
}

mod claude_conformance {
    use super::*;

    adk_vision::describer_conformance_tests! {
        backend = DescriberTarget::Remote(Arc::new(scripted("claude-sonnet-5-5", "claude"))),
        rate_limited = DescriberTarget::Remote(Arc::new(rate_limited("claude-sonnet-5-5"))),
    }
}

mod gemini_conformance {
    use super::*;

    adk_vision::describer_conformance_tests! {
        backend = DescriberTarget::Remote(Arc::new(scripted("gemini-3.8-flash", "gemini"))),
        rate_limited = DescriberTarget::Remote(Arc::new(rate_limited("gemini-3.8-flash"))),
    }
}

mod nim_conformance {
    use super::*;

    adk_vision::describer_conformance_tests! {
        backend = DescriberTarget::Remote(Arc::new(
            scripted("nvidia/nemotron-3.5-super-vl-120b-a12b", "nim").with_unpriced_hold(0)
        )),
        rate_limited = DescriberTarget::Remote(Arc::new(
            rate_limited("nvidia/nemotron-3.5-super-vl-120b-a12b")
        )),
    }
}

mod muse_conformance {
    use super::*;

    adk_vision::describer_conformance_tests! {
        backend = DescriberTarget::Remote(Arc::new(scripted("muse-spark-1.3", "muse"))),
    }
}

mod text_only_conformance {
    use super::*;

    /// The shapes fixture asking for text only: a describer with no region adapter refuses
    /// regions, so its conformance runs on a text request.
    fn text_fixture() -> adk_vision::conformance::Fixture {
        let mut fixture = adk_vision::conformance::Fixture::shapes();
        fixture.describe = DescribeSpec::new("What is in the image?");
        fixture.objects.clear();
        fixture
    }

    adk_vision::describer_conformance_tests! {
        backend = DescriberTarget::Remote(Arc::new(
            LlmDescriber::remote(Arc::new(Scripted::new("my-fine-tune", "a form")), ["models.example.com"])
                .unwrap()
        )),
        fixture = text_fixture(),
    }
}

#[tokio::test]
async fn a_model_with_no_adapter_refuses_regions_and_still_answers() {
    let model = Arc::new(Scripted::new("my-fine-tune", "{\"kind\": \"receipt\"}"));
    let describer = LlmDescriber::remote(model.clone(), ["models.example.com"]).unwrap();
    assert!(describer.adapter().is_none());
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    let runtime = VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(gate)
        .build()
        .unwrap();
    let (image, _, _) = shapes();
    let cx = VisionCx::new();
    let err = runtime
        .describe(&cx, &[&image], &DescribeSpec::new("where?").with_regions())
        .await
        .unwrap_err();
    assert!(matches!(err, VisionError::Unsupported(_)), "{err}");
    let schema = json!({"type": "object", "properties": {"kind": {"type": "string"}}});
    let out = runtime
        .describe(&cx, &[&image], &DescribeSpec::new("what is it?").with_schema(schema.clone()))
        .await
        .unwrap();
    assert_eq!(out.value.value, Some(json!({"kind": "receipt"})));
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "the refused call sent nothing");
    assert_eq!(seen[0].config.as_ref().unwrap().response_schema, Some(schema));
}

#[tokio::test]
async fn regions_and_a_value_come_back_together_with_the_released_image_first() {
    let model = Arc::new(Scripted::new(
        "claude-haiku-5-5",
        json!({"value": {"kind": "shapes"}, "regions": [{"label": "red", "box": [100, 80, 260, 200]}]})
            .to_string(),
    ));
    let describer = LlmDescriber::remote(model.clone(), ["api.anthropic.com"]).unwrap();
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    let runtime = VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(gate)
        .build()
        .unwrap();
    let image = adk_vision::conformance::Fixture::shapes().image;
    let spec = DescribeSpec::new("what is this?")
        .with_schema(json!({"type": "object", "properties": {"kind": {"type": "string"}}}))
        .with_regions();
    let out = runtime.describe(&VisionCx::new(), &[&image], &spec).await.unwrap();
    assert_eq!(out.value.value, Some(json!({"kind": "shapes"})));
    assert_eq!(out.value.regions[0].shape.bbox(), Some(BBox::new(100.0, 80.0, 260.0, 200.0)));
    let seen = model.seen.lock().unwrap();
    let parts = &seen[0].contents[0].parts;
    let Part::InlineData { annotations, .. } = &parts[0] else { panic!("the image is not first") };
    assert!(annotations.as_ref().unwrap()["adkVision"]["release"]["id"].is_string());
    let Part::Text { text } = &parts[1] else { panic!("no prompt") };
    assert!(text.contains("The image is 640x480 pixels"));
    let schema = seen[0].config.as_ref().unwrap().response_schema.as_ref().unwrap();
    assert_eq!(schema["required"], json!(["value", "regions"]));
}

#[tokio::test]
async fn a_local_describer_refuses_a_remote_model() {
    let remote = Arc::new(Scripted::new("claude-sonnet-5-5", "x"));
    let err = LlmDescriber::local(remote).unwrap_err();
    assert!(matches!(err, VisionError::InvalidInput(_)));
    assert!(LlmDescriber::remote(Arc::new(Scripted::new("m", "x")), Vec::<String>::new()).is_err());
}

#[tokio::test]
async fn an_answer_cut_off_at_the_output_cap_is_an_error_not_an_empty_answer() {
    struct CutOff;
    #[async_trait]
    impl Llm for CutOff {
        fn name(&self) -> &str {
            "nvidia/nemotron-3.5-super-vl-120b-a12b"
        }
        async fn generate_content(
            &self,
            _req: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<LlmResponseStream> {
            let response = LlmResponse {
                content: Some(Content::new("model").with_text("")),
                finish_reason: Some(adk_core::FinishReason::MaxTokens),
                turn_complete: true,
                ..LlmResponse::default()
            };
            Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
        }
    }
    let describer = LlmDescriber::remote(Arc::new(CutOff), ["integrate.api.nvidia.com"]).unwrap();
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    let runtime = VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(gate)
        .build()
        .unwrap();
    let image = adk_vision::conformance::Fixture::shapes().image;
    let err = runtime
        .describe(&VisionCx::new(), &[&image], &DescribeSpec::new("where?").with_regions())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("output cap"), "{err}");
}

// --- Preflight --------------------------------------------------------------------------

#[test]
fn preflight_counts_images_by_each_provider_formula() {
    let spec = DescribeSpec::new("Where is Save?").with_regions().with_max_output_tokens(1_000);
    let claude =
        LlmDescriber::remote(Arc::new(Scripted::new("claude-sonnet-5-5", "")), ["a.b"]).unwrap();
    // count_tokens on 2026-11-19: a 2356×1530 image is 4,675 visual tokens on Claude 5.5.
    let p = claude.preflight(&[(2356, 1530)], &spec);
    assert_eq!(p.image_tokens, 4_675);
    assert!(p.input_tokens > p.image_tokens);
    assert_eq!(p.output_tokens, Some(1_000));
    // Sonnet 5.5 bounds input at its highest input rate (1-hour cache writes, $4 per
    // million tokens) and output at $10 per million.
    let bound = p.max_cost_micro_usd.unwrap();
    let expected = p.input_tokens * 4 + 1_000 * 10;
    assert!((expected..=expected + 2).contains(&bound), "{bound} vs {expected}");

    let gemini =
        LlmDescriber::remote(Arc::new(Scripted::new("gemini-3.8-flash", "")), ["a.b"]).unwrap();
    assert_eq!(gemini.preflight(&[(1920, 1080)], &spec).image_tokens, 6 * 258);

    let nvidia = LlmDescriber::remote(
        Arc::new(Scripted::new("nvidia/nemotron-3.5-super-vl-120b-a12b", "")),
        ["a.b"],
    )
    .unwrap();
    let p = nvidia.preflight(&[(960, 540)], &spec);
    assert_eq!(p.image_tokens, 510);
    assert_eq!(p.max_cost_micro_usd, None, "NVIDIA publishes no price");
}

#[test]
fn preflight_bounds_the_recorded_usage() {
    for (fixture, model, sent) in [
        ("claude-sonnet-5-5.s5-screen.json", "claude-sonnet-5-5", (2356, 1530)),
        ("claude-haiku-5-5.shapes.json", "claude-haiku-5-5", (960, 540)),
    ] {
        let r = recording(fixture);
        let usage = &r["response"]["usage"];
        let input = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
            .iter()
            .map(|k| usage[*k].as_u64().unwrap_or(0))
            .sum::<u64>();
        let describer = LlmDescriber::remote(Arc::new(Scripted::new(model, "")), ["a.b"]).unwrap();
        let p = describer.preflight(&[sent], &DescribeSpec::new("x").with_regions());
        // The image is all but the text of the request.
        assert!(p.image_tokens <= input, "{fixture}: {} > {input}", p.image_tokens);
        assert!(input - p.image_tokens < 1_000, "{fixture}: text took {}", input - p.image_tokens);
    }
    // Super VL: 960×540 is 510 image tokens of the 663 the recorded request used.
    let r = recording("nemotron-super-vl.shapes.json");
    let prompt = r["response"]["usage"]["prompt_tokens"].as_u64().unwrap();
    let tokens = Normalized1000::nemotron("x").image_tokens().tokens((960, 540));
    assert!(tokens <= prompt && prompt - tokens < 300, "{tokens} of {prompt}");
}

#[test]
fn every_adapter_writes_its_frame_into_the_prompt() {
    let sizes = [(640, 480)];
    assert!(ClaudePixels::for_model("claude-haiku-5-5").instructions(&sizes).contains("640x480"));
    assert!(
        GeminiBox2d::for_model("gemini-3.8-flash").instructions(&sizes).contains("[ymin, xmin")
    );
    assert!(Normalized1000::gemma4("g").instructions(&sizes).contains("[x_min, y_min"));
    let two = [(640, 480), (100, 100)];
    assert!(
        ClaudePixels::for_model("claude-haiku-5-5")
            .instructions(&two)
            .contains("Image 1 is 100x100")
    );
}
