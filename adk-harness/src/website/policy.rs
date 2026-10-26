//! The back office's holds that no mode, level or standing rule lifts (spec 3.5 §7.6, §7.11,
//! A-I13, A-I21): decision cards and the launch.

use std::sync::Arc;

use adk_core::{PolicyDecision, ToolPolicy, ToolPolicyRequest};
use async_trait::async_trait;

use super::catalog::{self, Hold, WebsitePartition};
use super::site::SiteRuntime;

/// Holds, for a person and in every mode (spec 3.5 A-I13, A-I21):
///
/// | Call | Why |
/// |---|---|
/// | `charter_commit_request`, `job_promotion_request` | a decision card: the owner decides; the tool itself commits and promotes nothing |
/// | `site_publish` before the site's first production release | the launch is the owner's (A-I21) |
///
/// Every DNS change is held by `adk-drivers`' `DriverCapPolicy`, which
/// [`HarnessAgentBuilder::website_back_office`](crate::HarnessAgentBuilder::website_back_office)
/// composes beside this policy. The policy only ever tightens: a call it allows is decided
/// by the other layers.
#[derive(Clone, Debug)]
pub struct WebsitePolicy {
    site: Arc<SiteRuntime>,
}

impl WebsitePolicy {
    /// The holds of `site`.
    pub fn new(site: Arc<SiteRuntime>) -> Self {
        Self { site }
    }
}

#[async_trait]
impl ToolPolicy for WebsitePolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        let Some(spec) = catalog::spec(&request.tool_name)
            .filter(|spec| spec.partition == WebsitePartition::BackOffice)
        else {
            return PolicyDecision::Allow;
        };
        match spec.hold {
            Hold::Always => PolicyDecision::require_approval(format!(
                "{} raises a decision card: the owner decides it in every mode (spec 3.5 A-I13)",
                spec.name
            )),
            Hold::AtLaunch if !self.site.launched().await => PolicyDecision::require_approval(
                "the site's first production release is its launch, held for the owner in every \
                 mode (spec 3.5 A-I21)",
            ),
            Hold::AtLaunch | Hold::Ladder => PolicyDecision::Allow,
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some(format!("adk-harness.website.holds:{}", self.site.site().name))
    }
}
