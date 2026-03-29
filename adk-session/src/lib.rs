//! # adk-session
#![allow(clippy::result_large_err)]
//!
//! Session management and state persistence for ADK agents.
//!
//! ## Overview
//!
//! This crate provides session and state management:
//!
//! - [`InMemorySessionService`] - Simple in-memory session storage
//! - `VertexAiSessionService` - Vertex AI Session API backend (`vertex-session` feature)
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
pub mod migration;
pub mod service;
pub mod session;
pub mod state;
pub mod state_utils;

#[cfg(feature = "encrypted-session")]
pub mod encrypted;
#[cfg(feature = "encrypted-session")]
pub mod encryption_key;
#[cfg(feature = "firestore")]
pub mod firestore;
#[cfg(feature = "mongodb")]
pub mod mongodb;
#[cfg(feature = "neo4j")]
pub mod neo4j;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
pub mod redis;
#[cfg(feature = "sqlite")]
pub mod sqlite;
#[cfg(feature = "vertex-session")]
pub mod vertex;

pub use event::{Event, EventActions, Events};
pub use inmemory::InMemorySessionService;
pub use service::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, ListRequest, SessionService,
};
pub use session::{KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER, Session};
pub use state::{ReadonlyState, State};
pub use state_utils::{extract_state_deltas, merge_states};

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteSessionService;

/// Deprecated alias — use [`SqliteSessionService`] instead.
#[cfg(feature = "sqlite")]
#[deprecated(since = "0.4.0", note = "renamed to SqliteSessionService")]
pub type DatabaseSessionService = SqliteSessionService;
#[cfg(feature = "encrypted-session")]
pub use encrypted::EncryptedSession;
#[cfg(feature = "encrypted-session")]
pub use encryption_key::EncryptionKey;
#[cfg(feature = "firestore")]
pub use firestore::{
    FirestoreSessionConfig, FirestoreSessionService, app_state_path as firestore_app_state_path,
    event_path as firestore_event_path, session_path as firestore_session_path,
    user_state_path as firestore_user_state_path,
};
#[cfg(feature = "mongodb")]
pub use mongodb::MongoSessionService;
#[cfg(feature = "neo4j")]
pub use neo4j::Neo4jSessionService;
#[cfg(feature = "postgres")]
pub use postgres::PostgresSessionService;
#[cfg(feature = "redis")]
pub use redis::{
    RedisSessionConfig, RedisSessionService, app_state_key, events_key, index_key, session_key,
    user_state_key,
};
#[cfg(feature = "vertex-session")]
pub use vertex::{VertexAiSessionConfig, VertexAiSessionService};
