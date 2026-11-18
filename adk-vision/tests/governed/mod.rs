//! Fixtures for the governed scenarios: an identity card, the on-device detector and reader
//! the redactors run, recording remote backends, a planner model and an approver.
//!
//! The card is drawn, not photographed: the portrait is a red box the person detector finds,
//! each word a coloured box the reader reads only while its pixels are still there. Personal
//! words are blue, ordinary words green, so whether a redacted copy still shows a face or a
//! personal string is a question about its pixels.

#![allow(dead_code)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part,
    ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest, UnitRate,
    UsageUnit,
};
use adk_vision::{
    BBox, BackendDescriptor, Block, BlockKind, CoordFrame, DecodedImage, DescribeRequest,
    Describer, Description, DetectQuery, Detector, Digest, DocumentPage, DocumentReader,
    ImageRegion, ImageTokens, Locality, PriceRef, RawRegion, ReadOptions, ReleasedImage, Result,
    SegmentPrompt, Segmenter, SentImage, Shape, SourceImage, TokenEstimate, TransformChain,
    VisionCx, Word,
};
use futures::StreamExt;
use serde_json::{Value, json};

pub const RED: [u8; 3] = [255, 0, 0];
pub const BLUE: [u8; 3] = [0, 0, 255];
pub const GREEN: [u8; 3] = [0, 160, 0];

pub const APP: &str = "desk";
pub const USER: &str = "ana";
pub const SESSION: &str = "s1";
pub const CARD: &str = "id-card.png";

/// One word on the card: its box, its text, and whether it is personal.
#[derive(Clone, Debug)]
pub struct CardWord {
    pub bbox: BBox,
    pub text: &'static str,
    pub personal: bool,
}

/// A generated identity card: invented personal data, a drawn portrait.
pub struct Card {
    pub source: SourceImage,
    pub png: Vec<u8>,
    pub portrait: BBox,
    pub words: Vec<CardWord>,
}

impl Card {
    pub fn digest(&self) -> Digest {
        Digest::of(&self.png)
    }

    pub fn personal(&self) -> Vec<&CardWord> {
        self.words.iter().filter(|w| w.personal).collect()
    }

    pub fn ordinary(&self) -> Vec<&CardWord> {
        self.words.iter().filter(|w| !w.personal).collect()
    }
}

fn word(x0: f32, y0: f32, x1: f32, y1: f32, text: &'static str, personal: bool) -> CardWord {
    CardWord { bbox: BBox::new(x0, y0, x1, y1), text, personal }
}

/// The identity card; `shift` moves the portrait, which makes a different image.
pub fn identity_card_shifted(shift: f32) -> Card {
    let (width, height) = (640u32, 400u32);
    let portrait = BBox::new(30.0 + shift, 80.0, 200.0 + shift, 300.0);
    let words = vec![
        word(240.0, 20.0, 400.0, 50.0, "IDENTITY", false),
        word(410.0, 20.0, 480.0, 50.0, "CARD", false),
        word(240.0, 90.0, 300.0, 115.0, "Name:", false),
        word(310.0, 90.0, 360.0, 115.0, "Ana", true),
        word(370.0, 90.0, 430.0, 115.0, "Diaz", true),
        word(240.0, 140.0, 300.0, 165.0, "Email:", false),
        word(310.0, 140.0, 560.0, 165.0, "ana.diaz@example.com", true),
        word(240.0, 190.0, 270.0, 215.0, "ID:", false),
        word(280.0, 190.0, 420.0, 215.0, "123-45-6789", true),
        word(240.0, 240.0, 310.0, 265.0, "Issued", false),
        word(320.0, 240.0, 370.0, 265.0, "2026", false),
    ];
    let mut rgba = vec![255u8; width as usize * height as usize * 4];
    let mut paint = |b: &BBox, colour: [u8; 3]| {
        for y in b.y0 as u32..b.y1 as u32 {
            for x in b.x0 as u32..b.x1 as u32 {
                let i = (y as usize * width as usize + x as usize) * 4;
                rgba[i..i + 3].copy_from_slice(&colour);
            }
        }
    };
    paint(&portrait, RED);
    for w in &words {
        paint(&w.bbox, if w.personal { BLUE } else { GREEN });
    }
    let pixels = DecodedImage::from_rgba(width, height, rgba).unwrap();
    let png = pixels.encode_png().unwrap();
    let source = SourceImage::decode(&png, &Default::default()).unwrap();
    Card { source, png, portrait, words }
}

pub fn identity_card() -> Card {
    identity_card_shifted(0.0)
}

/// Whether most of `bbox` (in `image`'s pixels) is still `colour`.
pub fn shows(image: &DecodedImage, bbox: &BBox, colour: [u8; 3]) -> bool {
    let (w, h) = image.size();
    let (x0, y0, cw, ch) = bbox.pixel_bounds(w, h);
    if cw == 0 || ch == 0 {
        return false;
    }
    let mut hits = 0u64;
    for y in y0..y0 + ch {
        for x in x0..x0 + cw {
            let p = image.pixel(x, y);
            if (0..3).all(|c| (i32::from(p[c]) - i32::from(colour[c])).abs() < 64) {
                hits += 1;
            }
        }
    }
    hits * 2 > u64::from(cw) * u64::from(ch)
}

/// Whether every pixel of `bbox` (in `image`'s pixels) is black.
pub fn all_black(image: &DecodedImage, bbox: &BBox) -> bool {
    let (w, h) = image.size();
    let (x0, y0, cw, ch) = bbox.pixel_bounds(w, h);
    (y0..y0 + ch).all(|y| (x0..x0 + cw).all(|x| image.pixel(x, y) == [0, 0, 0, 255]))
}

/// The on-device person detector the face redactor runs: every red box is a `person`.
pub struct RedPersonDetector {
    descriptor: BackendDescriptor,
    pub calls: AtomicUsize,
}

impl RedPersonDetector {
    pub fn new() -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "rf-detr-n-double",
                Locality::InProcess,
                CoordFrame::SentPixels,
            ),
            calls: AtomicUsize::new(0),
        }
    }
}

/// The boxes of every separate red component, by a coarse scan: the card has one portrait.
fn red_boxes(image: &DecodedImage) -> Vec<BBox> {
    let (w, h) = image.size();
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in 0..h {
        for x in 0..w {
            let p = image.pixel(x, y);
            if p[0] > 200 && p[1] < 64 && p[2] < 64 {
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    bounds
        .map(|(x0, y0, x1, y1)| BBox::new(x0 as f32, y0 as f32, (x1 + 1) as f32, (y1 + 1) as f32))
        .into_iter()
        .collect()
}

#[async_trait::async_trait]
impl Detector for RedPersonDetector {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        _query: &DetectQuery,
    ) -> Result<Vec<RawRegion>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(red_boxes(image.pixels())
            .into_iter()
            .map(|b| RawRegion::boxed(b, "person", 0.93))
            .collect())
    }
}

/// The on-device reader the personal-text redactor runs: it knows where the card's words
/// are, and reads a word only while its pixels still show it.
pub struct FixtureReader {
    descriptor: BackendDescriptor,
    words: Vec<CardWord>,
}

impl FixtureReader {
    pub fn for_card(card: &Card) -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "ocr-double",
                Locality::InProcess,
                CoordFrame::SentPixels,
            ),
            words: card.words.clone(),
        }
    }

    /// The words a page shows, in the pixels of `pixels`, reached from the card through `chain`.
    pub fn read_pixels(
        &self,
        pixels: &DecodedImage,
        chain: &TransformChain,
    ) -> Vec<(String, BBox)> {
        let (w, h) = pixels.size();
        let mut found = Vec::new();
        for word in &self.words {
            // A word cut by the crop is read as far as it shows.
            let target = chain.bbox_to_target(&word.bbox).clamped(w, h);
            if target.width() < 1.0 || target.height() < 1.0 {
                continue;
            }
            let colour = if word.personal { BLUE } else { GREEN };
            if shows(pixels, &target, colour) {
                found.push((word.text.to_string(), target));
            }
        }
        found
    }
}

#[async_trait::async_trait]
impl DocumentReader for FixtureReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        _cx: &VisionCx,
        page: &SentImage,
        _options: &ReadOptions,
    ) -> Result<DocumentPage<RawRegion>> {
        // One block per line: words sharing a top edge.
        let mut lines: Vec<(f32, Vec<(String, BBox)>)> = Vec::new();
        for (text, bbox) in self.read_pixels(page.pixels(), page.chain()) {
            match lines.iter_mut().find(|(top, _)| (top - bbox.y0).abs() < 2.0) {
                Some((_, words)) => words.push((text, bbox)),
                None => lines.push((bbox.y0, vec![(text, bbox)])),
            }
        }
        let blocks = lines
            .into_iter()
            .map(|(_, words)| {
                let text = words.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(" ");
                let bbox = words.iter().fold(words[0].1, |acc, (_, b)| {
                    BBox::new(
                        acc.x0.min(b.x0),
                        acc.y0.min(b.y0),
                        acc.x1.max(b.x1),
                        acc.y1.max(b.y1),
                    )
                });
                Block {
                    kind: BlockKind::Text,
                    region: RawRegion::shape(Shape::Box(bbox)),
                    text,
                    confidence: Some(0.97),
                    words: words
                        .into_iter()
                        .map(|(text, b)| Word {
                            region: RawRegion::shape(Shape::Box(b)),
                            text,
                            confidence: Some(0.97),
                        })
                        .collect(),
                    cells: Vec::new(),
                }
            })
            .collect::<Vec<_>>();
        let reading_order = (0..blocks.len()).collect();
        Ok(DocumentPage { blocks, reading_order, markdown: None })
    }
}

/// What a recording remote backend received: the exact bytes and the gate's receipt.
#[derive(Clone, Debug)]
pub struct Received {
    pub bytes: Vec<u8>,
    pub receipt: adk_vision::EgressReceipt,
}

/// A remote describer standing in for Claude: it records the bytes it is handed and says
/// what kind of document it sees.
pub struct RecordingDescriber {
    descriptor: BackendDescriptor,
    pub received: Mutex<Vec<Received>>,
}

impl RecordingDescriber {
    pub fn claude() -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "claude",
                Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
                CoordFrame::SentPixels,
            )
            .with_price(PriceRef::Tokens {
                vendor: "anthropic".into(),
                model: "claude-sonnet-5-5".into(),
            })
            .with_token_estimate(
                TokenEstimate::new(ImageTokens::Claude, 3_000_000, 15_000_000)
                    .with_max_output_tokens(512),
            ),
            received: Mutex::new(Vec::new()),
        }
    }

    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Describer<ReleasedImage> for RecordingDescriber {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn describe(
        &self,
        _cx: &VisionCx,
        request: &DescribeRequest<ReleasedImage>,
    ) -> Result<Description<ImageRegion>> {
        let mut received = self.received.lock().unwrap();
        for image in &request.images {
            received
                .push(Received { bytes: image.bytes().to_vec(), receipt: image.receipt().clone() });
        }
        Ok(Description {
            text: Some("An identity card.".into()),
            value: None,
            regions: Vec::new(),
            confidence: Some(0.9),
            escalated_from: None,
        })
    }
}

/// A remote segmenter standing in for Meta `sam-3.1`: $2.50 per 1,000 images.
pub struct RemoteSegmenter {
    descriptor: BackendDescriptor,
    pub calls: AtomicUsize,
}

impl RemoteSegmenter {
    pub fn sam() -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "sam-3.1",
                Locality::Remote { hosts: vec!["api.meta.ai".into()] },
                CoordFrame::SentPixels,
            )
            .with_price(PriceRef::PerUnit {
                vendor: "meta".into(),
                model: "sam-3.1".into(),
                rate: UnitRate::new(UsageUnit::Images, 2_500_000, 1_000),
                min_units_per_request: 0,
            }),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Segmenter<ReleasedImage> for RemoteSegmenter {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn segment(
        &self,
        _cx: &VisionCx,
        image: &ReleasedImage,
        _prompts: &[SegmentPrompt],
    ) -> Result<Vec<RawRegion>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (w, h) = image.pixels().size();
        Ok(vec![RawRegion::boxed(BBox::new(0.0, 0.0, w as f32 / 2.0, h as f32 / 2.0), "part", 0.9)])
    }
}

/// A planner model: on a fresh turn it calls the scripted tools; once it has their results
/// it answers with a summary of the first one. Every request is recorded.
pub struct PlannerModel {
    calls: Vec<(String, Value)>,
    pub requests: Mutex<Vec<LlmRequest>>,
    locality: adk_core::DataLocality,
}

impl PlannerModel {
    pub fn calling(calls: Vec<(&str, Value)>) -> Self {
        Self {
            calls: calls.into_iter().map(|(n, a)| (n.to_string(), a)).collect(),
            requests: Mutex::new(Vec::new()),
            locality: adk_core::DataLocality::Remote,
        }
    }

    pub fn answering_only() -> Self {
        Self::calling(Vec::new())
    }

    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

fn response(parts: Vec<Part>) -> LlmResponse {
    let mut response = LlmResponse::new(Content { role: "model".to_string(), parts });
    response.finish_reason = Some(FinishReason::Stop);
    response.turn_complete = true;
    response
}

#[async_trait::async_trait]
impl Llm for PlannerModel {
    fn name(&self) -> &str {
        "planner"
    }

    fn data_locality(&self) -> adk_core::DataLocality {
        self.locality
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let turn = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        let answered = request.contents.last().is_some_and(|content| {
            content.parts.iter().any(|p| matches!(p, Part::FunctionResponse { .. }))
        });
        let reply = if answered || self.calls.is_empty() {
            let summary = request
                .contents
                .last()
                .and_then(|c| {
                    c.parts.iter().find_map(|p| match p {
                        Part::FunctionResponse { function_response, .. } => {
                            Some(function_response.response.to_string())
                        }
                        _ => None,
                    })
                })
                .unwrap_or_else(|| "nothing to do".to_string());
            response(vec![Part::Text { text: format!("done: {summary}") }])
        } else {
            let parts = self
                .calls
                .iter()
                .enumerate()
                .map(|(i, (name, args))| Part::FunctionCall {
                    name: name.clone(),
                    args: args.clone(),
                    id: Some(format!("call-{turn}-{i}")),
                    thought_signature: None,
                })
                .collect();
            response(parts)
        };
        Ok(Box::pin(futures::stream::iter([Ok(reply)])))
    }
}

/// Approves every held call and keeps what it was shown.
#[derive(Debug, Default)]
pub struct RecordingApprover {
    pub seen: Mutex<Vec<ToolConfirmationRequest>>,
}

#[async_trait::async_trait]
impl ToolConfirmationHandler for RecordingApprover {
    async fn decide(
        &self,
        request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        self.seen.lock().unwrap().push(request.clone());
        Ok(ToolConfirmationDecision::Approve)
    }
}

/// Saves `png` as `name` in the test session.
pub async fn save_artifact(artifacts: &InMemoryArtifactService, name: &str, png: &[u8]) {
    artifacts
        .save(SaveRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
            file_name: name.into(),
            part: Part::InlineData {
                mime_type: "image/png".into(),
                data: png.to_vec(),
                uri: None,
                annotations: None,
            },
            version: None,
        })
        .await
        .unwrap();
}

/// Runs one user turn and returns every event.
pub async fn run_turn(runner: &adk_runner::Runner, text: &str) -> Vec<adk_core::Event> {
    run_content(runner, Content::new("user").with_text(text)).await
}

/// Runs one user turn with `content` and returns every event.
pub async fn run_content(runner: &adk_runner::Runner, content: Content) -> Vec<adk_core::Event> {
    let mut stream = runner
        .run(
            adk_core::UserId::try_from(USER).unwrap(),
            adk_core::SessionId::try_from(SESSION).unwrap(),
            content,
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    events
}

/// The JSON responses of every function response in `events`, by tool name.
pub fn tool_responses(events: &[adk_core::Event], tool: &str) -> Vec<Value> {
    events
        .iter()
        .filter_map(|e| e.llm_response.content.as_ref())
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| match p {
            Part::FunctionResponse { function_response, .. } if function_response.name == tool => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

pub fn args_describe(region: [f32; 4]) -> Value {
    json!({
        "image": CARD,
        "region": region,
        "question": "What kind of document is this?",
        "backend": "claude",
    })
}

/// One field of a form: its box, its text, its colour (green printed, blue handwritten) and
/// the confidence a reader gives it.
#[derive(Clone, Debug)]
pub struct Field {
    pub bbox: BBox,
    pub text: String,
    pub colour: [u8; 3],
}

/// A form of three printed fields and five handwritten ones.
pub fn form() -> (SourceImage, Vec<Field>) {
    let mut fields = Vec::new();
    for i in 0..3 {
        let x = 60.0 + 200.0 * i as f32;
        fields.push(Field {
            bbox: BBox::new(x, 60.0, x + 150.0, 90.0),
            text: format!("printed {i}"),
            colour: GREEN,
        });
    }
    for i in 0..5 {
        let x = 40.0 + 150.0 * i as f32;
        fields.push(Field {
            bbox: BBox::new(x, 300.0, x + 110.0, 340.0),
            text: format!("handwritten {i}"),
            colour: BLUE,
        });
    }
    let (width, height) = (800u32, 600u32);
    let mut rgba = vec![255u8; width as usize * height as usize * 4];
    for field in &fields {
        let b = field.bbox;
        for y in b.y0 as u32..b.y1 as u32 {
            for x in b.x0 as u32..b.x1 as u32 {
                let i = (y as usize * width as usize + x as usize) * 4;
                rgba[i..i + 3].copy_from_slice(&field.colour);
            }
        }
    }
    let pixels = DecodedImage::from_rgba(width, height, rgba).unwrap();
    (SourceImage::from_pixels(pixels).unwrap(), fields)
}

/// Reads a form's fields where their pixels still show: printed at 0.95, handwritten at
/// `handwriting` on the device, and everything at 0.99 in the cloud.
pub struct FieldReader {
    descriptor: BackendDescriptor,
    fields: Vec<Field>,
    handwriting: f32,
}

impl FieldReader {
    pub fn local(fields: &[Field], handwriting: f32) -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "pp-ocrv6",
                Locality::InProcess,
                CoordFrame::SentPixels,
            ),
            fields: fields.to_vec(),
            handwriting,
        }
    }

    /// A cloud reader at $1.50 per 1,000 pages.
    pub fn cloud(fields: &[Field]) -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "cloud-ocr",
                Locality::Remote { hosts: vec!["ocr.example".into()] },
                CoordFrame::SentPixels,
            )
            .with_price(PriceRef::PerUnit {
                vendor: "ocr".into(),
                model: "cloud-ocr".into(),
                rate: UnitRate::new(UsageUnit::Images, 1_500_000, 1_000),
                min_units_per_request: 0,
            }),
            fields: fields.to_vec(),
            handwriting: 0.99,
        }
    }

    fn page(&self, image: &SentImage) -> DocumentPage<RawRegion> {
        let (w, h) = image.size();
        let remote = self.descriptor.locality.is_remote();
        let blocks: Vec<Block<RawRegion>> = self
            .fields
            .iter()
            .filter_map(|field| {
                let target = image.chain().bbox_to_target(&field.bbox);
                (target.clamped(w, h).area() > 0.5 * target.area()
                    && shows(image.pixels(), &target.clamped(w, h), field.colour))
                .then(|| {
                    let confidence = match (remote, field.colour == BLUE) {
                        (true, _) => 0.99,
                        (false, true) => self.handwriting,
                        (false, false) => 0.95,
                    };
                    let text =
                        if remote { format!("{} (cloud)", field.text) } else { field.text.clone() };
                    Block {
                        kind: BlockKind::Text,
                        region: RawRegion::shape(Shape::Box(target.clamped(w, h))),
                        text,
                        confidence: Some(confidence),
                        words: Vec::new(),
                        cells: Vec::new(),
                    }
                })
            })
            .collect();
        let reading_order = (0..blocks.len()).collect();
        DocumentPage { blocks, reading_order, markdown: None }
    }
}

#[async_trait::async_trait]
impl DocumentReader for FieldReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        _cx: &VisionCx,
        page: &SentImage,
        _o: &ReadOptions,
    ) -> Result<DocumentPage<RawRegion>> {
        Ok(self.page(page))
    }
}

#[async_trait::async_trait]
impl DocumentReader<ReleasedImage> for FieldReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        _cx: &VisionCx,
        page: &ReleasedImage,
        _o: &ReadOptions,
    ) -> Result<DocumentPage<RawRegion>> {
        Ok(self.page(page.image()))
    }
}
