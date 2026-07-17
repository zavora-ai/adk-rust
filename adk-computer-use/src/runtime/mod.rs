//! The [`ComputerUseRuntime`] boundary and its concrete adapters.
//!
//! [`ComputerUseRuntime`] is the extension point driven by the deterministic
//! graph in [`crate::build_reference_graph`]. The [`mcp`] submodule provides
//! [`ComputerUseMcpRuntime`], backed by a live `computer-use-mcp` server.
//! Tests and portable examples can supply an in-process implementation instead.

pub mod mcp;

pub use mcp::{ComputerUseMcpConfig, ComputerUseMcpRuntime, TraceCorrelation};

use crate::{
    ActionEnvelope, ActionPreview, ComputerUseError, ControlLease, ExecutionReceipt,
    TargetReservation,
};
use async_trait::async_trait;
use serde_json::Value;

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
/// Implementations must treat the runtime (not graph or model state) as
/// authoritative for policy, identity, lease ownership, and idempotency.
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
    /// Independently verify the receipt returned by [`execute_action`](Self::execute_action).
    async fn verify(&self, receipt: &ExecutionReceipt) -> Result<bool, ComputerUseError>;

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
