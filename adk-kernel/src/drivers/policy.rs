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
        // A coordinator's crossing renames the tool it reaches and so its declaration; it is
        // judged as that tool, whose declaration the listing that offered it compared with
        // its pin (spec 3.2 §7.8).
        let (tool, stale) = match self.drivers.wrapped_tool(id, tool) {
            Some(wrapped) => {
                let stale = record.stale.contains_key(&wrapped)
                    || (!pins.is_empty() && !pins.contains_key(&wrapped));
                (wrapped, stale)
            }
            None => {
                let pinned = pins.get(tool);
                let stale = record.stale.contains_key(tool)
                    || pinned.is_some_and(|pinned| pinned != digest)
                    || (pinned.is_none() && !pins.is_empty());
                (tool.to_string(), stale)
            }
        };
        let tool = tool.as_str();
        if stale {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::drivers::{DriverManifest, DriverRecord, DriverStore};
    use crate::stores::InMemoryDriverStore;

    /// A supervisor whose store holds `calendar`, with `list_events` pinned, `stale` marked
    /// stale and `pay` classed financial.
    async fn supervisor(stale: bool) -> DriverSupervisor {
        let manifest = DriverManifest::from_toml(
            "id = \"calendar\"\ncommand = \"mcp-calendar\"\npartitions = [\"work\"]\n\
             [tools]\nlist_events = \"read\"\npay = \"financial\"\n",
        )
        .unwrap();
        let mut record = DriverRecord::new(manifest, "alice", chrono::Utc::now());
        for tool in ["list_events", "pay"] {
            record.manifest.declaration_digests.insert(tool.into(), format!("sha256:{tool}"));
        }
        if stale {
            record.stale.insert("list_events".into(), "sha256:changed".into());
        }
        let store = Arc::new(InMemoryDriverStore::new());
        store.put(record).await.unwrap();
        let drivers = DriverSupervisor::builder(store).build();
        drivers.load().await.unwrap();
        drivers.register_crossing("calendar", "work__list_events", "list_events");
        drivers.register_crossing("calendar", "work__pay", "pay");
        drivers
    }

    /// A coordinator's crossing is judged as the driver tool it wraps: allowed while that
    /// tool's declaration is the approved one, asked once it is stale (K-I14), and asked
    /// every call for a class never auto-allowed (K-I9).
    #[tokio::test]
    async fn a_crossing_is_judged_as_the_driver_tool_it_wraps() {
        let fresh = DriverPolicy::new(supervisor(false).await);
        assert_eq!(
            fresh.decide("calendar", "work__list_events", "sha256:renamed"),
            PolicyDecision::Allow
        );
        assert_eq!(
            fresh.drivers.class_of("driver:calendar", "work__list_events"),
            Some(crate::drivers::EffectClass::Read)
        );
        assert!(matches!(
            fresh.decide("calendar", "work__pay", "sha256:renamed"),
            PolicyDecision::RequireApproval { .. }
        ));
        let stale = DriverPolicy::new(supervisor(true).await);
        assert!(matches!(
            stale.decide("calendar", "work__list_events", "sha256:renamed"),
            PolicyDecision::RequireApproval { .. }
        ));
        // A name no crossing registered is judged as itself: not there at approval.
        assert!(matches!(
            fresh.decide("calendar", "home__list_events", "sha256:x"),
            PolicyDecision::RequireApproval { .. }
        ));
    }
}
