//! # adk-session
//!
//! Session management and state persistence for ADK agents.
//!
//! ## Overview
//!
//! This crate provides session and state management:
//!
//! - [`InMemorySessionService`] - Simple in-memory session storage
//! - [`Session`] - Conversation session with state and events
//! - [`State`] - Key-value state with typed prefixes
//! - [`SessionService`] - Trait for custom session backends
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_session::InMemorySessionService;
//!
//! let service = InMemorySessionService::new();
//!
//! // Sessions are created and managed by the Runner
//! // State is accessed via the session
//! ```
//!
//! ## State Prefixes
//!
//! ADK uses prefixes to organize state:
//!
//! | Prefix | Constant | Purpose |
//! |--------|----------|---------|
//! | `user:` | [`KEY_PREFIX_USER`] | User preferences |
//! | `app:` | [`KEY_PREFIX_APP`] | Application state |
//! | `temp:` | [`KEY_PREFIX_TEMP`] | Temporary data |

pub mod event;
pub mod inmemory;
pub mod service;
pub mod session;
pub mod state;

#[cfg(feature = "database")]
pub mod database;

pub use event::{Event, EventActions, Events};
pub use inmemory::InMemorySessionService;
pub use service::{CreateRequest, DeleteRequest, GetRequest, ListRequest, SessionService};
pub use session::{Session, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER};
pub use state::{ReadonlyState, State};

#[cfg(feature = "database")]
pub use database::DatabaseSessionService;
