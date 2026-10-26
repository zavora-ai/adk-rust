//! Tool policies the edge compiles (spec 3.5 §7.4, §7.10, A-I2).
//!
//! | Policy | Governs | Decides |
//! |---|---|---|
//! | [`CapabilityPolicy`] | Every capability call, from every binding | Denies undeclared tools, tools of another origin and calls above the caller's tier or without a live consent receipt; holds calls the approval hint names; allows the rest |
//! | [`PublicPartitionPolicy`] | The tools of agents hosted at the edge | Allows only declared capabilities, `search_site`, `reply_in_thread` and `emit_signal`, each from the edge's own origin; denies everything else, web access included |

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_awp::v2::capability::CapabilityRegistry;
use adk_awp::v2::store::{ConsentReceiptStore, has_live_receipt};
use adk_core::{PolicyCall, PolicyDecision, ToolPolicy, ToolPolicyRequest};
use async_trait::async_trait;
use awp_types::TrustTier;
use chrono::Utc;
use uuid::Uuid;

/// Principal attribute carrying the caller's tier on the edge's acting context.
pub const ATTR_TIER: &str = "awp.tier";
/// Principal attribute carrying the consent receipt ids a call references, comma-separated.
pub const ATTR_CONSENT: &str = "awp.consent";

/// The governed path's policy for capability calls, compiled from the capability manifest:
/// tiers, consent requirements and approval hints (spec 3.5 §7.4, D-4).
///
/// The edge sets the caller's tier and consent references as attributes of the site's acting
/// principal ([`ATTR_TIER`], [`ATTR_CONSENT`]); the call's tool origin must be the site's,
/// `awp:{domain}`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry};
/// use adk_awp::v2::store::InMemoryConsentReceiptStore;
/// use adk_core::{PolicyCall, PolicyDecision, ToolPolicy, ToolPolicyRequest};
/// use adk_server::awp::policy::CapabilityPolicy;
/// use awp_types::{ApprovalHint, CapabilityEffect, CapabilityV2};
/// use serde_json::{Value, json};
///
/// struct Noop;
/// #[async_trait::async_trait]
/// impl CapabilityHandler for Noop {
///     async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> { Ok(call.input) }
/// }
///
/// # futures::executor::block_on(async {
/// let mut registry = CapabilityRegistry::new("example.com");
/// registry
///     .declare(
///         CapabilityV2::new("book", "Book", CapabilityEffect::NonIdempotent)
///             .with_approval(ApprovalHint::AboveAmount(50_000)),
///         Arc::new(Noop),
///     )
///     .unwrap();
/// let policy = CapabilityPolicy::new(Arc::new(registry), Arc::new(InMemoryConsentReceiptStore::default()));
/// let small = ToolPolicyRequest::new("book", json!({ "amount": 10_000 }));
/// assert_eq!(policy.evaluate_call(&small, &PolicyCall::new()).await, PolicyDecision::Allow);
/// let large = ToolPolicyRequest::new("book", json!({ "amount": 90_000 }));
/// assert!(matches!(policy.evaluate_call(&large, &PolicyCall::new()).await, PolicyDecision::RequireApproval { .. }));
/// let other = ToolPolicyRequest::new("publish_page", json!({}));
/// assert!(matches!(policy.evaluate(&other).await, PolicyDecision::Deny { .. }));
/// # });
/// ```
pub struct CapabilityPolicy {
    registry: Arc<CapabilityRegistry>,
    consent: Arc<dyn ConsentReceiptStore>,
    origin: String,
}

impl std::fmt::Debug for CapabilityPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityPolicy").field("origin", &self.origin).finish_non_exhaustive()
    }
}

impl CapabilityPolicy {
    /// The policy of `registry`'s capabilities, checking consent receipts in `consent`.
    pub fn new(registry: Arc<CapabilityRegistry>, consent: Arc<dyn ConsentReceiptStore>) -> Self {
        let origin = adk_awp::v2::capability::CapabilityTool::origin_for(registry.domain());
        Self { registry, consent, origin }
    }
}

#[async_trait]
impl ToolPolicy for CapabilityPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.evaluate_call(request, &PolicyCall::new()).await
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        let Some(tool) = self.registry.get(&request.tool_name) else {
            return PolicyDecision::deny(format!(
                "'{}' is not a capability this site declares",
                request.tool_name
            ));
        };
        if let Some(origin) = call.origin
            && origin.source != self.origin
        {
            return PolicyDecision::deny(format!(
                "'{}' is declared by {}, not by {}",
                request.tool_name, self.origin, origin.source
            ));
        }
        let attributes = request.acting.as_ref().map(|acting| &acting.agent.attributes);
        let tier = attributes
            .and_then(|attributes| attributes.get(ATTR_TIER))
            .and_then(|tier| serde_json::from_value(serde_json::Value::String(tier.clone())).ok())
            .unwrap_or(TrustTier::Public);
        let declaration = tool.declaration();
        if !declaration.visible_to(tier) {
            return PolicyDecision::deny(format!(
                "'{}' needs tier {} or above",
                declaration.name, declaration.min_trust
            ));
        }
        if let Some(requirement) = &declaration.consent {
            let receipts: Vec<Uuid> = attributes
                .and_then(|attributes| attributes.get(ATTR_CONSENT))
                .map(|ids| ids.split(',').filter_map(|id| id.trim().parse().ok()).collect())
                .unwrap_or_default();
            match has_live_receipt(self.consent.as_ref(), &receipts, requirement, Utc::now()).await
            {
                Ok(true) => {}
                Ok(false) => {
                    return PolicyDecision::deny(format!(
                        "'{}' needs a live consent receipt for {}",
                        declaration.name, requirement.scope
                    ));
                }
                Err(error) => {
                    return PolicyDecision::deny(format!("consent could not be checked: {error}"));
                }
            }
        }
        if declaration.approval.holds(tool.amount(&request.args).await) {
            return PolicyDecision::RequireApproval {
                reason: format!("the owner decides calls to '{}'", declaration.name),
            };
        }
        PolicyDecision::Allow
    }

    fn policy_ref(&self) -> Option<String> {
        Some("awp.capability_manifest".to_owned())
    }
}

/// The fixed policy of the public partition: its agents hold only declared capabilities,
/// `search_site`, `reply_in_thread` and `emit_signal` (spec 3.5 §7.10, A-I2).
///
/// A tool is allowed only by its name and its origin together, so a tool another toolset
/// publishes under an allowed name is denied, and so is every tool that publishes, deploys,
/// changes DNS, spends, sends in bulk, fetches the web or reads the owner's memory.
///
/// # Example
///
/// ```rust
/// use adk_core::{PolicyCall, PolicyDecision, ToolOrigin, ToolPolicy, ToolPolicyRequest};
/// use adk_server::awp::policy::PublicPartitionPolicy;
/// use serde_json::json;
///
/// # futures::executor::block_on(async {
/// let policy = PublicPartitionPolicy::new([("search_site", "awp-edge:example.com")]);
/// let search = ToolOrigin::new("awp-edge:example.com", &json!({ "name": "search_site" }), None);
/// let request = ToolPolicyRequest::new("search_site", json!({ "query": "hours" }));
/// assert_eq!(policy.evaluate_call(&request, &PolicyCall::new().with_origin(&search)).await, PolicyDecision::Allow);
/// let fetch = ToolPolicyRequest::new("web_fetch", json!({ "url": "https://evil.example" }));
/// assert!(matches!(policy.evaluate_call(&fetch, &PolicyCall::new()).await, PolicyDecision::Deny { .. }));
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct PublicPartitionPolicy {
    allowed: BTreeMap<String, String>,
}

impl PublicPartitionPolicy {
    /// Allows each `(tool name, origin)` pair and nothing else.
    pub fn new<N: Into<String>, O: Into<String>>(
        allowed: impl IntoIterator<Item = (N, O)>,
    ) -> Self {
        Self {
            allowed: allowed
                .into_iter()
                .map(|(name, origin)| (name.into(), origin.into()))
                .collect(),
        }
    }

    /// The allowed tool names.
    pub fn allowed(&self) -> impl Iterator<Item = &str> {
        self.allowed.keys().map(String::as_str)
    }
}

#[async_trait]
impl ToolPolicy for PublicPartitionPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.evaluate_call(request, &PolicyCall::new()).await
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        match (self.allowed.get(&request.tool_name), call.origin) {
            (Some(origin), Some(called)) if called.source == *origin => PolicyDecision::Allow,
            _ => {
                tracing::info!(
                    tool.name = %request.tool_name,
                    "public partition refused a tool outside its fixed set"
                );
                PolicyDecision::deny(format!(
                    "'{}' is not available to the site's public agents, which hold only the \
                     site's declared capabilities, search_site, reply_in_thread and emit_signal",
                    request.tool_name
                ))
            }
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some("awp.public_partition".to_owned())
    }
}
