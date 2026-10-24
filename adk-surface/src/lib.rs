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
//! | `web` | The web home's HTTP API under `/api/surface`, mounted with `ServerBuilder::add_api_routes`, with its OpenAPI document | Beta |
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

pub mod answers;
mod control;
mod engine;
pub mod error;
mod inbox;
mod local;
pub mod model;
mod origin;
mod project;
pub mod rules;
mod source;
#[cfg(feature = "web")]
pub mod web;
pub mod words;

pub use answers::{
    AnswerLog, AnswerRecord, AuditAnswerLog, InMemoryAnswerLog, SURFACE_AUDIT_DECIDE,
};
pub use control::ControlSource;
pub use engine::{
    Command, CommandOutcome, DEFAULT_POLL_INTERVAL, FeedFrame, SURFACE_AUDIT_RESUME_ALL,
    SURFACE_AUDIT_SEND, SURFACE_AUDIT_SET_MODE, SURFACE_AUDIT_STOP_ALL, SurfaceEngine,
    SurfaceEngineBuilder,
};
pub use error::{Result, SurfaceError};
pub use local::{LocalSource, LocalSourceBuilder};
pub use model::{
    AgentRef, AgentRow, AgentStatus, Avatar, Briefing, BriefingLine, CallView, CardStatus, Decided,
    DecidedAnswer, DecisionCard, FeedEntry, Mode, ModeView, Pill, QuietView, Reversibility,
    Speaker, SpendView, StatusLine, ThreadItem, TimelineStep,
};
pub use origin::{Caller, Origin, SurfaceKind, Via};
pub use project::{CALL_TEXT_LIMIT, Place};
pub use rules::{Allowed, Answer, AnswerOption, Risk};
pub use source::{AgentState, AlwaysAllowOffer, RuleCommit, Source, SpendToday};
