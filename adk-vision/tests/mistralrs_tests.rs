//! The mistral.rs describer (spec 3.7 §7.6, feature `mistralrs`): Qwen3-VL in this process,
//! regions in the model's own 0–1000 frame, `InProcess` so no image reaches the egress gate.
//!
//! The contract tests run a scripted in-process model that answers as Qwen3-VL does — a
//! bare list of `{"bbox_2d": [x1, y1, x2, y2], "label": ...}` on 0–1000 — through the
//! describer conformance suite. The live tests load `Qwen/Qwen3-VL-2B-Instruct` (4.26 GB)
//! through the fetcher:
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-vision --features mistralrs,conformance \
//!     --test mistralrs_tests --run-ignored only --no-capture
//! ```

use std::sync::{Arc, Mutex};
use std::time::Instant;

use adk_core::{
    Content, DataLocality, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, UsageUnit,
};
use adk_mistralrs::MistralRsConfig;
use adk_vision::cloud::Normalized1000;
use adk_vision::conformance::{DescriberTarget, Fixture};
use adk_vision::mistralrs::QWEN3_VL_2B;
use adk_vision::{
    AxisOrder, BBox, CoordFrame, DecodeLimits, DescribeSpec, Describer, Digest, FetchConfig,
    InMemoryAcceptanceStore, LicenceGate, LicencePolicy, LlmDescriber, Locality, ManifestSet,
    ModelFetcher, ModelManifest, SourceImage, VerifiedModel, VisionCx, VisionError, VisionRuntime,
};
use async_trait::async_trait;
use serde_json::json;

/// Qwen3-VL's answer for the conformance shapes (red `(100, 80)–(260, 200)`, blue
/// `(380, 260)–(560, 420)` on 640×480), in its native form and frame.
const QWEN_ANSWER: &str = r#"```json
[
  {"bbox_2d": [156, 167, 406, 417], "label": "red"},
  {"bbox_2d": [594, 542, 875, 875], "label": "blue"}
]
```"#;

/// A model in this process that answers every request with `answer`, keeping the requests.
struct InProcess {
    name: String,
    answer: String,
    seen: Mutex<Vec<LlmRequest>>,
}

impl InProcess {
    fn qwen() -> Self {
        Self {
            name: "Qwen/Qwen3-VL-2B-Instruct".into(),
            answer: QWEN_ANSWER.into(),
            seen: Mutex::default(),
        }
    }
}

#[async_trait]
impl Llm for InProcess {
    fn name(&self) -> &str {
        &self.name
    }

    fn data_locality(&self) -> DataLocality {
        DataLocality::InProcess
    }

    fn max_cost_micro_usd(&self, _request: &LlmRequest) -> Option<u64> {
        Some(0)
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.seen.lock().unwrap().push(request);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text(self.answer.clone())),
            turn_complete: true,
            ..LlmResponse::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

fn scripted() -> LlmDescriber<adk_vision::SentImage> {
    LlmDescriber::local(Arc::new(InProcess::qwen())).unwrap()
}

mod qwen3_vl_conformance {
    use super::*;

    adk_vision::describer_conformance_tests! {
        backend = DescriberTarget::Local(Arc::new(scripted())),
    }
}

#[test]
fn qwen3_vl_answers_in_its_own_zero_to_thousand_frame() {
    let describer = scripted();
    let descriptor = describer.descriptor();
    assert_eq!(descriptor.locality, Locality::InProcess);
    assert_eq!(descriptor.frame, CoordFrame::Normalized { scale: 1000, order: AxisOrder::Xy });
    assert_eq!(descriptor.price, None, "a model on this machine is never priced");
    let adapter = describer.adapter().expect("a region adapter by model id");
    assert!(!adapter.structured_output(), "mistral.rs takes no response schema");
    assert_eq!(adapter.image_tokens().tokens((640, 480)), 300);
}

#[tokio::test]
async fn regions_reach_the_caller_in_source_pixels_without_a_schema() {
    let model = Arc::new(InProcess::qwen());
    let describer = LlmDescriber::local(model.clone()).unwrap();
    let runtime = VisionRuntime::builder().describer(Arc::new(describer)).build().unwrap();
    let fixture = Fixture::shapes();

    let out =
        runtime.describe(&VisionCx::new(), &[&fixture.image], &fixture.describe).await.unwrap();

    let boxes: Vec<BBox> = out.value.regions.iter().filter_map(|r| r.shape.bbox()).collect();
    assert_eq!(boxes.len(), 2);
    for (found, expected) in boxes.iter().zip(&fixture.objects) {
        for (a, b) in [
            (found.x0, expected.bbox.x0),
            (found.y0, expected.bbox.y0),
            (found.x1, expected.bbox.x1),
            (found.y1, expected.bbox.y1),
        ] {
            assert!((a - b).abs() <= 1.0, "{found:?} vs {:?}", expected.bbox);
        }
    }
    let request = model.seen.lock().unwrap()[0].clone();
    assert!(request.config.as_ref().and_then(|c| c.response_schema.as_ref()).is_none());
    let prompt: String = request.contents[0]
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(prompt.contains("normalised to 0-1000"), "{prompt}");
    assert_eq!(out.calls[0].units.get(UsageUnit::Images), 1);
    assert_eq!(out.calls[0].cost_micro_usd, Some(0));
}

/// A toy model directory with a manifest for it.
fn toy_model(
    runtime: serde_json::Value,
    extra: Option<&str>,
) -> (tempfile::TempDir, VerifiedModel) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.json"), b"{}").unwrap();
    if let Some(name) = extra {
        std::fs::write(dir.path().join(name), b"unpinned").unwrap();
    }
    let manifest = ModelManifest::from_json(
        &json!({
            "id": "toy-vl",
            "task": "describe",
            "source": {"kind": "local", "dir": dir.path()},
            "files": [{"path": "config.json", "size": 2, "sha256": Digest::of(b"{}")}],
            "licence": {"id": "Apache-2.0", "url": "https://www.apache.org/licenses/LICENSE-2.0", "class": "permissive"},
            "runtime": runtime,
        })
        .to_string(),
    )
    .unwrap();
    let (policy, store) = (LicencePolicy::new(), InMemoryAcceptanceStore::new());
    let model = VerifiedModel::check_dir(&manifest, dir.path(), &LicenceGate::new(&policy, &store))
        .unwrap();
    (dir, model)
}

#[tokio::test]
async fn a_manifest_for_another_runtime_is_refused_before_loading() {
    let (_dir, model) = toy_model(json!({"kind": "candle"}), None);
    let err = LlmDescriber::mistralrs(&model, MistralRsConfig::default()).await.unwrap_err();
    assert!(
        matches!(&err, VisionError::Manifest { reason, .. } if reason.contains("mistralRs")),
        "{err}"
    );
}

#[tokio::test]
async fn unpinned_weights_beside_the_model_are_refused_before_loading() {
    let (_dir, model) = toy_model(
        json!({"kind": "mistralRs", "architecture": "qwen3vl"}),
        Some("extra.safetensors"),
    );
    let err = LlmDescriber::mistralrs(&model, MistralRsConfig::default()).await.unwrap_err();
    assert!(
        matches!(&err, VisionError::Manifest { reason, .. } if reason.contains("would load unverified")),
        "{err}"
    );
}

#[test]
fn the_builtin_manifest_pins_qwen3_vl_2b() {
    let manifests = ManifestSet::builtin().unwrap();
    let qwen = manifests.get(QWEN3_VL_2B).expect("built in");
    assert_eq!(qwen.licence.id, "Apache-2.0");
    assert!(qwen.file("model.safetensors").is_some_and(|f| f.size == 4_255_140_312));
    assert_eq!(
        serde_json::to_value(&qwen.runtime).unwrap(),
        json!({"kind": "mistralRs", "architecture": "qwen3vl"})
    );
}

// --- Live: Qwen/Qwen3-VL-2B-Instruct in this process -----------------------------------------

/// The model's text, recorded, so the live run shows the frame it answers in.
struct Recorded {
    inner: Arc<dyn Llm>,
    texts: Mutex<Vec<String>>,
}

#[async_trait]
impl Llm for Recorded {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn data_locality(&self) -> DataLocality {
        self.inner.data_locality()
    }

    fn max_cost_micro_usd(&self, request: &LlmRequest) -> Option<u64> {
        self.inner.max_cost_micro_usd(request)
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        use futures::StreamExt;
        let responses: Vec<_> = self.inner.generate_content(request, stream).await?.collect().await;
        let text: String = responses
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .filter(|r| !r.partial)
            .flat_map(|r| r.content.iter().flat_map(|c| c.parts.iter()))
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        self.texts.lock().unwrap().push(text);
        Ok(Box::pin(futures::stream::iter(responses)))
    }
}

async fn qwen_model() -> VerifiedModel {
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    fetcher.fetch(ManifestSet::builtin().unwrap().get(QWEN3_VL_2B).unwrap()).await.unwrap()
}

/// Box IoU.
fn iou(a: &BBox, b: &BBox) -> f32 {
    let w = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let h = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    let inter = w * h;
    inter / (a.width() * a.height() + b.width() * b.height() - inter)
}

/// The `bbox_2d` boxes of a Qwen answer, as written.
fn native_boxes(text: &str) -> Vec<[f32; 4]> {
    let start = text.find('[').expect("a JSON list");
    let end = text.rfind(']').expect("a JSON list");
    let items: Vec<serde_json::Value> = serde_json::from_str(&text[start..=end]).unwrap();
    items
        .iter()
        .filter_map(|item| {
            let b = item.get("bbox_2d")?.as_array()?;
            let n: Vec<f32> = b.iter().filter_map(|v| v.as_f64()).map(|v| v as f32).collect();
            (n.len() == 4).then(|| [n[0], n[1], n[2], n[3]])
        })
        .collect()
}

/// Qwen3-VL-2B asked for the conformance shapes in its own words, then through the
/// describer. Read as 0–1000 of the image its boxes land on the shapes; read as pixels of
/// its input (640×480, which its resize keeps) they do not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "loads Qwen/Qwen3-VL-2B-Instruct (4.26 GB); see the module docs"]
async fn qwen3_vl_2b_grounds_in_its_own_frame() {
    let verified = qwen_model().await;
    let loaded = Instant::now();
    let describer = LlmDescriber::mistralrs(&verified, MistralRsConfig::default()).await.unwrap();
    println!("loaded {QWEN3_VL_2B} in {:?}", loaded.elapsed());
    assert_eq!(describer.descriptor().locality, Locality::InProcess);
    assert_eq!(describer.model().data_locality(), DataLocality::InProcess);
    let recorded = Arc::new(Recorded { inner: describer.model().clone(), texts: Mutex::default() });
    let describer = LlmDescriber::local(recorded.clone())
        .unwrap()
        .with_id(QWEN3_VL_2B)
        .with_adapter(Arc::new(Normalized1000::qwen3_vl(QWEN3_VL_2B)));
    let runtime = VisionRuntime::builder().describer(Arc::new(describer)).build().unwrap();
    let fixture = Fixture::shapes();
    let (w, h) = (640.0, 480.0);

    // The model's own frame: no format instructions, Qwen's grounding prompt.
    let native = DescribeSpec::new(
        "Locate the red box and the blue box. Output their bbox_2d coordinates in JSON.",
    );
    let started = Instant::now();
    runtime.describe(&VisionCx::new(), &[&fixture.image], &native).await.unwrap();
    let native_text = recorded.texts.lock().unwrap().last().cloned().unwrap();
    println!("native answer in {:?}: {native_text}", started.elapsed());
    let boxes = native_boxes(&native_text);
    assert_eq!(boxes.len(), 2, "{native_text}");
    for (raw, expected) in boxes.iter().zip(&fixture.objects) {
        let as_thousandths = BBox::new(
            raw[0] / 1000.0 * w,
            raw[1] / 1000.0 * h,
            raw[2] / 1000.0 * w,
            raw[3] / 1000.0 * h,
        );
        let as_pixels = BBox::new(raw[0], raw[1], raw[2], raw[3]);
        let (thousandths, pixels) =
            (iou(&as_thousandths, &expected.bbox), iou(&as_pixels, &expected.bbox));
        println!(
            "{:?}: raw {raw:?}; IoU read as 0-1000 {thousandths:.3}, read as pixels {pixels:.3}",
            expected.label
        );
        assert!(
            thousandths >= 0.7 && pixels <= 0.3,
            "{:?}: the frame is not 0-1000",
            expected.label
        );
    }

    let started = Instant::now();
    let out =
        runtime.describe(&VisionCx::new(), &[&fixture.image], &fixture.describe).await.unwrap();
    let elapsed = started.elapsed();
    let text = recorded.texts.lock().unwrap().last().cloned().unwrap();
    println!("describer answer in {elapsed:?}: {text}");
    for expected in &fixture.objects {
        let best = out
            .value
            .regions
            .iter()
            .filter_map(|r| r.shape.bbox())
            .map(|b| (iou(&b, &expected.bbox), b))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .expect("a region");
        println!("{:?}: best {:?} IoU {:.3}", expected.label, best.1, best.0);
        assert!(best.0 >= 0.7, "{:?} IoU {}", expected.label, best.0);
    }
    assert_eq!(out.calls[0].cost_micro_usd, Some(0));
}

/// A photo of a skier: Qwen3-VL-2B's person box against RF-DETR's on the same photo
/// (COCO 2017 val 000000000785; `ADK_VISION_COCO_DIR` holds it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "loads Qwen/Qwen3-VL-2B-Instruct (4.26 GB) and a COCO photo; see the module docs"]
async fn qwen3_vl_2b_finds_the_skier() {
    let dir = std::env::var("ADK_VISION_COCO_DIR").expect("ADK_VISION_COCO_DIR");
    let bytes = std::fs::read(std::path::Path::new(&dir).join("000000000785.jpg")).unwrap();
    assert_eq!(
        Digest::of(&bytes).to_string(),
        "sha256:83981537a7baeafbeb9c8cb67b3484dc26433f574b3685d021fa537e277e4726"
    );
    let photo = SourceImage::decode(&bytes, &DecodeLimits::default()).unwrap();
    let describer =
        LlmDescriber::mistralrs(&qwen_model().await, MistralRsConfig::default()).await.unwrap();
    let runtime = VisionRuntime::builder().describer(Arc::new(describer)).build().unwrap();
    let spec =
        DescribeSpec::new("What is the person doing? Give a box for every person.").with_regions();

    let started = Instant::now();
    let out = runtime.describe(&VisionCx::new(), &[&photo], &spec).await.unwrap();

    println!("in {:?}: {:?}", started.elapsed(), out.value.text);
    // COCO's ground-truth person box for this photo, rounded.
    let truth = BBox::new(280.0, 44.0, 500.0, 392.0);
    let best = out
        .value
        .regions
        .iter()
        .filter_map(|r| r.shape.bbox())
        .map(|b| (iou(&b, &truth), b))
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .expect("a person");
    println!("person {:?} IoU {:.3} against {truth:?}", best.1, best.0);
    assert!(best.0 >= 0.7, "IoU {}", best.0);
}
