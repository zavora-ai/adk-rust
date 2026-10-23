//! The kernel's policy layer for driver tools (spec 3.2 §7.7, K-I6, K-I9, K-I13, K-I14).

use adk_core::{PolicyCall, PolicyDecision, ToolPolicy, ToolPolicyRequest};
use async_trait::async_trait;

use super::driver_of;
use super::supervisor::DriverSupervisor;

/// The policy reference of [`DriverPolicy`] in audit records: `tool_policy:adk-kernel/drivers`.
pub const DRIVER_POLICY_REF: &str = "adk-kernel/drivers";

/// Decides calls to driver tools by the approved manifests (spec 3.2 §7.7).
///
/// It composes with the harness's policy through `StrictestPolicy` and never loosens it
/// (K-I6): a call it does not concern, or that it allows, is decided by the other layers.
///
/// | Call | Decision |
/// |---|---|
/// | A tool whose source is not a driver's | Allow (not this layer's call) |
/// | A tool of a driver that is not mounted | Deny (K-I13) |
/// | A tool whose declaration differs from its pin, or that was not there at approval | RequireApproval until the driver is re-approved (K-I14) |
/// | A tool of class `publish_public` or `financial` | RequireApproval, every call (K-I9) |
/// | Any other driver tool | Allow |
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::PolicyDecision;
/// use adk_kernel::drivers::DriverSupervisor;
/// use adk_kernel::stores::InMemoryDriverStore;
///
/// let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
/// let policy = adk_kernel::drivers::DriverPolicy::new(drivers);
/// assert!(matches!(policy.decide("email", "send_draft", "sha256:x"), PolicyDecision::Deny { .. }));
/// ```
#[derive(Debug, Clone)]
pub struct DriverPolicy {
    drivers: DriverSupervisor,
}

impl DriverPolicy {
    /// The policy over `drivers`' mounted manifests.
    pub fn new(drivers: DriverSupervisor) -> Self {
        Self { drivers }
    }

    /// The decision for a call to `tool` of driver `id`, whose declaration digest is
    /// `digest`.
    pub fn decide(&self, id: &str, tool: &str, digest: &str) -> PolicyDecision {
        let Some(record) = self.drivers.record(id) else {
            return PolicyDecision::deny(format!(
                "driver `{id}` is not mounted; a driver runs only with a manifest the user \
                 approved (adk-rust drivers mount <file>)"
            ));
        };
        let pins = &record.manifest.declaration_digests;
        let pinned = pins.get(tool);
        if record.stale.contains_key(tool)
            || pinned.is_some_and(|pinned| pinned != digest)
            || (pinned.is_none() && !pins.is_empty())
        {
            return PolicyDecision::require_approval(format!(
                "`{tool}` of driver `{id}` declares something other than what was approved, so \
                 its standing rules do not apply and every call needs approval until the driver \
                 is re-approved (adk-rust drivers reapprove {id})"
            ));
        }
        let class = record.manifest.class_of(tool);
        if class.is_never_auto_allowed() {
            return PolicyDecision::require_approval(format!(
                "`{tool}` of driver `{id}` is `{class}`, which runs only when a person approves \
                 the call"
            ));
        }
        PolicyDecision::Allow
    }
}

#[async_trait]
impl ToolPolicy for DriverPolicy {
    async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
        PolicyDecision::Allow
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        match call.origin {
            Some(origin) => match driver_of(&origin.source) {
                Some(id) => self.decide(id, &request.tool_name, &origin.declaration_digest),
                None => PolicyDecision::Allow,
            },
            None => PolicyDecision::Allow,
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some(DRIVER_POLICY_REF.to_string())
    }
}
