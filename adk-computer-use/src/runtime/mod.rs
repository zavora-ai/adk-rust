//! The [`ComputerUseRuntime`] boundary and its concrete adapters.
//!
//! [`ComputerUseRuntime`] is the extension point driven by the deterministic
//! graph in [`crate::build_reference_graph`]. The [`mcp`](crate::runtime::mcp) submodule provides
//! [`ComputerUseMcpRuntime`], backed by a live `computer-use-mcp` server.
//! Tests and portable examples can supply an in-process implementation instead.

/// Binds MCP responses back to the request that produced them.
pub mod binding;

pub mod mcp;

pub use mcp::{ComputerUseMcpConfig, ComputerUseMcpRuntime, TraceCorrelation};

use crate::{
    ActionEnvelope, ActionPreview, ComputerUseError, ControlLease, ExecutionReceipt,
    TargetReservation,
};
use async_trait::async_trait;
use serde_json::Value;

/// What is actually known about an action's effect after execution.
///
/// `verify` previously returned `bool`, computed as `receipt.status == Committed`. That
/// collapsed two different claims: that the runtime accepted the action, and that the
/// intended effect was observed. A committed action whose effect did not occur was reported
/// as completed, and the reference graph labelled the node and its output "verification".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationOutcome {
    /// The declared postcondition was observed to hold, with evidence bound to it.
    Verified,
    /// The runtime committed the action, but no independent postcondition evidence is
    /// available — either none was declared, or the receipt carried none.
    CommittedUnverified {
        /// Why verification could not be performed, for the operator reading the result.
        reason: String,
    },
    /// The action did not commit, or the evidence contradicts the postcondition.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

impl VerificationOutcome {
    /// Whether the postcondition was independently observed.
    ///
    /// Deliberately false for [`VerificationOutcome::CommittedUnverified`]: a caller asking
    /// "was this verified?" must not be told yes because the action merely committed.
    pub fn is_verified(&self) -> bool {
        matches!(self, VerificationOutcome::Verified)
    }

    /// Whether the action was performed, whether or not its effect was verified.
    pub fn is_committed(&self) -> bool {
        matches!(
            self,
            VerificationOutcome::Verified | VerificationOutcome::CommittedUnverified { .. }
        )
    }

    /// A stable status string for the graph's `result.status` field.
    pub fn status(&self) -> &'static str {
        match self {
            VerificationOutcome::Verified => "completed",
            VerificationOutcome::CommittedUnverified { .. } => "committed_unverified",
            VerificationOutcome::Failed { .. } => "verification_failed",
        }
    }
}

/// Runtime boundary implemented by the computer-use MCP server or an in-process adapter.
///
/// The [`crate::build_reference_graph`] workflow drives this trait in a fixed,
/// safe order: parallel observation ([`discover_capabilities`](Self::discover_capabilities),
/// [`observe_visual`](Self::observe_visual), [`observe_semantic`](Self::observe_semantic)),
/// then [`preview_action`](Self::preview_action), optional
/// [`reserve_target`](Self::reserve_target), [`acquire_lease`](Self::acquire_lease),
/// exactly one [`execute_action`](Self::execute_action),
/// [`verify`](Self::verify), and [`release_target`](Self::release_target).
///
/// The reference graph validates leases, reservations, receipts, envelope expiry, and
/// approval bindings independently of the implementation. After a reservation is accepted,
/// it calls [`release_target`](Self::release_target) on every later success or error path.
///
/// Implementations must treat the runtime (not graph or model state) as
/// authoritative for policy, identity, lease ownership, exact preview binding, and
/// idempotency. Implementations that expose these methods outside the reference graph must
/// enforce the same invariants at that direct-call boundary.
///
/// # Errors
///
/// Every method returns [`ComputerUseError`]. Transport faults map to
/// [`ComputerUseError::Mcp`], payload decoding failures to
/// [`ComputerUseError::Decode`], and identity checks to
/// [`ComputerUseError::IdentityMismatch`]. The cancellation control methods
/// default to [`ComputerUseError::Unsupported`] so adapters can opt in.
#[async_trait]
pub trait ComputerUseRuntime: Send + Sync {
    /// Enumerate the execution capabilities available for the target.
    async fn discover_capabilities(&self) -> Result<Value, ComputerUseError>;
    /// Capture a fresh visual (screenshot/annotation) observation frame.
    async fn observe_visual(&self) -> Result<Value, ComputerUseError>;
    /// Capture a fresh semantic (accessibility/window-tree) observation frame.
    async fn observe_semantic(&self) -> Result<Value, ComputerUseError>;
    /// Preview a proposed action, returning the runtime-bound envelope, policy, and route.
    async fn preview_action(
        &self,
        proposed_action: Value,
    ) -> Result<ActionPreview, ComputerUseError>;
    /// Reserve a non-authoritative planner intent for multi-agent conflict checks.
    ///
    /// Returns `Ok(None)` when the adapter does not model reservations.
    async fn reserve_target(
        &self,
        _envelope: &ActionEnvelope,
    ) -> Result<Option<TargetReservation>, ComputerUseError> {
        Ok(None)
    }
    /// Release a previously acquired [`TargetReservation`].
    ///
    /// The graph reports cleanup failures, including when another operation already failed,
    /// so implementations should return an error instead of hiding an uncertain release.
    async fn release_target(
        &self,
        _reservation: &TargetReservation,
    ) -> Result<(), ComputerUseError> {
        Ok(())
    }
    /// Acquire the one-writer control lease required before any mutation.
    async fn acquire_lease(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<ControlLease, ComputerUseError>;
    /// Execute the previewed action exactly once under the supplied lease.
    async fn execute_action(
        &self,
        envelope: &ActionEnvelope,
        lease: &ControlLease,
        approval_grant_id: Option<&str>,
    ) -> Result<ExecutionReceipt, ComputerUseError>;
    /// Report whether the action's postcondition was independently observed to hold.
    ///
    /// A committed receipt is an acknowledgement that the runtime accepted and performed the
    /// action. It is not evidence that the intended effect occurred, so the two are reported
    /// separately: see [`VerificationOutcome`].
    ///
    /// `postcondition` is the envelope's declared expected state, or `None` when the action
    /// declared none — in which case there is nothing to verify and the honest answer is
    /// [`VerificationOutcome::CommittedUnverified`].
    async fn verify(
        &self,
        receipt: &ExecutionReceipt,
        postcondition: Option<&crate::ActionPostcondition>,
    ) -> Result<VerificationOutcome, ComputerUseError>;

    /// Pause the session's desktop authority. Defaults to unsupported.
    async fn pause_session(
        &self,
        _session_id: &str,
        _reason: &str,
    ) -> Result<(), ComputerUseError> {
        Err(ComputerUseError::Unsupported { operation: "pause_session" })
    }

    /// Stop the session's desktop authority. Defaults to unsupported.
    async fn stop_session(&self, _session_id: &str, _reason: &str) -> Result<(), ComputerUseError> {
        Err(ComputerUseError::Unsupported { operation: "stop_session" })
    }

    /// Revoke all desktop authority immediately. Defaults to unsupported.
    async fn emergency_stop(&self, _reason: &str) -> Result<(), ComputerUseError> {
        Err(ComputerUseError::Unsupported { operation: "emergency_stop" })
    }
}
