//! NVIDIA NIM describers on responses recorded from build.nvidia.com on 2026-11-18
//! (`tests/fixtures/cloud/nim/`, from the NIM endpoint map), served to the real
//! OpenAI-compatible client a [`NimConfig`] builds.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_core::{
    AdkError, BudgetTracker, ErrorCategory, InMemorySpendLedger, ModelCallScope, RunBudget,
    SpendKey, SpendLedger, SpendPeriod,
};
use adk_vision::cloud::nim::NimConfig;
use adk_vision::{
    AxisOrder, BBox, CoordFrame, DecodeLimits, DescribeSpec, Describer, EgressAction, EgressConfig,
    EgressGate, InMemoryEgressRecorder, LlmDescriber, SourceImage, VisionCx, VisionRuntime,
};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn nim_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cloud/nim").join(name)
}

fn recorded(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(nim_fixture(name)).unwrap()).unwrap()
}

async fn serve(server: &MockServer, response: &Value) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(response["status"].as_u64().unwrap() as u16)
                .set_body_json(&response["body"]),
        )
        .mount(server)
        .await;
}

fn runtime(describer: LlmDescriber) -> VisionRuntime {
    let gate = EgressGate::new(
        // The local server stands in for integrate.api.nvidia.com.
        EgressPolicy::allow_private_networks(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(gate)
        .build()
        .unwrap()
}

fn shapes_512() -> (SourceImage, Vec<(String, BBox)>) {
    let image = SourceImage::decode(
        &std::fs::read(nim_fixture("shapes-512x512.png")).unwrap(),
        &DecodeLimits::default(),
    )
    .unwrap();
    let truth: Value = recorded("shapes-512x512.groundtruth.json");
    let objects = truth["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let b: Vec<f32> =
                o["bbox"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
            (o["label"].as_str().unwrap().to_string(), BBox::new(b[0], b[1], b[2], b[3]))
        })
        .collect();
    (image, objects)
}

#[tokio::test]
async fn nano_omni_s_qwen_style_bbox_2d_answers_are_read_on_the_0_to_1000_grid() {
    let server = MockServer::start().await;
    serve(&server, &recorded("nano-omni-boxes-n1000-512sq.response.json")).await;
    let config = NimConfig::new(
        format!("{}/v1", server.uri()),
        "k",
        "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
    );
    let describer = config.describer().unwrap();
    assert_eq!(
        describer.descriptor().frame,
        CoordFrame::Normalized { scale: 1000, order: AxisOrder::Xy }
    );
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let cx = VisionCx::new().with_scope(
        ModelCallScope::new().with_budget(budget.clone()).with_spend_ledger(ledger.clone(), "acme"),
    );
    let (image, truth) = shapes_512();
    let out = runtime(describer)
        .describe(
            &cx,
            &[&image],
            &DescribeSpec::new("Locate the shapes and the text.").with_regions(),
        )
        .await
        .unwrap();
    assert_eq!(out.value.regions.len(), 5);
    // The answer maps exactly: the red rectangle's [80, 75, 345, 355] on 0–1000 of 512×512.
    let got = out.value.regions[0].shape.bbox().unwrap();
    let want = BBox::new(40.96, 38.4, 176.64, 181.76);
    for (a, b) in [(got.x0, want.x0), (got.y0, want.y0), (got.x1, want.x1), (got.y1, want.y1)] {
        assert!((a - b).abs() < 1e-3, "{got:?} vs {want:?}");
    }
    // The drawn shapes overlap their truth; the text boxes are the model's.
    for (name, want) in &truth[..3] {
        let region = out
            .value
            .regions
            .iter()
            .find(|r| r.label.as_deref().is_some_and(|l| l.contains(name.as_str())))
            .unwrap();
        let got = region.shape.bbox().unwrap();
        // Nano Omni's boxes are coarse; the NIM map measured up to 72 px on this image.
        assert!(got.iou(want) > 0.3, "{name}: {got:?} vs {want:?}");
    }
    // Reasoning off for a Nemotron model, and nothing priced.
    let request: Value = server.received_requests().await.unwrap()[0].body_json().unwrap();
    assert_eq!(request["chat_template_kwargs"]["enable_thinking"], false);
    assert!(
        request["messages"][0]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(), 0);
    assert_eq!(budget.usage().model_calls, 1);
}

#[tokio::test]
async fn an_answer_spent_on_reasoning_is_an_error() {
    // Super VL with reasoning on spent its whole budget thinking: `content: null` and
    // `finish_reason: "length"`.
    let server = MockServer::start().await;
    serve(&server, &recorded("super-vl-reasoning-cut-off.response.json")).await;
    let config = NimConfig::new(
        format!("{}/v1", server.uri()),
        "k",
        "nvidia/nemotron-3.5-super-vl-120b-a12b",
    );
    let (image, _) = shapes_512();
    let err = runtime(config.describer().unwrap())
        .describe(&VisionCx::new(), &[&image], &DescribeSpec::new("Where?").with_regions())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("output cap"), "{err}");
}

#[tokio::test]
async fn a_model_the_key_is_not_entitled_to_is_not_found() {
    let server = MockServer::start().await;
    serve(&server, &recorded("nvcf-not-found.response.json")).await;
    let config = NimConfig::new(format!("{}/v1", server.uri()), "k", "nvidia/cosmos-reason2-8b");
    let (image, _) = shapes_512();
    let err = runtime(config.describer().unwrap())
        .describe(&VisionCx::new(), &[&image], &DescribeSpec::new("What is this?"))
        .await
        .unwrap_err();
    let adk: AdkError = err.into();
    assert_eq!(adk.category, ErrorCategory::NotFound, "{adk}");
}

#[tokio::test]
async fn gemma_4_is_sent_no_reasoning_switch() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "status": 200,
        "body": {
            "id": "c", "object": "chat.completion", "created": 0, "model": "google/gemma-4-31b-it",
            "choices": [{"index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "A white image with three shapes."}}],
            "usage": {"prompt_tokens": 300, "completion_tokens": 9, "total_tokens": 309}
        }
    });
    serve(&server, &body).await;
    let config = NimConfig::new(format!("{}/v1", server.uri()), "k", "google/gemma-4-31b-it");
    let (image, _) = shapes_512();
    let out = runtime(config.describer().unwrap())
        .describe(&VisionCx::new(), &[&image], &DescribeSpec::new("What is this?"))
        .await
        .unwrap();
    assert_eq!(out.value.text.as_deref(), Some("A white image with three shapes."));
    let request: Value = server.received_requests().await.unwrap()[0].body_json().unwrap();
    assert!(request.get("chat_template_kwargs").is_none());
    assert!(request.get("response_format").is_none(), "no schema, no regions: plain text");
}
