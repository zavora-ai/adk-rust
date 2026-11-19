//! S8, same tool, any backend: one `vision_read` call shape for PP-OCRv6 tiny (on the
//! device), PaddleOCR-VL-1.6 (its recorded page; its 1.9 GB weights do not fit CI) and NVIDIA
//! NIM's CV endpoints (recorded responses through the real client), and one `vision_detect`
//! shape for RF-DETR Nano (on the device, COCO's closed classes) and `sam-3.1` (open
//! vocabulary, a recorded live response) on drawn shapes. Each
//! answer matches the published schema; accuracy, latency and cost are reported side by
//! side; the backends that run here pass the conformance suite.

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_harness::HarnessMode;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_vision::conformance::{AnyProbe, DetectorTarget, Expected, Fixture, ReaderTarget};
use adk_vision::{
    BackendDescriptor, Block, CoordFrame, DetectQuery, DocumentPage, DocumentReader, Locality,
    NIM_READER_ID, NimCvConfig, NimDocumentReader, RawRegion, ReadOptions, Route, SamConfig,
    SamSegmenter, SentImage, VisionConfig, VisionCx, VisionRuntime,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::Checks;
use crate::fixtures::{
    DeskSpec, Factory, crate_fixtures, desk, load_image, models, normalise, page, read_json, save,
    tool_responses,
};
use crate::replay::{Recording, client_to, serve_always};

const DOC_PAGE: &str = "doc-page-1000x1300.png";
const VL: &str = "paddleocr-vl-1.6";
const NIM_HOST: &str = "ai.api.nvidia.com";

/// PaddleOCR-VL-1.6's recorded page for the report page, answered as the on-device reader
/// answers it.
struct RecordedVl {
    descriptor: BackendDescriptor,
    page: DocumentPage<RawRegion>,
}

impl RecordedVl {
    fn load() -> Result<Self> {
        let recorded: DocumentPage = serde_json::from_value(read_json(
            &crate_fixtures().join("documents/recorded/paddleocr-vl-1.6.doc-page-1000x1300.json"),
        )?)?;
        let page = DocumentPage {
            blocks: recorded
                .blocks
                .into_iter()
                .map(|b| Block {
                    kind: b.kind,
                    region: RawRegion::shape(b.region.shape),
                    text: b.text,
                    confidence: b.confidence,
                    words: Vec::new(),
                    cells: Vec::new(),
                })
                .collect(),
            reading_order: recorded.reading_order,
            markdown: recorded.markdown,
        };
        let descriptor = BackendDescriptor::new(VL, Locality::InProcess, CoordFrame::SentPixels)
            .with_manifest(VL);
        Ok(Self { descriptor, page })
    }
}

#[async_trait::async_trait]
impl DocumentReader for RecordedVl {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        _cx: &VisionCx,
        _page: &SentImage,
        _options: &ReadOptions,
    ) -> adk_vision::Result<DocumentPage<RawRegion>> {
        Ok(self.page.clone())
    }
}

/// NVIDIA's four CV endpoints answering the report page with their recorded responses.
async fn nim_server() -> Result<MockServer> {
    let server = MockServer::start().await;
    for (model, pair) in [
        ("nvidia/nemotron-ocr-v1", "cv-doc-nemotron-ocr-v1-merge-word"),
        ("nvidia/nemotron-page-elements-v3", "cv-doc-nemotron-page-elements-v3"),
        ("nvidia/nemotron-table-structure-v1", "cv-doc-nemotron-table-structure-v1"),
        ("nvidia/nemotron-graphic-elements-v1", "cv-doc-nemotron-graphic-elements-v1"),
    ] {
        let response = read_json(&crate_fixtures().join(format!("nim/cv/{pair}.response.json")))?;
        Mock::given(method("POST"))
            .and(path(format!("/v1/cv/{model}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(response["body"].clone()))
            .mount(&server)
            .await;
    }
    Ok(server)
}

fn nim_reader(server: &MockServer) -> Result<NimDocumentReader> {
    let address = *server.address();
    Ok(NimDocumentReader::new(
        NimCvConfig::new(format!("http://{NIM_HOST}:{}/v1/cv", address.port()))
            .with_api_key("recorded")
            .with_http_client(client_to(&address, NIM_HOST)?),
    )?)
}

/// The share of the report page's printed lines a reader's page holds.
fn text_accuracy(answer: &Value, truth: &[String]) -> f64 {
    let text = normalise(answer["content"].as_str().unwrap_or_default());
    let found = truth.iter().filter(|t| text.contains(&normalise(t))).count();
    found as f64 / truth.len().max(1) as f64
}

/// Runs one conformance case, reporting a panic as a failure.
async fn conform<F, Fut>(probe: AnyProbe, fixture: Fixture, case: F) -> bool
where
    F: FnOnce(AnyProbe, Fixture) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move { case(probe, fixture).await }).await.is_ok()
}

async fn suite(target: impl Fn() -> AnyProbe, fixture: &Fixture) -> (usize, usize) {
    use adk_vision::conformance as c;
    let mut passed = 0;
    let cases = 7;
    passed += usize::from(
        conform(target(), fixture.clone(), |p, f| async move { c::coordinates(&p, &f).await })
            .await,
    );
    passed += usize::from(
        conform(target(), fixture.clone(), |p, f| async move { c::usage(&p, &f).await }).await,
    );
    passed += usize::from(
        conform(target(), fixture.clone(), |p, f| async move { c::cancellation(&p, &f).await })
            .await,
    );
    passed += usize::from(
        conform(target(), fixture.clone(), |p, f| async move { c::kill_switch(&p, &f).await })
            .await,
    );
    passed += usize::from(
        conform(target(), fixture.clone(), |p, f| async move { c::egress(&p, &f).await }).await,
    );
    passed += usize::from(
        conform(target(), fixture.clone(), |p, f| async move { c::unreleased(&p, &f).await }).await,
    );
    passed += usize::from(
        conform(target(), fixture.clone(), |p, f| async move { c::oversized(&p, &f).await }).await,
    );
    (passed, cases)
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note("PP-OCRv6 tiny and RF-DETR Nano on the device; NIM and sam-3.1 on recorded responses; PaddleOCR-VL-1.6 replayed from its recorded page");
    let local = models().await?;
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let report = std::fs::read(crate_fixtures().join("documents").join(DOC_PAGE))?;
    save(&artifacts, DOC_PAGE, report).await?;
    let card = page("id-card-01.png")?;
    let shapes = std::fs::read(crate_fixtures().join("cloud/shapes-960x540.png"))?;
    save(&artifacts, "shapes.png", shapes).await?;

    let nim_server = nim_server().await?;
    let sam_server = MockServer::start().await;
    let sam_recording = crate_fixtures().join("cloud/recorded/sam-3-1.shapes-red-rectangle.json");
    serve_always(&sam_server, &Recording::load(&sam_recording)?).await;
    let sam = SamSegmenter::new(
        SamConfig::new("recorded").with_base_url(format!("{}/v1", sam_server.uri())),
    )?
    .with_hosts(["api.meta.ai"]);
    let nim = nim_reader(&nim_server)?;
    let (reader, detector) = (local.reader.clone(), local.detector.clone());
    let factory: Factory = Box::new(move |gate, store| {
        VisionRuntime::builder()
            .reader(reader)
            .reader(Arc::new(
                RecordedVl::load()
                    .map_err(|e| adk_vision::VisionError::InvalidInput(e.to_string()))?,
            ))
            .remote_reader(Arc::new(nim))
            .detector(detector)
            .remote_detector(Arc::new(sam))
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    });
    let rules = VisionConfig::from_toml_str(&format!(
        "[egress]\ndefault = \"deny\"\nredact = [\"faces\", \"personal_text\"]\n\n\
         [[egress.rule]]\nhost = \"{NIM_HOST}\"\naction = \"allow\"\n\n\
         [[egress.rule]]\nhost = \"api.meta.ai\"\naction = \"allow\"\n"
    ))?;
    let readers = ["pp-ocrv6-tiny", VL, NIM_READER_ID];
    let detectors = ["rf-detr-n", "sam-3.1"];
    let mut script = Vec::new();
    for backend in readers {
        script.push(ScriptedResponse::function_call(
            "vision_read",
            json!({ "image": DOC_PAGE, "backend": backend }),
        ));
        script.push(ScriptedResponse::text("Read."));
    }
    for backend in detectors {
        script.push(ScriptedResponse::function_call(
            "vision_detect",
            json!({ "image": "shapes.png", "backend": backend, "classes": ["red rectangle"] }),
        ));
        script.push(ScriptedResponse::text("Found."));
    }
    let planner = Arc::new(ScriptedLlm::new("planner", script));
    let desk = desk(
        DeskSpec {
            planner: planner.clone(),
            mode: HarnessMode::Auto,
            rules,
            budget: None,
            artifacts,
            redact: true,
        },
        factory,
    )
    .await?;

    let truth: Vec<String> = read_json(
        &crate_fixtures().join("documents/doc-page-1000x1300.groundtruth.json"),
    )?["objects"]
        .as_array()
        .context("the report page's ground truth")?
        .iter()
        .filter_map(|o| o["text"].as_str().map(str::to_string))
        .collect();
    let mut read_answers = Vec::new();
    for backend in readers {
        let events = desk.send(&format!("Read the report with {backend}.")).await?;
        let answer =
            tool_responses(&events, "vision_read").into_iter().next().unwrap_or(Value::Null);
        read_answers.push((backend, answer));
    }
    let mut detect_answers = Vec::new();
    for backend in detectors {
        let events = desk.send(&format!("Find the red rectangle with {backend}.")).await?;
        let answer =
            tool_responses(&events, "vision_detect").into_iter().next().unwrap_or(Value::Null);
        detect_answers.push((backend, answer));
    }

    // The tool calls differ only in `backend`.
    let calls: Vec<Value> = planner
        .requests()
        .iter()
        .flat_map(|r| r.contents.iter())
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| match p {
            adk_core::Part::FunctionCall { name, args, .. } => {
                let mut args = args.clone();
                args.as_object_mut().map(|a| a.remove("backend"));
                Some(json!({ "name": name, "args": args }))
            }
            _ => None,
        })
        .collect();
    let distinct: BTreeSet<String> = calls.iter().map(Value::to_string).collect();
    checks.check(
        "the tool calls are identical but for the backend: one shape for reading, one for detecting",
        distinct.len() == 2,
    );
    let shapes = |answers: &[(&str, Value)]| -> BTreeSet<Vec<String>> {
        answers
            .iter()
            .map(|(_, a)| a.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default())
            .collect()
    };
    let read_ok = read_answers.iter().all(|(_, a)| a.get("error").is_none());
    let detect_ok = detect_answers.iter().all(|(_, a)| a.get("error").is_none());
    checks.check(
        "every backend answered with the same output schema",
        read_ok
            && detect_ok
            && shapes(&read_answers).len() == 1
            && shapes(&detect_answers).len() == 1,
    );
    for (backend, answer) in read_answers.iter().chain(&detect_answers) {
        if let Some(error) = answer.get("error") {
            checks.note(format!("{backend} failed: {error}"));
        }
    }

    // The page each reader returns validates against the published DocumentPage schema.
    let schema = read_json(&crate_fixtures().join("../../schema/document-page.json"))?;
    let region = read_json(&crate_fixtures().join("../../schema/region-set.json"))?;
    let id = region["$id"].as_str().context("region-set.json names its $id")?.to_string();
    // The page schema refers to the published region-set schema by its `$id`.
    let registry = jsonschema::Registry::new().add(id, region)?.prepare()?;
    let validator = jsonschema::options().with_registry(&registry).build(&schema)?;
    let image = load_image(&crate_fixtures().join("documents").join(DOC_PAGE))?;
    let runtime = VisionRuntime::builder()
        .reader(local.reader.clone())
        .reader(Arc::new(RecordedVl::load()?))
        .remote_reader(Arc::new(nim_reader(&nim_server)?))
        .egress_gate(adk_vision::EgressGate::new(
            adk_core::egress::EgressPolicy::public_only(),
            adk_vision::EgressConfig::default().with_default(adk_vision::EgressAction::Allow),
            Arc::new(adk_vision::InMemoryEgressRecorder::new()),
        ))
        .build()?;
    let mut valid = true;
    for backend in readers {
        let options = adk_vision::CallOptions::new().with_route(Route::to(backend));
        let page =
            runtime.read_with(&VisionCx::new(), &image, &ReadOptions::default(), &options).await?;
        valid &= validator.is_valid(&serde_json::to_value(&page.value)?);
    }
    checks.check("each reader's page validates against schema/document-page.json", valid);

    // Side by side: accuracy, latency and cost per backend.
    let calls = desk.records("vision.call");
    let shapes_truth = read_json(&crate_fixtures().join("cloud/shapes-960x540.groundtruth.json"))?;
    let rectangle = shapes_truth["objects"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["label"] == "red rectangle")
        .and_then(|o| crate::fixtures::bbox_of(&o["bbox"]))
        .context("the red rectangle's ground truth")?;
    let mut table = Vec::new();
    for (backend, answer) in &read_answers {
        let record = calls.iter().find(|c| c.ext["backend"] == *backend);
        table.push(format!(
            "{backend}: text {:.0}%, {} ms, {} µUSD",
            100.0 * text_accuracy(answer, &truth),
            record.and_then(|c| c.ext["durationMs"].as_u64()).unwrap_or(0),
            record
                .and_then(|c| c.ext.get("costMicroUsd"))
                .and_then(Value::as_u64)
                .map_or("unpriced".to_string(), |c| c.to_string()),
        ));
    }
    for (backend, answer) in &detect_answers {
        let record = calls.iter().find(|c| c.ext["backend"] == *backend);
        let best = answer["regions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| crate::fixtures::bbox_of(&r["box"]))
            .map(|b| b.iou(&rectangle))
            .fold(0.0, f32::max);
        table.push(format!(
            "{backend}: red rectangle IoU {best:.2}, {} ms, {} µUSD",
            record.and_then(|c| c.ext["durationMs"].as_u64()).unwrap_or(0),
            record.and_then(|c| c.ext.get("costMicroUsd")).and_then(Value::as_u64).unwrap_or(0),
        ));
    }
    checks.note(format!("side by side: {}", table.join("; ")));
    checks.check("accuracy, latency and cost are reported for all five backends", table.len() == 5);

    // The conformance suite, on every backend that runs here.
    let mut doc_fixture = Fixture::shapes();
    doc_fixture.image = image.clone();
    let objects =
        read_json(&crate_fixtures().join("documents/doc-page-1000x1300.groundtruth.json"))?;
    let expected = |label: &str| -> Option<Expected> {
        objects["objects"].as_array()?.iter().find(|o| o["label"] == label).and_then(|o| {
            let b = crate::fixtures::bbox_of(&o["bbox"])?;
            Some(Expected { label: None, bbox: b })
        })
    };
    doc_fixture.objects = ["title", "footer"].iter().filter_map(|l| expected(l)).collect();
    doc_fixture.tolerance_px = 10.0;
    let ocr = local.reader.clone();
    let (ocr_pass, cases) =
        suite(|| AnyProbe::from(ReaderTarget::Local(ocr.clone())), &doc_fixture).await;
    let nim = Arc::new(nim_reader(&nim_server)?);
    let (nim_pass, _) =
        suite(|| AnyProbe::from(ReaderTarget::Remote(nim.clone())), &doc_fixture).await;
    let mut card_fixture = Fixture::shapes();
    card_fixture.image = card.source()?;
    let portrait = card.portrait().context("the card's portrait")?;
    card_fixture.objects = vec![Expected { label: Some("person".into()), bbox: portrait }];
    card_fixture.tolerance_px = 16.0;
    card_fixture.query = DetectQuery::Classes(vec!["person".into()]);
    let rf = local.detector.clone();
    let (rf_pass, _) =
        suite(|| AnyProbe::from(DetectorTarget::Local(rf.clone())), &card_fixture).await;
    checks.check(
        &format!("conformance: pp-ocrv6-tiny {ocr_pass}/{cases}, {NIM_READER_ID} {nim_pass}/{cases}, rf-detr-n {rf_pass}/{cases}"),
        ocr_pass == cases && nim_pass == cases && rf_pass == cases,
    );
    checks.note("paddleocr-vl-1.6 and sam-3.1 run their conformance in adk-vision's ocr_vl_live_tests and sam_tests");
    Ok(checks)
}
