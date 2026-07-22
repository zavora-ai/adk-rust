//! camelCase wire contracts for `computer-use-mcp`.
//!
//! These types round-trip the JSON payloads shared with the TypeScript runtime
//! and enforce disclosure-safe invariants on deserialization (digest-only
//! postconditions, value-free sensitivity evidence, bounded approval scopes).
//! They carry no desktop-actuation logic — the `computer-use-mcp` server remains
//! authoritative for policy, identity, leases, and idempotency.
//!
//! The submodules group related contracts:
//!
//! - [`action`] — action classification, resource context, provenance,
//!   postconditions, and the [`ActionEnvelope`]
//! - [`target`] — target evidence and value-free sensitivity signals
//! - [`approval`] — disclosure-safe [`ApprovalGrant`] scopes
//! - [`receipt`] — capabilities, policy decisions, previews, and receipts
//! - [`lease`] — one-writer control leases and target reservations
//! - [`session`] — session lifecycle, events, follow-ups, and deletion results
//! - [`safety`] — the versioned cross-runtime safety corpus

pub mod action;
pub mod approval;
pub mod lease;
pub mod receipt;
pub mod safety;
pub mod session;
pub mod target;

pub use action::{
    ActionClass, ActionEnvelope, ActionPostcondition, ActionProvenance, ActionResourceContext,
    ExecutionMode,
};
pub use approval::{ApprovalGrant, ApprovalGrantScope};
pub use lease::{ControlLease, LeaseBoundaries, TargetReservation, TargetReservationScope};
pub use receipt::{
    ActionPreview, ExecutionCapability, ExecutionReceipt, PolicyDecision, ReceiptStatus,
};
pub use safety::{SafetyCorpus, SafetyExpectation, SafetyScenario};
pub use session::{
    PostconditionEvidence, RuntimeSession, SessionCompletionEvidence, SessionDeletionResult,
    SessionEvent, SessionFollowUp, SessionFollowUpPage,
};
pub use target::{
    Bounds, TargetEvidence, TargetSensitivityAssessment, TargetSensitivityEvidence,
    TargetSensitivitySignal, TargetSensitivitySource,
};
