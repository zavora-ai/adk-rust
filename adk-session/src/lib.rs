//! # adk-session
#![allow(clippy::result_large_err)]
#![deny(missing_docs)]
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

#[cfg(any(feature = "sqlite", feature = "postgres"))]
/// SQL-backed approval stores for calls held for confirmation.
pub mod approval;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod durable_sql;
/// Event types and the [`Events`] trait for accessing session event history.
pub mod event;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
/// SQL-backed governance state stores for a kill switch shared by every replica.
pub mod governance_state;
/// In-memory session backend for testing and lightweight use cases.
pub mod inmemory;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
/// SQL-backed lease stores with fencing epochs.
pub mod lease;
/// Schema migration utilities for database-backed session stores.
pub mod migration;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
/// SQL-backed durable run queues.
pub mod run_store;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
/// SQL-backed schedule stores.
pub mod schedule;
/// Session service trait and request/response types.
pub mod service;
/// The [`Session`] trait and state key prefix constants.
pub mod session;
/// State access traits ([`State`] and [`ReadonlyState`]).
pub mod state;
/// Shared utilities for extracting and merging state deltas across backends.
pub mod state_utils;

#[cfg(feature = "encrypted-session")]
/// AES-256-GCM encrypted session wrapper with key rotation.
pub mod encrypted;
#[cfg(feature = "encrypted-session")]
/// Encryption key management for encrypted sessions.
pub mod encryption_key;
#[cfg(feature = "firestore")]
/// Google Cloud Firestore session backend.
pub mod firestore;
#[cfg(feature = "firestore")]
/// Firestore-backed spend ledger, action ledger, and approval store.
pub mod firestore_stores;
#[cfg(feature = "mongodb")]
/// MongoDB session backend.
pub mod mongodb;
#[cfg(feature = "mongodb")]
/// MongoDB-backed spend ledger, action ledger, and approval store.
pub mod mongodb_stores;
#[cfg(feature = "neo4j")]
/// Neo4j graph database session backend.
pub mod neo4j;
#[cfg(feature = "neo4j")]
/// Neo4j-backed spend ledger, action ledger, and approval store.
pub mod neo4j_stores;
#[cfg(feature = "postgres")]
/// PostgreSQL session backend.
pub mod postgres;
#[cfg(feature = "postgres")]
/// PostgreSQL-backed action ledger for non-idempotent tool calls.
pub mod postgres_action_ledger;
#[cfg(feature = "redis")]
/// Redis session backend with TTL support.
pub mod redis;
#[cfg(feature = "redis")]
/// Redis-backed spend ledger, action ledger, and approval store.
pub mod redis_stores;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
/// SQL-backed spend ledgers shared by model calls and payments.
pub mod spend;
#[cfg(feature = "sqlite")]
/// SQLite session backend.
pub mod sqlite;
#[cfg(feature = "sqlite")]
/// SQLite-backed action ledger for non-idempotent tool calls.
pub mod sqlite_action_ledger;
#[cfg(feature = "vertex-session")]
/// Vertex AI Session API backend.
pub mod vertex;

pub use event::{Event, EventActions, Events};
pub use inmemory::InMemorySessionService;
pub use service::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, ListRequest, SessionService,
};
pub use session::{KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER, Session};
pub use state::{ReadonlyState, State};
pub use state_utils::{extract_state_deltas, merge_states};

#[cfg(feature = "postgres")]
pub use approval::PostgresApprovalStore;
#[cfg(feature = "sqlite")]
pub use approval::SqliteApprovalStore;
#[cfg(feature = "firestore")]
pub use firestore_stores::{FirestoreActionLedger, FirestoreApprovalStore, FirestoreSpendLedger};
#[cfg(feature = "postgres")]
pub use governance_state::PostgresGovernanceStateStore;
#[cfg(feature = "sqlite")]
pub use governance_state::SqliteGovernanceStateStore;
#[cfg(feature = "postgres")]
pub use lease::PostgresLeaseStore;
#[cfg(feature = "sqlite")]
pub use lease::SqliteLeaseStore;
#[cfg(feature = "mongodb")]
pub use mongodb_stores::{MongoActionLedger, MongoApprovalStore, MongoSpendLedger};
#[cfg(feature = "neo4j")]
pub use neo4j_stores::{Neo4jActionLedger, Neo4jApprovalStore, Neo4jSpendLedger};
#[cfg(feature = "postgres")]
pub use postgres_action_ledger::PostgresActionLedger;
#[cfg(feature = "redis")]
pub use redis_stores::{RedisActionLedger, RedisApprovalStore, RedisSpendLedger};
#[cfg(feature = "postgres")]
pub use run_store::PostgresRunStore;
#[cfg(feature = "sqlite")]
pub use run_store::SqliteRunStore;
#[cfg(feature = "postgres")]
pub use schedule::PostgresScheduleStore;
#[cfg(feature = "sqlite")]
pub use schedule::SqliteScheduleStore;
#[cfg(feature = "postgres")]
pub use spend::PostgresSpendLedger;
#[cfg(feature = "sqlite")]
pub use spend::SqliteSpendLedger;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteSessionService;
#[cfg(feature = "sqlite")]
pub use sqlite_action_ledger::SqliteActionLedger;

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
