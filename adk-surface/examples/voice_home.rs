//! The voice view on Gemini Live or the OpenAI Realtime API (spec 3.3 §5.6).
//!
//! "Chief of staff" is a harness agent in "Ask first", marked for voice, whose `bash` runs in
//! the workspace given. Publishing a release (a command that writes `published.txt`) is high
//! risk; anything else is medium. The models offered are those whose key is set:
//! `GOOGLE_API_KEY` (or `GEMINI_API_KEY`) for Gemini Live, `OPENAI_API_KEY` for the OpenAI
//! Realtime API, read from the environment, the repository's `.env`, or the file
//! `ADK_ENV_FILE` names. Open the page on localhost: browsers give the microphone only to a
//! secure context.
//!
//! ```sh
//! cargo run -p adk-surface --features web,voice-live --example voice_home -- 8794 /tmp/voice-workspace
//! # then open http://localhost:8794/ui/voice
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{
    ApprovalCall, ApprovalPolicy, ApprovalRisk, ApprovalRoute, ApprovalService, ApproverSelector,
    InMemoryApprovalStore, RequestContext, SingleAgentLoader, StoreApprovalService,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig, Pack};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_realtime::RealtimeModel;
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::voice::{FramePolicy, VoiceHub, VoiceModel, VoiceSource};
use adk_surface::{LocalSource, Place, SurfaceEngine};
use async_trait::async_trait;

const APP: &str = "home";
const OWNER: &str = "ana";

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

fn load_env() {
    let env = std::env::var_os("ADK_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    if env.exists() {
        let _ = dotenvy::from_path(&env);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_env();
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8794), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&workspace)?;

    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(route),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let agent = Arc::new(
        HarnessAgent::builder("chief_of_staff")
            .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
            .workspace(adk_devtools::Workspace::new(&workspace).allow_bash(true))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Ask)
            .pack(Pack::Voice)
            .approval_service(Arc::clone(&approvals))
            .build()?,
    );
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
        .build()?;
    let harness = Arc::new(Harness::with_runner(agent.clone(), runner)?);

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
        .build();

    let mut hub = VoiceHub::builder(engine.clone(), Arc::clone(&harness), live);
    let mut offered = 0;
    let gemini = std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"));
    if let Ok(key) = gemini {
        let model = GeminiRealtimeModel::new(
            GeminiLiveBackend::studio(key),
            adk_model::catalog::GEMINI_LIVE_DEFAULT,
        );
        let info = VoiceModel {
            id: "gemini".into(),
            name: "Gemini Live".into(),
            input_rate: 16_000,
            output_rate: 24_000,
        };
        hub = hub
            .model(info, Arc::new(model), FramePolicy::GEMINI)
            .context_window_compression("gemini");
        offered += 1;
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let model = OpenAIRealtimeModel::with_default_model(key);
        let info = VoiceModel {
            id: "openai".into(),
            name: format!("OpenAI Realtime ({})", model.model_id()),
            input_rate: 24_000,
            output_rate: 24_000,
        };
        hub = hub.model(info, Arc::new(model), FramePolicy::OPENAI);
        offered += 1;
    }
    if offered == 0 {
        return Err("set GOOGLE_API_KEY or OPENAI_API_KEY to offer a voice model".into());
    }
    let hub = hub.build();

    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine))
        .add_api_routes(adk_surface::voice::router(hub.clone()))
        .add_root_routes(adk_surface::web::home())
        .add_root_routes(adk_surface::voice::socket_routes(hub))
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("voice home on http://localhost:{port}/ui/voice");
    axum::serve(listener, app).await?;
    Ok(())
}
