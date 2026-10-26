//! The voice fixture of the voice tests (spec 3.3 §5.6): "Chief of staff", a harness marked
//! for voice in Ask first over a scripted realtime model, with the engine and the hub over it.
//!
//! "Send the invoice" and "publish the release" are `bash` commands that append to `sent.txt`
//! and `published.txt`: the files' lengths count the runs. The inbox's router makes the
//! release high risk. The harness also has a `calendar` tool of its own, added with `.tool()`,
//! and writes its records to the same audit log as the engine, as `adk-rust up` does.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalCall, ApprovalPolicy, ApprovalRisk, ApprovalRoute, ApprovalService, ApprovalState,
    ApproverSelector, InMemoryApprovalStore, StoreApprovalService,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig, Pack};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply, ScriptedStep};
use adk_realtime::{BoxedModel, ServerEvent};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::voice::{
    FramePolicy, VoiceHub, VoiceInput, VoiceLink, VoiceModel, VoiceOutput, VoiceSource, VoiceUpdate,
};
use adk_surface::{Caller, LocalSource, Origin, SurfaceEngine, SurfaceKind};
use serde_json::json;
use sha2::{Digest, Sha256};

pub const APP: &str = "home";
pub const OWNER: &str = "ana";
pub const WINDOW: Duration = Duration::from_millis(400);
/// Longer than the one second a prompt is on screen before speech counts.
pub const DWELL: Duration = Duration::from_millis(1_150);

pub fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// The command that sends the invoice, and the one that publishes the release.
pub const SEND: &str = "printf x >> sent.txt";
pub const PUBLISH: &str = "printf x >> published.txt";

pub struct Voice {
    pub dir: tempfile::TempDir,
    pub service: Arc<dyn ApprovalService>,
    pub sessions: Arc<dyn SessionService>,
    pub audit: InMemoryAuditRecorder,
    pub engine: SurfaceEngine,
    pub hub: VoiceHub,
    pub model: ScriptedRealtimeModel,
}

impl Voice {
    /// How often a command ran: the length of the file it appends to.
    pub fn runs(&self, file: &str) -> usize {
        std::fs::read(self.dir.path().join(file)).map_or(0, |bytes| bytes.len())
    }

    pub fn sent(&self) -> usize {
        self.runs("sent.txt")
    }

    pub fn published(&self) -> usize {
        self.runs("published.txt")
    }
}

/// The inbox's router: publishing the release is high risk, everything else medium.
pub fn route(call: &ApprovalCall) -> Option<ApprovalRoute> {
    let command = call.request.args["command"].as_str().unwrap_or_default();
    let risk = if command.contains("published") || call.request.tool_name == "publish_release" {
        ApprovalRisk::High
    } else {
        ApprovalRisk::Medium
    };
    Some(
        ApprovalRoute::new(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        )
        .with_risk(risk),
    )
}

/// A tool of the harness's own, added with `.tool()`: today's events.
pub struct Calendar;

#[async_trait::async_trait]
impl adk_core::Tool for Calendar {
    fn name(&self) -> &str {
        "calendar"
    }

    fn description(&self) -> &str {
        "Today's events."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        _args: serde_json::Value,
    ) -> adk_core::Result<serde_json::Value> {
        Ok(json!({ "events": ["Standup at 09:30"] }))
    }
}

/// "Chief of staff" in Ask first, marked for voice, over `script`.
pub async fn voice(script: Vec<ScriptedStep>) -> Voice {
    voice_with(script, |model| Arc::new(model)).await
}

/// [`voice`], with the realtime model wrapped by `wrap`.
pub async fn voice_with(
    script: Vec<ScriptedStep>,
    wrap: impl FnOnce(ScriptedRealtimeModel) -> BoxedModel,
) -> Voice {
    voice_built(script, wrap, |service| service).await
}

/// [`voice_with`], with the inbox's approval service wrapped by `serve`.
pub async fn voice_built(
    script: Vec<ScriptedStep>,
    wrap: impl FnOnce(ScriptedRealtimeModel) -> BoxedModel,
    serve: impl FnOnce(Arc<dyn ApprovalService>) -> Arc<dyn ApprovalService>,
) -> Voice {
    let dir = tempfile::tempdir().unwrap();
    let service: Arc<dyn ApprovalService> = serve(Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(route),
    ));
    let audit = InMemoryAuditRecorder::new();
    let agent = Arc::new(
        HarnessAgent::builder("chief_of_staff")
            .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Ask)
            .pack(Pack::Voice)
            .approval_service(Arc::clone(&service))
            .tool(Arc::new(Calendar))
            .audit_log(Arc::new(audit.clone()))
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(Arc::clone(&sessions)),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let harness = Arc::new(Harness::with_runner(agent, runner).unwrap());
    let local = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .harness(Arc::clone(&harness))
        .build()
        .unwrap();
    let source = VoiceSource::new(Arc::new(local));
    let live = source.live();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(AuditTrail::new(Arc::new(audit.clone())))
        .agent_name("chief_of_staff", "Chief of staff")
        .poll_interval(Duration::from_millis(20))
        .voice_cancel_window(WINDOW)
        .build();
    let model = ScriptedRealtimeModel::new(script).with_idle_timeout(Duration::from_millis(100));
    let hub = VoiceHub::builder(engine.clone(), harness, live)
        .model(
            VoiceModel {
                id: "scripted".into(),
                name: "Scripted".into(),
                input_rate: 16_000,
                output_rate: 24_000,
            },
            wrap(model.clone()),
            FramePolicy::GEMINI,
        )
        .poll_interval(Duration::from_millis(20))
        .build();
    Voice { dir, service, sessions, audit, engine, hub, model }
}

pub fn web() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner", "operator"])
}

pub fn speaker() -> Caller {
    Caller::human(OWNER, SurfaceKind::Voice, Origin::Voice).with_groups(["owner", "operator"])
}

/// The person starts speaking and is transcribed as `text`.
pub fn says(text: &str) -> ScriptedStep {
    ScriptedReply::new(vec![speech_started(), scripted::input_transcript(text)]).on_audio()
}

pub fn speech_started() -> ServerEvent {
    ServerEvent::SpeechStarted { event_id: "speech".into(), audio_start_ms: 0 }
}

/// The model's audio: `millis` of 24 kHz PCM16 silence.
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

/// The model runs `command` once the person asked, then says the call waits.
pub fn asks_for(command: &str) -> Vec<ScriptedStep> {
    vec![
        ScriptedReply::new(
            [
                vec![speech_started(), scripted::input_transcript("please do it")],
                scripted::calls("call-1", "bash", json!({ "command": command })),
            ]
            .concat(),
        )
        .on_audio(),
        ScriptedReply::new(scripted::spoken("That needs your answer on screen.")).on_tool_output(),
    ]
}

pub async fn speak(link: &VoiceLink) {
    link.input.send(VoiceInput::Audio(vec![0; 640])).await.unwrap();
}

/// The next update `pick` accepts, within ten seconds.
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

/// The prompted decision, once there is one.
pub async fn prompted(link: &mut VoiceLink) -> (String, u64) {
    match until(link, "a prompted decision", |update| {
        matches!(update, VoiceUpdate::Prompted { decision_id: Some(_), .. })
    })
    .await
    {
        VoiceUpdate::Prompted { decision_id: Some(id), version: Some(version) } => (id, version),
        other => panic!("{other:?}"),
    }
}

pub async fn eventually<T>(what: &str, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn decide_records(audit: &InMemoryAuditRecorder) -> Vec<AuditRecordDraft> {
    audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_surface::SURFACE_AUDIT_DECIDE)
        .collect()
}

/// The spoken answers the `surface.stop_all` records withdrew.
pub fn stopped_answers(voice: &Voice) -> Vec<serde_json::Value> {
    voice
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_surface::SURFACE_AUDIT_STOP_ALL)
        .flat_map(|record| {
            record
                .ext
                .get("cancelledAnswers")
                .and_then(|ids| ids.as_array().cloned())
                .unwrap_or_default()
        })
        .collect()
}

pub async fn state(voice: &Voice, id: &str) -> ApprovalState {
    voice.service.get(id).await.unwrap().unwrap().state
}
