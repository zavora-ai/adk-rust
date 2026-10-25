//! The voice view over a scripted realtime model (spec 3.3 §5.6): what
//! `adk-surface/web/e2e/voice.spec.ts` opens.
//!
//! "Chief of staff" is a harness agent in "Ask first", marked for voice. Its realtime model is
//! a puppet the test drives over plain HTTP routes, standing in for the person's speech and the
//! model's turns; everything else is real — the harness and its governed `bash`, the approval
//! inbox, the session store, the engine, the voice socket and the page. The inbox makes
//! publishing the release high risk; "send the invoice" is medium. A held call of the Finance
//! agent waits in the same inbox from the start.
//!
//! | Route | What the puppet does |
//! |---|---|
//! | `POST /puppet/say {"text"}` | The person speaks, and is transcribed as `text` |
//! | `POST /puppet/call {"command"}` | The model runs `command` in `bash` |
//! | `POST /puppet/speak {"text", "millis"}` | The model speaks `text` for `millis` of audio |
//! | `GET /puppet/counts` | Audio chunks and frames the model got, the invoice sends and the release publishes |
//!
//! ```sh
//! cargo run -p adk-surface --features web,voice --example scripted_voice_home -- 8792 /tmp/voice-workspace
//! ```

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{
    ActingContext, ApprovalCall, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalRoute,
    ApprovalScope, ApprovalService, ApproverSelector, InMemoryApprovalStore, RequestContext,
    SingleAgentLoader, StoreApprovalService, SubjectKind, ToolConfirmationRequest,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig, Pack};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_realtime::audio::{AudioChunk, AudioFormat};
use adk_realtime::config::RealtimeConfig;
use adk_realtime::scripted;
use adk_realtime::session::{BoxedSession, ContextMutationOutcome, RealtimeSession};
use adk_realtime::{ClientEvent, RealtimeModel, ServerEvent, ToolResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::voice::{FramePolicy, VoiceHub, VoiceModel, VoiceSource};
use adk_surface::{LocalSource, Place, SurfaceEngine};
use async_trait::async_trait;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Notify, mpsc};

const APP: &str = "home";
const OWNER: &str = "ana";
/// What the model runs to send the invoice: one byte per send.
const SEND: &str = "printf x >> sent.txt";

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

/// A realtime model the test drives: whatever it is told to emit, every session emits.
#[derive(Clone, Default)]
struct Puppet {
    state: Arc<PuppetState>,
}

#[derive(Default)]
struct PuppetState {
    sessions: Mutex<Vec<mpsc::UnboundedSender<ServerEvent>>>,
    audio: AtomicUsize,
    frames: AtomicUsize,
    calls: AtomicUsize,
}

impl Puppet {
    fn emit(&self, events: Vec<ServerEvent>) {
        let mut sessions = self.state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|session| events.iter().all(|event| session.send(event.clone()).is_ok()));
    }
}

#[async_trait]
impl RealtimeModel for Puppet {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn model_id(&self) -> &str {
        "puppet"
    }

    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_16khz()]
    }

    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz()]
    }

    fn available_voices(&self) -> Vec<&str> {
        Vec::new()
    }

    async fn connect(&self, _config: RealtimeConfig) -> adk_realtime::Result<BoxedSession> {
        let (sender, events) = mpsc::unbounded_channel();
        self.state.sessions.lock().unwrap_or_else(|e| e.into_inner()).push(sender);
        Ok(Box::new(PuppetSession {
            id: format!("puppet-{}", uuid::Uuid::new_v4().simple()),
            state: Arc::clone(&self.state),
            events: tokio::sync::Mutex::new(events),
            closed: AtomicBool::new(false),
            close: Notify::new(),
        }))
    }
}

struct PuppetSession {
    id: String,
    state: Arc<PuppetState>,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<ServerEvent>>,
    closed: AtomicBool,
    close: Notify,
}

#[async_trait]
impl RealtimeSession for PuppetSession {
    fn session_id(&self) -> &str {
        &self.id
    }

    fn is_connected(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    async fn send_audio(&self, _audio: &AudioChunk) -> adk_realtime::Result<()> {
        self.state.audio.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn send_audio_base64(&self, _audio: &str) -> adk_realtime::Result<()> {
        self.state.audio.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn send_text(&self, _text: &str) -> adk_realtime::Result<()> {
        Ok(())
    }

    async fn send_video_frame(&self, _mime_type: &str, _data: &str) -> adk_realtime::Result<()> {
        self.state.frames.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn send_tool_response(&self, _response: ToolResponse) -> adk_realtime::Result<()> {
        Ok(())
    }

    async fn commit_audio(&self) -> adk_realtime::Result<()> {
        Ok(())
    }

    async fn clear_audio(&self) -> adk_realtime::Result<()> {
        Ok(())
    }

    async fn create_response(&self) -> adk_realtime::Result<()> {
        Ok(())
    }

    async fn interrupt(&self) -> adk_realtime::Result<()> {
        Ok(())
    }

    async fn send_event(&self, _event: ClientEvent) -> adk_realtime::Result<()> {
        Ok(())
    }

    async fn next_event(&self) -> Option<adk_realtime::Result<ServerEvent>> {
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        let mut events = self.events.lock().await;
        tokio::select! {
            event = events.recv() => event.map(Ok),
            () = self.close.notified() => None,
        }
    }

    fn events(&self) -> Pin<Box<dyn Stream<Item = adk_realtime::Result<ServerEvent>> + Send + '_>> {
        Box::pin(futures::stream::unfold(self, |session| async move {
            session.next_event().await.map(|event| (event, session))
        }))
    }

    async fn close(&self) -> adk_realtime::Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        self.close.notify_waiters();
        Ok(())
    }

    async fn mutate_context(
        &self,
        _config: RealtimeConfig,
    ) -> adk_realtime::Result<ContextMutationOutcome> {
        Ok(ContextMutationOutcome::Applied)
    }
}

#[derive(Clone)]
struct Drive {
    puppet: Puppet,
    workspace: PathBuf,
}

#[derive(Deserialize)]
struct Say {
    text: String,
}

#[derive(Deserialize)]
struct Call {
    command: String,
}

#[derive(Deserialize)]
struct Speak {
    text: String,
    #[serde(default)]
    millis: usize,
}

fn puppet_routes(drive: Drive) -> Router {
    Router::new()
        .route(
            "/puppet/say",
            post(|State(drive): State<Drive>, Json(say): Json<Say>| async move {
                drive.puppet.emit(vec![
                    ServerEvent::SpeechStarted { event_id: "speech".into(), audio_start_ms: 0 },
                    scripted::input_transcript(&say.text),
                ]);
                Json(json!({}))
            }),
        )
        .route(
            "/puppet/call",
            post(|State(drive): State<Drive>, Json(call): Json<Call>| async move {
                let n = drive.puppet.state.calls.fetch_add(1, Ordering::SeqCst);
                drive.puppet.emit(scripted::calls(
                    &format!("call-{n}"),
                    "bash",
                    json!({ "command": call.command }),
                ));
                Json(json!({}))
            }),
        )
        .route(
            "/puppet/speak",
            post(|State(drive): State<Drive>, Json(speak): Json<Speak>| async move {
                let audio = ServerEvent::AudioDelta {
                    event_id: "audio".into(),
                    response_id: "resp-puppet".into(),
                    item_id: "item-puppet".into(),
                    output_index: 0,
                    content_index: 0,
                    delta: vec![0; speak.millis * 48],
                };
                drive.puppet.emit(
                    [vec![scripted::response_created(), audio], scripted::spoken(&speak.text)]
                        .concat(),
                );
                Json(json!({}))
            }),
        )
        .route(
            "/puppet/counts",
            get(|State(drive): State<Drive>| async move {
                let runs = |file: &str| {
                    std::fs::read(drive.workspace.join(file)).map_or(0, |bytes| bytes.len())
                };
                Json(json!({
                    "audio": drive.puppet.state.audio.load(Ordering::SeqCst),
                    "frames": drive.puppet.state.frames.load(Ordering::SeqCst),
                    "sent": runs("sent.txt"),
                    "published": runs("published.txt"),
                }))
            }),
        )
        .with_state(drive)
}

/// The inbox's router: publishing the release is high risk, everything else medium.
fn route(call: &ApprovalCall) -> Option<ApprovalRoute> {
    let command = call.request.args["command"].as_str().unwrap_or_default();
    let risk =
        if command.contains("published") { ApprovalRisk::High } else { ApprovalRisk::Medium };
    Some(
        ApprovalRoute::new(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        )
        .with_risk(risk),
    )
}

/// What a call will do, in plain words.
fn heading(request: &ApprovalRequest) -> String {
    let command = request.args_redacted["command"].as_str().unwrap_or_default();
    if request.tool_name == "send_email" || command == SEND {
        "Send the October invoice to [CLIENT]".to_string()
    } else if command.contains("published") {
        "Publish the release".to_string()
    } else {
        format!("Run {}", request.tool_name)
    }
}

/// Finance's held call, waiting in the inbox from the start.
async fn finance_waits(
    sessions: &Arc<dyn SessionService>,
    approvals: &Arc<dyn ApprovalService>,
) -> Result<(), Box<dyn std::error::Error>> {
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: Some("finance".into()),
            state: Default::default(),
        })
        .await?;
    let call = ToolConfirmationRequest {
        tool_name: "send_email".into(),
        function_call_id: Some("finance-1".into()),
        args: json!({ "to": "[CLIENT]", "subject": "October invoice" }),
        origin: None,
        acting: None,
    };
    let mut request = ApprovalRequest::new(
        "apr-finance",
        ApprovalScope::new(APP, OWNER, "finance"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .with_risk(ApprovalRisk::Medium);
    request.acting = Some(ActingContext::derived(APP, "finance", OWNER, SubjectKind::Human));
    approvals.submit(request).await?;
    Ok(())
}

fn harness(
    workspace: &Path,
    approvals: &Arc<dyn ApprovalService>,
    sessions: &Arc<dyn SessionService>,
) -> Result<Arc<Harness>, Box<dyn std::error::Error>> {
    let agent = Arc::new(
        HarnessAgent::builder("chief_of_staff")
            .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
            .workspace(adk_devtools::Workspace::new(workspace).allow_bash(true))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Ask)
            .pack(Pack::Voice)
            .approval_service(Arc::clone(approvals))
            .build()?,
    );
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
        .build()?;
    Ok(Arc::new(Harness::with_runner(agent, runner)?))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8792), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&workspace)?;

    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(route),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let harness = harness(&workspace, &approvals, &sessions)?;
    finance_waits(&sessions, &approvals).await?;

    let local = LocalSource::builder(APP)
        .sessions(sessions.clone())
        .approvals(approvals)
        .harness(Arc::clone(&harness))
        .build()?;
    let source = VoiceSource::new(Arc::new(local));
    let live = source.live();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web()])
        .agent_name("chief_of_staff", "Chief of staff")
        .agent_name("finance", "Finance")
        .heading(heading)
        .build();
    let puppet = Puppet::default();
    let hub = VoiceHub::builder(engine.clone(), Arc::clone(&harness), live)
        .model(
            VoiceModel {
                id: "gemini".into(),
                name: "Gemini Live".into(),
                input_rate: 16_000,
                output_rate: 24_000,
            },
            Arc::new(puppet.clone()),
            FramePolicy::GEMINI,
        )
        .build();

    let config =
        ServerConfig::new(Arc::new(SingleAgentLoader::new(harness.agent().clone())), sessions)
            .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine))
        .add_api_routes(adk_surface::voice::router(hub.clone()))
        .add_root_routes(adk_surface::web::home())
        .add_root_routes(adk_surface::voice::socket_routes(hub))
        .add_root_routes(puppet_routes(Drive { puppet, workspace }))
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("scripted voice home on http://127.0.0.1:{port}/ui/voice");
    axum::serve(listener, app).await?;
    Ok::<(), Box<dyn std::error::Error>>(())
}
