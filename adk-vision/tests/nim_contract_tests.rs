//! The NVIDIA NIM document reader against the CV endpoints' recorded answers
//! (`tests/fixtures/nim`, from the 2026-11-18 probe): request bodies, coordinates through
//! the egress gate and the runtime's transform chain, the page it composes, units, error
//! mapping, and the conformance suite (spec 3.7 §7.7, §7.18).
//!
//! A mock server answers for `ai.api.nvidia.com`: the client resolves that host to the
//! mock, so the backend's declared host, the egress decision and the URL it calls are the
//! production ones.

mod doc_support;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::egress::EgressPolicy;
use adk_core::{
    AdkError, ErrorCategory, InMemorySpendLedger, ModelCallScope, ReservationId, SpendKey,
    SpendLedger, SpendLimits, SpendPeriod, UsageUnit,
};
use adk_vision::{
    AxisOrder, BlockKind, CoordFrame, DocumentPage, DocumentReader, EgressAction, EgressConfig,
    EgressGate, InMemoryEgressRecorder, InputLimits, NimCvConfig, NimDocumentReader, NimOcrModel,
    RawRegion, ReadOptions, ReleasedImage, Transform, VisionCx, VisionError, VisionRuntime,
};
use async_trait::async_trait;
use base64::Engine;
use doc_support::{DOC_PAGE, page, truth};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "test-key";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nim")
}

fn recorded(file: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join(file)).expect("recorded fixture");
    serde_json::from_str(&text).expect("fixture parses")
}

/// A request body equal to the recorded one, with the inline image a PNG data URI.
struct SameBody(Value);

fn without_image(mut body: Value) -> Option<Value> {
    let url = body.pointer("/input/0/url")?.as_str()?.to_string();
    if let Some(b64) = url.strip_prefix("data:image/png;base64,") {
        let png = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        if !png.starts_with(b"\x89PNG") {
            return None;
        }
    } else if !url.starts_with("{{datauri:image/png:") {
        return None;
    }
    body["input"][0]["url"] = json!("<image>");
    Some(body)
}

impl Match for SameBody {
    fn matches(&self, request: &Request) -> bool {
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else { return false };
        without_image(body) == without_image(self.0.clone())
    }
}

/// Mounts the recorded answer for one endpoint, matched on path, key and body shape,
/// expected `times` times when given.
async fn answer(server: &MockServer, model: &str, pair: &str, times: Option<u64>) {
    let request = recorded(&format!("cv/{pair}.request.json"));
    let response = recorded(&format!("cv/{pair}.response.json"));
    let mock = Mock::given(method("POST"))
        .and(path(format!("/v1/cv/{model}")))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(header("accept", "application/json"))
        .and(SameBody(request["body"].clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(response["body"].clone()));
    match times {
        Some(n) => mock.expect(n).mount(server).await,
        None => mock.mount(server).await,
    }
}

/// A client that sends `ai.api.nvidia.com` to the mock server.
fn config(addr: SocketAddr) -> NimCvConfig {
    let http = reqwest::Client::builder()
        .resolve("ai.api.nvidia.com", addr)
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    NimCvConfig::new(format!("http://ai.api.nvidia.com:{}/v1/cv", addr.port()))
        .with_api_key(KEY)
        .with_http_client(http)
}

/// The reader with the mock server it talks to, so the server lives as long as it.
struct Hosted {
    server: MockServer,
    reader: NimDocumentReader,
}

#[async_trait]
impl DocumentReader<ReleasedImage> for Hosted {
    fn descriptor(&self) -> &adk_vision::BackendDescriptor {
        DocumentReader::<ReleasedImage>::descriptor(&self.reader)
    }

    async fn read(
        &self,
        cx: &VisionCx,
        page: &ReleasedImage,
        options: &ReadOptions,
    ) -> adk_vision::Result<DocumentPage<RawRegion>> {
        self.reader.read(cx, page, options).await
    }
}

/// A server answering the four endpoints with the report page's recorded answers.
async fn recorded_server(times: Option<u64>) -> MockServer {
    let server = MockServer::start().await;
    answer(&server, "nvidia/nemotron-ocr-v1", "cv-doc-nemotron-ocr-v1-merge-word", times).await;
    answer(&server, "nvidia/nemotron-page-elements-v3", "cv-doc-nemotron-page-elements-v3", times)
        .await;
    answer(
        &server,
        "nvidia/nemotron-table-structure-v1",
        "cv-doc-nemotron-table-structure-v1",
        times,
    )
    .await;
    answer(
        &server,
        "nvidia/nemotron-graphic-elements-v1",
        "cv-doc-nemotron-graphic-elements-v1",
        times,
    )
    .await;
    server
}

async fn hosted() -> Hosted {
    let server = recorded_server(None).await;
    let reader = NimDocumentReader::new(config(*server.address())).unwrap();
    Hosted { server, reader }
}

/// A spend ledger that remembers the keys it reserved under.
#[derive(Debug)]
struct KeyedLedger {
    inner: InMemorySpendLedger,
    keys: Mutex<Vec<SpendKey>>,
}

impl KeyedLedger {
    fn new() -> Self {
        Self { inner: InMemorySpendLedger::new(SpendLimits::default()), keys: Mutex::default() }
    }
}

#[async_trait]
impl SpendLedger for KeyedLedger {
    async fn reserve(&self, key: &SpendKey, amount: u64) -> adk_core::Result<ReservationId> {
        self.keys.lock().unwrap().push(key.clone());
        self.inner.reserve(key, amount).await
    }
    async fn commit(&self, id: ReservationId, actual: u64) -> adk_core::Result<()> {
        self.inner.commit(id, actual).await
    }
    async fn release(&self, id: ReservationId) -> adk_core::Result<()> {
        self.inner.release(id).await
    }
    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> adk_core::Result<u64> {
        self.inner.spent(key, period).await
    }
}

fn allowing_gate(recorder: Arc<InMemoryEgressRecorder>) -> EgressGate {
    EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        recorder,
    )
}

fn within(found: Option<adk_vision::BBox>, want: [f32; 4], px: f32) -> bool {
    found.is_some_and(|b| {
        (b.x0 - want[0]).abs() <= px
            && (b.y0 - want[1]).abs() <= px
            && (b.x1 - want[2]).abs() <= px
            && (b.y1 - want[3]).abs() <= px
    })
}

#[tokio::test]
async fn the_report_page_is_composed_from_four_recorded_answers() {
    let server = recorded_server(Some(1)).await;
    let reader = NimDocumentReader::new(config(*server.address())).unwrap();
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_reader(Arc::new(reader))
        .egress_gate(allowing_gate(recorder.clone()))
        .build()
        .unwrap();
    let ledger = Arc::new(KeyedLedger::new());
    let cx = VisionCx::new()
        .with_scope(ModelCallScope::new().with_spend_ledger(ledger.clone(), "contract"));
    let source = page(DOC_PAGE);
    let mut options = ReadOptions::default();
    options.markdown = true;
    let out = runtime.read(&cx, &source, &options).await.unwrap();
    let page = &out.value;
    let objects = truth(DOC_PAGE);
    let gt = |label: &str| objects.iter().find(|o| o.label == label).unwrap().bbox;

    // Every region is in source pixels, from this backend, in 0–1 of the image sent.
    let regions = page.blocks.iter().flat_map(|b| {
        std::iter::once(&b.region)
            .chain(b.words.iter().map(|w| &w.region))
            .chain(b.cells.iter().map(|c| &c.region))
    });
    for region in regions {
        assert_eq!(region.image, source.digest());
        assert_eq!(region.origin.backend.as_str(), "nim-document-reader");
        assert_eq!(region.origin.frame, CoordFrame::Normalized { scale: 1, order: AxisOrder::Xy });
        assert!(region.shape.is_within(1000, 1300), "{region:?}");
    }

    let title = &page.blocks[page.reading_order[0]];
    assert_eq!((title.kind, title.text.as_str()), (BlockKind::Title, "Quarterly Parts Report"));
    assert!(within(title.region.shape.bbox(), gt("title"), 10.0), "{:?}", title.region.shape);

    let paragraph = page.blocks.iter().find(|b| b.text.starts_with("The conveyor")).unwrap();
    assert_eq!(
        paragraph.text,
        "The conveyor inspected 200 printed parts this quarter.\n\
         Twelve parts carried seeded defects of known length,\n\
         and each defect was measured against the fiducial."
    );
    assert_eq!(paragraph.words.len(), 24);

    let table = page.blocks.iter().find(|b| b.kind == BlockKind::Table).unwrap();
    let grid: Vec<(u32, u32, &str)> =
        table.cells.iter().map(|c| (c.row, c.col, c.text.as_str())).collect();
    let mut want = Vec::new();
    for object in objects.iter().filter(|o| o.label.starts_with("cell ")) {
        let rc = &object.label[5..];
        let row: u32 = rc[1..rc.find('c').unwrap()].parse().unwrap();
        let col: u32 = rc[rc.find('c').unwrap() + 1..].parse().unwrap();
        want.push((row, col, object.text.as_deref().unwrap()));
    }
    want.sort();
    assert_eq!(grid, want, "table cells");
    for cell in &table.cells {
        assert_eq!((cell.row_span, cell.col_span), (1, 1));
    }
    assert!(table.text.starts_with("Part\tDefect\tLength (mm)\nP-017\tcrack\t4.2"));

    let chart = page.blocks.iter().find(|b| b.region.label.as_deref() == Some("chart")).unwrap();
    assert_eq!(chart.kind, BlockKind::Figure);
    let labelled: Vec<(&str, Option<&str>)> =
        chart.words.iter().map(|w| (w.text.as_str(), w.region.label.as_deref())).collect();
    for label in ["crack", "void", "burr"] {
        assert!(labelled.contains(&(label, Some("xlabel"))), "{labelled:?}");
    }
    assert!(labelled.contains(&("5", Some("ylabel"))), "{labelled:?}");

    // The recorded OCR answer itself misses the first "1".
    let footer = page.blocks.iter().find(|b| b.text == "Page of 1").unwrap();
    assert!(within(footer.region.shape.bbox(), gt("footer"), 10.0));

    let read: Vec<&str> =
        page.reading_order.iter().map(|&i| page.blocks[i].text.as_str()).collect();
    let chart_title = read.iter().position(|t| *t == "Defects by type").unwrap();
    assert!(
        read[chart_title + 1].starts_with("5\n4"),
        "the chart's title reads before the chart: {read:?}"
    );
    let mut order = page.reading_order.clone();
    order.sort_unstable();
    assert_eq!(order, (0..page.blocks.len()).collect::<Vec<_>>());
    let markdown = page.markdown.as_deref().unwrap();
    assert!(markdown.starts_with("# Quarterly Parts Report\n\n"), "{markdown}");
    assert!(markdown.contains("| Part | Defect | Length (mm) |"), "{markdown}");

    // One call: the page released once, counted as one image and one page, at no price,
    // billed under the endpoint's host.
    assert_eq!(out.calls.len(), 1);
    let call = &out.calls[0];
    assert_eq!(call.units.get(UsageUnit::Images), 1);
    assert_eq!(call.units.get(UsageUnit::Pages), 1);
    assert_eq!(call.cost_micro_usd, Some(0));
    let released = recorder.released();
    assert_eq!(released.len(), 1);
    assert_eq!(call.egress, vec![released[0].egress]);
    assert_eq!(released[0].hosts, vec!["ai.api.nvidia.com".to_string()]);
    let keys = ledger.keys.lock().unwrap().clone();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].vendor.as_deref(), Some("ai.api.nvidia.com"));
    // The mock's `expect(1)` on each endpoint is checked when the server drops.
    drop(server);
}

#[tokio::test]
async fn a_downscaled_page_maps_back_to_source_pixels() {
    let hosted = hosted().await;
    let reader = Hosted {
        reader: hosted.reader.with_limits(InputLimits::long_edge(500)),
        server: hosted.server,
    };
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .remote_reader(Arc::new(reader))
        .egress_gate(allowing_gate(recorder.clone()))
        .build()
        .unwrap();
    let out =
        runtime.read(&VisionCx::new(), &page(DOC_PAGE), &ReadOptions::default()).await.unwrap();
    let released = recorder.released();
    assert!(
        released[0].chain.steps.contains(&Transform::Resize { width: 385, height: 500 }),
        "{:?}",
        released[0].chain
    );
    let title = out.value.blocks.iter().find(|b| b.kind == BlockKind::Title).unwrap();
    // 0–1 of a 385×500 image, mapped through the resize back to 1000×1300.
    assert!(within(title.region.shape.bbox(), [80.0, 78.0, 549.0, 119.0], 10.0));
    assert!(title.region.origin.chain.steps.iter().any(|s| matches!(s, Transform::Resize { .. })));
}

#[tokio::test]
async fn paddleocr_lines_without_layout_are_one_block_each() {
    let server = MockServer::start().await;
    answer(&server, "baidu/paddleocr", "cv-doc-paddleocr", Some(1)).await;
    let reader = NimDocumentReader::new(config(*server.address()))
        .unwrap()
        .with_ocr(NimOcrModel::PaddleOcr)
        .with_layout(false);
    let runtime = VisionRuntime::builder()
        .remote_reader(Arc::new(reader))
        .egress_gate(allowing_gate(Arc::new(InMemoryEgressRecorder::new())))
        .build()
        .unwrap();
    let out =
        runtime.read(&VisionCx::new(), &page(DOC_PAGE), &ReadOptions::default()).await.unwrap();
    let texts: Vec<&str> =
        out.value.reading_order.iter().map(|&i| out.value.blocks[i].text.as_str()).collect();
    assert_eq!(texts.len(), 27);
    assert_eq!(texts[0], "Quarterly Parts Report");
    assert_eq!(texts[26], "Page 1 of 1");
    assert!(out.value.blocks.iter().all(|b| b.kind == BlockKind::Text));
}

/// Answers the OCR endpoint with a recorded error, and reads.
async fn read_against(status: u16, content_type: &str, body: &str, times: u64) -> VisionError {
    read_against_with(
        ResponseTemplate::new(status).set_body_raw(body.as_bytes().to_vec(), content_type),
        times,
    )
    .await
}

async fn read_against_with(response: ResponseTemplate, times: u64) -> VisionError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/cv/nvidia/nemotron-ocr-v1"))
        .respond_with(response)
        .expect(times)
        .mount(&server)
        .await;
    let reader = NimDocumentReader::new(config(*server.address())).unwrap().with_layout(false);
    let runtime = VisionRuntime::builder()
        .remote_reader(Arc::new(reader))
        .egress_gate(allowing_gate(Arc::new(InMemoryEgressRecorder::new())))
        .build()
        .unwrap();
    runtime.read(&VisionCx::new(), &page(DOC_PAGE), &ReadOptions::default()).await.unwrap_err()
}

/// The recorded error answer in `errors/<name>.response.json`.
async fn recorded_error(name: &str, times: u64) -> VisionError {
    let fixture = recorded(&format!("errors/{name}.response.json"));
    let status = fixture["status"].as_u64().unwrap() as u16;
    let content_type = fixture["headers"]["Content-Type"].as_str().unwrap_or("application/json");
    let body = match &fixture["body"] {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    read_against(status, content_type, &body, times).await
}

#[tokio::test]
async fn recorded_errors_map_to_their_categories() {
    let cases: [(&str, ErrorCategory, &str); 6] = [
        ("probe-cv-nvidia-nemotron-ocr", ErrorCategory::NotFound, "vision.nim.no_route"),
        ("cv-nvidia-nv-grounding-dino", ErrorCategory::Forbidden, "vision.nim.not_entitled"),
        ("cv-nvidia-nemoretriever-ocr-v1", ErrorCategory::NotFound, "vision.nim.retired"),
        ("cv-nvidia-visual-changenet", ErrorCategory::InvalidInput, "vision.invalid_input"),
        ("emb-nemotron-3-embed-1b-image", ErrorCategory::Unsupported, "vision.unsupported"),
        ("emb-nemotron-3.5-embed-1b-image", ErrorCategory::Timeout, "vision.timeout"),
    ];
    for (name, category, code) in cases {
        let err = recorded_error(name, 1).await;
        let message = err.to_string();
        let adk: AdkError = err.into();
        assert_eq!((adk.category, adk.code), (category, code), "{name}: {message}");
        match name {
            "probe-cv-nvidia-nemotron-ocr" => {
                assert!(message.contains("/v1/cv/nvidia/nemotron-ocr-v1"), "{message}")
            }
            // The NVCF body names the caller's account; the message must not repeat it.
            "cv-nvidia-nv-grounding-dino" => {
                assert!(
                    !message.contains("account '") && !message.contains("50128865"),
                    "{message}"
                )
            }
            "cv-nvidia-nemoretriever-ocr-v1" => {
                assert!(message.contains("2026-05-18"), "{message}")
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_worker_error_is_retried_once_then_unavailable() {
    // `expect(2)`: the first answer and one retry.
    let err = recorded_error("probe-chat-ai-synthetic-video-detector", 2).await;
    let adk: AdkError = err.into();
    assert_eq!(adk.category, ErrorCategory::Unavailable);
    assert!(adk.retry.should_retry);
}

#[tokio::test]
async fn a_rate_limit_carries_retry_after() {
    let err = read_against_with(
        ResponseTemplate::new(429).insert_header("retry-after", "7").set_body_string("slow down"),
        1,
    )
    .await;
    assert!(
        matches!(err, VisionError::RateLimited { retry_after: Some(d), .. } if d == Duration::from_secs(7)),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_misconfigured_url_is_refused_when_the_reader_is_built() {
    let err = NimDocumentReader::new(
        NimCvConfig::new("https://ai.api.nvidia.com/v1/cv")
            .with_model_url("nvidia/nemotron-ocr-v1", "not a url"),
    )
    .unwrap_err();
    assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
}

mod nim_conformance {
    use std::sync::Arc;
    use std::time::Duration;

    use adk_vision::NimDocumentReader;
    use adk_vision::conformance::ReaderTarget;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::doc_support::conformance_fixture;
    use super::{Hosted, config, hosted};

    async fn answering(response: ResponseTemplate) -> Hosted {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(response).mount(&server).await;
        let reader = NimDocumentReader::new(config(*server.address())).unwrap();
        Hosted { server, reader }
    }

    adk_vision::document_reader_conformance_tests! {
        backend = ReaderTarget::Remote(Arc::new(hosted().await)),
        fixture = conformance_fixture(10.0),
        slow = ReaderTarget::Remote(Arc::new(
            answering(ResponseTemplate::new(200).set_delay(Duration::from_secs(30))).await,
        )),
        rate_limited = ReaderTarget::Remote(Arc::new(
            answering(ResponseTemplate::new(429).insert_header("retry-after", "3")).await,
        )),
    }
}
