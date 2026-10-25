//! The monthly running-cost estimate (spec 3.5 §7.18 stage 5 "budget with a running-cost
//! estimate", §7.22, S17).
//!
//! Onboarding shows the estimate before deployment and the charter's budget is drafted
//! against it; Finance reports the actual cost every week. The estimate counts the model runs
//! each running agent makes in a month — the probe and the weekly review's composition run
//! without a model — at the owner's chosen model's prices, plus hosting and the domain. No
//! provider's prices are built in: the owner's choices are inputs.

use std::collections::BTreeMap;

use awp_types::{Budget, SpendCategory};
use serde::{Deserialize, Serialize};

use crate::team::{AgentRole, SiteTeam};

/// A model's prices, in minor units of the budget's currency per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    /// Per million input tokens.
    pub input_per_mtok: u64,
    /// Per million output tokens.
    pub output_per_mtok: u64,
}

/// What a month of the site looks like (spec 3.5 §7.22).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostInputs {
    /// The static host and the edge and kernel's server or container, per month, in minor
    /// units.
    pub hosting: u64,
    /// The domain, per year, in minor units.
    pub domain_per_year: u64,
    /// The model's prices.
    pub model: ModelPrice,
    /// Input tokens per model run.
    pub input_tokens_per_run: u64,
    /// Output tokens per model run.
    pub output_tokens_per_run: u64,
    /// Visitor and agent conversations per month.
    pub conversations: u64,
    /// Inbound emails and social messages per month.
    pub messages: u64,
    /// Leads per month.
    pub leads: u64,
}

impl CostInputs {
    /// A small site: 100 conversations, 40 messages and 10 leads a month, 6,000 input and
    /// 800 output tokens a run.
    pub fn small(hosting: u64, domain_per_year: u64, model: ModelPrice) -> Self {
        Self {
            hosting,
            domain_per_year,
            model,
            input_tokens_per_run: 6_000,
            output_tokens_per_run: 800,
            conversations: 100,
            messages: 40,
            leads: 10,
        }
    }
}

/// The estimate (spec 3.5 §7.18 stage 5, §7.22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningCostEstimate {
    /// Model runs per agent per month.
    pub runs: BTreeMap<AgentRole, u64>,
    /// Per month, in minor units, by category.
    pub monthly: BTreeMap<SpendCategory, u64>,
    /// The month's total, in minor units.
    pub total: u64,
}

impl RunningCostEstimate {
    /// Whether every category fits `budget`'s cap; a category the budget does not cap does
    /// not fit unless its estimate is zero.
    pub fn fits(&self, budget: &Budget) -> bool {
        self.monthly.iter().all(|(category, cost)| {
            *cost == 0 || budget.caps.get(category).is_some_and(|cap| cost <= cap)
        })
    }
}

/// Estimates the monthly running cost of `team` with `inputs` (spec 3.5 §7.22).
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteRef;
/// use adk_website::kinds::template;
/// use adk_website::onboarding::cost::{CostInputs, ModelPrice, estimate};
/// use adk_website::team::SiteTeam;
/// use awp_types::SiteKind;
///
/// let personal = template(SiteKind::Personal);
/// let team = SiteTeam::new(SiteRef::new(uuid::Uuid::nil(), "notes", "https://notes.example"), &personal);
/// let price = ModelPrice { input_per_mtok: 30, output_per_mtok: 250 };
/// let estimate = estimate(&team, &CostInputs::small(400, 1_200, price));
/// assert_eq!(estimate.runs.len(), 4, "a personal site runs four agents");
/// ```
pub fn estimate(team: &SiteTeam, inputs: &CostInputs) -> RunningCostEstimate {
    // Model runs per month, from each agent's wakes (spec 3.5 §7.19).
    let runs: BTreeMap<AgentRole, u64> = team
        .members()
        .iter()
        .map(|role| {
            let runs = match role {
                AgentRole::Concierge | AgentRole::AgentDesk => inputs.conversations,
                AgentRole::Inbox => inputs.messages,
                // A daily plan, four weekly reviews' summaries and the owner's messages.
                AgentRole::Steward => 30 + 4 + 8,
                // A schedule a day and the content gaps visitors show.
                AgentRole::Growth => 30 + inputs.conversations / 10,
                // A nurture run a day and one per lead.
                AgentRole::Revenue => 30 + inputs.leads,
                // Advisories once a day; the five-minute probe needs no model.
                AgentRole::Guardian => 30,
                // The daily spend check and the monthly reconciliation.
                AgentRole::Finance => 31,
            };
            (*role, runs)
        })
        .collect();
    let total_runs: u64 = runs.values().sum();
    let per_run_micro = inputs.input_tokens_per_run * inputs.model.input_per_mtok
        + inputs.output_tokens_per_run * inputs.model.output_per_mtok;
    // Tokens times price per million, rounded up to a whole minor unit.
    let models = (total_runs * per_run_micro).div_ceil(1_000_000);
    let domains = inputs.domain_per_year.div_ceil(12);
    let monthly: BTreeMap<SpendCategory, u64> = [
        (SpendCategory::Hosting, inputs.hosting),
        (SpendCategory::Models, models),
        (SpendCategory::Domains, domains),
    ]
    .into_iter()
    .collect();
    let total = monthly.values().sum();
    RunningCostEstimate { runs, monthly, total }
}
