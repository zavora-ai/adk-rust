//! The web home on a served harness (spec 3.3 §5.1, §10 S1): the engine `adk-surface` serves
//! at `/api/surface` and the page at `/ui/`.

use std::sync::Arc;

use adk_core::{ApprovalService, Result};
use adk_session::SessionService;
use adk_surface::{LocalSource, Source, SurfaceEngine};

/// What the served harness's engine records: how each answer was given, and every command,
/// on the signed audit log, so "You said yes in Telegram" survives a restart (spec 3.3 §11).
#[derive(Clone)]
pub(crate) struct SurfaceRecords {
    pub(crate) trail: adk_core::audit_log::AuditTrail,
    pub(crate) answers: Arc<dyn adk_surface::AnswerLog>,
}

/// The source over a served harness (spec 3.3 §6): its sessions and approval inbox, its
/// modes and standing rules, and, when resident, the kernel that runs the turns.
pub(crate) fn harness_source(
    app_name: &str,
    harness: Arc<adk_harness::Harness>,
    sessions: Arc<dyn SessionService>,
    approvals: Option<Arc<dyn ApprovalService>>,
    kernel: Option<adk_kernel::Kernel>,
) -> Result<Arc<dyn Source>> {
    let mut source = LocalSource::builder(app_name).sessions(sessions).harness(harness);
    if let Some(approvals) = approvals {
        source = source.approvals(approvals);
    }
    if let Some(kernel) = kernel {
        source = source.kernel(kernel, app_name);
    }
    Ok(Arc::new(source.build()?))
}

/// The engine over a served harness's `source` (spec 3.3 §6), with what a feature adds to its
/// builder — card hints, granted uses (spec 3.3 §5.7, §7.1).
pub(crate) fn harness_engine(
    app_name: &str,
    source: Arc<dyn Source>,
    records: Option<SurfaceRecords>,
    phone: bool,
    extend: impl FnOnce(adk_surface::SurfaceEngineBuilder) -> adk_surface::SurfaceEngineBuilder,
) -> SurfaceEngine {
    // Decisions also wait in the terminal: `adk-rust approvals`, `attach`. Stop all and Resume
    // all freeze and unfreeze every run the server makes, as the kernel's own freeze route
    // does, which needs the operator too (spec 3.3 §11 S-I10).
    let mut engine = SurfaceEngine::builder(source)
        .places(
            [adk_surface::Place::terminal()]
                .into_iter()
                .chain(phone.then(adk_surface::Place::phone)),
        )
        .agent_name(app_name, "ADK-Rust")
        .stops_need(crate::harness::OPERATOR_SCOPE);
    if let Some(records) = records {
        engine = engine.audit(records.trail).answers(records.answers);
    }
    extend(engine).build()
}

/// A realtime model the served harness talks on (spec 3.3 §5.6).
#[cfg(feature = "voice")]
#[derive(Clone)]
pub(crate) struct VoiceOffer {
    pub(crate) info: adk_surface::voice::VoiceModel,
    pub(crate) model: adk_realtime::BoxedModel,
    pub(crate) frames: adk_surface::voice::FramePolicy,
    /// Gemini Live context window compression, which a conversation sending frames needs.
    pub(crate) compress: bool,
}

/// Voice on the served home (spec 3.3 §5.6), when `harness` is marked `Pack::Voice` and a
/// model is offered: `source` wrapped so a decision on a call a voice conversation holds
/// resumes it there, and where the hub registers its conversations.
#[cfg(feature = "voice")]
pub(crate) fn voice_source(
    source: Arc<dyn Source>,
    harness: &adk_harness::Harness,
    offers: &[VoiceOffer],
) -> (Arc<dyn Source>, Option<adk_surface::voice::LiveVoice>) {
    let marked = harness.agent().info().packs.iter().any(|pack| pack == "voice");
    if !marked || offers.is_empty() {
        return (source, None);
    }
    let voice = adk_surface::voice::VoiceSource::new(source);
    let live = voice.live();
    (Arc::new(voice), Some(live))
}

/// The hub of the served harness's voice conversations, on `offers`, the first by default, with
/// what a feature adds to its builder: screen actions said aloud (spec 3.3 §5.6).
#[cfg(feature = "voice")]
pub(crate) fn voice_hub(
    engine: SurfaceEngine,
    harness: Arc<adk_harness::Harness>,
    live: adk_surface::voice::LiveVoice,
    offers: Vec<VoiceOffer>,
    extend: impl FnOnce(adk_surface::voice::VoiceHubBuilder) -> adk_surface::voice::VoiceHubBuilder,
) -> adk_surface::voice::VoiceHub {
    let mut hub =
        extend(adk_surface::voice::VoiceHub::builder(engine, harness, live).agent_name("ADK-Rust"));
    for offer in offers {
        let id = offer.info.id.clone();
        hub = hub.model(offer.info, offer.model, offer.frames);
        if offer.compress {
            hub = hub.context_window_compression(&id);
        }
    }
    hub.build()
}
