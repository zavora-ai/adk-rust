//! The annotated viewer on the web home (spec 3.7 §8.2, X12): a harness with the vision pack
//! reads an invoice page and detects parts on a conveyor still; each call leaves its answer
//! record beside its image, the thread links each call to the viewer, and a person opens it,
//! corrects a region and saves it as a labelled example that `cargo adk eval` runs.
//!
//! The pictures are the signed-off boards' fixtures (`examples/fixtures/viewer/`): Northwind
//! Packaging's invoice INV-2291 and the line 2 still, all content invented. Two on-device doubles
//! answer for them — a reader that reads the page's blocks out of reading order, one handwritten
//! value misread as `PO-7781S`, and a detector that finds the parts and the fiducial — as the
//! harness's pack tests do; the toolset, the media store, the answer records and every route are
//! the real ones.
//!
//! ```sh
//! cargo run -p adk-surface --features web --example vision_viewer -- 8793 /tmp/evals
//! ```
//!
//! The second argument is the eval fixtures folder labelled examples are saved under. The harness
//! model is `ScriptedLlm`: `vision_read` on `INV-2291.png`, `vision_detect` on `line-2.png`, then
//! a summary.

use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_core::{Part, RequestContext, SingleAgentLoader};
use adk_harness::packs::vision::VisionPack;
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::{LocalSource, Place, SurfaceEngine};
use adk_vision::{
    BBox, BackendDescriptor, Block, BlockKind, CoordFrame, DetectQuery, Detector, DocumentPage,
    DocumentReader, Locality, MediaStore, RawRegion, ReadOptions, SentImage, Shape, VisionCx,
    VisionRuntime, Word,
};
use async_trait::async_trait;
use serde_json::json;

const APP: &str = "desk";
const OWNER: &str = "ana";
const SESSION: &str = "s-desk";
const PAGE: &str = "INV-2291.png";
const STILL: &str = "line-2.png";
const PAGE_PNG: &[u8] = include_bytes!("fixtures/viewer/invoice-inv-2291-p1.png");
const STILL_PNG: &[u8] = include_bytes!("fixtures/viewer/line-2-10-41-07.png");

/// The page's blocks as a layout model emits them: kind, box in page pixels, confidence, text.
const BLOCKS: [(BlockKind, [f32; 4], f32, &str); 11] = [
    (BlockKind::Title, [1100.0, 150.0, 1524.0, 262.0], 0.99, "INVOICE"),
    (
        BlockKind::Text,
        [124.0, 140.0, 900.0, 400.0],
        0.97,
        "Northwind Packaging Ltd · Unit 7, Harbour Road · Port Examplia EX4 2PL",
    ),
    (BlockKind::Seal, [1232.0, 332.0, 1430.0, 528.0], 0.88, "RECEIVED 07 NOV 2026"),
    (
        BlockKind::Text,
        [1000.0, 296.0, 1524.0, 704.0],
        0.93,
        "Invoice no. INV-2291 · Invoice date 06.11.2026 · Due date 06.12.2026 · Currency EUR",
    ),
    (BlockKind::Handwriting, [1250.0, 466.0, 1532.0, 540.0], 0.58, "PO-7781S"),
    (
        BlockKind::Text,
        [120.0, 520.0, 770.0, 760.0],
        0.96,
        "Bill to · Mara Kestrel · 14 Ferry Lane · Port Examplia EX9 1QT",
    ),
    (BlockKind::Table, [124.0, 816.0, 1530.0, 1322.0], 0.95, "4 rows × 4 columns"),
    (BlockKind::Checkbox, [120.0, 1376.0, 560.0, 1436.0], 0.81, "☑ Paid by bank transfer"),
    (
        BlockKind::Text,
        [1000.0, 1356.0, 1530.0, 1572.0],
        0.94,
        "Subtotal 970.60 · VAT 20% 194.12 · Total due € 1,164.72",
    ),
    (BlockKind::Handwriting, [120.0, 1488.0, 700.0, 1604.0], 0.47, "Approved — J.T. 12/11"),
    (
        BlockKind::Text,
        [120.0, 2076.0, 1534.0, 2214.0],
        0.90,
        "Bank: Examplia Savings · IBAN EX12 3456 7890 1234 56 · BIC EXSVEX22",
    ),
];
/// The order a person reads them in.
const READING_ORDER: [usize; 11] = [0, 1, 3, 2, 4, 5, 6, 8, 7, 9, 10];

/// The still's parts and its 40 mm fiducial, in still pixels.
const FOUND: [(&str, [f32; 4], f32); 5] = [
    ("part", [330.0, 300.0, 630.0, 530.0], 0.96),
    ("part", [840.0, 400.0, 1140.0, 630.0], 0.97),
    ("part", [1290.0, 250.0, 1590.0, 480.0], 0.95),
    ("part", [1720.0, 470.0, 1920.0, 700.0], 0.88),
    ("fiducial", [40.0, 660.0, 285.0, 905.0], 0.99),
];

/// Admits every request as the owner, as a loopback-only server's extractor does.
struct Owner;

#[async_trait]
impl RequestContextExtractor for Owner {
    async fn extract(
        &self,
        _parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        Ok(RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into()],
            metadata: Default::default(),
        })
    }
}

/// A layout box in the pixels `image` holds: the chain maps the source to them.
fn sent(image: &SentImage, region: [f32; 4]) -> Option<BBox> {
    let (w, h) = image.pixels().size();
    let target = image
        .chain()
        .bbox_to_target(&BBox::new(region[0], region[1], region[2], region[3]))
        .clamped(w, h);
    (target.width() >= 1.0 && target.height() >= 1.0).then_some(target)
}

/// The on-device reader double: the invoice's blocks, out of reading order, with the order.
struct InvoiceReader {
    descriptor: BackendDescriptor,
}

#[async_trait]
impl DocumentReader for InvoiceReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        _cx: &VisionCx,
        page: &SentImage,
        _options: &ReadOptions,
    ) -> adk_vision::Result<DocumentPage<RawRegion>> {
        let mut blocks = Vec::new();
        let mut order = Vec::new();
        for (index, (kind, region, confidence, text)) in BLOCKS.iter().enumerate() {
            let Some(found) = sent(page, *region) else { continue };
            let shape = RawRegion::shape(Shape::Box(found));
            blocks.push(Block {
                kind: *kind,
                region: shape.clone(),
                text: (*text).to_string(),
                confidence: Some(*confidence),
                words: vec![Word {
                    region: shape,
                    text: (*text).to_string(),
                    confidence: Some(*confidence),
                }],
                cells: Vec::new(),
            });
            order.push(index);
        }
        let reading_order = READING_ORDER
            .iter()
            .filter_map(|wanted| order.iter().position(|kept| kept == wanted))
            .collect();
        Ok(DocumentPage { blocks, reading_order, markdown: None })
    }
}

/// The on-device detector double: the still's parts and fiducial, for the classes asked.
struct PartDetector {
    descriptor: BackendDescriptor,
}

#[async_trait]
impl Detector for PartDetector {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        let asked = match query {
            DetectQuery::Classes(names) | DetectQuery::Concepts(names) => names.clone(),
            _ => Vec::new(),
        };
        Ok(FOUND
            .iter()
            .filter(|(label, _, _)| asked.is_empty() || asked.iter().any(|name| name == label))
            .filter_map(|(label, region, score)| {
                sent(image, *region).map(|found| RawRegion::boxed(found, *label, *score))
            })
            .collect())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    adk_core::ensure_crypto_provider();
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8793), |port| port.parse())?;
    let evals =
        args.next().map_or_else(|| std::env::temp_dir().join("vision-viewer-evals"), Into::into);
    std::fs::create_dir_all(&evals)?;
    let workspace = std::env::temp_dir().join(format!("vision-viewer-{}", std::process::id()));
    std::fs::create_dir_all(&workspace)?;

    let artifacts = Arc::new(InMemoryArtifactService::new());
    for (name, bytes) in [(PAGE, PAGE_PNG), (STILL, STILL_PNG)] {
        artifacts
            .save(SaveRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: SESSION.into(),
                file_name: name.into(),
                part: Part::InlineData {
                    mime_type: "image/png".into(),
                    data: bytes.to_vec(),
                    uri: None,
                    annotations: None,
                },
                version: None,
            })
            .await?;
    }
    let on_device =
        |id: &str| BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels);
    let reader = Arc::new(InvoiceReader { descriptor: on_device("pp-ocrv6-small") });
    let detector = Arc::new(PartDetector { descriptor: on_device("rf-detr-s") });
    let pack = VisionPack::new(MediaStore::new(artifacts.clone()), move |gate, store| {
        VisionRuntime::builder()
            .reader(reader)
            .detector(detector)
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    });

    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_call("vision_read", json!({ "image": PAGE })),
            ScriptedResponse::function_call(
                "vision_detect",
                json!({ "image": STILL, "classes": ["part", "fiducial"] }),
            ),
            ScriptedResponse::text(
                "INV-2291 is from Northwind Packaging; the PO number reads PO-7781S. Line 2 \
                 shows four parts.",
            ),
        ],
    ));
    let agent = Arc::new(
        HarnessAgent::builder("desk")
            .model(model)
            .workspace(adk_devtools::Workspace::new(&workspace))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Ask)
            .pack_vision(pack)
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(sessions.clone())
                .artifact_service(artifacts.clone()),
        )
        .build()?;
    let harness = Arc::new(Harness::with_runner(agent.clone(), runner)?);
    let source = LocalSource::builder(APP)
        .sessions(sessions.clone())
        .harness(harness)
        .artifacts(artifacts as Arc<dyn ArtifactService>)
        .eval_fixtures(&evals)
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web()])
        .agent_name("desk", "Document desk")
        .build();

    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine))
        .add_root_routes(adk_surface::web::home())
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("vision viewer on http://127.0.0.1:{port}/ui/ (examples under {})", evals.display());
    axum::serve(listener, app).await?;
    Ok(())
}
