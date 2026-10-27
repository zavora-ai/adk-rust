//! The home of the screen-action tests (spec 3.3 §2 item 7, §12 "One verb, three origins"):
//! a kernel hosting Finance, a harness agent with the screen-action tools, the engine with the
//! actions' card hints and grant, and the gate; plus an agenda in memory for the handler's
//! own tests.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalPolicy, ApprovalService, ApprovalStore, ApproverSelector, DurableRunStore,
    GovernanceControl, InMemoryApprovalStore, InMemoryRunStore, Llm, RequestContext,
    StoreApprovalService,
};
use adk_harness::{
    Harness, HarnessAgent, HarnessMode, InMemoryStandingRuleStore, InstructionsConfig, OriginPin,
    RuleScope, StandingDecision, StandingRule, StandingRuleStore,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    AgendaHandle, AgendaItem, AgendaOrigin, AgendaState, InMemoryProcessStore, Kernel,
    KernelConfig, PartitionId, ProcessStore, ProcessTable, Wake, WakeSet,
};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::verbs::{
    AgendaAccess, KernelAgenda, VerbError, VerbGate, VerbRegistry, VerbRegistryBuilder,
};
use adk_surface::{Caller, LocalSource, Origin, Place, SurfaceEngine, SurfaceKind};
use async_trait::async_trait;
use chrono::{DateTime, Duration as Span, FixedOffset, Timelike, Utc};

pub const APP: &str = "adk-rust";
pub const OWNER: &str = "ana";
/// The engine's cancel window in these tests.
pub const WINDOW: Duration = Duration::from_millis(300);
/// The id of Research's follow-up "News digest".
pub const DIGEST: &str = "news-digest";
/// The id of "Water plants", in the Home area.
pub const PLANTS: &str = "water-plants";

/// Times are shown at UTC+03:00.
pub fn offset() -> FixedOffset {
    FixedOffset::east_opt(3 * 3600).unwrap()
}

/// When News digest wakes: the whole hour eight hours from now, so nothing fires in a test.
pub fn digest_at() -> DateTime<Utc> {
    let later = Utc::now() + Span::hours(8);
    later.with_minute(0).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap()
}

/// `at` as the home reads it: "19:00" today at UTC+03:00, else "Tue 19:00".
pub fn time_text(at: DateTime<Utc>) -> String {
    let local = at.with_timezone(&offset());
    if local.date_naive() == Utc::now().with_timezone(&offset()).date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%a %H:%M").to_string()
    }
}

pub fn web() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner", "operator"])
}

pub fn phone() -> Caller {
    Caller::human(OWNER, SurfaceKind::Phone, Origin::Touch).with_groups(["owner", "operator"])
}

pub fn terminal() -> Caller {
    Caller::human(OWNER, SurfaceKind::Terminal, Origin::Keyboard)
        .with_groups(["owner", "operator"])
        .with_paired_local()
}

pub fn telegram() -> Caller {
    Caller::human(OWNER, SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() })
        .with_groups(["owner", "operator"])
}

pub fn tray() -> Caller {
    Caller::human(OWNER, SurfaceKind::Tray, Origin::Pointer).with_groups(["owner", "operator"])
}

pub fn voice() -> Caller {
    Caller::human(OWNER, SurfaceKind::Voice, Origin::Voice).with_groups(["owner", "operator"])
}

/// The kernel-hosted home.
pub struct Home {
    pub _dir: tempfile::TempDir,
    pub kernel: Kernel,
    pub runs: Arc<dyn DurableRunStore>,
    pub store: Arc<dyn ApprovalStore>,
    pub service: Arc<dyn ApprovalService>,
    pub sessions: Arc<dyn SessionService>,
    pub audit: InMemoryAuditRecorder,
    pub registry: VerbRegistry,
    pub engine: SurfaceEngine,
    pub gate: VerbGate,
    pub governance: GovernanceControl,
    pub harness: Arc<Harness>,
    /// When News digest wakes.
    pub at: DateTime<Utc>,
}

/// How Finance runs.
pub struct Setup {
    pub model: Arc<dyn Llm>,
    pub mode: HarnessMode,
    /// A standing rule of ana's that would allow `verb_snooze` from any agent.
    pub rule: bool,
}

impl Setup {
    pub fn new(model: Arc<dyn Llm>) -> Self {
        Self { model, mode: HarnessMode::Auto, rule: true }
    }
}

/// The standing rule ana made that would allow every `verb_snooze`.
pub fn snooze_rule() -> StandingRule {
    StandingRule {
        id: "rule-snooze".into(),
        scope: RuleScope::User {
            app_name: APP.into(),
            user_id: OWNER.into(),
            subject_kind: adk_core::SubjectKind::Human,
        },
        tool: "verb_*".into(),
        origin: OriginPin::AnyDeclaration { source: format!("verbs:{APP}") },
        agent: None,
        predicates: Vec::new(),
        decision: StandingDecision::Allow,
        created_by: SubjectRef::human(OWNER),
        created_at: Utc::now(),
        expires_at: None,
        label: "Snoozing without asking".into(),
        source_fingerprint: None,
    }
}

/// Finance in Go ahead with a matching standing rule, by default, hosted on a kernel whose
/// agenda holds News digest (Work, the default area) and Water plants (Home).
pub async fn home(setup: Setup) -> Home {
    let dir = tempfile::tempdir().unwrap();
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let processes: Arc<dyn ProcessStore> = Arc::new(InMemoryProcessStore::new());
    let store: Arc<dyn ApprovalStore> = Arc::new(InMemoryApprovalStore::new());
    let audit = InMemoryAuditRecorder::new();
    let trail = AuditTrail::new(Arc::new(audit.clone()));
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let governance = GovernanceControl::new();
    // Where each answer was given, shared by the engine and the actions' records.
    let answers: Arc<dyn adk_surface::AnswerLog> = Arc::new(adk_surface::InMemoryAnswerLog::new());
    let handle = AgendaHandle::new();
    let agenda = KernelAgenda::new(
        handle.clone(),
        ProcessTable::new(Arc::clone(&runs), Arc::clone(&processes)),
    );
    let registry = VerbRegistry::builder(APP, Arc::new(agenda))
        .approval_store(Arc::clone(&store))
        .audit(trail.clone())
        .governance(governance.clone())
        .sessions(Arc::clone(&sessions))
        .answers(Arc::clone(&answers))
        .utc_offset(offset())
        .agent_name("finance", "Finance")
        .build()
        .unwrap();
    let operator = ApprovalPolicy::new([
        ApproverSelector::group("owner"),
        ApproverSelector::group("operator"),
    ])
    .allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::clone(&store))
            .with_run_store(Arc::clone(&runs))
            .with_router(registry.router(operator)),
    );
    let rules: Arc<dyn StandingRuleStore> = Arc::new(InMemoryStandingRuleStore::new());
    if setup.rule {
        rules.grant(snooze_rule()).await.unwrap();
    }
    let area = PartitionId::default();
    let agent = Arc::new(
        HarnessAgent::builder("finance")
            .model(setup.model)
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(setup.mode)
            .approval_service(Arc::clone(&service))
            .standing_rules(rules)
            .audit_log(Arc::new(audit.clone()))
            .toolset(Arc::new(registry.toolset(area.clone())))
            .tool_classes(Arc::new(registry.classes()))
            .add_operator_policy(Arc::new(registry.policy(area)))
            .build()
            .unwrap(),
    );
    let runner = agent
        .configure_runner(
            Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(Arc::clone(&sessions))
                .governance(governance.clone()),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let harness =
        Arc::new(Harness::with_runner(agent, runner).unwrap().with_run_store(Arc::clone(&runs)));
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("k1")
                        .with_lease_ttl(Duration::from_millis(600))
                        .with_poll_interval(Duration::from_millis(20)),
                )
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(Arc::clone(&processes))
        .agenda_handle(handle)
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .harness(Arc::clone(&harness))
        .kernel(kernel.clone(), APP)
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(trail)
        .answers(answers)
        .places([Place::web(), Place::phone(), Place::chat("Telegram")])
        .agent_name("finance", "Finance")
        .utc_offset(offset())
        .poll_interval(Duration::from_millis(20))
        .voice_cancel_window(WINDOW)
        .card_hints(Arc::new(registry.hints()))
        .grant(registry.grant())
        .build();
    let gate = VerbGate::new(registry.clone(), engine.clone());
    let at = digest_at();
    let home = Home {
        _dir: dir,
        kernel,
        runs,
        store,
        service,
        sessions,
        audit,
        registry,
        engine,
        gate,
        governance,
        harness,
        at,
    };
    home.schedule(DIGEST, "News digest", PartitionId::default(), at).await;
    home.schedule(PLANTS, "Water plants", PartitionId::new("home"), at + Span::hours(1)).await;
    home
}

impl Home {
    /// Puts `title` in `area` on the agenda, asleep until `at`.
    pub async fn schedule(&self, id: &str, title: &str, area: PartitionId, at: DateTime<Utc>) {
        let item = AgendaItem::new(
            id,
            SubjectRef::human(OWNER),
            title,
            "A follow-up Research set",
            AgendaOrigin::User,
            Utc::now(),
        )
        .with_partition(area)
        .with_state(AgendaState::Approved);
        self.kernel.agenda().store().put(item).await.unwrap();
        self.move_to(id, at).await;
    }

    /// Moves item `id` to wake at `at`, directly through the kernel, as the fixture does
    /// before each origin's step.
    pub async fn move_to(&self, id: &str, at: DateTime<Utc>) {
        self.kernel
            .agenda()
            .schedule(id, WakeSet::any_of([Wake::At(at)]), &SubjectRef::human(OWNER))
            .await
            .unwrap();
        // A process spawned or moved asleep is claimed once and parks on its wake.
        eventually("the item's process to sleep", || async {
            let processes = self.kernel.processes().ps(Default::default()).await.unwrap();
            processes
                .iter()
                .any(|process| {
                    process.meta.agenda_item.as_deref() == Some(id)
                        && process.run.state == adk_core::RunState::Waiting
                })
                .then_some(())
        })
        .await;
    }

    /// When item `id` wakes now.
    pub async fn wake_of(&self, id: &str) -> Option<DateTime<Utc>> {
        let item = self.kernel.agenda().get(id).await.unwrap()?;
        match item.state {
            AgendaState::Scheduled { wake } => match wake.any_of.as_slice() {
                [Wake::At(at)] => Some(*at),
                _ => None,
            },
            _ => None,
        }
    }

    /// The `surface.verb` records, oldest first.
    pub fn verb_records(&self) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|record| record.action == "surface.verb").collect()
    }

    /// The `tool_call` records of `tool`, oldest first.
    pub fn tool_records(&self, tool: &str) -> Vec<AuditRecordDraft> {
        self.audit
            .records()
            .into_iter()
            .filter(|record| {
                record.kind == adk_core::audit_log::AuditRecordKind::ToolCall
                    && record.action == tool
            })
            .collect()
    }

    /// The kernel's routes, the engine's and the screen actions' under `/api`, behind a
    /// stand-in for the server's auth middleware that authenticates `context`.
    pub fn api(&self, context: Option<RequestContext>) -> axum::Router {
        let routes = adk_kernel::control::router(self.kernel.clone())
            .merge(adk_surface::web::router(self.engine.clone()))
            .merge(adk_surface::web::verbs::router(self.gate.clone()));
        axum::Router::new().nest("/api", routes).layer(axum::Extension(context))
    }
}

/// The owner, as the server's auth middleware admits them.
pub fn owner_context() -> RequestContext {
    RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into(), "operator".into()],
        metadata: Default::default(),
    }
}

/// Polls `probe` until it answers, for at most ten seconds.
pub async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(10) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

// ---------------------------------------------------------------------------
// An agenda in memory, for the handler's own tests
// ---------------------------------------------------------------------------

/// The agenda as [`AgendaAccess`] sees it, in memory: items by id, and which have started.
#[derive(Default, Clone)]
pub struct MemoryAgenda {
    pub items: Arc<Mutex<BTreeMap<String, AgendaItem>>>,
    pub started: Arc<Mutex<Vec<String>>>,
}

impl MemoryAgenda {
    pub fn put(&self, item: AgendaItem) {
        self.items.lock().unwrap().insert(item.id.clone(), item);
    }

    pub fn get(&self, id: &str) -> Option<AgendaItem> {
        self.items.lock().unwrap().get(id).cloned()
    }

    pub fn start(&self, id: &str) {
        self.started.lock().unwrap().push(id.to_string());
    }
}

/// A scheduled item of `subject` in `area`, waking at `at`.
pub fn scheduled(
    id: &str,
    title: &str,
    subject: &str,
    area: &str,
    at: DateTime<Utc>,
) -> AgendaItem {
    AgendaItem::new(id, SubjectRef::human(subject), title, "why", AgendaOrigin::User, Utc::now())
        .with_partition(PartitionId::new(area))
        .with_state(AgendaState::Scheduled { wake: WakeSet::any_of([Wake::At(at)]) })
}

#[async_trait]
impl AgendaAccess for MemoryAgenda {
    async fn items(
        &self,
        subject: &str,
        area: Option<&PartitionId>,
    ) -> Result<Vec<AgendaItem>, VerbError> {
        Ok(self
            .items
            .lock()
            .unwrap()
            .values()
            .filter(|item| item.subject.id == subject)
            .filter(|item| area.is_none_or(|area| item.partition == *area))
            .filter(|item| {
                matches!(item.state, AgendaState::Scheduled { .. } | AgendaState::Running { .. })
            })
            .cloned()
            .collect())
    }

    async fn item(&self, id: &str) -> Result<Option<AgendaItem>, VerbError> {
        Ok(self.get(id))
    }

    async fn started(&self, item: &AgendaItem) -> Result<bool, VerbError> {
        Ok(self.started.lock().unwrap().contains(&item.id))
    }

    async fn reschedule(
        &self,
        id: &str,
        wake: WakeSet,
        _by: &SubjectRef,
    ) -> Result<AgendaItem, VerbError> {
        let mut items = self.items.lock().unwrap();
        let item = items.get_mut(id).ok_or_else(|| VerbError::not_found(id))?;
        item.state = AgendaState::Scheduled { wake };
        Ok(item.clone())
    }
}

/// A registry over `agenda`, showing times at UTC+03:00.
pub fn memory_registry(agenda: &MemoryAgenda) -> VerbRegistryBuilder {
    VerbRegistry::builder(APP, Arc::new(agenda.clone())).utc_offset(offset())
}

/// A voice conversation whose spoken actions reach the home's agenda (spec 3.3 §5.6).
#[cfg(feature = "voice")]
pub mod voice {
    use std::sync::Arc;
    use std::time::Duration;

    use adk_core::audit_log::AuditTrail;
    use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig, Pack};
    use adk_model::{ScriptedLlm, ScriptedResponse};
    use adk_realtime::ServerEvent;
    use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply, ScriptedStep};
    use adk_runner::Runner;
    use adk_surface::voice::{
        FramePolicy, VoiceHub, VoiceInput, VoiceLink, VoiceModel, VoiceOutput, VoiceSource,
        VoiceUpdate,
    };
    use adk_surface::{LocalSource, SurfaceEngine};

    use super::{APP, Home, WINDOW};

    pub fn says(text: &str) -> ScriptedStep {
        ScriptedReply::new(vec![speech_started(), scripted::input_transcript(text)]).on_audio()
    }

    pub fn speech_started() -> ServerEvent {
        ServerEvent::SpeechStarted { event_id: "speech".into(), audio_start_ms: 0 }
    }

    pub fn model_audio(millis: usize) -> ServerEvent {
        ServerEvent::AudioDelta {
            event_id: "audio".into(),
            response_id: "resp-scripted".into(),
            item_id: "item-scripted".into(),
            output_index: 0,
            content_index: 0,
            delta: vec![0; millis * 48],
        }
    }

    /// A voice conversation with a harness marked for voice, whose spoken actions reach the
    /// home's agenda through its registry.
    pub async fn hub(home: &Home, script: Vec<ScriptedStep>) -> VoiceHub {
        let agent = Arc::new(
            HarnessAgent::builder("chief_of_staff")
                .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
                .workspace(adk_devtools::Workspace::new(home._dir.path()))
                .project_instructions(InstructionsConfig {
                    user_file: None,
                    require_trust: false,
                    ..InstructionsConfig::default()
                })
                .without_compaction()
                .without_default_members()
                .mode(HarnessMode::Ask)
                .pack(Pack::Voice)
                .build()
                .unwrap(),
        );
        let runner = agent
            .configure_runner(
                Runner::builder()
                    .app_name(APP)
                    .agent(agent.clone())
                    .session_service(home.sessions.clone()),
            )
            .build()
            .unwrap();
        let harness = Arc::new(Harness::with_runner(agent, runner).unwrap());
        let local = LocalSource::builder(APP)
            .sessions(home.sessions.clone())
            .approvals(home.service.clone())
            .harness(Arc::clone(&harness))
            .build()
            .unwrap();
        let source = VoiceSource::new(Arc::new(local));
        let live = source.live();
        let engine = SurfaceEngine::builder(Arc::new(source))
            .audit(AuditTrail::new(Arc::new(home.audit.clone())))
            .poll_interval(Duration::from_millis(20))
            .voice_cancel_window(WINDOW)
            .build();
        let gate = adk_surface::verbs::VerbGate::new(home.registry.clone(), engine.clone());
        let model =
            ScriptedRealtimeModel::new(script).with_idle_timeout(Duration::from_millis(100));
        VoiceHub::builder(engine, harness, live)
            .model(
                VoiceModel {
                    id: "scripted".into(),
                    name: "Scripted".into(),
                    input_rate: 16_000,
                    output_rate: 24_000,
                },
                Arc::new(model),
                FramePolicy::GEMINI,
            )
            .poll_interval(Duration::from_millis(20))
            .verbs(gate)
            .build()
    }

    pub async fn speak(link: &VoiceLink) {
        link.input.send(VoiceInput::Audio(vec![0; 640])).await.unwrap();
    }

    pub async fn until(
        link: &mut VoiceLink,
        what: &str,
        pick: impl Fn(&VoiceUpdate) -> bool,
    ) -> VoiceUpdate {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let next = tokio::time::timeout_at(deadline, link.output.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap_or_else(|| panic!("the conversation ended before {what}"));
            if let VoiceOutput::Update(update) = next
                && pick(&update)
            {
                return update;
            }
        }
    }
}
