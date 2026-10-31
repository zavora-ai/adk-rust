//! Voice decisions on parked kernel processes (spec 3.2 §7.11, K-I21): the voice surface
//! answers a process the kernel parked on a held call through the kernel's approval service,
//! whose run store wakes the run, and refuses a process a freeze parked.
//!
//! The realtime provider is `ScriptedRealtimeModel`, as the voice pack's own tests use it:
//! the user's transcribed "yes" is scripted, while the kernel, its worker, the harness it
//! hosts, the approval service and the audit trail are real. On `origin/main` the surface
//! decides only the calls its own realtime session held: `VoiceSurface::decide` answers a
//! parked process's fingerprint with `harness.voice.not_held` and the process stays parked.

#![cfg(all(feature = "voice", feature = "kernel"))]

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::audit_log::{AuditActor, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService, ApprovalState,
    ApproverSelector, DurableRunStore, InMemoryApprovalStore, InMemoryRunStore, NewRun,
    RequestContext, RunCheckpointRef, RunState, StoreApprovalService, SubjectRef, Tool,
    ToolConfirmationRequest, ToolContext, ToolEffect,
};
use adk_devtools::Workspace;
use adk_harness::{
    DecisionSource, HarnessAgent, HarnessMode, Pack, SpokenDecision, VOICE_AUDIT_DECIDED,
    VOICE_AUDIT_REFUSED, VOICE_FROZEN_CODE, VoiceConfig, VoiceEvent, VoiceSurface,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    InMemoryProcessStore, Kernel, KernelConfig, ProcessKind, ProcessSpec, ProcessStore, Suspension,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_realtime::audio::AudioChunk;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_server::background::DurableWorkerConfig;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use common::*;
use serde_json::{Value, json};

const OPERATOR: &str = "operator";
const VOICE_USER: &str = "ana";

/// The kernel's approval service: held calls routed to the operator group, decisions waking
/// the parked run.
fn approvals(runs: &Arc<dyn DurableRunStore>) -> Arc<dyn ApprovalService> {
    Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(runs))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group(OPERATOR)]).allow_self_approval(),
            ),
    )
}

/// A kernel hosting the fixture's harness, with fast polls and no control plane.
fn kernel(
    f: Fixture,
    runs: &Arc<dyn DurableRunStore>,
) -> (Kernel, Fixture, Arc<InMemoryProcessStore>) {
    let Fixture { dir, model, harness, audit, user, session } = f;
    let harness = harness.with_run_store(Arc::clone(runs));
    let processes = Arc::new(InMemoryProcessStore::new());
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
        .process_store(Arc::clone(&processes) as Arc<dyn ProcessStore>)
        .build()
        .unwrap();
    (kernel, Fixture { dir, model, harness, audit, user, session }, processes)
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
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

/// Spawns a process of the hosted harness on `text` and returns its pid.
async fn spawn(kernel: &Kernel, f: &Fixture, session: &str, text: &str) -> String {
    kernel
        .processes()
        .spawn(
            NewRun::new("background", APP, f.user.as_ref(), APP, json!({ "text": text }))
                .with_session_id(session),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap()
        .meta
        .pid
}

/// The voice surface: its own harness marked for voice, the scripted provider, and the user
/// in the operator group.
async fn surface(
    dir: &std::path::Path,
    realtime: ScriptedRealtimeModel,
    audit: &InMemoryAuditRecorder,
) -> VoiceSurface {
    let harness = Arc::new(
        HarnessAgent::builder("voice")
            .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
            .workspace(Workspace::new(dir))
            .project_instructions(no_user_file())
            .mode(HarnessMode::Ask)
            .pack(Pack::Voice)
            .audit_log(Arc::new(audit.clone()))
            .build()
            .unwrap(),
    );
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: VOICE_USER.into(),
            session_id: Some("voice-1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let context = RequestContext {
        user_id: VOICE_USER.into(),
        scopes: vec![OPERATOR.into()],
        metadata: Default::default(),
    };
    VoiceSurface::open(
        harness,
        Arc::new(realtime),
        sessions as Arc<dyn SessionService>,
        VOICE_USER,
        "voice-1",
        VoiceConfig::new(APP).with_request_context(context),
    )
    .unwrap()
}

fn records(audit: &InMemoryAuditRecorder, action: &str) -> Vec<Value> {
    audit
        .records()
        .into_iter()
        .filter(|record| record.action == action)
        .map(|record| Value::Object(record.ext.into_iter().collect()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spoken_yes_wakes_a_kernel_process_parked_on_a_held_call() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let service = approvals(&runs);
    let f = fixture(
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "note.txt", "content": "hi" }),
            ),
            ScriptedResponse::text("written"),
        ],
        {
            let service = service.clone();
            move |b| b.mode(HarnessMode::Ask).without_default_members().approval_service(service)
        },
    )
    .await;
    let (kernel, f, _) = kernel(f, &runs);
    kernel.start().await.unwrap();
    let pid = spawn(&kernel, &f, "s-parked", "write the note").await;
    let parked = eventually("the process parked on its held write", || async {
        runs.get(&pid).await.unwrap().filter(|run| {
            run.state == RunState::Waiting
                && matches!(run.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. }))
        })
    })
    .await;
    assert!(!f.path("note.txt").exists(), "held, not written");

    let audit = InMemoryAuditRecorder::new();
    let realtime = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![scripted::input_transcript("yes")]).on_audio(),
    ])
    .with_model_id("gemini-3.1-flash-live-preview")
    .with_idle_timeout(Duration::from_millis(150));
    let surface = surface(f.dir.path(), realtime, &audit).await;

    // As on main: the parked call is not one the voice session holds.
    let Some(RunCheckpointRef::AwaitingApproval { fingerprints, .. }) = parked.checkpoint else {
        unreachable!()
    };
    let not_held = surface.decide(&fingerprints[0], SpokenDecision::Approve).await.unwrap_err();
    assert_eq!(not_held.code, "harness.voice.not_held");

    let surface = surface.with_parked_processes(kernel.processes().clone(), service.clone());
    let listed = surface.parked_processes().await.unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!((listed[0].pid.as_str(), listed[0].frozen), (pid.as_str(), false));
    assert_eq!(listed[0].request.tool_name, "write_file");
    surface.offer_parked(&pid).await.unwrap();

    surface.connect().await.unwrap();
    surface.send_audio(&AudioChunk::pcm16_16khz(vec![0; 640])).await.unwrap();
    let decided = eventually("the spoken decision", || async {
        match tokio::time::timeout(Duration::from_millis(500), surface.next_event()).await {
            Ok(Some(Ok(VoiceEvent::Decided { call_id, decision, by }))) => {
                Some((call_id, decision, by))
            }
            Ok(Some(Err(error))) => panic!("the voice session failed: {error}"),
            _ => None,
        }
    })
    .await;
    assert_eq!(decided, (pid.clone(), SpokenDecision::Approve, DecisionSource::Voice));
    assert!(surface.offered_processes().is_empty());

    // The decision went through the kernel's approval service, which woke the process.
    eventually("the woken process to complete", || async {
        runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Completed)
    })
    .await;
    assert_eq!(std::fs::read_to_string(f.path("note.txt")).unwrap(), "hi");
    let request = service.get(&listed[0].request.id).await.unwrap().unwrap();
    assert_eq!(request.state, ApprovalState::Approved);
    let decided = records(&audit, VOICE_AUDIT_DECIDED);
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(
        (decided[0]["pid"].as_str(), decided[0]["by"].as_str()),
        (Some(pid.as_str()), Some("voice"))
    );
    assert!(decided[0]["transcriptDigest"].as_str().is_some_and(|d| d.len() == 64));
    surface.close().await;
    kernel.stop().await.unwrap();
}

/// A read-only step the test releases, so a freeze lands while it runs.
struct Gate {
    calls: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Tool for Gate {
    fn name(&self) -> &str {
        "survey"
    }

    fn description(&self) -> &str {
        "looks around"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.release.acquire().await.unwrap().forget();
        Ok(json!({ "seen": "nothing" }))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_voice_decision_on_a_freeze_parked_process_is_refused() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let service = approvals(&runs);
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let f = fixture(
        vec![ScriptedResponse::function_call("survey", json!({})), ScriptedResponse::text("done")],
        {
            let (service, calls, release) = (service.clone(), calls.clone(), release.clone());
            move |b| {
                b.mode(HarnessMode::Auto)
                    .without_default_members()
                    .approval_service(service)
                    .tool(Arc::new(Gate { calls, release }))
            }
        },
    )
    .await;
    let (kernel, f, processes) = kernel(f, &runs);
    kernel.start().await.unwrap();
    let pid = spawn(&kernel, &f, "s-frozen", "look around").await;
    eventually("the step to start", || async { (calls.load(Ordering::SeqCst) == 1).then_some(()) })
        .await;

    // The freeze lands mid-step; the run parks at its next model-call boundary.
    kernel.freeze("incident 12", AuditActor::subject(SubjectRef::human("ops"))).await.unwrap();
    release.add_permits(1);
    eventually("the process parked by the freeze", || async {
        let meta = processes.get(&pid).await.unwrap()?;
        let run = runs.get(&pid).await.unwrap()?;
        (run.state == RunState::Waiting && meta.suspended_by == Some(Suspension::Freeze))
            .then_some(())
    })
    .await;
    let model_calls = f.model.requests().len();

    // A hold the freeze caught: its request names the frozen run.
    let scope = ApprovalScope::new(APP, f.user.as_ref(), "s-frozen");
    let call = ToolConfirmationRequest {
        tool_name: "survey".into(),
        function_call_id: Some("call-9".into()),
        args: json!({}),
        acting: None,
        origin: None,
    };
    let request = service
        .submit(
            ApprovalRequest::new(
                "req-frozen",
                scope,
                &call,
                ApprovalPolicy::new([ApproverSelector::group(OPERATOR)]).allow_self_approval(),
            )
            .with_run_id(pid.clone()),
        )
        .await
        .unwrap();

    let audit = InMemoryAuditRecorder::new();
    let realtime = ScriptedRealtimeModel::new(Vec::new())
        .with_model_id("gemini-3.1-flash-live-preview")
        .with_idle_timeout(Duration::from_millis(150));
    let surface = surface(f.dir.path(), realtime, &audit)
        .await
        .with_parked_processes(kernel.processes().clone(), service.clone());
    let listed = surface.parked_processes().await.unwrap();
    assert!(listed.iter().any(|parked| parked.pid == pid && parked.frozen), "{listed:?}");

    let offered = surface.offer_parked(&pid).await.unwrap_err();
    assert_eq!(offered.code, VOICE_FROZEN_CODE);
    let decided = surface.decide_process(&pid, SpokenDecision::Approve).await.unwrap_err();
    assert_eq!(decided.code, VOICE_FROZEN_CODE, "{decided}");
    assert_eq!(records(&audit, VOICE_AUDIT_REFUSED).len(), 1);

    // Nothing woke it: the request is pending, the run asleep, the model untouched (K-I21).
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still = service.get(&request.id).await.unwrap().unwrap();
    assert_eq!(still.state, ApprovalState::Pending);
    assert_eq!(runs.get(&pid).await.unwrap().unwrap().state, RunState::Waiting);
    assert_eq!(f.model.requests().len(), model_calls);

    // Only unfreeze wakes it.
    kernel.unfreeze(AuditActor::subject(SubjectRef::human("ops"))).await.unwrap();
    eventually("the process to resume and complete", || async {
        runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Completed)
    })
    .await;
    kernel.stop().await.unwrap();
}
