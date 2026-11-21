//! The vision pack (spec 3.7 X15, S3, S4): an identity-card crop sent to Claude from a
//! harness in Ask mode is held with the exact egress image, approval releases that digest,
//! the provider receives only the redacted copy, and the audit trail links the digests.
//!
//! The card is `fixtures/vision/id-card.png`, rendered from `id-card.html`: invented personal
//! data and a drawn portrait, labelled as such. Its layout (`id-card.json`) drives two
//! on-device doubles — a person detector that finds the portrait and a reader that reads each
//! word — which report only what the pixels they are handed still show, so a covered word or
//! face is not found again. The live run asks `claude-sonnet-5-5` through a recording proxy;
//! its key comes from `ANTHROPIC_API_KEY` or the workspace `.env`.
//!
//! With feature `vision-local` the pack's default redactors — RF-DETR Nano and PP-OCRv6 —
//! replace the doubles, and the bytes the provider receives are checked by the same real
//! backends. Those runs need the weights:
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-harness --features vision-local \
//!     --test integration pack_vision_tests::local --run-ignored only --no-capture
//! ```

#![cfg(feature = "vision")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::{
    Content, Evidence, Llm, LlmRequest, LlmResponseStream, ModelCallScope, Part, SessionId, UserId,
};
use adk_harness::packs::vision::{VisionPack, class_for};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, Harness, HarnessAgent, HarnessMode, PromptAnswer, ToolKind,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_vision::{
    BBox, BackendDescriptor, Block, BlockKind, CoordFrame, DecodedImage, DescribeRequest,
    Describer, Description, DetectQuery, Detector, Digest, DocumentPage, DocumentReader,
    EgressAction, ImageRegion, ImageTokens, InMemoryEgressRecorder, Locality, MediaStore,
    PersonRedactor, PersonalTextRedactor, PriceRef, RawRegion, ReadOptions, ReleasedImage,
    SentImage, Shape, TokenEstimate, TransformChain, VisionConfig, VisionCx, VisionRuntime, Word,
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

const APP: &str = "vision-desk";
const USER: &str = "ana";
const SESSION: &str = "s1";
const CARD: &str = "id-card.png";
const REGION: [f32; 4] = [20.0, 10.0, 620.0, 390.0];

const RULES: &str = r#"
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "api.anthropic.com"
classes = ["document_crop"]
action = "ask"
"#;

#[derive(Clone, Debug, Deserialize)]
struct LayoutWord {
    text: String,
    personal: bool,
    #[serde(rename = "box")]
    bbox: [f32; 4],
}

#[derive(Clone, Debug, Deserialize)]
struct Layout {
    portrait: [f32; 4],
    words: Vec<LayoutWord>,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vision")
}

fn card_png() -> Vec<u8> {
    std::fs::read(fixtures().join("id-card.png")).unwrap()
}

fn layout() -> Layout {
    serde_json::from_slice(&std::fs::read(fixtures().join("id-card.json")).unwrap()).unwrap()
}

fn bbox(edges: [f32; 4]) -> BBox {
    BBox::new(edges[0], edges[1], edges[2], edges[3])
}

/// Whether some pixel of `region` is not black: the region still shows something.
fn visible(pixels: &DecodedImage, region: &BBox) -> bool {
    let (w, h) = pixels.size();
    let (x0, y0, cw, ch) = region.pixel_bounds(w, h);
    (y0..y0 + ch).any(|y| (x0..x0 + cw).any(|x| pixels.pixel(x, y) != [0, 0, 0, 255]))
}

/// The layout's regions as `pixels` shows them: `chain` maps the card to `pixels`.
fn shown(pixels: &DecodedImage, chain: &TransformChain, region: [f32; 4]) -> Option<BBox> {
    let (w, h) = pixels.size();
    let target = chain.bbox_to_target(&bbox(region)).clamped(w, h);
    (target.width() >= 1.0 && target.height() >= 1.0 && visible(pixels, &target)).then_some(target)
}

/// The words of the card `pixels` still shows.
fn readable(layout: &Layout, pixels: &DecodedImage, chain: &TransformChain) -> Vec<String> {
    layout
        .words
        .iter()
        .filter(|word| shown(pixels, chain, word.bbox).is_some())
        .map(|word| word.text.clone())
        .collect()
}

/// The on-device person detector double: the portrait, while it shows.
struct LayoutPersonDetector {
    descriptor: BackendDescriptor,
    portrait: [f32; 4],
}

#[async_trait::async_trait]
impl Detector for LayoutPersonDetector {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        _query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        Ok(shown(image.pixels(), image.chain(), self.portrait)
            .map(|found| RawRegion::boxed(found, "person", 0.94))
            .into_iter()
            .collect())
    }
}

/// The on-device reader double: one block per line of the layout's words, while they show.
struct LayoutReader {
    descriptor: BackendDescriptor,
    layout: Layout,
}

#[async_trait::async_trait]
impl DocumentReader for LayoutReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        _cx: &VisionCx,
        page: &SentImage,
        _options: &ReadOptions,
    ) -> adk_vision::Result<DocumentPage<RawRegion>> {
        let mut lines: Vec<(f32, Vec<Word<RawRegion>>)> = Vec::new();
        for word in &self.layout.words {
            let Some(found) = shown(page.pixels(), page.chain(), word.bbox) else { continue };
            let entry = Word {
                region: RawRegion::shape(Shape::Box(found)),
                text: word.text.clone(),
                confidence: Some(0.96),
            };
            match lines.iter_mut().find(|(top, _)| (top - word.bbox[1]).abs() < 8.0) {
                Some((_, words)) => words.push(entry),
                None => lines.push((word.bbox[1], vec![entry])),
            }
        }
        let blocks: Vec<Block<RawRegion>> = lines
            .into_iter()
            .map(|(_, words)| Block {
                kind: BlockKind::Text,
                region: words[0].region.clone(),
                text: words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
                confidence: Some(0.96),
                words,
                cells: Vec::new(),
            })
            .collect();
        let reading_order = (0..blocks.len()).collect();
        Ok(DocumentPage { blocks, reading_order, markdown: None })
    }
}

/// What the provider received: the exact image bytes of every request.
#[derive(Default)]
struct RecordingProxy {
    images: Mutex<Vec<Vec<u8>>>,
}

/// A model behind the recording proxy.
struct Recorded {
    inner: Arc<dyn Llm>,
    proxy: Arc<RecordingProxy>,
}

#[async_trait::async_trait]
impl Llm for Recorded {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let images = request.contents.iter().flat_map(|c| c.parts.iter()).filter_map(|p| match p {
            Part::InlineData { mime_type, data, .. } if mime_type.starts_with("image/") => {
                Some(data.clone())
            }
            _ => None,
        });
        self.proxy.images.lock().unwrap().extend(images);
        self.inner.generate_content(request, stream).await
    }
}

/// A remote describer over a Claude model: the released image and the question, metered
/// under the call's scope.
struct ClaudeDescriber {
    descriptor: BackendDescriptor,
    model: Arc<dyn Llm>,
    answers: Mutex<Vec<String>>,
}

impl ClaudeDescriber {
    fn new(model: Arc<dyn Llm>) -> Self {
        let descriptor = BackendDescriptor::new(
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
                .with_max_output_tokens(400),
        );
        Self { descriptor, model, answers: Mutex::new(Vec::new()) }
    }
}

#[async_trait::async_trait]
impl Describer<ReleasedImage> for ClaudeDescriber {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn describe(
        &self,
        _cx: &VisionCx,
        request: &DescribeRequest<ReleasedImage>,
    ) -> adk_vision::Result<Description<ImageRegion>> {
        let mut content = Content::new("user").with_text(request.prompt.clone());
        content.parts.extend(request.images.iter().map(ReleasedImage::to_part));
        let mut llm_request = LlmRequest::new(self.model.name(), vec![content]);
        llm_request.config = Some(adk_core::GenerateContentConfig {
            max_output_tokens: Some(400),
            ..Default::default()
        });
        let stream = match ModelCallScope::current() {
            Some(scope) => scope.generate(self.model.as_ref(), llm_request, false).await,
            None => self.model.generate_content(llm_request, false).await,
        }?;
        let mut text = String::new();
        let mut stream = stream;
        while let Some(response) = stream.next().await {
            if let Some(content) = response?.content {
                for part in content.parts {
                    if let Part::Text { text: piece } = part {
                        text.push_str(&piece);
                    }
                }
            }
        }
        self.answers.lock().unwrap().push(text.clone());
        Ok(Description {
            text: Some(text),
            value: None,
            regions: Vec::new(),
            confidence: Some(0.9),
            escalated_from: None,
        })
    }
}

/// Approves once and keeps every prompt, as a person at the approval card would.
#[derive(Default)]
struct Approver {
    prompts: Mutex<Vec<ApprovalPrompt>>,
}

impl std::fmt::Debug for Approver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Approver")
    }
}

#[async_trait::async_trait]
impl ApprovalPrompter for Approver {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        self.prompts.lock().unwrap().push(prompt);
        Ok(PromptAnswer::ApproveOnce)
    }
}

struct Desk {
    harness: Harness,
    approver: Arc<Approver>,
    describer: Arc<ClaudeDescriber>,
    proxy: Arc<RecordingProxy>,
    recorder: Arc<InMemoryEgressRecorder>,
    audit: InMemoryAuditRecorder,
    layout: Layout,
    _dir: tempfile::TempDir,
}

async fn desk(model: Arc<dyn Llm>, provider: Arc<dyn Llm>, mode: HarnessMode) -> Desk {
    let layout = layout();
    let on_device =
        |id: &str| BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels);
    let people = Arc::new(LayoutPersonDetector {
        descriptor: on_device("rf-detr-n"),
        portrait: layout.portrait,
    });
    let reader =
        Arc::new(LayoutReader { descriptor: on_device("pp-ocrv6"), layout: layout.clone() });
    desk_with(model, provider, mode, RULES, &[(CARD, card_png())], move |pack| {
        pack.redactor(Arc::new(PersonRedactor::new(people)))
            .redactor(Arc::new(PersonalTextRedactor::new(reader).with_labelled_fields(["Name"])))
    })
    .await
}

/// A harness desk whose session holds `images` as artifacts, its vision pack under `rules`
/// and finished by `pack`.
async fn desk_with(
    model: Arc<dyn Llm>,
    provider: Arc<dyn Llm>,
    mode: HarnessMode,
    rules: &str,
    images: &[(&str, Vec<u8>)],
    pack: impl FnOnce(VisionPack) -> VisionPack,
) -> Desk {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    for (name, png) in images {
        artifacts
            .save(SaveRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: SESSION.into(),
                file_name: (*name).into(),
                part: Part::InlineData {
                    mime_type: "image/png".into(),
                    data: png.clone(),
                    uri: None,
                    annotations: None,
                },
                version: None,
            })
            .await
            .unwrap();
    }
    let proxy = Arc::new(RecordingProxy::default());
    let describer = Arc::new(ClaudeDescriber::new(Arc::new(Recorded {
        inner: provider,
        proxy: proxy.clone(),
    })));
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let factory_describer = describer.clone();
    let pack = pack(
        VisionPack::new(MediaStore::new(artifacts.clone()), move |gate, store| {
            VisionRuntime::builder()
                .remote_describer(factory_describer)
                .shared_egress_gate(gate)
                .media_store(store)
                .build()
        })
        .config(VisionConfig::from_toml_str(rules).unwrap())
        .recorder(recorder.clone())
        .model_hosts(["api.anthropic.com"]),
    );
    let approver = Arc::new(Approver::default());
    let audit = InMemoryAuditRecorder::new();
    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(adk_harness::InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..Default::default()
            })
            .mode(mode)
            .prompter(approver.clone())
            .audit_log(Arc::new(audit.clone()))
            .pack_vision(pack)
            .without_compaction()
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            adk_runner::Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(sessions)
                .artifact_service(artifacts),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    harness
        .open_session(&UserId::new(USER).unwrap(), Some(&SessionId::new(SESSION).unwrap()))
        .await
        .unwrap();
    Desk { harness, approver, describer, proxy, recorder, audit, layout: layout(), _dir: dir }
}

async fn send(desk: &Desk, text: &str) -> Vec<adk_core::Event> {
    let mut stream = desk
        .harness
        .send(&UserId::new(USER).unwrap(), &SessionId::new(SESSION).unwrap(), text)
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    events
}

fn describe_args() -> Value {
    json!({
        "image": CARD,
        "backend": "claude",
        "region": REGION,
        "question": "What kind of document is this? Transcribe every word you can read.",
    })
}

/// The checks every run of the scenario makes: held with the image, that digest released,
/// the provider got only the redacted copy, and the audit trail links the digests.
fn assert_governed(desk: &Desk) -> Digest {
    let prompts = desk.approver.prompts.lock().unwrap();
    let held: Vec<&ApprovalPrompt> =
        prompts.iter().filter(|p| p.request.tool_name == "vision_describe").collect();
    assert_eq!(held.len(), 1, "the remote call is held once");
    assert_eq!(held[0].kind, class_for(Some(EgressAction::Ask)));
    let Evidence::Image(evidence) = &held[0].request.evidence[0] else { panic!("image evidence") };
    let egress: Digest = evidence.digest.parse().unwrap();
    assert!(evidence.regions.is_some(), "the redactions are outlined");

    let receipts = desk.recorder.released();
    assert_eq!(receipts.len(), 1, "one release");
    assert_eq!(receipts[0].egress, egress, "approval released the approved digest");
    assert_eq!(receipts[0].source, Digest::of(&card_png()));
    let sent = desk.proxy.images.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "the provider received one image");
    assert_eq!(Digest::of(&sent[0]), egress, "the provider received exactly the released bytes");

    let (pixels, _) = DecodedImage::decode(&sent[0], &Default::default()).unwrap();
    let chain = &receipts[0].chain;
    let still = readable(&desk.layout, &pixels, chain);
    for word in desk.layout.words.iter().filter(|w| w.personal) {
        assert!(!still.contains(&word.text), "{} reached the provider", word.text);
    }
    assert!(shown(&pixels, chain, desk.layout.portrait).is_none(), "the portrait is covered");
    assert!(still.contains(&"IDENTITY".to_string()));

    let released: Vec<_> =
        desk.audit.records().into_iter().filter(|r| r.action == "vision.egress.released").collect();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].ext["sourceDigest"], json!(Digest::of(&card_png())));
    assert_eq!(released[0].ext["egressDigest"], json!(egress));
    let decided: Vec<Value> = desk
        .audit
        .records()
        .into_iter()
        .filter(|r| r.action == "vision_describe")
        .filter_map(|r| r.ext.get("evidenceDigests").cloned())
        .collect();
    assert!(!decided.is_empty(), "the tool call records name the image");
    assert!(decided.iter().all(|d| d == &json!([egress])), "{decided:?}");
    egress
}

#[tokio::test]
async fn ask_mode_holds_a_remote_call_with_its_egress_image_and_releases_that_digest() {
    let model = Arc::new(ScriptedLlm::new(
        "planner",
        [
            ScriptedResponse::function_call("vision_describe", describe_args()),
            ScriptedResponse::text("It is an identity card."),
        ],
    ));
    let provider = Arc::new(ScriptedLlm::new(
        "claude-sonnet-5-5",
        [ScriptedResponse::text("An identity card of the Republic of Zavoria.")],
    ));
    let desk = desk(model, provider, HarnessMode::Ask).await;

    send(&desk, "What kind of document is id-card.png?").await;

    assert_governed(&desk);
}

#[tokio::test]
async fn read_only_mode_denies_a_remote_call_without_asking() {
    let model = Arc::new(ScriptedLlm::new(
        "planner",
        [
            ScriptedResponse::function_call("vision_describe", describe_args()),
            ScriptedResponse::text("I may not send it."),
        ],
    ));
    let provider = Arc::new(ScriptedLlm::new("claude-sonnet-5-5", Vec::<ScriptedResponse>::new()));
    let desk = desk(model, provider, HarnessMode::ReadOnly).await;

    send(&desk, "What kind of document is id-card.png?").await;

    assert!(desk.approver.prompts.lock().unwrap().is_empty());
    assert!(desk.recorder.released().is_empty());
    assert!(desk.proxy.images.lock().unwrap().is_empty());
}

#[test]
fn calls_are_classed_by_where_their_image_goes() {
    assert_eq!(class_for(None), ToolKind::Read);
    assert_eq!(
        class_for(Some(EgressAction::Allow)),
        ToolKind::External { effect: adk_core::ToolEffect::ReadOnly, trusted_origin: true }
    );
    assert_eq!(
        class_for(Some(EgressAction::Deny)),
        ToolKind::External { effect: adk_core::ToolEffect::ReadOnly, trusted_origin: false }
    );
}

#[test]
fn a_workspace_allow_rule_asks_until_the_operator_trusts_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".adk")).unwrap();
    std::fs::write(
        dir.path().join(".adk/vision.toml"),
        "[egress]\nredact = []\n[[egress.rule]]\nhost = \"api.anthropic.com\"\naction = \"allow\"\n",
    )
    .unwrap();
    let class = adk_vision::DataClass::new("document_crop");
    let untrusted = adk_harness::packs::vision::load_vision_config(dir.path(), false).unwrap();
    assert_eq!(untrusted.egress.decide("api.anthropic.com", &class).0, EgressAction::Ask);
    let trusted = adk_harness::packs::vision::load_vision_config(dir.path(), true).unwrap();
    assert_eq!(trusted.egress.decide("api.anthropic.com", &class).0, EgressAction::Allow);
    // Faces and personal text are redacted whatever the file says.
    assert_eq!(trusted.egress.redact.len(), 2);
}

/// Reads a key from the environment or the workspace `.env`, printing nothing.
fn env_key(name: &str) -> Option<String> {
    if let Some(value) = std::env::var(name).ok().filter(|value| !value.is_empty()) {
        return Some(value);
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".env");
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let (key, value) =
            line.trim().strip_prefix("export ").unwrap_or(line.trim()).split_once('=')?;
        let value = value.trim().trim_matches('"').trim_matches('\'');
        (key.trim() == name && !value.is_empty()).then(|| value.to_string())
    })
}

fn claude(key: &str, model: &str) -> Arc<dyn Llm> {
    Arc::new(
        adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
            key, model,
        ))
        .unwrap(),
    )
}

/// S4 live: a harness on `claude-haiku-5-5` in Ask mode asks to send the identity card's crop
/// to `claude-sonnet-5-5`; the scripted approver at the card approves; the provider, behind
/// a recording proxy, receives only the redacted copy and cannot read the personal data.
#[tokio::test]
#[ignore = "calls the Anthropic API with ANTHROPIC_API_KEY"]
async fn live_an_identity_card_crop_reaches_claude_only_redacted() {
    let Some(key) = env_key("ANTHROPIC_API_KEY") else {
        println!("skipped: no ANTHROPIC_API_KEY");
        return;
    };
    let desk =
        desk(claude(&key, "claude-haiku-5-5"), claude(&key, "claude-sonnet-5-5"), HarnessMode::Ask)
            .await;
    let ask = format!(
        "The workspace session holds an identity card as the artifact named `{CARD}`. Call the \
         `vision_describe` tool exactly once with image \"{CARD}\", backend \"claude\", region \
         {REGION:?} and question \"What kind of document is this? Transcribe every word you can \
         read.\" Then tell me what the tool answered."
    );

    let events = send(&desk, &ask).await;

    let egress = assert_governed(&desk);
    let answers = desk.describer.answers.lock().unwrap().clone();
    assert_eq!(answers.len(), 1);
    let answer = &answers[0];
    for secret in ["Ana", "Wanjiru", "Diaz", "ana.diaz", "555-201", "123-45"] {
        assert!(!answer.contains(secret), "Claude read `{secret}`: {answer}");
    }
    let final_text: String = events
        .iter()
        .filter_map(|e| e.llm_response.content.as_ref())
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    println!("egress digest: {egress}");
    println!("source digest: {}", Digest::of(&card_png()));
    println!("claude-sonnet-5-5 answered: {answer}");
    println!("claude-haiku-5-5 reported: {final_text}");
    let prompts = desk.approver.prompts.lock().unwrap();
    let Evidence::Image(evidence) = &prompts[0].request.evidence[0] else { unreachable!() };
    println!(
        "held call evidence: {} {}x{} {:?}",
        evidence.digest, evidence.width, evidence.height, evidence.caption
    );
}

/// Egress rules that release both classes to Claude without asking, so a run makes one
/// release per call; faces and personal text are still redacted.
const ALLOW_RULES: &str = r#"
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "api.anthropic.com"
classes = ["document", "document_crop"]
action = "allow"
"#;

/// A planner that calls `vision_describe` once per argument set, then answers.
fn planner(calls: Vec<Value>) -> Arc<ScriptedLlm> {
    let mut script: Vec<ScriptedResponse> = calls
        .into_iter()
        .map(|args| ScriptedResponse::function_call("vision_describe", args))
        .collect();
    script.push(ScriptedResponse::text("Done."));
    Arc::new(ScriptedLlm::new("planner", script))
}

/// The egress refusals the run recorded.
fn refusals(desk: &Desk) -> Vec<adk_vision::EgressRefusal> {
    desk.recorder
        .records()
        .into_iter()
        .filter_map(|record| match record {
            adk_vision::EgressRecord::Refused(refusal) => Some(refusal),
            _ => None,
        })
        .collect()
}

/// Without a redactor for a class the rules require, the remote call is refused at the
/// redact step and nothing reaches the provider (spec 3.7 Q1, Q3).
#[cfg(not(feature = "vision-local"))]
#[tokio::test]
async fn a_pack_without_redactors_refuses_every_release() {
    let provider = Arc::new(ScriptedLlm::new("claude-sonnet-5-5", Vec::<ScriptedResponse>::new()));
    let desk = desk_with(
        planner(vec![describe_args()]),
        provider,
        HarnessMode::Ask,
        ALLOW_RULES,
        &[(CARD, card_png())],
        |pack| pack,
    )
    .await;

    send(&desk, "What kind of document is id-card.png?").await;

    assert!(desk.proxy.images.lock().unwrap().is_empty(), "nothing reached the provider");
    assert!(desk.recorder.released().is_empty());
    let refused = refusals(&desk);
    assert!(!refused.is_empty(), "the refusal is recorded");
    assert!(refused.iter().all(|r| r.step == "redact"), "{refused:?}");
    assert!(refused[0].reason.contains("no redactor"), "{}", refused[0].reason);
}

/// The default redactors (feature `vision-local`): RF-DETR Nano for faces and PP-OCRv6 for
/// personal text, loaded on first use; checked here by the same backends on the bytes the
/// provider received.
#[cfg(feature = "vision-local")]
mod local {
    use super::*;
    use adk_vision::onnx::{OnnxOptions, RfDetr};
    use adk_vision::{
        DecodeLimits, DetectQuery, FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher,
        PpOcrReader, PpOcrV6Size, RedactionClass, SourceImage,
    };

    /// The seeded personal strings of the card, and fragments of them.
    const SEEDED: &[&str] =
        &["Ana", "Wanjiru", "Diaz", "ana.diaz", "example.com", "555-201", "7788", "123-45", "6789"];

    /// The score `PersonRedactor` covers from.
    const PERSON_SCORE: f32 = 0.25;

    /// A COCO 2017 validation photo, pinned by SHA-256.
    struct Coco {
        id: &'static str,
        sha256: &'static str,
    }

    /// A skier, one person.
    const SKIER: Coco = Coco {
        id: "000000000785",
        sha256: "83981537a7baeafbeb9c8cb67b3484dc26433f574b3685d021fa537e277e4726",
    };

    /// A baseball runner and a fielder, two people, one partly behind the other.
    const BASEBALL: Coco = Coco {
        id: "000000000872",
        sha256: "c2aa138ee3a59b057a7ba6fc5a6a18e62af531aa7dab78a7bfd33c1cd7e55eb6",
    };

    fn fetcher() -> ModelFetcher {
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()))
    }

    /// The photo's pixels, cached under `ADK_VISION_COCO_DIR` (a temporary directory by
    /// default) and checked against its digest.
    async fn coco(photo: &Coco) -> DecodedImage {
        let dir = std::env::var_os("ADK_VISION_COCO_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("adk-vision-coco"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.jpg", photo.id));
        if !path.exists() {
            let url = format!("http://images.cocodataset.org/val2017/{}.jpg", photo.id);
            let bytes = reqwest::get(url).await.unwrap().bytes().await.unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(Digest::of(&bytes).to_string(), format!("sha256:{}", photo.sha256));
        DecodedImage::decode(&bytes, &DecodeLimits::default()).unwrap().0
    }

    /// The card with `crop` of `photo` in the portrait's place, scaled nearest-neighbour.
    fn card_with_photo(photo: &DecodedImage, crop: [u32; 4]) -> Vec<u8> {
        let (card, _) = DecodedImage::decode(&card_png(), &DecodeLimits::default()).unwrap();
        let (width, height) = card.size();
        let [px0, py0, px1, py1] = layout().portrait.map(|v| v as u32);
        let (px0, py0, px1, py1) = (px0 + 1, py0 + 1, px1 - 1, py1 - 1);
        let mut rgba = card.rgba().to_vec();
        for y in py0..py1 {
            for x in px0..px1 {
                let sx = crop[0] + (x - px0) * (crop[2] - crop[0]) / (px1 - px0);
                let sy = crop[1] + (y - py0) * (crop[3] - crop[1]) / (py1 - py0);
                let at = ((y * width + x) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&photo.pixel(sx, sy));
            }
        }
        DecodedImage::from_rgba(width, height, rgba).unwrap().encode_png().unwrap()
    }

    /// RF-DETR Nano and PP-OCRv6 tiny, small and medium, to check what left.
    struct Checkers {
        people: VisionRuntime,
        readers: Vec<(PpOcrV6Size, VisionRuntime)>,
    }

    impl Checkers {
        async fn load() -> Self {
            let manifest = ManifestSet::builtin().unwrap().get("rf-detr-n").cloned().unwrap();
            let model = fetcher().fetch(&manifest).await.unwrap();
            let detector =
                RfDetr::new(&model, &OnnxOptions::default()).unwrap().with_threshold(0.05);
            let people = VisionRuntime::builder().detector(Arc::new(detector)).build().unwrap();
            let mut readers = Vec::new();
            for size in [PpOcrV6Size::Tiny, PpOcrV6Size::Small, PpOcrV6Size::Medium] {
                let reader = PpOcrReader::fetch(&fetcher(), size).await.unwrap();
                let runtime = VisionRuntime::builder().reader(Arc::new(reader)).build().unwrap();
                readers.push((size, runtime));
            }
            Self { people, readers }
        }

        /// The person scores RF-DETR reports, best first.
        async fn people(&self, png: &[u8]) -> Vec<f32> {
            let image = SourceImage::decode(png, &DecodeLimits::default()).unwrap();
            let query = DetectQuery::Classes(vec!["person".into()]);
            let found = self.people.detect(&VisionCx::new(), &image, &query).await.unwrap();
            let mut scores: Vec<f32> = found.value.iter().filter_map(|r| r.score).collect();
            scores.sort_by(|a, b| b.total_cmp(a));
            scores
        }

        /// The seeded strings each reader finds, with what it read.
        async fn seeded(&self, png: &[u8]) -> Vec<(PpOcrV6Size, Vec<&'static str>, String)> {
            let image = SourceImage::decode(png, &DecodeLimits::default()).unwrap();
            let mut out = Vec::new();
            for (size, reader) in &self.readers {
                let page =
                    reader.read(&VisionCx::new(), &image, &Default::default()).await.unwrap();
                let text: String = page
                    .value
                    .blocks
                    .iter()
                    .map(|b| b.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ");
                let found = SEEDED.iter().copied().filter(|s| text.contains(s)).collect();
                out.push((*size, found, text));
            }
            out
        }
    }

    /// The defaults load on first use: with the weights missing offline, the release is
    /// refused at the redact step and nothing reaches the provider.
    #[tokio::test]
    async fn the_default_redactors_fail_closed_without_their_weights() {
        let empty = tempfile::tempdir().unwrap();
        let offline = ModelFetcher::new(
            FetchConfig::from_lookup(|_| None).with_cache_dir(empty.path()).with_offline(true),
            Arc::new(InMemoryAcceptanceStore::new()),
        );
        let provider =
            Arc::new(ScriptedLlm::new("claude-sonnet-5-5", Vec::<ScriptedResponse>::new()));
        let desk = desk_with(
            planner(vec![describe_args()]),
            provider,
            HarnessMode::Ask,
            ALLOW_RULES,
            &[(CARD, card_png())],
            move |pack| pack.model_fetcher(offline),
        )
        .await;

        send(&desk, "What kind of document is id-card.png?").await;

        assert!(desk.proxy.images.lock().unwrap().is_empty(), "nothing reached the provider");
        assert!(desk.recorder.released().is_empty());
        let refused = refusals(&desk);
        assert!(!refused.is_empty(), "the refusal is recorded");
        assert_eq!(refused[0].step, "redact");
        assert!(refused[0].reason.contains("HF_HUB_OFFLINE"), "{}", refused[0].reason);
    }

    /// S3 with the real backends: the drawn identity card, the card with a real person's
    /// photo in the portrait's place, and a photo of two people go to Claude through the
    /// pack's default redactors. In the bytes the provider received RF-DETR finds no person
    /// and PP-OCRv6 tiny, small and medium find none of the seeded personal strings; each
    /// egress copy is an artifact and its audit record links both digests.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs the RF-DETR Nano and PP-OCRv6 weights and two COCO photos; see the module docs"]
    async fn s3_the_default_redactors_leave_no_person_and_no_personal_string() {
        let checkers = Checkers::load().await;
        let photo_card = card_with_photo(&coco(&SKIER).await, [250, 30, 530, 400]);
        let baseball = coco(&BASEBALL).await.encode_png().unwrap();
        let images: Vec<(&str, Vec<u8>)> =
            vec![(CARD, card_png()), ("id-card-photo.png", photo_card), ("baseball.png", baseball)];
        let question = "What kind of image is this? Transcribe every word you can read.";
        let calls = vec![
            describe_args(),
            json!({"image": "id-card-photo.png", "backend": "claude", "region": REGION, "question": question}),
            json!({"image": "baseball.png", "backend": "claude", "question": question}),
        ];
        let provider = Arc::new(ScriptedLlm::new(
            "claude-sonnet-5-5",
            [
                ScriptedResponse::text("An identity card."),
                ScriptedResponse::text("An identity card."),
                ScriptedResponse::text("A baseball field."),
            ],
        ));
        let desk =
            desk_with(planner(calls), provider, HarnessMode::Ask, ALLOW_RULES, &images, |pack| {
                pack
            })
            .await;

        let started = std::time::Instant::now();
        send(&desk, "Describe the three images.").await;
        let elapsed = started.elapsed();

        let sent = desk.proxy.images.lock().unwrap().clone();
        let receipts = desk.recorder.released();
        assert_eq!(sent.len(), 3, "the provider received three images");
        assert_eq!(receipts.len(), 3, "three releases");
        let released: Vec<_> = desk
            .audit
            .records()
            .into_iter()
            .filter(|r| r.action == "vision.egress.released")
            .collect();
        assert_eq!(released.len(), 3);
        println!("three releases through the default redactors in {elapsed:?}");
        for (((name, source), bytes), receipt) in images.iter().zip(&sent).zip(&receipts) {
            assert_eq!(receipt.source, Digest::of(source), "{name}");
            assert_eq!(Digest::of(bytes), receipt.egress, "{name}: exactly the released bytes");
            assert!(receipt.artifact.is_some(), "{name}: the egress copy is an artifact");
            assert!(
                released.iter().any(|r| r.ext["sourceDigest"] == json!(receipt.source)
                    && r.ext["egressDigest"] == json!(receipt.egress)),
                "{name}: the audit record links both digests"
            );

            let before = checkers.people(source).await;
            let after = checkers.people(bytes).await;
            let covered = |scores: &[f32]| -> Vec<f32> {
                scores.iter().copied().filter(|s| *s >= PERSON_SCORE).collect()
            };
            let count = |class: RedactionClass| receipt.redactions.get(&class).copied();
            println!(
                "{name}: source {} egress {}; redactions faces {:?} personal_text {:?}; people \
                 (score >= {PERSON_SCORE}) before {:?} after {:?}; best person score before \
                 {:.3} after {:.3}",
                receipt.source,
                receipt.egress,
                count(RedactionClass::faces()),
                count(RedactionClass::personal_text()),
                covered(&before),
                covered(&after),
                before.first().copied().unwrap_or(0.0),
                after.first().copied().unwrap_or(0.0),
            );
            assert!(
                covered(&after).is_empty(),
                "{name}: RF-DETR finds a person in the egress copy"
            );
            if *name != CARD {
                assert!(
                    !covered(&before).is_empty(),
                    "{name}: RF-DETR finds the person in the source"
                );
                assert!(count(RedactionClass::faces()) > Some(0), "{name}: the person was covered");
            }

            let source_seen = checkers.seeded(source).await;
            let egress_seen = checkers.seeded(bytes).await;
            for ((size, before, _), (_, after, read)) in source_seen.iter().zip(&egress_seen) {
                println!(
                    "{name}: PP-OCRv6 {size:?} seeded strings before {before:?} after {after:?}"
                );
                println!("    egress copy reads: {read}");
                assert!(after.is_empty(), "{name}: {size:?} reads {after:?} in the egress copy");
                if *name != "baseball.png" {
                    assert_eq!(before.len(), SEEDED.len(), "{name}: {size:?} reads the source");
                }
            }
        }
    }
}
