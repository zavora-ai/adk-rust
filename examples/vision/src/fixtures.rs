//! The fixtures every scenario shares: the forty generated pages, the on-device models, the
//! harness with the vision pack, and readers for what a run left behind.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_core::audit_log::{AuditRecordDraft, InMemoryAuditRecorder};
use adk_core::{
    Event, InMemorySpendLedger, Llm, Part, RunBudget, SessionId, SpendKey, SpendLedger,
    SpendLimits, SpendPeriod, UserId,
};
use adk_harness::packs::vision::VisionPack;
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, Harness, HarnessAgent, HarnessMode, PromptAnswer,
    ScreenPlanAnswer, ScreenPlanPrompt, Workspace,
};
use adk_vision::onnx::{OnnxOptions, RfDetr};
use adk_vision::{
    BBox, DecodeLimits, EgressGate, FetchConfig, InMemoryAcceptanceStore, InMemoryEgressRecorder,
    ManifestSet, MediaStore, ModelFetcher, PersonRedactor, PersonalTextRedactor, PpOcrReader,
    PpOcrV6Size, SourceImage, VisionConfig, VisionRuntime,
};
use anyhow::{Context, Result};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::OnceCell;

pub const APP: &str = "vision-desk";
pub const USER: &str = "ana";
pub const SESSION: &str = "desk";
/// The detector the face redactor runs: RF-DETR Nano, 108 MB.
pub const PERSON_DETECTOR: &str = "rf-detr-n";
/// The detector reports down to the person redactor's floor, 0.25; the drawn portraits score
/// 0.31 to 0.46 as a person.
pub const PERSON_THRESHOLD: f32 = 0.25;

/// This crate's fixtures.
pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// adk-vision's own fixtures: the S5 screen, the report page and recorded cloud answers.
pub fn crate_fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adk-vision/tests/fixtures")
}

pub fn read_json(path: &Path) -> Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn load_image(path: &Path) -> Result<SourceImage> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(SourceImage::decode(&bytes, &DecodeLimits::default())?)
}

/// One field of a generated page.
#[derive(Clone, Debug, Deserialize)]
pub struct Field {
    pub name: String,
    pub value: String,
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
    #[serde(default)]
    pub hard: Option<String>,
}

impl Field {
    pub fn bbox(&self) -> BBox {
        BBox::new(self.bbox[0], self.bbox[1], self.bbox[2], self.bbox[3])
    }
}

/// One generated page and its ground truth.
#[derive(Clone, Debug, Deserialize)]
pub struct Page {
    pub file: String,
    pub kind: String,
    pub width: u32,
    pub height: u32,
    pub fields: Vec<Field>,
    pub personal: Vec<String>,
    #[serde(default)]
    pub portrait: Option<[f32; 4]>,
}

impl Page {
    pub fn png(&self) -> Result<Vec<u8>> {
        let path = fixtures().join("documents").join(&self.file);
        std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
    }

    pub fn source(&self) -> Result<SourceImage> {
        Ok(SourceImage::decode(&self.png()?, &DecodeLimits::default())?)
    }

    pub fn portrait(&self) -> Option<BBox> {
        self.portrait.map(|p| BBox::new(p[0], p[1], p[2], p[3]))
    }
}

/// The forty pages of `fixtures/documents/documents.json`.
pub fn documents() -> Result<Vec<Page>> {
    #[derive(Deserialize)]
    struct File {
        pages: Vec<Page>,
    }
    let file: File =
        serde_json::from_value(read_json(&fixtures().join("documents").join("documents.json"))?)?;
    Ok(file.pages)
}

pub fn page(file: &str) -> Result<Page> {
    documents()?
        .into_iter()
        .find(|p| p.file == file)
        .with_context(|| format!("no page {file} in documents.json"))
}

/// Lower case without spaces, for comparing what a reader read with what is printed.
pub fn normalise(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// The on-device models, fetched once by revision and verified by SHA-256.
pub struct Models {
    pub reader: Arc<PpOcrReader>,
    pub detector: Arc<RfDetr>,
}

static MODELS: OnceCell<Models> = OnceCell::const_new();

pub fn fetcher() -> ModelFetcher {
    ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()))
}

pub async fn models() -> Result<&'static Models> {
    MODELS
        .get_or_try_init(|| async {
            let fetcher = fetcher();
            let reader = PpOcrReader::fetch(&fetcher, PpOcrV6Size::Tiny)
                .await
                .context("fetching PP-OCRv6 tiny (set HF_HUB_CACHE to keep it)")?;
            let manifest = ManifestSet::builtin()?
                .get(PERSON_DETECTOR)
                .cloned()
                .context("RF-DETR Nano is in the built-in manifest")?;
            let verified = fetcher.fetch(&manifest).await.context("fetching RF-DETR Nano")?;
            let detector =
                RfDetr::new(&verified, &OnnxOptions::default())?.with_threshold(PERSON_THRESHOLD);
            Ok::<_, anyhow::Error>(Models {
                reader: Arc::new(reader),
                detector: Arc::new(detector),
            })
        })
        .await
}

/// The redactors of the egress gate: people by RF-DETR (spec 3.7 Q3: a person's whole box
/// until a face detector is chosen), personal text by PP-OCRv6 and the PII patterns.
pub fn redactors(models: &Models) -> (Arc<PersonRedactor>, Arc<PersonalTextRedactor>) {
    (
        Arc::new(PersonRedactor::new(models.detector.clone())),
        Arc::new(
            PersonalTextRedactor::new(models.reader.clone())
                .with_labelled_fields(["Name", "Client"]),
        ),
    )
}

pub async fn save(artifacts: &InMemoryArtifactService, name: &str, data: Vec<u8>) -> Result<()> {
    artifacts
        .save(SaveRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
            file_name: name.into(),
            part: Part::InlineData {
                mime_type: "image/png".into(),
                data,
                uri: None,
                annotations: None,
            },
            version: None,
        })
        .await?;
    Ok(())
}

/// Approves every prompt once and keeps it, as a person at the card would.
#[derive(Default)]
pub struct Approver {
    prompts: Mutex<Vec<ApprovalPrompt>>,
    plans: Mutex<Vec<ScreenPlanPrompt>>,
}

impl Approver {
    pub fn prompts(&self) -> Vec<ApprovalPrompt> {
        self.prompts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The screen-plan cards the person answered.
    pub fn plans(&self) -> Vec<ScreenPlanPrompt> {
        self.plans.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl std::fmt::Debug for Approver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Approver")
    }
}

#[async_trait::async_trait]
impl ApprovalPrompter for Approver {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        self.prompts.lock().unwrap_or_else(|e| e.into_inner()).push(prompt);
        Ok(PromptAnswer::ApproveOnce)
    }

    async fn prompt_screen_plan(
        &self,
        prompt: ScreenPlanPrompt,
    ) -> adk_core::Result<Option<ScreenPlanAnswer>> {
        self.plans.lock().unwrap_or_else(|e| e.into_inner()).push(prompt);
        Ok(Some(ScreenPlanAnswer::Approve))
    }
}

/// A harness with the vision pack, as an application builds it.
pub struct Desk {
    pub harness: Harness,
    pub approver: Arc<Approver>,
    pub recorder: Arc<InMemoryEgressRecorder>,
    pub audit: InMemoryAuditRecorder,
    pub ledger: Arc<InMemorySpendLedger>,
    _dir: tempfile::TempDir,
}

/// What a desk is built from.
pub struct DeskSpec {
    pub planner: Arc<dyn Llm>,
    pub mode: HarnessMode,
    pub rules: VisionConfig,
    pub budget: Option<RunBudget>,
    pub artifacts: Arc<InMemoryArtifactService>,
    pub redact: bool,
}

pub type Factory =
    Box<dyn FnOnce(Arc<EgressGate>, MediaStore) -> adk_vision::Result<VisionRuntime> + Send>;

pub async fn desk(spec: DeskSpec, factory: Factory) -> Result<Desk> {
    desk_with_ledger(spec, factory, Arc::new(InMemorySpendLedger::new(SpendLimits::new()))).await
}

/// A desk whose spend goes to `ledger`, such as one on a clock of the scenario's choosing.
pub async fn desk_with_ledger(
    spec: DeskSpec,
    factory: Factory,
    ledger: Arc<InMemorySpendLedger>,
) -> Result<Desk> {
    let dir = tempfile::tempdir()?;
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let mut pack = VisionPack::new(MediaStore::new(spec.artifacts.clone()), factory)
        .config(spec.rules)
        .recorder(recorder.clone())
        .model_hosts(["planner.invalid"]);
    if spec.redact {
        let (people, text) = redactors(models().await?);
        pack = pack.redactor(people).redactor(text);
    }
    let approver = Arc::new(Approver::default());
    let audit = InMemoryAuditRecorder::new();
    let mut builder = HarnessAgent::builder("desk")
        .model(spec.planner)
        .workspace(Workspace::new(dir.path()))
        .project_instructions(adk_harness::InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..Default::default()
        })
        .mode(spec.mode)
        .prompter(approver.clone())
        .audit_log(Arc::new(audit.clone()))
        .spend_ledger(ledger.clone())
        .pack_vision(pack)
        .without_compaction();
    if let Some(budget) = spec.budget {
        builder = builder.budget(budget);
    }
    let agent = Arc::new(builder.build()?);
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            adk_runner::Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(sessions)
                .artifact_service(spec.artifacts.clone()),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()?;
    let harness = Harness::with_runner(agent, runner)?;
    harness.open_session(&UserId::new(USER)?, Some(&SessionId::new(SESSION)?)).await?;
    Ok(Desk { harness, approver, recorder, audit, ledger, _dir: dir })
}

impl Desk {
    /// Sends one message and collects the turn's events.
    pub async fn send(&self, text: &str) -> Result<Vec<Event>> {
        let mut stream =
            self.harness.send(&UserId::new(USER)?, &SessionId::new(SESSION)?, text).await?;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event?);
        }
        Ok(events)
    }

    /// What the vision calls committed to the ledger. The scripted planner's own model calls
    /// are committed at the harness's estimate for an unpriced model, under vendor `unknown`.
    /// The ledger is this desk's own, so its lifetime is the run, which a run crossing midnight
    /// UTC would split between two calendar days.
    pub async fn vision_spend(&self) -> Result<u64> {
        let all = self.ledger.spent(&SpendKey::org(APP), SpendPeriod::Lifetime).await?;
        let planner = self
            .ledger
            .spent(&SpendKey::org(APP).with_vendor("unknown"), SpendPeriod::Lifetime)
            .await?;
        Ok(all - planner)
    }

    pub fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|r| r.action == action).collect()
    }
}

/// What each call of `tool` answered, in order.
pub fn tool_responses(events: &[Event], tool: &str) -> Vec<Value> {
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

/// A box from a tool answer's `[x0, y0, x1, y1]`.
pub fn bbox_of(value: &Value) -> Option<BBox> {
    let v: Vec<f32> =
        value.as_array()?.iter().filter_map(|n| n.as_f64().map(|n| n as f32)).collect();
    (v.len() == 4).then(|| BBox::new(v[0], v[1], v[2], v[3]))
}

/// The share of `inner`'s area inside `outer`.
pub fn covered(inner: &BBox, outer: &BBox) -> f32 {
    let w = (inner.x1.min(outer.x1) - inner.x0.max(outer.x0)).max(0.0);
    let h = (inner.y1.min(outer.y1) - inner.y0.max(outer.y0)).max(0.0);
    let area = inner.width() * inner.height();
    if area <= 0.0 { 0.0 } else { w * h / area }
}

/// The largest edge error between two boxes, in pixels.
pub fn edge_error(found: &BBox, want: &BBox) -> f32 {
    [found.x0 - want.x0, found.y0 - want.y0, found.x1 - want.x1, found.y1 - want.y1]
        .into_iter()
        .map(f32::abs)
        .fold(0.0, f32::max)
}
