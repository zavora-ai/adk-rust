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
//! ## Surfaces
//!
//! | Feature | Surface | Stability |
//! |---|---|---|
//! | `web` | The web home: its HTTP API under `/api/surface`, mounted with `ServerBuilder::add_api_routes`, with its OpenAPI document, and its page at `/ui/`, mounted with `ServerBuilder::add_root_routes` | Beta |
//! | `channels` | Chat: the Telegram adapter over the kernel's channel contract, `channels::TelegramAdapter` (spec 3.3 §5.3) | Beta |
//! | `whatsapp`, `slack`, `discord`, `matrix` | Chat on WhatsApp, Slack, Discord and Matrix over the same contract, `channels::WhatsAppAdapter`, `SlackAdapter`, `DiscordAdapter`, `MatrixAdapter` (spec 3.5 §10 X12); each implies `channels` | Experimental |
//! | `mobile` | The phone: the web home installed on a phone, Web Push from the person's own kernel, pairing QR codes and "Confirm it's you", `mobile::router` (spec 3.3 §5.4); implies `web` | Beta |
//! | `voice` | Voice and video: spoken "allow once" and "deny" through the engine with a cancel window, the camera and the screen as input while their switches are on; with `web`, the voice view's socket (`voice::router`, `voice::socket_routes`) (spec 3.3 §5.6) | Beta |
//! | `website` | The website's views on the web home (spec 3.5 §8.2): the web editor, onboarding, the charter cards, the site view and the weekly review over `adk-website`'s desk, `web::website::router`; implies `web` | Experimental |
//! | `verb-ui` | Screen actions people and agents share (spec 3.3 §2 item 7, §6.1): snoozing something in Coming up from the home, the terminal, voice and an agent, always through the governed path, `verbs::VerbRegistry`; with `web`, `web::verbs::router` | Experimental |
//! | `workflow` | The workflow observer (spec 3.3 §12 developer track): a graph agent's runs, steps and forks, card hints for its governed nodes, `workflow::Workflows`; with `web`, the Runs tab's routes, `web::workflow::router` | Beta |
//! | `genui` | Agent-drawn UI (spec 3.3 §6.1, §7.4 rule 5): the A2UI v0.9.1 model, the `forms` 1.0 catalog, the validator that keeps approval controls and secrets out of drawings, the render tools agents draw with (`genui::DrawToolset`), drawings in the thread and a drawn form's Send, which asks the person first (`Command::SubmitDrawn`), and with `web` the capabilities route (`web::genui::router`) | Beta |
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
#[cfg(feature = "channels")]
pub mod channels;
mod control;
mod engine;
pub mod error;
pub mod genui;
mod hints;
mod inbox;
mod local;
#[cfg(feature = "mobile")]
pub mod mobile;
pub mod model;
mod origin;
mod project;
pub mod rules;
mod source;
#[cfg(feature = "tui")]
pub mod tui;
#[cfg(feature = "verb-ui")]
pub mod verbs;
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
    AgentPreviewRequest, AreaChoice, CardLine, ColorChoice, EndedWork, FileUse, ModelView,
    RemovedAgent, RemovedThread, ShellUse, UsesDraft, WebUse,
};
pub use answers::{
    AnswerLog, AnswerRecord, AuditAnswerLog, InMemoryAnswerLog, RECORD_LINES, RecordAnswerLog,
    SURFACE_AUDIT_DECIDE,
};
pub use control::ControlSource;
pub use engine::SURFACE_AUDIT_ANSWER;
pub use engine::SURFACE_AUDIT_DRAWN_SUBMITTED;
pub use engine::{
    Command, CommandOutcome, DEFAULT_POLL_INTERVAL, FeedFrame, SURFACE_AUDIT_RESUME_ALL,
    SURFACE_AUDIT_SEND, SURFACE_AUDIT_SET_MODE, SURFACE_AUDIT_STOP_ALL, SurfaceEngine,
    SurfaceEngineBuilder,
};
pub use error::{Result, SurfaceError};
pub use hints::{CardHint, CardHints};
pub use local::{LocalSource, LocalSourceBuilder};
pub use model::CardDetail;
pub use model::{
    AgentRef, AgentRow, AgentStatus, Avatar, Briefing, BriefingLine, CallView, CardStatus, Decided,
    DecidedAnswer, DecisionCard, FeedEntry, Mode, ModeView, Pill, QuietView, Reversibility,
    Speaker, SpendView, StatusLine, ThreadItem, TimelineStep,
};
pub use origin::{Caller, Origin, SurfaceKind, Via};
pub use project::{CALL_TEXT_LIMIT, Place};
pub use rules::{Allowed, Answer, AnswerOption, Risk};
pub use source::{AgentState, AlwaysAllowOffer, RuleCommit, Source, SpendToday};
