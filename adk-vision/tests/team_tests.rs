//! Spec 3.7 §7.12 and S15: the perceiver, reasoner and verifier roles and `VisionTeam`.
//!
//! - A perceiver cannot hold a tool with a remote destination, a remote model or a runtime
//!   with a remote backend: construction refuses; a call that would route off the device is
//!   refused when it runs.
//! - The reasoner never receives an unredacted image: the bytes the recording provider got
//!   are the egress gate's redacted crops, never the frame.
//! - A seeded disagreement raises exactly one card with both opinions and the frame's
//!   digest; the person's decision resolves it on the resumed run without asking again.

mod governed;

use std::sync::Arc;
use std::sync::Mutex;

use adk_agent::{TeamArchitectureTemplate, TeamPolicy};
use adk_artifact::InMemoryArtifactService;
use adk_core::egress::{EgressPolicy, ToolEgress};
use adk_core::{
    Agent, ApprovalScope, ApprovalStore, Content, DataLocality, Event, Evidence,
    InMemoryApprovalStore, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, RunConfig, Tool,
    ToolApproval, ToolContext,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::team::{
    DISAGREEMENT_TOOL, FindingSource, Inspection, Opinion, Perceiver, Reasoner, TeamRoleError,
    Verifier, VisionTeam,
};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodedImage, DetectQuery, Detector, Digest,
    EgressGate, InMemoryEgressRecorder, Locality, MediaStore, PersonRedactor, RawRegion,
    SentImage, SourceImage, VisionConfig, VisionCx, VisionRuntime, VisionToolset,
};
use governed::{APP, PlannerModel, RedPersonDetector, SESSION, USER, all_black, save_artifact};
use serde_json::{Value, json};

const FRAME: &str = "part-0042.png";
/// The seeded mark the perceiver is unsure about.
const MARK: BBox = BBox { x0: 180.0, y0: 120.0, x1: 240.0, y1: 140.0 };
/// A face (a red box the person detector finds) inside the mark's crop.
const FACE: BBox = BBox { x0: 244.0, y0: 108.0, x1: 252.0, y1: 150.0 };

const ALLOW_TOML: &str = r#"
[egress]
default = "allow"
redact = ["faces"]
"#;

/// A light part with a dark mark and, inside its crop, a red face.
fn frame() -> (Vec<u8>, SourceImage) {
    let (w, h) = (400u32, 300u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let pixel = if (180..240).contains(&x) && (120..140).contains(&y) {
                [70, 70, 72, 255]
            } else if (244..252).contains(&x) && (108..150).contains(&y) {
                [255, 0, 0, 255]
            } else {
                [214, 214, 210, 255]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    let png = DecodedImage::from_rgba(w, h, rgba).unwrap().encode_png().unwrap();
    let source = SourceImage::decode(&png, &Default::default()).unwrap();
    (png, source)
}

/// An on-device detector answering fixed regions.
struct Fixed {
    descriptor: BackendDescriptor,
    regions: Vec<(BBox, &'static str, f32)>,
}

impl Fixed {
    fn new(id: &str, regions: Vec<(BBox, &'static str, f32)>) -> Arc<Self> {
        Arc::new(Self {
            descriptor: BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels),
            regions,
        })
    }
}

#[async_trait::async_trait]
impl Detector for Fixed {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        _image: &SentImage,
        _query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        Ok(self.regions.iter().map(|(b, l, s)| RawRegion::boxed(*b, *l, *s)).collect())
    }
}

/// A model that records every request and answers one fixed text.
struct Recording {
    name: &'static str,
    locality: DataLocality,
    answer: String,
    requests: Mutex<Vec<LlmRequest>>,
}

impl Recording {
    fn remote(name: &'static str, answer: Value) -> Arc<Self> {
        Arc::new(Self {
            name,
            locality: DataLocality::Remote,
            answer: answer.to_string(),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Llm for Recording {
    fn name(&self) -> &str {
        self.name
    }

    fn data_locality(&self) -> DataLocality {
        self.locality
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(request);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text(self.answer.clone())),
            turn_complete: true,
            ..LlmResponse::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// A scripted planner that runs on the device.
struct OnDevice(Arc<PlannerModel>);

#[async_trait::async_trait]
impl Llm for OnDevice {
    fn name(&self) -> &str {
        "qwen3-vl-local"
    }

    fn data_locality(&self) -> DataLocality {
        DataLocality::InProcess
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.0.generate_content(request, stream).await
    }
}

/// A tool whose network use depends on its arguments: none without a `url`, that URL's host
/// with one.
struct Lookup;

#[async_trait::async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Looks a part number up, locally or at a URL"
    }

    fn egress(&self, args: &Value) -> ToolEgress {
        match args.get("url").and_then(Value::as_str) {
            Some(url) => ToolEgress::for_urls([url]),
            None => ToolEgress::None,
        }
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "part": "A-17" }))
    }
}

/// A tool that says nothing about its network use.
struct Undeclared;

#[async_trait::async_trait]
impl Tool for Undeclared {
    fn name(&self) -> &str {
        "notify"
    }

    fn description(&self) -> &str {
        "Sends a notification"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(Value::Null)
    }
}

fn local_runtime(detector: Arc<Fixed>) -> Arc<VisionRuntime> {
    Arc::new(VisionRuntime::builder().detector(detector).build().unwrap())
}

fn perceiver_runtime() -> Arc<VisionRuntime> {
    local_runtime(Fixed::new("marks", vec![(MARK, "scratch", 0.45)]))
}

fn gate(store: &MediaStore, recorder: Arc<InMemoryEgressRecorder>) -> Arc<EgressGate> {
    Arc::new(
        EgressGate::new(
            EgressPolicy::public_only(),
            VisionConfig::from_toml_str(ALLOW_TOML).unwrap().egress,
            recorder,
        )
        .with_redactor(Arc::new(PersonRedactor::new(Arc::new(RedPersonDetector::new()))))
        .with_media_store(store.clone()),
    )
}

fn defect_answer() -> Value {
    json!({
        "verdict": "defect",
        "label": "scratch",
        "confidence": 0.81,
        "rationale": "a dark linear mark crosses the surface"
    })
}

// ---------------------------------------------------------------------------
// The perceiver holds nothing that leaves the device
// ---------------------------------------------------------------------------

#[test]
fn a_perceiver_refuses_a_tool_with_a_remote_destination() {
    struct Remote;

    #[async_trait::async_trait]
    impl Tool for Remote {
        fn name(&self) -> &str {
            "upload_frame"
        }
        fn description(&self) -> &str {
            "Uploads a frame"
        }
        fn egress(&self, _args: &Value) -> ToolEgress {
            ToolEgress::for_urls(["https://uploads.example.com"])
        }
        async fn execute(&self, _: Arc<dyn ToolContext>, _: Value) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    let error = Perceiver::builder("perceiver")
        .runtime(perceiver_runtime())
        .tool(Arc::new(Remote))
        .build()
        .unwrap_err();
    assert!(
        matches!(&error, TeamRoleError::RemoteTool { tool, .. } if tool == "upload_frame"),
        "{error}"
    );
    let error = Perceiver::builder("perceiver")
        .runtime(perceiver_runtime())
        .tool(Arc::new(Undeclared))
        .build()
        .unwrap_err();
    assert!(matches!(&error, TeamRoleError::RemoteTool { tool, .. } if tool == "notify"), "{error}");
}

#[test]
fn a_perceiver_refuses_a_remote_model_and_a_remote_backend() {
    let cloud = Recording::remote("claude-haiku-5-5", defect_answer());
    let error =
        Perceiver::builder("perceiver").runtime(perceiver_runtime()).model(cloud).build().unwrap_err();
    assert!(matches!(&error, TeamRoleError::RemoteModel { model, .. } if model == "claude-haiku-5-5"));

    let store = MediaStore::new(Arc::new(InMemoryArtifactService::new()));
    let runtime = VisionRuntime::builder()
        .detector(Fixed::new("marks", Vec::new()))
        .remote_describer(Arc::new(governed::RecordingDescriber::claude()))
        .egress_gate(EgressGate::new(
            EgressPolicy::public_only(),
            VisionConfig::default().egress,
            Arc::new(InMemoryEgressRecorder::new()),
        ))
        .media_store(store)
        .build()
        .unwrap();
    let toolset = VisionToolset::builder(Arc::new(runtime)).build().unwrap();
    let error = Perceiver::builder("perceiver").toolset(toolset.clone()).build().unwrap_err();
    assert!(matches!(&error, TeamRoleError::RemoteBackend { .. }), "{error}");

    // A vision tool handed over loose cannot have its runtime checked.
    let describe = toolset.tools().into_iter().find(|t| t.name() == "vision_describe").unwrap();
    let error = Perceiver::builder("perceiver")
        .runtime(perceiver_runtime())
        .tool(describe)
        .build()
        .unwrap_err();
    assert!(matches!(&error, TeamRoleError::UncheckedVisionTool { .. }), "{error}");
}

#[test]
fn a_local_perceiver_builds_and_wraps_every_tool() {
    let store = MediaStore::new(Arc::new(InMemoryArtifactService::new()));
    let runtime = VisionRuntime::builder()
        .detector(Fixed::new("marks", vec![(MARK, "scratch", 0.45)]))
        .editor(Arc::new(adk_vision::DeterministicEditor::new()))
        .media_store(store)
        .build()
        .unwrap();
    let toolset = VisionToolset::builder(Arc::new(runtime)).build().unwrap();
    let perceiver = Perceiver::builder("perceiver")
        .toolset(toolset)
        .tool(Arc::new(Lookup))
        .model(Arc::new(OnDevice(Arc::new(PlannerModel::answering_only()))))
        .build()
        .unwrap();
    let names: Vec<&str> = perceiver.tools().iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        vec!["vision_detect", "vision_crop", "vision_redact", "vision_annotate", "lookup"]
    );
}

#[tokio::test]
async fn a_perceiver_call_that_would_leave_the_device_is_refused_when_it_runs() {
    // `lookup` declares no network use without a URL, so the perceiver may hold it; the model
    // then asks for a URL, and the call is refused before the tool runs.
    let planner = Arc::new(PlannerModel::calling(vec![(
        "lookup",
        json!({ "url": "https://parts.example.com/A-17" }),
    )]));
    let perceiver = Perceiver::builder("perceiver")
        .runtime(perceiver_runtime())
        .tool(Arc::new(Lookup))
        .model(Arc::new(OnDevice(planner.clone())))
        .build()
        .unwrap();
    let runner = runner_for(Arc::new(perceiver), RunConfig::default(), None).await;
    let events = governed::run_turn(&runner.runner, "Which part is this?").await;
    let responses = governed::tool_responses(&events, "lookup");
    assert_eq!(responses.len(), 1, "{events:?}");
    let text = responses[0].to_string();
    assert!(text.contains("stay on the device"), "{text}");
    assert!(!text.contains("A-17\"}"), "the tool did not run: {text}");
}

// ---------------------------------------------------------------------------
// The reasoner sees only redacted crops
// ---------------------------------------------------------------------------

struct Harness {
    runner: Runner,
}

async fn runner_for(
    agent: Arc<dyn Agent>,
    config: RunConfig,
    artifacts: Option<Arc<InMemoryArtifactService>>,
) -> Harness {
    let artifacts = artifacts.unwrap_or_else(|| Arc::new(InMemoryArtifactService::new()));
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    runner_on(agent, config, artifacts, sessions, Arc::new(InMemoryApprovalStore::new()))
}

fn runner_on(
    agent: Arc<dyn Agent>,
    mut config: RunConfig,
    artifacts: Arc<InMemoryArtifactService>,
    sessions: Arc<InMemorySessionService>,
    approvals: Arc<InMemoryApprovalStore>,
) -> Harness {
    config.approval_store = Some(approvals.clone());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(sessions)
        .artifact_service(artifacts)
        .run_config(config)
        .build()
        .unwrap();
    Harness { runner }
}

struct Team {
    team: Arc<VisionTeam>,
    reasoner_model: Arc<Recording>,
    recorder: Arc<InMemoryEgressRecorder>,
    artifacts: Arc<InMemoryArtifactService>,
}

fn team(verifier_finds: Vec<(BBox, &'static str, f32)>) -> Team {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts.clone());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = gate(&store, recorder.clone());
    let reasoner_model = Recording::remote("claude-haiku-5-5", defect_answer());
    let reasoner = Reasoner::builder("reasoner")
        .model(reasoner_model.clone())
        .egress_gate(gate)
        .hosts(["api.anthropic.com"])
        .build()
        .unwrap();
    let verifier = Verifier::builder("verifier")
        .runtime(local_runtime(Fixed::new("strict", verifier_finds)))
        .build()
        .unwrap();
    let perceiver = Perceiver::builder("perceiver").runtime(perceiver_runtime()).build().unwrap();
    let team = VisionTeam::builder("inspector")
        .perceiver(perceiver)
        .reasoner(reasoner)
        .verifier(verifier)
        .question("Is this printed part defective?")
        .verdicts("defect", "no_defect")
        .classes(["scratch"])
        .media_store(store)
        .build()
        .unwrap();
    Team { team: Arc::new(team), reasoner_model, recorder, artifacts }
}

fn images(request: &LlmRequest) -> Vec<Vec<u8>> {
    request
        .contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| match p {
            Part::InlineData { mime_type, data, .. } if mime_type.starts_with("image/") => {
                Some(data.clone())
            }
            _ => None,
        })
        .collect()
}

fn held(events: &[Event]) -> Vec<adk_core::ToolConfirmationRequest> {
    events.iter().filter_map(|e| e.actions.tool_confirmation.clone()).collect()
}

fn inspection(events: &[Event]) -> Inspection {
    let text = events
        .iter()
        .rev()
        .find_map(|e| {
            e.llm_response.content.as_ref().and_then(|c| c.parts.iter().find_map(Part::text))
        })
        .map(str::to_string)
        .expect("a final answer");
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn the_reasoner_receives_only_the_gates_redacted_crop() {
    let (png, source) = frame();
    let team = team(Vec::new());
    save_artifact(&team.artifacts, FRAME, &png).await;
    let harness =
        runner_for(team.team.clone(), RunConfig::default(), Some(team.artifacts.clone())).await;
    governed::run_turn(&harness.runner, &json!({ "image": FRAME }).to_string()).await;

    let requests = team.reasoner_model.requests();
    assert_eq!(requests.len(), 1, "the reasoner was asked once");
    let sent = images(&requests[0]);
    assert_eq!(sent.len(), 1, "one crop");
    let released: Vec<Digest> = team.recorder.released().iter().map(|r| r.egress).collect();
    for bytes in &sent {
        let digest = Digest::of(bytes);
        assert!(released.contains(&digest), "every image the provider got was released");
        assert_ne!(digest, source.digest(), "never the frame");
        let (pixels, _) = DecodedImage::decode(bytes, &Default::default()).unwrap();
        // The mark plus a 16 px margin, not the 400x300 frame.
        assert_eq!(pixels.size(), (92, 52));
        // The face, in crop pixels, is filled solid.
        let face = BBox::new(FACE.x0 - 164.0, FACE.y0 - 104.0, FACE.x1 - 164.0, FACE.y1 - 104.0);
        assert!(all_black(&pixels, &face), "the face was redacted before release");
    }
    let receipt = &team.recorder.released()[0];
    assert_eq!(receipt.source, source.digest());
    assert_eq!(receipt.hosts, vec!["api.anthropic.com".to_string()]);
    assert_eq!(receipt.redactions.values().sum::<u32>(), 1);
}

#[tokio::test]
async fn a_reasoner_agent_refuses_a_crop_the_gate_did_not_release() {
    let (png, source) = frame();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts.clone());
    let model = Recording::remote("claude-haiku-5-5", defect_answer());
    let reasoner = Reasoner::builder("reasoner")
        .model(model.clone())
        .egress_gate(gate(&store, Arc::new(InMemoryEgressRecorder::new())))
        .build()
        .unwrap();
    // The frame is in the session under its digest, but no gate released it.
    save_artifact(&artifacts, &format!("vision-{}.png", source.digest().hex()), &png).await;
    let harness = runner_for(Arc::new(reasoner), RunConfig::default(), Some(artifacts)).await;
    let request = json!({
        "question": "Is it defective?",
        "regions": { "image": source.digest(), "width": 400, "height": 300, "regions": [] },
        "crops": [source.digest()],
    });
    let mut stream = harness
        .runner
        .run(
            adk_core::UserId::try_from(USER).unwrap(),
            adk_core::SessionId::try_from(SESSION).unwrap(),
            Content::new("user").with_text(request.to_string()),
        )
        .await
        .unwrap();
    let mut failed = false;
    while let Some(item) = futures::StreamExt::next(&mut stream).await {
        failed |= item.is_err();
    }
    assert!(failed, "the run fails on an unreleased crop");
    assert!(model.requests().is_empty(), "the model was never called");
}

// ---------------------------------------------------------------------------
// A disagreement is one card with both opinions and the frame
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_disagreement_raises_one_card_with_both_opinions_and_the_frame() {
    let (png, source) = frame();
    let team = team(Vec::new());
    save_artifact(&team.artifacts, FRAME, &png).await;
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let harness = runner_on(
        team.team.clone(),
        RunConfig::default(),
        team.artifacts.clone(),
        sessions.clone(),
        approvals.clone(),
    );
    let ask = json!({ "image": FRAME }).to_string();
    let events = governed::run_turn(&harness.runner, &ask).await;

    let cards = held(&events);
    assert_eq!(cards.len(), 1, "exactly one card");
    let card = &cards[0];
    assert_eq!(card.tool_name, DISAGREEMENT_TOOL);
    let reasoner: Opinion = serde_json::from_value(card.args["reasoner"].clone()).unwrap();
    let verifier: Opinion = serde_json::from_value(card.args["verifier"].clone()).unwrap();
    assert_eq!((reasoner.verdict.as_str(), reasoner.provider.as_str()), ("defect", "anthropic"));
    assert_eq!((verifier.verdict.as_str(), verifier.provider.as_str()), ("no_defect", "local"));
    assert_eq!(card.args["frame"], json!(source.digest()));
    let crop = team.recorder.released()[0].egress;
    assert_eq!(card.args["crop"], json!(crop));
    let digests: Vec<&str> = card.evidence.iter().map(Evidence::digest).collect();
    let (frame_digest, crop_digest) = (source.digest().to_string(), crop.to_string());
    assert_eq!(digests, vec![frame_digest.as_str(), crop_digest.as_str()]);
    let Evidence::Image(frame) = &card.evidence[0] else { panic!("image evidence") };
    assert!(frame.regions.is_some(), "the frame carries the claimed region");
    let pending = approvals
        .pending(&ApprovalScope::new(APP, USER, SESSION))
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "one held call waits in the approval store");

    // The person approves: the resumed run accepts the reasoner's verdict without asking the
    // reasoner again or raising another card.
    let decided = RunConfig::builder().tool_approval(card.fingerprint(), ToolApproval::approve()).build();
    let resumed = runner_on(team.team.clone(), decided, team.artifacts.clone(), sessions, approvals);
    let events = governed::run_turn(&resumed.runner, &ask).await;
    assert!(held(&events).is_empty(), "no second card");
    let inspection = inspection(&events);
    assert_eq!(inspection.findings.len(), 1);
    assert_eq!(inspection.findings[0].verdict, "defect");
    assert_eq!(inspection.findings[0].source, FindingSource::Person);
    assert_eq!(team.reasoner_model.requests().len(), 1, "the reasoner was asked once");
}

#[tokio::test]
async fn agreement_raises_no_card() {
    let (png, _) = frame();
    let team = team(vec![(MARK, "scratch", 0.9)]);
    save_artifact(&team.artifacts, FRAME, &png).await;
    let harness =
        runner_for(team.team.clone(), RunConfig::default(), Some(team.artifacts.clone())).await;
    let events = governed::run_turn(&harness.runner, &json!({ "image": FRAME }).to_string()).await;
    assert!(held(&events).is_empty());
    let inspection = inspection(&events);
    assert_eq!(inspection.findings[0].source, FindingSource::Agreed);
    assert_eq!(inspection.findings[0].verdict, "defect");
}

#[test]
fn the_verifier_runs_on_another_provider_by_default() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts);
    let gate = gate(&store, Arc::new(InMemoryEgressRecorder::new()));
    let reasoner = Arc::new(
        Reasoner::builder("reasoner")
            .model(Recording::remote("claude-haiku-5-5", defect_answer()))
            .egress_gate(gate.clone())
            .build()
            .unwrap(),
    );
    let same = Arc::new(
        Verifier::builder("verifier")
            .runtime(local_runtime(Fixed::new("strict", Vec::new())))
            .model(Recording::remote("claude-sonnet-5-5", defect_answer()))
            .egress_gate(gate)
            .build()
            .unwrap(),
    );
    let perceiver = Arc::new(Perceiver::builder("perceiver").runtime(perceiver_runtime()).build().unwrap());
    let build = |allow: bool| {
        let builder = VisionTeam::builder("inspector")
            .perceiver(perceiver.clone())
            .reasoner(reasoner.clone())
            .verifier(same.clone())
            .media_store(store.clone());
        if allow { builder.allow_same_provider().build() } else { builder.build() }
    };
    assert_eq!(
        build(false).unwrap_err(),
        TeamRoleError::SameProvider { provider: "anthropic".to_string() }
    );
    assert!(build(true).is_ok(), "the default is not a rule");
}

#[tokio::test]
async fn the_roles_bind_to_the_portable_team_template() {
    let spec = TeamArchitectureTemplate::PerceiveReasonVerify {
        name: "inspector".into(),
        coordinator: "lead".into(),
        perceiver: "perceiver".into(),
        reasoner: "reasoner".into(),
        verifier: "verifier".into(),
        policy: TeamPolicy::default(),
    }
    .lower()
    .unwrap();
    let team = team(Vec::new());
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts);
    let reasoner = Reasoner::builder("reasoner")
        .model(Recording::remote("claude-haiku-5-5", defect_answer()))
        .egress_gate(gate(&store, Arc::new(InMemoryEgressRecorder::new())))
        .build()
        .unwrap();
    let lead = adk_agent::LlmAgentBuilder::new("lead")
        .model(Arc::new(PlannerModel::answering_only()))
        .build()
        .unwrap();
    let agents: Vec<Arc<dyn Agent>> = vec![
        Arc::new(lead),
        Arc::new(Perceiver::builder("perceiver").runtime(perceiver_runtime()).build().unwrap()),
        Arc::new(reasoner),
        Arc::new(
            Verifier::builder("verifier")
                .runtime(local_runtime(Fixed::new("strict", Vec::new())))
                .build()
                .unwrap(),
        ),
    ];
    let compiled = spec.compile(agents).unwrap();
    assert_eq!(compiled.coordinator_name(), "lead");
    drop(team);
}
