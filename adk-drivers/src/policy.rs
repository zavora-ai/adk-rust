//! The policy layer for driver tools' autonomy caps (spec 3.5 §7.13, A-I21).

use std::collections::BTreeMap;

use adk_core::{PolicyDecision, ToolPolicy, ToolPolicyRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::class::{AutonomyCap, tool_spec};

/// The policy reference of [`DriverCapPolicy`] in audit records: `tool_policy:adk-drivers/caps`.
pub const DRIVER_CAP_POLICY_REF: &str = "adk-drivers/caps";

/// The owner's budget for one [`AutonomyCap::WithinBudget`] tool (spec 3.5 §7.13): the
/// most a single call may commit, in minor units of `currency`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BudgetCap {
    /// The currency, as ISO 4217.
    pub currency: String,
    /// The most one call may commit, in minor units.
    pub max_minor: u64,
}

/// The owner's budgets for the driver tools that act within budget (spec 3.5 §7.13): a
/// domain renewal's price, an advertising bid.
///
/// # Example
///
/// ```rust
/// use adk_drivers::DriverBudget;
///
/// let budget = DriverBudget::new()
///     .cap("registrar_renew_domain", "USD", 30_00)
///     .cap("ads_set_bid", "USD", 2_50);
/// assert_eq!(budget.get("ads_set_bid").unwrap().max_minor, 250);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverBudget {
    caps: BTreeMap<String, BudgetCap>,
}

impl DriverBudget {
    /// No budget: every within-budget call is held.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lets one call of `tool` commit at most `max_minor` of `currency` without a person.
    #[must_use]
    pub fn cap(
        mut self,
        tool: impl Into<String>,
        currency: impl Into<String>,
        max_minor: u64,
    ) -> Self {
        self.caps.insert(tool.into(), BudgetCap { currency: currency.into(), max_minor });
        self
    }

    /// The cap of `tool`.
    pub fn get(&self, tool: &str) -> Option<&BudgetCap> {
        self.caps.get(tool)
    }
}

/// Enforces the driver classes' autonomy caps on the governed path (spec 3.5 §7.13, A-I21).
///
/// It only ever tightens: compose it with the harness's mode policy and the kernel's
/// `DriverPolicy` through `StrictestPolicy`, so a call it allows is decided by those
/// layers. It knows driver tools by name, whether an in-process adapter or an MCP server
/// serves them.
///
/// | Tool's cap | Decision |
/// |---|---|
/// | [`AlwaysHeld`](AutonomyCap::AlwaysHeld) — a DNS change, a campaign launch | RequireApproval on every call, in every mode (A-I21) |
/// | [`WithinBudget`](AutonomyCap::WithinBudget) with an amount — a renewal, a bid | Allow within the owner's [`DriverBudget`] for the tool, in its currency; RequireApproval beyond it or without one |
/// | Any other driver tool, and every tool that is not a driver's | Allow (decided by the other layers) |
///
/// # Example
///
/// ```rust
/// use adk_core::{PolicyDecision, ToolPolicy, ToolPolicyRequest};
/// use adk_drivers::{DriverBudget, DriverCapPolicy};
/// use serde_json::json;
///
/// # futures::executor::block_on(async {
/// let policy = DriverCapPolicy::new(DriverBudget::new().cap("ads_set_bid", "USD", 200));
/// let change = ToolPolicyRequest::new("dns_change_records", json!({"zone": "example.com."}));
/// assert!(matches!(policy.evaluate(&change).await, PolicyDecision::RequireApproval { .. }));
/// let bid = ToolPolicyRequest::new("ads_set_bid", json!({"campaign": "c1", "bid_minor": 150, "currency": "USD"}));
/// assert_eq!(policy.evaluate(&bid).await, PolicyDecision::Allow);
/// let high = ToolPolicyRequest::new("ads_set_bid", json!({"campaign": "c1", "bid_minor": 900, "currency": "USD"}));
/// assert!(matches!(policy.evaluate(&high).await, PolicyDecision::RequireApproval { .. }));
/// # });
/// ```
#[derive(Debug, Clone, Default)]
pub struct DriverCapPolicy {
    budget: DriverBudget,
}

impl DriverCapPolicy {
    /// The policy with the owner's `budget`.
    pub fn new(budget: DriverBudget) -> Self {
        Self { budget }
    }

    /// The decision for a call to `tool` with `args`.
    pub fn decide(&self, tool: &str, args: &Value) -> PolicyDecision {
        let Some(spec) = tool_spec(tool) else {
            return PolicyDecision::Allow;
        };
        match spec.cap {
            AutonomyCap::AlwaysHeld => PolicyDecision::require_approval(format!(
                "`{tool}` is held for the owner in every mode (spec 3.5 §7.13, A-I21)"
            )),
            AutonomyCap::WithinBudget => {
                let Some(pointer) = spec.budget_arg else {
                    return PolicyDecision::Allow;
                };
                let Some(cap) = self.budget.get(tool) else {
                    return PolicyDecision::require_approval(format!(
                        "`{tool}` acts within the owner's budget, and none is set for it"
                    ));
                };
                let amount = args.pointer(pointer).and_then(Value::as_u64);
                let currency = args.get("currency").and_then(Value::as_str);
                match (amount, currency) {
                    (Some(amount), Some(currency))
                        if amount <= cap.max_minor
                            && currency.eq_ignore_ascii_case(&cap.currency) =>
                    {
                        PolicyDecision::Allow
                    }
                    _ => PolicyDecision::require_approval(format!(
                        "`{tool}` goes beyond the owner's budget of {} {} (minor units) per call",
                        cap.max_minor, cap.currency
                    )),
                }
            }
            AutonomyCap::Read | AutonomyCap::Propose | AutonomyCap::WithinPlaybook => {
                PolicyDecision::Allow
            }
        }
    }
}

#[async_trait]
impl ToolPolicy for DriverCapPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.decide(&request.tool_name, &request.args)
    }

    fn policy_ref(&self) -> Option<String> {
        Some(DRIVER_CAP_POLICY_REF.to_string())
    }
}
