//! # adk-surface
//!
//! Agent surfaces for ADK-Rust (spec 3.3): one engine under every window a person opens onto
//! their agents — the web home, the terminal, chat, the phone, the desktop tray, voice.
//! Nothing lives in a surface: a decision is made once, from anywhere, and every other window
//! shows who made it.
//!
//! ## The engine
//!
//! [`SurfaceEngine`] is always compiled. It reads a [`Source`] — an in-process kernel,
//! harness or runner ([`LocalSource`]), or a kernel's control plane ([`ControlSource`]) — and
//! gives every surface the same three things (spec 3.3 §6):
//!
//! | Part | What | Where |
//! |---|---|---|
//! | Feed | A session's events numbered by their position in the session store, as a snapshot then a live tail, resumable after the last `seq` seen | [`SurfaceEngine::feed`], [`SurfaceEngine::thread`] |
//! | Projection | Decision cards (§7.1), thread items, agent rows, the status line and the "While you were away" briefing, in the words of §8 | [`model`], [`words`] |
//! | Commands | Decide at the version seen, send, queue, stop all, resume all, change the mode — each with its caller's subject and [`Origin`], audited | [`SurfaceEngine::execute`], [`Command`] |
//!
//! What each surface may decide (§7.2) and the rules on every surface (§7.4) are enforced by
//! the engine in [`rules`], not left to renderers.
//!
//! The live camera surface (spec 3.7 §8.2) is [`camera`]: a camera source's facts, the hub that
//! drops every frame of a camera switched off or paused by Stop all, and the view model; its
//! routes are served with `web` once the engine is built with [`SurfaceEngineBuilder::cameras`],
//! and with `vision-camera` its cameras run on adk-vision's frame pipeline.
//!
//! ## Surfaces
//!
//! | Feature | Surface | Stability |
//! |---|---|---|
//! | `web` | The web home: its HTTP API under `/api/surface`, mounted with `ServerBuilder::add_api_routes`, with its OpenAPI document, and its page at `/ui/`, mounted with `ServerBuilder::add_root_routes` | Beta |
//! | `tui` | The terminal (spec 3.3 §5.2): full screen, inline and JSON lines on the engine, decisions answered by key, light and dark; the `adk-rust` binary's `tui` feature | Beta |
//! | `channels` | Chat: the Telegram adapter over the kernel's channel contract, `channels::TelegramAdapter` (spec 3.3 §5.3) | Beta |
//! | `whatsapp`, `slack`, `discord`, `matrix`, `teams` | Chat on WhatsApp, Slack, Discord, Matrix and Microsoft Teams over the same contract, `channels::WhatsAppAdapter`, `SlackAdapter`, `DiscordAdapter`, `MatrixAdapter`, `TeamsAdapter` (spec 3.5 §10 X12); each implies `channels` | Experimental |
//! | `mobile` | The phone: the web home installed on a phone, Web Push from the person's own kernel, pairing QR codes and "Confirm it's you", `mobile::router` (spec 3.3 §5.4); implies `web` | Beta |
//! | `voice` | Voice and video: spoken "allow once" and "deny" through the engine with a cancel window, the camera and the screen as input while their switches are on; with `web`, the voice view's socket (`voice::router`, `voice::socket_routes`) (spec 3.3 §5.6) | Beta |
//! | `voice-live` | `voice` with Gemini Live and the OpenAI Realtime API compiled in, for the live voice test and the `voice_home` example; a host passes its own provider models | Beta |
//! | `vision-camera` | The live camera surface on adk-vision's frame pipeline (spec 3.7 §7.13, §8.2): `camera::PipelineCameras` runs each camera through `StreamPipeline` and reports what it measured; the voice screen's camera through `SurfaceCameraSource` | Experimental |
//! | `website` | The website's views on the web home (spec 3.5 §8.2): the web editor, onboarding, the charter cards, the site view and the weekly review over `adk-website`'s desk, `web::website::router`; implies `web` | Experimental |
//! | `verb-ui` | Screen actions people and agents share (spec 3.3 §2 item 7, §6.1): snoozing something in Coming up from the home, the terminal, voice and an agent, always through the governed path, `verbs::VerbRegistry`; with `web`, `web::verbs::router` | Experimental |
//! | `workflow` | The workflow observer (spec 3.3 §12 developer track): a graph agent's runs, steps and forks, card hints for its governed nodes, `workflow::Workflows`; with `web`, the Runs tab's routes, `web::workflow::router` | Beta |
//! | `genui` | Agent-drawn UI (spec 3.3 §6.1, §7.4 rule 5): the A2UI v0.9.1 model, the `forms` 1.0 catalog, the validator that keeps approval controls and secrets out of drawings, the render tools agents draw with (`genui::DrawToolset`), drawings in the thread and a drawn form's Send, which asks the person first (`Command::SubmitDrawn`), and with `web` the capabilities route (`web::genui::router`) | Beta |
//! | `genui-apps` | Apps from MCP servers in a thread (spec 3.3 §6.1): framed sandboxed on their own origin, their tool calls held for the person, never able to approve anything, `genui::McpAppsHost`; implies `genui` and `web` | Beta |
//!
//! ## Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_core::{InMemoryApprovalStore, StoreApprovalService};
//! use adk_session::InMemorySessionService;
//! use adk_surface::{Caller, LocalSource, Origin, SurfaceEngine, SurfaceKind};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_surface::Result<()> {
//! let source = LocalSource::builder("adk-rust")
//!     .sessions(Arc::new(InMemorySessionService::new()))
//!     .approvals(Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))))
//!     .build()?;
//! let engine = SurfaceEngine::builder(Arc::new(source)).build();
//!
//! let me = Caller::human("ana", SurfaceKind::Phone, Origin::Touch);
//! let status = engine.status(&me, None).await?;
//! assert_eq!(status.decisions, 0);
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod agents;
pub mod answers;
pub mod camera;
#[cfg(feature = "channels")]
pub mod channels;
mod control;
mod engine;
pub mod error;
mod evidence;
pub mod genui;
mod hints;
pub mod images;
mod inbox;
mod local;
#[cfg(feature = "mobile")]
pub mod mobile;
pub mod model;
mod origin;
mod project;
pub mod rules;
mod source;
pub mod suggest;
#[cfg(feature = "tui")]
pub mod tui;
#[cfg(feature = "verb-ui")]
pub mod verbs;
pub mod viewer;
#[cfg(any(feature = "voice", feature = "web"))]
pub mod voice;
#[cfg(feature = "web")]
pub mod web;
pub mod words;
#[cfg(feature = "workflow")]
pub mod workflow;

pub use agents::GrantedUse;
pub use agents::{
    AgentCardView, AgentChoices, AgentColor, AgentDetail, AgentDraft, AgentLife, AgentPreview,
    AgentPreviewRequest, AgentThreads, AreaChoice, CardLine, ColorChoice, EndedWork, FileUse,
    ModelView, RemovedAgent, RemovedThread, ShellUse, ThreadSummary, UsesDraft, WebUse,
};
pub use answers::{
    AnswerLog, AnswerRecord, AnswerSummary, AuditAnswerLog, InMemoryAnswerLog, RECORD_LINES,
    RecordAnswerLog, SURFACE_AUDIT_DECIDE, SurfaceCount,
};
pub use control::ControlSource;
pub use engine::SURFACE_AUDIT_ANSWER;
pub use engine::SURFACE_AUDIT_IMAGE_CHANGE;
pub use engine::SURFACE_AUDIT_VISION_EXAMPLE;
pub use engine::{
    Command, CommandOutcome, DEFAULT_POLL_INTERVAL, FeedFrame, SURFACE_AUDIT_RESUME_ALL,
    SURFACE_AUDIT_SEND, SURFACE_AUDIT_SET_MODE, SURFACE_AUDIT_STEER, SURFACE_AUDIT_STOP_ALL,
    SURFACE_AUDIT_STOP_TURN, SurfaceEngine, SurfaceEngineBuilder,
};
pub use engine::{
    SURFACE_AUDIT_APP_CALL_AUTO_APPROVED, SURFACE_AUDIT_DRAWN_SEALED, SURFACE_AUDIT_DRAWN_SUBMITTED,
};
pub use error::{Result, SurfaceError};
pub use evidence::{
    CardMedia, CoverClass, CoveredLine, EVIDENCE_RETENTION_DAYS, EvidenceBytes, EvidenceImage,
    EvidenceKept, EvidenceRegion, GoesTo, ImageChange, ImageChanged, ImageFacts, MediaFacts,
    MediaNotice, MediaNoticeKind, PurgedView, short_digest,
};
pub use hints::{CardHint, CardHints};
pub use images::{MAX_IMAGE_BYTES, MAX_IMAGES, MessageImage};
pub use local::{LocalSource, LocalSourceBuilder};
pub use model::CardDetail;
pub use model::{
    AgentRef, AgentRow, AgentStatus, Avatar, Briefing, BriefingLine, CallView, CardStatus, Decided,
    DecidedAnswer, DecisionCard, ElicitationView, ElicitedField, ElicitedKind, ElicitedOption,
    FeedEntry, Mode, ModeView, ParkedView, Pill, QuietView, Reversibility, Speaker, SpendView,
    StatusLine, ThreadItem, TimelineStep,
};
pub use model::{CommandRun, ImageData, OutputLine, RunStop, ShellPlace, ShellView};
pub use origin::{Caller, Origin, SurfaceKind, Via};
pub use project::RUN_LINES;
pub use project::{CALL_TEXT_LIMIT, Place};
pub use rules::{Allowed, Answer, AnswerOption, Risk};
pub use source::{AgentState, AlwaysAllowOffer, RuleCommit, Source, SpendToday, Steered, Steering};
