//! Voice decisions on parked processes (spec 3.2 §7.11, K-I21): the voice surface answers a
//! kernel process parked on a held call through the kernel's approval service, whose run
//! store wakes the run. Scripted over `ScriptedRealtimeModel`; live over Gemini Live
//! (`gemini-3.1-flash-live-preview`) with a recording of the word "yes", the only provider
//! here with a realtime API.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, DurableRunStore, InMemoryApprovalStore,
    InMemoryRunStore, Llm, NewRun, RequestContext, RunCheckpointRef, RunState,
    StoreApprovalService,
};
use adk_harness::{
    DecisionSource, Harness, HarnessAgent, HarnessMode, InstructionsConfig, Pack, SpokenDecision,
    VOICE_AUDIT_DECIDED, VoiceConfig, VoiceEvent, VoiceSurface,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{InMemoryProcessStore, Kernel, KernelConfig, ProcessKind, ProcessSpec};
use adk_managed::ModelResolver;
use adk_managed::resolver::DefaultModelResolver;
use adk_managed::types::ModelRef;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_realtime::audio::{AudioChunk, AudioFormat};
use adk_realtime::model::BoxedModel;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_realtime::{RealtimeModel, ServerEvent};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::Result;
use serde_json::{Value, json};

use crate::Checks;

/// The scripted scenarios of this module, and what `main` does instead.
pub const SCRIPTED: &[(&str, &str)] = &[(
    "v1_voice_answers_a_parked_process",
    "the voice surface decides only its own session's holds: a parked process's call answers `harness.voice.not_held` and stays parked",
)];

/// The live scenarios of this module.
pub const LIVE: &[&str] = &["live_voice_parked_process"];

const APP: &str = "adk-rust";
const USER: &str = "ana";
const OPERATOR: &str = "operator";

/// A recording of the word "yes", mono PCM16 at 16 kHz, as Gemini Live takes it.
const YES_16K: &[u8] = include_bytes!("../fixtures/yes-16k.wav");

/// A kernel hosting a harness in Ask mode on `model`, its held calls routed to the operator
/// group and their decisions waking the parked run.
struct Hosted {
    kernel: Kernel,
    runs: Arc<dyn DurableRunStore>,
    service: Arc<dyn ApprovalService>,
}

fn host(model: Arc<dyn Llm>, workspace: &Path) -> Result<Hosted> {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group(OPERATOR)]).allow_self_approval(),
            ),
    );
    let agent = Arc::new(
        HarnessAgent::builder(APP)
            .model(model)
            .workspace(adk_devtools::Workspace::new(workspace))
            .project_instructions(InstructionsConfig {
                user_file: None,
                ..InstructionsConfig::default()
            })
            .without_default_members()
            .mode(HarnessMode::Ask)
            .approval_service(Arc::clone(&service))
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(agent.agent())
        .session_service(sessions)
        .approval_store(agent.runtime().approval_store().clone())
        .build()?;
    let harness = Harness::with_runner(agent, runner)?.with_run_store(Arc::clone(&runs));
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_scheduler_tick(Duration::from_millis(250))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(Arc::new(InMemoryProcessStore::new()))
        .build()?;
    Ok(Hosted { kernel, runs, service })
}

/// Spawns the note process and waits until it parks on its held write; returns its pid.
async fn parked_note(hosted: &Hosted, limit: Duration) -> Result<Option<String>> {
    let pid = hosted
        .kernel
        .processes()
        .spawn(
            NewRun::new(
                "background",
                APP,
                USER,
                APP,
                json!({ "text": "Write the line `standup moved to 9:30` to note.txt in the workspace with the write_file tool." }),
            )
            .with_session_id("s-note"),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await?
        .meta
        .pid;
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        let run = hosted.runs.get(&pid).await?;
        if run.is_some_and(|run| {
            run.state == RunState::Waiting
                && matches!(run.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. }))
        }) {
            return Ok(Some(pid));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(None)
}

async fn completed(hosted: &Hosted, pid: &str, limit: Duration) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if hosted.runs.get(pid).await?.is_some_and(|run| run.state == RunState::Completed) {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(false)
}

/// The voice surface over `realtime`: its own harness marked for voice, the user in the
/// operator group, answering the kernel's parked processes.
async fn surface(
    workspace: &Path,
    realtime: BoxedModel,
    hosted: &Hosted,
    audit: &InMemoryAuditRecorder,
) -> Result<VoiceSurface> {
    let harness = Arc::new(
        HarnessAgent::builder("voice")
            .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
            .workspace(adk_devtools::Workspace::new(workspace))
            .project_instructions(InstructionsConfig {
                user_file: None,
                ..InstructionsConfig::default()
            })
            .mode(HarnessMode::Ask)
            .pack(Pack::Voice)
            .audit_log(Arc::new(audit.clone()))
            .build()?,
    );
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some("voice-1".into()),
            state: HashMap::new(),
        })
        .await?;
    let context = RequestContext {
        user_id: USER.into(),
        scopes: vec![OPERATOR.into()],
        metadata: HashMap::new(),
    };
    Ok(VoiceSurface::open(
        harness,
        realtime,
        sessions as Arc<dyn SessionService>,
        USER,
        "voice-1",
        VoiceConfig::new(APP).with_request_context(context),
    )?
    .with_parked_processes(hosted.kernel.processes().clone(), Arc::clone(&hosted.service)))
}

fn decided_records(audit: &InMemoryAuditRecorder) -> Vec<Value> {
    audit
        .records()
        .into_iter()
        .filter(|record| record.action == VOICE_AUDIT_DECIDED)
        .map(|record| Value::Object(record.ext.into_iter().collect()))
        .collect()
}

pub async fn run_scripted(name: &str) -> Option<Result<Checks>> {
    match name {
        "v1_voice_answers_a_parked_process" => Some(scripted_parked().await),
        _ => None,
    }
}

async fn scripted_parked() -> Result<Checks> {
    let mut checks = Checks::default();
    let workspace = tempfile::tempdir()?;
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "note.txt", "content": "standup moved to 9:30" }),
            ),
            ScriptedResponse::text("written"),
        ],
    ));
    let hosted = host(model, workspace.path())?;
    hosted.kernel.start().await?;
    let Some(pid) = parked_note(&hosted, Duration::from_secs(10)).await? else {
        checks.check("the process parked on its held write", false);
        return Ok(checks);
    };
    checks.check("the process parked on its held write", true);
    let audit = InMemoryAuditRecorder::new();
    let realtime = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![scripted::input_transcript("yes")]).on_audio(),
    ])
    .with_model_id("gemini-3.1-flash-live-preview")
    .with_idle_timeout(Duration::from_millis(200));
    let surface = surface(workspace.path(), Arc::new(realtime), &hosted, &audit).await?;
    let listed = surface.parked_processes().await?;
    checks.check(
        "the surface lists the parked process",
        listed.iter().any(|parked| parked.pid == pid && !parked.frozen),
    );
    surface.offer_parked(&pid).await?;
    surface.connect().await?;
    surface.send_audio(&AudioChunk::pcm16_16khz(vec![0; 640])).await?;
    let mut decided = None;
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_secs(2), surface.next_event()).await
    {
        if let VoiceEvent::Decided { call_id, decision, by } = event? {
            decided = Some((call_id, decision, by));
            break;
        }
    }
    surface.close().await;
    checks.check(
        "the spoken yes decided the parked process",
        decided == Some((pid.clone(), SpokenDecision::Approve, DecisionSource::Voice)),
    );
    checks.check(
        "the approval service woke the process and it completed",
        completed(&hosted, &pid, Duration::from_secs(10)).await?,
    );
    checks.check(
        "note.txt was written once, after the approval",
        std::fs::read_to_string(workspace.path().join("note.txt"))
            .is_ok_and(|text| text == "standup moved to 9:30"),
    );
    checks.check(
        "harness.voice.decided names the process",
        decided_records(&audit)
            .iter()
            .any(|record| record["pid"] == pid.as_str() && record["by"] == "voice"),
    );
    hosted.kernel.stop().await?;
    Ok(checks)
}

pub async fn run_live(name: &str, provider: &str, model: &str) -> Option<Result<Checks>> {
    match name {
        "live_voice_parked_process" => Some(live_parked(provider, model).await),
        _ => None,
    }
}

/// Sends the recording as the user's turn the way a microphone does: 100 ms chunks at the
/// rate they are spoken, then 1.5 s of silence, which ends the turn for Gemini Live's
/// server-side voice activity detection.
async fn say_yes(surface: &VoiceSurface) -> Result<()> {
    let chunk_bytes = 16_000 / 10 * 2;
    let silence = vec![0u8; 16_000 * 3];
    for chunk in YES_16K[44..].chunks(chunk_bytes).chain(silence.chunks(chunk_bytes)) {
        surface.send_audio(&AudioChunk::new(chunk.to_vec(), AudioFormat::pcm16_16khz())).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    surface.commit_audio().await?;
    Ok(())
}

/// Live: a harness on `model` hosted by a kernel parks its write; Gemini Live asks the user
/// about it, the recording of "yes" answers, the kernel resumes the process.
async fn live_parked(provider: &str, model: &str) -> Result<Checks> {
    let mut checks = Checks::default();
    let Some(key) = std::env::var("GOOGLE_API_KEY")
        .ok()
        .or_else(|| std::env::var("GEMINI_API_KEY").ok())
        .filter(|key| !key.trim().is_empty())
    else {
        checks.check(
            &format!("skipped on {provider}: voice runs on Gemini Live, and no Gemini key is set"),
            true,
        );
        return Ok(checks);
    };
    if provider != "gemini" {
        checks.check(
            &format!("skipped on {provider}: it has no realtime API; voice runs on Gemini Live"),
            true,
        );
        return Ok(checks);
    }
    let chat = DefaultModelResolver::new().resolve(&ModelRef::Shorthand(model.into())).await?;
    let workspace = tempfile::tempdir()?;
    let hosted = host(chat, workspace.path())?;
    hosted.kernel.start().await?;
    let Some(pid) = parked_note(&hosted, Duration::from_secs(120)).await? else {
        checks.check("the process parked on its held write", false);
        return Ok(checks);
    };
    checks.check("the process parked on its held write", true);
    let live = adk_realtime::gemini::GeminiRealtimeModel::new(
        adk_realtime::gemini::GeminiLiveBackend::studio(key),
        adk_realtime::gemini::DEFAULT_MODEL,
    );
    println!("    realtime model: {}", live.model_id());
    let audit = InMemoryAuditRecorder::new();
    let surface = surface(workspace.path(), Arc::new(live), &hosted, &audit).await?;
    let parked = surface.offer_parked(&pid).await?;
    surface.connect().await?;
    surface
        .say(&format!(
            "A background task of mine is waiting for my approval: it wants to call {} to write \
             note.txt. In one short sentence, ask me whether to approve it, then wait for my \
             answer. Do not call any tool.",
            parked.request.tool_name
        ))
        .await?;
    let mut spoken = String::new();
    let mut answered = false;
    let mut decided = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while tokio::time::Instant::now() < deadline && decided.is_none() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Some(event)) = tokio::time::timeout(remaining, surface.next_event()).await else {
            break;
        };
        match event? {
            VoiceEvent::Decided { call_id, decision, by } => {
                decided = Some((call_id, decision, by))
            }
            VoiceEvent::Provider(
                ServerEvent::TranscriptDelta { delta, .. } | ServerEvent::TextDelta { delta, .. },
            ) => {
                spoken.push_str(&delta);
            }
            VoiceEvent::Provider(ServerEvent::InputTranscriptCompleted { transcript, .. }) => {
                println!("    [user] {}", adk_harness::escape_control(&transcript));
            }
            VoiceEvent::Provider(ServerEvent::ResponseDone { .. })
                if !answered && !spoken.trim().is_empty() =>
            {
                println!("    [model] {}", adk_harness::escape_control(spoken.trim()));
                say_yes(&surface).await?;
                answered = true;
            }
            _ => {}
        }
    }
    surface.close().await;
    checks.check(
        format!("the spoken yes decided the parked process ({decided:?})").as_str(),
        decided == Some((pid.clone(), SpokenDecision::Approve, DecisionSource::Voice)),
    );
    checks.check(
        "the approval service woke the process and it completed",
        completed(&hosted, &pid, Duration::from_secs(120)).await?,
    );
    let note = std::fs::read_to_string(workspace.path().join("note.txt")).unwrap_or_default();
    println!("    note.txt: {}", adk_harness::escape_control(note.trim()));
    checks.check("note.txt was written after the approval", note.contains("9:30"));
    checks.check(
        "harness.voice.decided names the process",
        decided_records(&audit)
            .iter()
            .any(|record| record["pid"] == pid.as_str() && record["by"] == "voice"),
    );
    hosted.kernel.stop().await?;
    Ok(checks)
}
