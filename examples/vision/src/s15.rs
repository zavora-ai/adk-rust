//! S15 (perceiver, reasoner, verifier): the Inspector as a `VisionTeam` on one conveyor frame.
//!
//! The frame shows two whole parts: one with a seeded crack the perceiver is sure of, and one
//! with a short scratch it is not, and above the belt an operator, a generated portrait. The
//! perceiver runs the Inspector's detector on the device and holds only local tools. The
//! ambiguous scratch goes, as a crop the egress gate cut and screened for faces, to the
//! reasoner, Claude Haiku 5.5; the verifier re-checks it with a stricter on-device detector
//! and decides on NVIDIA NIM's `nemotron-3.5-super-vl`, another provider. Both answer from
//! responses recorded live, through their real clients. They disagree, so the team holds one
//! card with both opinions, the frame and the crop; the person's approval resolves it on the
//! resumed run.

use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest};
use adk_core::audit_log::{AuditDecision, InMemoryAuditRecorder};
use adk_core::egress::{EgressPolicy, ToolEgress};
use adk_core::{
    Agent, ApprovalScope, ApprovalStore, Content, DataLocality, Event, Evidence,
    GenerateContentConfig, InMemoryApprovalStore, Llm, LlmRequest, LlmResponseStream, Part,
    RunConfig, SessionId, Tool, ToolApproval, ToolConfirmationRequest, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::cloud::nim::NimConfig;
use adk_vision::onnx::{OnnxOptions, YuNet};
use adk_vision::stream::fixtures::{ConveyorClip, draw_portrait};
use adk_vision::team::{
    DISAGREEMENT_TOOL, FindingSource, Inspection, Opinion, Perceiver, Reasoner, TeamRoleError,
    Verifier, VisionTeam,
};
use adk_vision::{
    BBox, DEFAULT_FACE_DETECTOR, DecodedImage, DetectQuery, DeterministicEditor, Digest,
    EgressGate, FaceRedactor, InMemoryEgressRecorder, LlmDescriber, ManifestSet, MediaStore,
    SourceImage, VisionConfig, VisionCx, VisionRuntime, VisionToolset,
};
use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::MockServer;

use crate::Checks;
use crate::conveyor::{BeltDetector, inspector_spec};
use crate::fixtures::{APP, SESSION, USER, fetcher, fixtures, models, read_json, save};
use crate::replay::{Recording, images_in, live_key, recording_proxy, requests, serve_always};

const FRAME: &str = "inspector-frame.png";
const REASONER: &str = "claude-haiku-5-5";
const VERIFIER: &str = "nvidia/nemotron-3.5-super-vl-120b-a12b";
const ANTHROPIC_HOST: &str = "api.anthropic.com";
const NIM_HOST: &str = "integrate.api.nvidia.com";
/// Faces are looked for from this score, under the redactor's own, so a weakened face counts.
const FACE_SCORE: f32 = 0.3;
const QUESTION: &str = "This is a crop of a 3D-printed part on an inspection line, around a dark \
                        mark the camera found. Is the mark a crack that makes the part \
                        defective, or a surface mark that needs no action?";

/// The frame and what was drawn in it.
struct Scene {
    png: Vec<u8>,
    source: SourceImage,
    /// The seeded crack the perceiver is sure of.
    crack: BBox,
    /// The short scratch it is not.
    scratch: BBox,
    /// The operator's face.
    face: BBox,
}

/// The first clip frame with a cracked part and a sound part both wholly in view; a short
/// scratch is drawn on the sound part and an operator above the belt.
fn scene() -> Result<Scene> {
    let clip = ConveyorClip::new(inspector_spec());
    let (width, height) = (clip.truth().spec.width as f32, clip.truth().spec.height as f32);
    let whole = |b: &[f32; 4]| b[0] > 8.0 && b[2] < width - 8.0;
    let (index, crack, part) = (0..clip.frame_count())
        .find_map(|i| {
            let parts = clip.visible(i);
            let crack = parts.iter().filter(|p| whole(&p.bbox)).find_map(|p| p.defect)?;
            let sound = parts.iter().find(|p| whole(&p.bbox) && p.defect.is_none())?;
            Some((i, crack, sound.bbox))
        })
        .context("the clip shows a cracked and a sound part together")?;
    let image = clip.render(index);
    let mut rgba = image.rgba().to_vec();
    // A 2 px diagonal, 24 px across: dark enough to be found, too short to be sure of.
    let (cx, cy) = (((part[0] + part[2]) / 2.0) as u32, ((part[1] + part[3]) / 2.0) as u32);
    let (x0, y0) = (cx - 12, cy - 6);
    for step in 0..24u32 {
        for dy in 0..2 {
            let (x, y) = (x0 + step, y0 + step / 2 + dy);
            let i = ((y * image.width() + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&[34, 30, 28]);
        }
    }
    let scratch = BBox::new(x0 as f32, y0 as f32, (x0 + 24) as f32, (y0 + 14) as f32);
    let mut image = DecodedImage::from_rgba(image.width(), image.height(), rgba)?;
    // The operator stands above the belt, on the side away from the wall clock.
    let clock = clip.truth().clock;
    let figure_h = 0.36 * height;
    let figure_w = figure_h * 180.0 / 228.0;
    let left = if clock[0] > width / 2.0 { 24.0 } else { width - figure_w - 24.0 };
    let face = draw_portrait(&mut image, BBox::new(left, 6.0, left + figure_w, 6.0 + figure_h), 2);
    let png = image.encode_png()?;
    let source = SourceImage::decode(&png, &Default::default())?;
    Ok(Scene {
        png,
        source,
        crack: BBox::new(crack[0], crack[1], crack[2], crack[3]),
        scratch,
        face,
    })
}

/// The NIM client as the hosted NIM it stands for. The replay server and the recording proxy
/// listen on a loopback address, which the client counts as this machine; the verifier's crop
/// then would not pass the egress gate it passes on `integrate.api.nvidia.com`.
struct Hosted(Arc<dyn Llm>);

#[async_trait::async_trait]
impl Llm for Hosted {
    fn name(&self) -> &str {
        self.0.name()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.0.generate_content(request, stream).await
    }

    fn data_locality(&self) -> DataLocality {
        DataLocality::Remote
    }
}

/// A tool that uploads a frame: what a perceiver must never hold.
struct Upload;

#[async_trait::async_trait]
impl Tool for Upload {
    fn name(&self) -> &str {
        "upload_frame"
    }

    fn description(&self) -> &str {
        "Uploads a frame for review"
    }

    fn egress(&self, _args: &Value) -> ToolEgress {
        ToolEgress::for_urls(["https://uploads.example.com"])
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(Value::Null)
    }
}

/// A recorded provider behind a local server, or behind the recording proxy `name` names.
async fn provider(name: &str, key: &str, file: &str) -> Result<(String, String, MockServer)> {
    let server = MockServer::start().await;
    Ok(match recording_proxy(name) {
        Some(proxy) => (format!("http://{proxy}"), live_key(key), server),
        None => {
            serve_always(&server, &Recording::load(&fixtures().join("recorded").join(file))?).await;
            (server.uri(), "recorded".to_string(), server)
        }
    })
}

/// The digest of the image a recorded request carried, as the proxy reduced it.
fn recorded_image(file: &str) -> Result<Option<String>> {
    let text = read_json(&fixtures().join("recorded").join(file))?["request"].to_string();
    Ok(text.split_once("sha256:").map(|(_, rest)| format!("sha256:{}", &rest[..64])))
}

/// The faces YuNet finds in `image`.
async fn faces(yunet: &Arc<YuNet>, image: &SourceImage) -> Result<Vec<BBox>> {
    let runtime = VisionRuntime::builder().detector(yunet.clone()).build()?;
    let found = runtime
        .detect(&VisionCx::new(), image, &DetectQuery::Classes(vec!["face".into()]))
        .await?
        .value;
    Ok(found
        .iter()
        .filter(|r| r.score.unwrap_or(1.0) >= FACE_SCORE)
        .filter_map(|r| r.shape.bbox())
        .collect())
}

async fn turn(runner: &Runner, text: &str) -> Result<Vec<Event>> {
    let mut stream = runner
        .run(UserId::new(USER)?, SessionId::new(SESSION)?, Content::new("user").with_text(text))
        .await?;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }
    Ok(events)
}

fn cards(events: &[Event]) -> Vec<ToolConfirmationRequest> {
    events.iter().filter_map(|e| e.actions.tool_confirmation.clone()).collect()
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note(
        "the Inspector's detector and YuNet on the device; Claude Haiku 5.5 and NIM \
         nemotron-3.5-super-vl on responses recorded live",
    );
    let scene = scene()?;
    let frame = scene.source.digest();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts.clone());
    save(&artifacts, FRAME, scene.png.clone()).await?;

    // The redactor every crop passes: YuNet, with RF-DETR's whole person for a face it misses.
    let manifest = ManifestSet::builtin()?
        .get(DEFAULT_FACE_DETECTOR)
        .cloned()
        .context("YuNet is in the built-in manifest")?;
    let verified = fetcher().fetch(&manifest).await.context("fetching YuNet")?;
    let yunet =
        Arc::new(YuNet::new(&verified, &OnnxOptions::default())?.with_threshold(FACE_SCORE));
    let redactor =
        FaceRedactor::new(yunet.clone()).with_person_fallback(models().await?.detector.clone());
    let rules = VisionConfig::from_toml_str(&format!(
        "[egress]\ndefault = \"deny\"\nredact = [\"faces\"]\n\n\
         [[egress.rule]]\nhost = \"{ANTHROPIC_HOST}\"\nclasses = [\"team_crop\"]\naction = \"allow\"\n\n\
         [[egress.rule]]\nhost = \"{NIM_HOST}\"\nclasses = [\"team_crop\"]\naction = \"allow\"\n"
    ))?;
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = Arc::new(
        EgressGate::new(EgressPolicy::public_only(), rules.egress, recorder.clone())
            .with_redactor(Arc::new(redactor))
            .with_media_store(store.clone()),
    );

    let (claude_base, claude_key, claude_server) =
        provider("ANTHROPIC", "ANTHROPIC_API_KEY", "claude/s15-haiku-crop.json").await?;
    let config = adk_model::anthropic::AnthropicConfig::new(claude_key, REASONER)
        .allow_insecure_http()
        .with_base_url(claude_base);
    let claude: Arc<dyn Llm> = Arc::new(adk_model::AnthropicClient::new(config)?);
    let (nim_base, nim_key, nim_server) =
        provider("NIM_VLM", "NVIDIA_API_KEY", "nim-vlm/s15-nemotron-crop.json").await?;
    let nim = NimConfig::new(format!("{nim_base}/v1"), nim_key, VERIFIER).client()?;
    let nemotron: Arc<dyn Llm> = Arc::new(Hosted(Arc::new(nim)));

    // The perceiver: the Inspector's detector and the local vision tools, nothing remote.
    let local = Arc::new(
        VisionRuntime::builder()
            .detector(Arc::new(BeltDetector::new()))
            .editor(Arc::new(DeterministicEditor::new()))
            .media_store(store.clone())
            .build()?,
    );
    let toolset = VisionToolset::builder(local.clone()).build()?;
    let perceiver = Perceiver::builder("perceiver").toolset(toolset.clone()).build()?;
    let call = json!({ "image": FRAME, "classes": ["crack"] });
    let held: Vec<&str> = perceiver.tools().iter().map(|t| t.name()).collect();
    checks.check(
        &format!(
            "the perceiver holds {} tools, {}, and each declares no network use",
            held.len(),
            held.join(", ")
        ),
        !held.is_empty()
            && perceiver.tools().iter().all(|t| matches!(t.egress(&call), ToolEgress::None)),
    );
    let remote_model =
        Perceiver::builder("perceiver").runtime(local.clone()).model(claude.clone()).build();
    let describer = LlmDescriber::remote(claude.clone(), [ANTHROPIC_HOST])?;
    let remote_runtime = VisionRuntime::builder()
        .detector(Arc::new(BeltDetector::new()))
        .remote_describer(Arc::new(describer))
        .shared_egress_gate(gate.clone())
        .media_store(store.clone())
        .build()?;
    let remote_tools = Perceiver::builder("perceiver")
        .toolset(VisionToolset::builder(Arc::new(remote_runtime)).build()?)
        .build();
    let upload =
        Perceiver::builder("perceiver").runtime(local.clone()).tool(Arc::new(Upload)).build();
    checks.check(
        "a perceiver is refused Claude as its model, vision tools with a cloud backend, and a tool that uploads",
        matches!(remote_model, Err(TeamRoleError::RemoteModel { .. }))
            && matches!(remote_tools, Err(TeamRoleError::RemoteBackend { .. }))
            && matches!(upload, Err(TeamRoleError::RemoteTool { ref tool, .. }) if tool == "upload_frame"),
    );

    let reasoner = Reasoner::builder("reasoner")
        .model(claude)
        .egress_gate(gate.clone())
        .hosts([ANTHROPIC_HOST])
        .max_output_tokens(400)
        .build()?;
    let mut thinking_off = GenerateContentConfig::default();
    thinking_off
        .extensions
        .insert("openai".into(), json!({ "chat_template_kwargs": { "enable_thinking": false } }));
    // Stricter than the perceiver: a crack scored under 0.75 is not one.
    let strict =
        Arc::new(VisionRuntime::builder().detector(Arc::new(BeltDetector::new())).build()?);
    let verifier = Verifier::builder("verifier")
        .runtime(strict)
        .model(nemotron)
        .provider("nvidia")
        .egress_gate(gate)
        .hosts([NIM_HOST])
        .min_score(0.75)
        .generate_config(thinking_off)
        .max_output_tokens(600)
        .build()?;
    let team: Arc<VisionTeam> = Arc::new(
        VisionTeam::builder("inspector")
            .perceiver(perceiver)
            .reasoner(reasoner)
            .verifier(verifier)
            .question(QUESTION)
            .verdicts("defect", "no_defect")
            .classes(["crack"])
            .media_store(store.clone())
            .build()?,
    );

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await?;
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let audit = InMemoryAuditRecorder::new();
    let runner = |config: RunConfig| -> Result<Runner> {
        Ok(Runner::builder()
            .app_name(APP)
            .agent(team.clone() as Arc<dyn Agent>)
            .session_service(sessions.clone())
            .artifact_service(artifacts.clone())
            .run_config(config)
            .audit_log(Arc::new(audit.clone()))
            .build()?)
    };
    let ask = json!({ "image": FRAME }).to_string();
    let first = runner(RunConfig::builder().approval_store(approvals.clone()).build())?;
    let events = turn(&first, &ask).await?;

    // What left the machine: one crop for each of the two models, both of the scratch.
    let released = recorder.released();
    let sent: Vec<Vec<u8>> = [&claude_server, &nim_server]
        .iter()
        .map(|server| async move { requests(server).await })
        .collect::<futures::future::JoinAll<_>>()
        .await
        .iter()
        .flatten()
        .flat_map(images_in)
        .collect();
    let egress: Vec<Digest> = released.iter().map(|r| r.egress).collect();
    let crop_of_scratch = |r: &adk_vision::EgressReceipt| {
        let (w, h) = r.chain.output_size();
        r.source == frame
            && (w, h) < scene.source.size()
            && r.redactions.contains_key(&adk_vision::RedactionClass::faces())
    };
    checks.check(
        &format!(
            "the reasoner and the verifier were each sent one crop, {}, the egress gate's copy after its face redactor; never the frame",
            released
                .first()
                .map(|r| format!("{}×{} px", r.chain.output_size().0, r.chain.output_size().1))
                .unwrap_or_default()
        ),
        released.len() == 2
            && released.iter().all(crop_of_scratch)
            && released.iter().map(|r| r.hosts.clone()).collect::<Vec<_>>()
                == vec![vec![ANTHROPIC_HOST.to_string()], vec![NIM_HOST.to_string()]]
            && sent.len() == 2
            && sent.iter().all(|bytes| egress.contains(&Digest::of(bytes)) && Digest::of(bytes) != frame),
    );
    let mut faces_sent = 0;
    for bytes in &sent {
        faces_sent += faces(&yunet, &SourceImage::decode(bytes, &Default::default())?).await?.len();
    }
    let in_frame = faces(&yunet, &scene.source).await?;
    checks.check(
        &format!(
            "YuNet finds the operator's face in the frame and none in what Claude or NIM received ({faces_sent})"
        ),
        in_frame.iter().any(|f| scene.face.contains(f.center())) && faces_sent == 0,
    );
    if recording_proxy("ANTHROPIC").is_none() && recording_proxy("NIM_VLM").is_none() {
        let live = [
            recorded_image("claude/s15-haiku-crop.json")?,
            recorded_image("nim-vlm/s15-nemotron-crop.json")?,
        ];
        checks.check(
            "the crops sent now are the bytes Claude and NIM were sent when their answers were recorded",
            live.iter().zip(&egress).all(|(live, now)| live.as_deref() == Some(&*now.to_string())),
        );
    }

    // One card, with both opinions, the frame and the crop.
    let held = cards(&events);
    checks.check(
        &format!("the disagreement raises {} card, `{DISAGREEMENT_TOOL}`", held.len()),
        held.len() == 1 && held[0].tool_name == DISAGREEMENT_TOOL,
    );
    let card = held.first().context("a card")?;
    let reasoner: Opinion = serde_json::from_value(card.args["reasoner"].clone())?;
    let verifier: Opinion = serde_json::from_value(card.args["verifier"].clone())?;
    checks.note(format!(
        "reasoner ({}): {} at {:.2}, \"{}\"; verifier ({}): {} at {:.2}, \"{}\"",
        reasoner.provider,
        reasoner.verdict,
        reasoner.confidence,
        reasoner.rationale,
        verifier.provider,
        verifier.verdict,
        verifier.confidence,
        verifier.rationale
    ));
    checks.check(
        "the card carries both opinions, from Anthropic and from NVIDIA, with different verdicts",
        reasoner.provider == "anthropic"
            && verifier.provider == "nvidia"
            && !reasoner.agrees_with(&verifier),
    );
    let mut frame_shown = false;
    if let Some(Evidence::Image(shown)) = card.evidence.first() {
        let at = &shown.artifact;
        let loaded = artifacts
            .load(LoadRequest {
                app_name: at.app_name.clone(),
                user_id: at.user_id.clone(),
                session_id: at.session_id.clone(),
                file_name: at.file_name.clone(),
                version: Some(at.version),
            })
            .await?;
        frame_shown = matches!(loaded.part, Part::InlineData { ref data, .. } if Digest::of(data) == frame)
            && shown.regions.is_some();
    }
    let evidence: Vec<&str> = card.evidence.iter().map(Evidence::digest).collect();
    let crop = released.first().map(|r| r.egress.to_string()).unwrap_or_default();
    checks.check(
        "the card shows the stored frame with the claimed region, then the crop the reasoner saw",
        card.args["frame"] == json!(frame)
            && frame_shown
            && evidence == vec![frame.to_string().as_str(), crop.as_str()],
    );
    let pending = approvals.pending(&ApprovalScope::new(APP, USER, SESSION)).await?;
    checks.check("one held call waits in the approval store", pending.len() == 1);

    // The person approves: the resumed run takes the reasoner's verdict, asking nobody again.
    let decided = RunConfig::builder()
        .approval_store(approvals.clone())
        .tool_approval(card.fingerprint(), ToolApproval::approve())
        .build();
    let events = turn(&runner(decided)?, &ask).await?;
    let text = events
        .iter()
        .rev()
        .find_map(|e| {
            e.llm_response.content.as_ref().and_then(|c| c.parts.iter().find_map(Part::text))
        })
        .unwrap_or_default();
    let inspection: Option<Inspection> = serde_json::from_str(text).ok();
    let findings = inspection.map(|i| i.findings).unwrap_or_default();
    let found = |bbox: &BBox, source: FindingSource, verdict: &str| {
        findings.iter().any(|f| {
            f.region.shape.bbox().is_some_and(|b| b.iou(bbox) > 0.5)
                && f.source == source
                && f.verdict == verdict
        })
    };
    let asked = requests(&claude_server).await.len() + requests(&nim_server).await.len();
    checks.check(
        "approved on the resumed run: the scratch is a defect by the person's decision, the seeded crack by the perceiver's, with no second card and no provider asked again",
        cards(&events).is_empty()
            && findings.len() == 2
            && found(&scene.scratch, FindingSource::Person, &reasoner.verdict)
            && found(&scene.crack, FindingSource::Perceiver, "defect")
            && (recording_proxy("ANTHROPIC").is_some() || asked == 2),
    );
    let decisions: Vec<(Option<AuditDecision>, Value)> = audit
        .records()
        .into_iter()
        .filter(|r| r.action == DISAGREEMENT_TOOL)
        .map(|r| (r.decision, r.ext.get("evidenceDigests").cloned().unwrap_or_default()))
        .collect();
    let bound = json!([frame.to_string(), crop]);
    checks.check(
        "the audit records hold the card, then the approval, each bound to the frame's and the crop's digests",
        decisions.iter().any(|(d, e)| *d == Some(AuditDecision::ApprovalRequired) && *e == bound)
            && decisions.iter().any(|(d, e)| *d == Some(AuditDecision::Approved) && *e == bound),
    );
    Ok(checks)
}
