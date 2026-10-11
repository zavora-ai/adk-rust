//! Signed, verifiable audit log (`signed-audit` feature).
//!
//! | Layer | Mechanism |
//! |-------|-----------|
//! | Record | [`AuditRecord`](adk_core::audit_log::AuditRecord), canonicalized with RFC 8785 ([`canonicalize`]) |
//! | Chain | Each record's `prev_hash` is the previous record's leaf hash |
//! | Tree | RFC 9162 Merkle tree over leaf hashes `SHA-256(0x00 ‖ record)`, with inclusion and consistency proofs ([`merkle`]) |
//! | Checkpoint | C2SP tlog checkpoint in a signed note, Ed25519 by default ([`AuditSigner`], [`Ed25519Signer`]) |
//! | Anchor | Optional publication of checkpoints outside the log ([`CheckpointAnchor`], [`FileCheckpointAnchor`]) |
//! | Store | [`AuditLogStore`](adk_core::audit_log::AuditLogStore): in-memory in `adk-core`, SQLite and PostgreSQL in `adk-session` |
//! | Verify | [`verify`] over an export, also `adk-rust audit verify` |
//!
//! Checkpoint notes use the same format and key encoding as Go's
//! `golang.org/x/mod/sumdb/note`, so standard signed-note and witness tooling can
//! verify them.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_auth::signed_audit::{AuditExport, AuditLog, Ed25519Signer, NoteVerifier, AuditSigner};
//! use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, InMemoryAuditLogStore};
//!
//! # async fn demo() -> adk_core::Result<()> {
//! let store = Arc::new(InMemoryAuditLogStore::new());
//! let signer = Arc::new(Ed25519Signer::generate("audit.example.com").unwrap());
//! let trusted: NoteVerifier = signer.verifier_key().parse().unwrap();
//! let log = AuditLog::builder(store.clone(), "replica-1", signer).open().await?;
//! log.append(AuditRecordDraft::new(AuditRecordKind::ToolCall, "search")).await?;
//! log.checkpoint().await?;
//!
//! let export = AuditExport::from_store(store.as_ref(), "replica-1").await?;
//! assert!(export.verify(&[trusted]).is_valid());
//! # Ok(())
//! # }
//! ```

mod jcs;
mod log;
pub mod merkle;
mod note;
mod verify;

pub use jcs::{CanonicalizeError, canonicalize};
pub use log::{
    AuditLog, AuditLogBuilder, CheckpointAnchor, DEFAULT_CHECKPOINT_EVERY,
    DEFAULT_CHECKPOINT_INTERVAL, FileCheckpointAnchor, json_sha256,
};
pub use note::{
    AuditSigner, CheckpointBody, Ed25519Signer, Note, NoteError, NoteSignature, NoteVerifier,
    SignatureAlgorithm, sign_note,
};
pub use verify::{AuditExport, ExportLine, VerifyFailure, VerifyFailureKind, VerifyReport, verify};
