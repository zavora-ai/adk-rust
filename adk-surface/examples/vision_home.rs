//! Approve with the image on the web home (spec 3.7 §8.2, X11): a harness in "Ask first" with
//! the vision pack holds a `vision_describe` call that would send the identity-card fixture to
//! Claude, and the card shows the exact image that would leave, after redaction.
//!
//! The card is `adk-harness/tests/fixtures/vision/id-card.png`: invented personal data and a
//! drawn portrait. Two on-device doubles read its layout (`id-card.json`) — a person detector
//! that finds the portrait and a reader that reads each word — and report only what the pixels
//! they are handed still show, as the harness's pack tests do. The describer is
//! `claude-sonnet-5-5` behind a recording proxy; `GET /e2e/provider` returns the SHA-256 of
//! every image it received, so a driver checks the provider got exactly the approved digest.
//!
//! | Mode | The harness model |
//! |---|---|
//! | `live` | `claude-haiku-5-5`, told to call `vision_describe` once (needs `ANTHROPIC_API_KEY`) |
//! | `scripted` | `ScriptedLlm`: the call, then "It is an identity card."; the describer answers from a script too |
//!
//! ```sh
//! cargo run -p adk-surface --features web --example vision_home -- 8792 scripted
//! ```

use std::sync::{Arc, Mutex};

use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalService, ApproverSelector, Content, DurableRunStore,
    InMemoryApprovalStore, InMemoryRunStore, Llm, LlmRequest, LlmResponseStream, ModelCallScope,
    Part, RequestContext, SingleAgentLoader, StoreApprovalService,
};
use adk_harness::packs::vision::VisionPack;
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::{
    CardHint, CardHints, LocalSource, MediaFacts, Place, Reversibility, SurfaceEngine,
};
use adk_vision::{
    BBox, BackendDescriptor, Block, BlockKind, CoordFrame, DecodedImage, DescribeRequest,
    Describer, Description, DetectQuery, Detector, Digest, DocumentPage, DocumentReader,
    ImageRegion, ImageTokens, Locality, MediaStore, PersonRedactor, PersonalTextRedactor, PriceRef,
    RawRegion, ReadOptions, ReleasedImage, SentImage, Shape, TokenEstimate, TransformChain,
    VisionConfig, VisionCx, VisionRuntime, Word,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

const APP: &str = "desk";
const OWNER: &str = "ana";
const CARD: &str = "id-card.png";
const REGION: [f32; 4] = [20.0, 10.0, 620.0, 390.0];
const CARD_PNG: &[u8] = include_bytes!("../../adk-harness/tests/fixtures/vision/id-card.png");
const CARD_LAYOUT: &[u8] = include_bytes!("../../adk-harness/tests/fixtures/vision/id-card.json");
const PROVIDER_MODEL: &str = "claude-sonnet-5-5";
/// Claude's published rates in micro-USD per million tokens, input and output, for the estimate.
const RATES: (u64, u64) = (3_000_000, 15_000_000);
const MAX_ANSWER_TOKENS: u32 = 400;

const RULES: &str = r#"
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "api.anthropic.com"
classes = ["document_crop"]
action = "ask"
"#;

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
            scopes: vec!["owner".into(), "operator".into()],
            metadata: Default::default(),
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
struct LayoutWord {
    text: String,
    #[serde(rename = "box")]
    bbox: [f32; 4],
}

#[derive(Clone, Debug, Deserialize)]
struct Layout {
    portrait: [f32; 4],
    words: Vec<LayoutWord>,
}

/// Whether some pixel of `region` is not black: the region still shows something.
fn visible(pixels: &DecodedImage, region: &BBox) -> bool {
    let (w, h) = pixels.size();
    let (x0, y0, cw, ch) = region.pixel_bounds(w, h);
    (y0..y0 + ch).any(|y| (x0..x0 + cw).any(|x| pixels.pixel(x, y) != [0, 0, 0, 255]))
}

/// A layout region as `pixels` shows it: `chain` maps the card to `pixels`.
fn shown(pixels: &DecodedImage, chain: &TransformChain, region: [f32; 4]) -> Option<BBox> {
    let (w, h) = pixels.size();
    let target =
        chain.bbox_to_target(&BBox::new(region[0], region[1], region[2], region[3])).clamped(w, h);
    (target.width() >= 1.0 && target.height() >= 1.0 && visible(pixels, &target)).then_some(target)
}

/// The on-device person detector double: the portrait, while it shows.
struct LayoutPersonDetector {
    descriptor: BackendDescriptor,
    portrait: [f32; 4],
}

#[async_trait]
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

#[async_trait]
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

/// What the provider received: the digest of every image of every request.
#[derive(Default)]
struct RecordingProxy {
    digests: Mutex<Vec<String>>,
    answers: Mutex<Vec<String>>,
}

/// The describer's model behind the recording proxy.
struct Recorded {
    inner: Arc<dyn Llm>,
    proxy: Arc<RecordingProxy>,
}

#[async_trait]
impl Llm for Recorded {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let digests =
            request.contents.iter().flat_map(|c| c.parts.iter()).filter_map(|p| match p {
                Part::InlineData { mime_type, data, .. } if mime_type.starts_with("image/") => {
                    Some(Digest::of(data).to_string())
                }
                _ => None,
            });
        self.proxy.digests.lock().unwrap_or_else(|e| e.into_inner()).extend(digests);
        self.inner.generate_content(request, stream).await
    }
}

/// A remote describer over Claude: the released image and the question.
struct ClaudeDescriber {
    descriptor: BackendDescriptor,
    model: Arc<dyn Llm>,
    proxy: Arc<RecordingProxy>,
}

#[async_trait]
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
            max_output_tokens: Some(MAX_ANSWER_TOKENS as i32),
            ..Default::default()
        });
        let mut stream = match ModelCallScope::current() {
            Some(scope) => scope.generate(self.model.as_ref(), llm_request, false).await,
            None => self.model.generate_content(llm_request, false).await,
        }?;
        let mut text = String::new();
        while let Some(response) = stream.next().await {
            if let Some(content) = response?.content {
                for part in content.parts {
                    if let Part::Text { text: piece } = part {
                        text.push_str(&piece);
                    }
                }
            }
        }
        self.proxy.answers.lock().unwrap_or_else(|e| e.into_inner()).push(text.clone());
        Ok(Description {
            text: Some(text),
            value: None,
            regions: Vec::new(),
            confidence: Some(0.9),
            escalated_from: None,
        })
    }
}

/// What the vision pack knows of its own calls, for their cards (spec 3.7 §8.2): where the
/// image goes, its data class, the preflight's estimate from the image's size, and what it was
/// cut from.
struct VisionCards;

#[async_trait]
impl CardHints for VisionCards {
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
        if request.tool_name != "vision_describe" || request.evidence.is_empty() {
            return None;
        }
        let adk_core::Evidence::Image(image) = &request.evidence[0] else { return None };
        let class = if request.args_redacted.get("region").is_some() {
            "document_crop"
        } else {
            "document"
        };
        let estimate = TokenEstimate::new(ImageTokens::Claude, RATES.0, RATES.1)
            .with_max_output_tokens(MAX_ANSWER_TOKENS);
        let tokens = ImageTokens::Claude.tokens((image.width, image.height));
        let micro = estimate.estimate_micro_usd(&[(image.width, image.height)], 120, None);
        let source = request.args_redacted.get("image").and_then(Value::as_str).unwrap_or(CARD);
        let cut_from = if source.starts_with("sha256:") { CARD } else { source };
        Some(
            CardHint::new()
                .with_heading("Send this image to Claude")
                .with_chat_heading("Send an image to Claude")
                .with_reversibility(Reversibility::CantBeUnsent)
                .with_media(
                    MediaFacts::new()
                        .to("Claude", PROVIDER_MODEL, ["api.anthropic.com"])
                        .with_data_class(class)
                        .with_estimated_cost(micro, Some(format!("{tokens} image tokens")))
                        .with_cut_from(cut_from)
                        .with_cut_note("nothing resized"),
                ),
        )
    }
}

fn claude(key: &str, model: &str) -> Result<Arc<dyn Llm>, Box<dyn std::error::Error>> {
    Ok(Arc::new(adk_model::anthropic::AnthropicClient::new(
        adk_model::anthropic::AnthropicConfig::new(key, model),
    )?))
}

/// The workspace `.env`'s value of `name`, or the environment's; printed nowhere.
fn env_key(name: &str) -> Option<String> {
    if let Some(value) = std::env::var(name).ok().filter(|value| !value.is_empty()) {
        return Some(value);
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".env");
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let (key, value) =
            line.trim().strip_prefix("export ").unwrap_or(line.trim()).split_once('=')?;
        let value = value.trim().trim_matches('"').trim_matches('\'');
        (key.trim() == name && !value.is_empty()).then(|| value.to_string())
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    adk_core::ensure_crypto_provider();
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8792), |port| port.parse())?;
    let live = args.next().as_deref() == Some("live");
    let workspace = tempfile_dir()?;

    let ask = format!(
        "The session holds an identity card as the artifact named `{CARD}`. Call the \
         `vision_describe` tool exactly once with image \"{CARD}\", backend \"claude\", region \
         {REGION:?} and question \"What kind of document is this?\". Then tell me what the tool \
         answered."
    );
    let (model, provider): (Arc<dyn Llm>, Arc<dyn Llm>) = if live {
        let key = env_key("ANTHROPIC_API_KEY").ok_or("ANTHROPIC_API_KEY is not set")?;
        (claude(&key, "claude-haiku-5-5")?, claude(&key, PROVIDER_MODEL)?)
    } else {
        let call = json!({
            "image": CARD,
            "backend": "claude",
            "region": REGION,
            "question": "What kind of document is this?",
        });
        (
            Arc::new(ScriptedLlm::new(
                "scripted",
                [
                    ScriptedResponse::function_call("vision_describe", call),
                    ScriptedResponse::text("It is an identity card."),
                ],
            )),
            Arc::new(ScriptedLlm::new(
                PROVIDER_MODEL,
                [ScriptedResponse::text("An identity card of the Republic of Zavoria.")],
            )),
        )
    };

    let artifacts = Arc::new(InMemoryArtifactService::new());
    artifacts
        .save(SaveRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: "s-desk".into(),
            file_name: CARD.into(),
            part: Part::InlineData {
                mime_type: "image/png".into(),
                data: CARD_PNG.to_vec(),
                uri: None,
                annotations: None,
            },
            version: None,
        })
        .await?;
    let layout: Layout = serde_json::from_slice(CARD_LAYOUT)?;
    let proxy = Arc::new(RecordingProxy::default());
    let describer = Arc::new(ClaudeDescriber {
        descriptor: BackendDescriptor::new(
            "claude",
            Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
            CoordFrame::SentPixels,
        )
        .with_price(PriceRef::Tokens { vendor: "anthropic".into(), model: PROVIDER_MODEL.into() })
        .with_token_estimate(
            TokenEstimate::new(ImageTokens::Claude, RATES.0, RATES.1)
                .with_max_output_tokens(MAX_ANSWER_TOKENS),
        ),
        model: Arc::new(Recorded { inner: provider, proxy: proxy.clone() }),
        proxy: proxy.clone(),
    });
    let on_device =
        |id: &str| BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels);
    let people = Arc::new(LayoutPersonDetector {
        descriptor: on_device("rf-detr-n"),
        portrait: layout.portrait,
    });
    let reader = Arc::new(LayoutReader { descriptor: on_device("pp-ocrv6"), layout });
    let pack = VisionPack::new(MediaStore::new(artifacts.clone()), move |gate, store| {
        VisionRuntime::builder()
            .remote_describer(describer)
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    })
    .redactor(Arc::new(PersonRedactor::new(people)))
    .redactor(Arc::new(PersonalTextRedactor::new(reader).with_labelled_fields(["Name"])))
    .config(VisionConfig::from_toml_str(RULES)?)
    .model_hosts(["api.anthropic.com"]);

    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            ),
    );
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
            .approval_service(Arc::clone(&approvals))
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
    let harness =
        Arc::new(Harness::with_runner(agent.clone(), runner)?.with_run_store(Arc::clone(&runs)));
    let source = LocalSource::builder(APP)
        .sessions(sessions.clone())
        .approvals(approvals)
        .harness(harness)
        .artifacts(artifacts as Arc<dyn ArtifactService>)
        .runs(runs)
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web()])
        .agent_name("desk", "Document desk")
        .card_hints(Arc::new(VisionCards))
        .build();

    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_request_context(Arc::new(Owner));
    let witness = proxy.clone();
    let provider_route = axum::Router::new().route(
        "/e2e/provider",
        axum::routing::get(move || {
            let witness = witness.clone();
            async move {
                let digests = witness.digests.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let answers = witness.answers.lock().unwrap_or_else(|e| e.into_inner()).clone();
                axum::Json(json!({ "digests": digests, "answers": answers }))
            }
        }),
    );
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine))
        .add_root_routes(adk_surface::web::home())
        .add_root_routes(provider_route)
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!(
        "vision home on http://127.0.0.1:{port}/ui/ ({})",
        if live { "live" } else { "scripted" }
    );
    println!("ask: {ask}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// A fresh workspace directory for the harness, removed with the process's temp files.
fn tempfile_dir() -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("vision-home-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
