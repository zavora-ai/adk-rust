//! The charter: a site's goals, KPIs, guardrails, authority, budget and voice (spec 3.5
//! §7.6, §7.11, §7.16.1, §7.21, §7.8.2).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::consent::ConsentScope;
use super::content::LanguageTag;
use super::contract::ContactLinks;
use super::release::Digest;
use super::signal::Channel;

/// Principals with this prefix are agents, and no agent holds a role (spec 3.5 §7.6, A-I23).
const AGENT_PRINCIPAL_PREFIX: &str = "urn:adk:agent:";

/// A site's charter (spec 3.5 §7.6).
///
/// The public part, [`PublicTerms`], is published at `/.well-known/awp/charter.json` and its
/// digest appears in the contract; the rest stays in the charter store. A version takes
/// effect only when an owner's decision commits it, and only when it satisfies the rules
/// listed on `validate`.
///
/// `priceRule` is the charter's price rule that §7.8.2 refers to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Charter {
    /// Version; increments with every committed change.
    pub version: u32,
    /// The site's id.
    pub site_id: Uuid,
    /// The kind of site; selects the charter template.
    pub kind: SiteKind,
    /// Roles and approval chains.
    pub governance: Governance,
    /// The mission.
    pub mission: String,
    /// Strategy pillars.
    #[serde(default)]
    pub strategy: Vec<StrategyPillar>,
    /// Goals, each naming the KPIs that measure it.
    #[serde(default)]
    pub goals: Vec<Goal>,
    /// KPIs.
    #[serde(default)]
    pub kpis: Vec<Kpi>,
    /// Guardrails that override growth targets (A-I15).
    #[serde(default)]
    pub guardrails: Vec<Guardrail>,
    /// Per-job autonomy and limits.
    #[serde(default)]
    pub authority: Vec<JobAuthority>,
    /// Budget.
    pub budget: Budget,
    /// Voice.
    pub voice: Voice,
    /// Claims, topics and actions never allowed.
    #[serde(default)]
    pub red_lines: Vec<RedLine>,
    /// Reporting.
    pub reporting: Reporting,
    /// The published part.
    pub public: PublicTerms,
    /// Price changes that act within budget; larger ones are held (§7.8.2, A-I25).
    #[serde(default)]
    pub price_rule: PriceRule,
    /// The owner's signature on this version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<OwnerSignature>,
}

impl Charter {
    /// Checks the rules a charter must satisfy before it is committed.
    ///
    /// | Rule | Spec |
    /// |---|---|
    /// | No KPI has a target without a baseline | A-I14 |
    /// | At least one principal holds the owner role | §7.6 |
    /// | No agent principal holds a role | §7.6, A-I23 |
    /// | Every approval chain names at least one role, and distinct principals holding its roles can satisfy it | §7.6, A-I23 |
    /// | One approval chain per action class | §7.6 |
    /// | KPI ids are unique, and every goal names existing KPIs | §7.6 |
    ///
    /// # Errors
    ///
    /// Returns [`CharterError`] listing every violation found.
    ///
    /// # Example
    ///
    /// ```
    /// use awp_types::{Charter, CharterViolation};
    ///
    /// # fn charter() -> Charter { serde_json::from_str(include_str!("../../tests/fixtures/v2/charter.json")).unwrap() }
    /// let mut charter = charter();
    /// assert!(charter.validate().is_ok());
    ///
    /// charter.kpis[0].baseline = None;
    /// let error = charter.validate().unwrap_err();
    /// assert!(matches!(error.violations[0], CharterViolation::TargetWithoutBaseline { .. }));
    /// ```
    pub fn validate(&self) -> Result<(), CharterError> {
        let mut violations = Vec::new();

        for kpi in &self.kpis {
            if kpi.target.is_some() && kpi.baseline.is_none() {
                violations.push(CharterViolation::TargetWithoutBaseline { kpi: kpi.id.clone() });
            }
        }

        if self.governance.holders(Role::Owner).next().is_none() {
            violations.push(CharterViolation::NoOwner);
        }
        for holder in &self.governance.roles {
            if holder.principal.starts_with(AGENT_PRINCIPAL_PREFIX) && !holder.roles.is_empty() {
                violations
                    .push(CharterViolation::AgentHoldsRole { principal: holder.principal.clone() });
            }
        }

        let mut chain_actions = BTreeSet::new();
        for chain in &self.governance.approval_chains {
            if !chain_actions.insert(chain.action.clone()) {
                violations.push(CharterViolation::DuplicateApprovalChain {
                    action: chain.action.clone(),
                });
            }
            if chain.roles.is_empty() {
                violations
                    .push(CharterViolation::EmptyApprovalChain { action: chain.action.clone() });
            } else {
                let people: Vec<&str> =
                    self.governance.roles.iter().map(|holder| holder.principal.as_str()).collect();
                if !chain.is_satisfied_by(&self.governance, &people) {
                    violations.push(CharterViolation::UnsatisfiableApprovalChain {
                        action: chain.action.clone(),
                    });
                }
            }
        }

        let mut kpi_ids = BTreeSet::new();
        for kpi in &self.kpis {
            if !kpi_ids.insert(kpi.id.as_str()) {
                violations.push(CharterViolation::DuplicateKpi { kpi: kpi.id.clone() });
            }
        }
        for goal in &self.goals {
            for kpi in &goal.kpis {
                if !kpi_ids.contains(kpi.as_str()) {
                    violations.push(CharterViolation::UnknownKpi {
                        goal: goal.id.clone(),
                        kpi: kpi.clone(),
                    });
                }
            }
        }

        if violations.is_empty() { Ok(()) } else { Err(CharterError { violations }) }
    }

    /// Returns the approval chain for `action`, if the charter has one.
    pub fn approval_chain(&self, action: &ActionClass) -> Option<&ApprovalChain> {
        self.governance.approval_chains.iter().find(|chain| &chain.action == action)
    }
}

/// The violations [`Charter::validate`] found (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("charter is invalid: {}", .violations.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
pub struct CharterError {
    /// Every violation, in the order found.
    pub violations: Vec<CharterViolation>,
}

/// One rule a charter breaks (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CharterViolation {
    /// A KPI has a target but no baseline (A-I14).
    #[error("KPI `{kpi}` has a target without a baseline; measure a baseline first (A-I14)")]
    TargetWithoutBaseline {
        /// The KPI id.
        kpi: String,
    },
    /// No principal holds the owner role.
    #[error("no principal holds the owner role; name at least one owner")]
    NoOwner,
    /// An agent principal holds a role.
    #[error("agent `{principal}` holds a role; roles belong to people (A-I23)")]
    AgentHoldsRole {
        /// The agent principal.
        principal: String,
    },
    /// An approval chain names no roles.
    #[error("approval chain for `{action}` names no roles")]
    EmptyApprovalChain {
        /// The action class.
        action: ActionClass,
    },
    /// Not enough distinct principals hold the chain's roles to satisfy it.
    #[error(
        "approval chain for `{action}` needs more distinct principals holding its roles than governance names"
    )]
    UnsatisfiableApprovalChain {
        /// The action class.
        action: ActionClass,
    },
    /// Two approval chains name one action class.
    #[error("action class `{action}` has more than one approval chain")]
    DuplicateApprovalChain {
        /// The action class.
        action: ActionClass,
    },
    /// Two KPIs share an id.
    #[error("KPI id `{kpi}` is used more than once")]
    DuplicateKpi {
        /// The KPI id.
        kpi: String,
    },
    /// A goal names a KPI the charter does not define.
    #[error("goal `{goal}` names KPI `{kpi}`, which the charter does not define")]
    UnknownKpi {
        /// The goal id.
        goal: String,
        /// The missing KPI id.
        kpi: String,
    },
}

/// The kind of site; selects a charter template (spec 3.5 §7.16.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SiteKind {
    /// Blogs, portfolios, CVs.
    Personal,
    /// Services, local firms, professionals.
    Business,
    /// Shops.
    Ecommerce,
    /// Government and public sector.
    Government,
    /// Nonprofits and charities.
    Nonprofit,
    /// Schools, colleges, training.
    Education,
    /// News and media.
    Media,
    /// Clinics and practitioners.
    Health,
    /// Clubs, associations, places of worship.
    Community,
    /// Conferences and festivals.
    Events,
}

impl SiteKind {
    /// Every kind, in the order of spec 3.5 §7.16.1.
    pub const ALL: [SiteKind; 10] = [
        SiteKind::Personal,
        SiteKind::Business,
        SiteKind::Ecommerce,
        SiteKind::Government,
        SiteKind::Nonprofit,
        SiteKind::Education,
        SiteKind::Media,
        SiteKind::Health,
        SiteKind::Community,
        SiteKind::Events,
    ];
}

/// Who holds each role and which approval chains apply (spec 3.5 §7.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Governance {
    /// People and the roles they hold.
    #[serde(default)]
    pub roles: Vec<RoleHolder>,
    /// Approval chains per action class.
    #[serde(default)]
    pub approval_chains: Vec<ApprovalChain>,
}

impl Governance {
    /// Returns the principals holding `role`.
    pub fn holders(&self, role: Role) -> impl Iterator<Item = &str> {
        self.roles
            .iter()
            .filter(move |holder| holder.roles.contains(&role))
            .map(|holder| holder.principal.as_str())
    }

    /// Returns `true` when `principal` holds `role`. Agent principals hold no role.
    pub fn holds(&self, principal: &str, role: Role) -> bool {
        !principal.starts_with(AGENT_PRINCIPAL_PREFIX)
            && self.holders(role).any(|holder| holder == principal)
    }
}

/// A person and the roles they hold (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RoleHolder {
    /// The person's principal.
    pub principal: String,
    /// Their roles.
    pub roles: Vec<Role>,
}

/// A governance role (spec 3.5 §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    /// Owns the site and its charter.
    Owner,
    /// Approves publication.
    Publisher,
    /// Drafts and proposes content.
    Editor,
    /// Approves other actions.
    Approver,
    /// Reads everything, changes nothing.
    Auditor,
}

open_enum! {
    /// A class of action that may need an approval chain (spec 3.5 §7.6).
    pub enum ActionClass {
        /// Publishing a release.
        Publish => "publish",
        /// Committing a charter change.
        CharterChange => "charter_change",
        @other
        /// An action class the site defines.
        Custom(String),
    }
}

/// The roles that must approve an action class, each by a distinct principal
/// (spec 3.5 §7.6, A-I23).
///
/// A publication needing an editor's proposal and a publisher's approval is
/// `roles: [editor, publisher]`; a charter change needing two owners is `[owner, owner]`.
///
/// # Example
///
/// ```
/// use awp_types::{ActionClass, ApprovalChain, Governance, Role, RoleHolder};
///
/// let governance = Governance {
///     roles: vec![
///         RoleHolder { principal: "user:ana".into(), roles: vec![Role::Owner, Role::Editor] },
///         RoleHolder { principal: "user:ben".into(), roles: vec![Role::Publisher] },
///     ],
///     approval_chains: vec![],
/// };
/// let chain = ApprovalChain { action: ActionClass::Publish, roles: vec![Role::Editor, Role::Publisher] };
/// assert!(chain.is_satisfied_by(&governance, &["user:ana", "user:ben"]));
/// assert!(!chain.is_satisfied_by(&governance, &["user:ana"]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ApprovalChain {
    /// The action class.
    pub action: ActionClass,
    /// The roles that must approve, one distinct principal each.
    pub roles: Vec<Role>,
}

impl ApprovalChain {
    /// Returns `true` when `principals` — the proposer and the approvers of one action —
    /// fill every role of the chain, each role by a different principal who holds it under
    /// `governance`. Agent principals and repeated principals count once at most.
    pub fn is_satisfied_by(&self, governance: &Governance, principals: &[&str]) -> bool {
        let mut people: Vec<&str> = principals
            .iter()
            .copied()
            .filter(|principal| !principal.starts_with(AGENT_PRINCIPAL_PREFIX))
            .collect();
        people.sort_unstable();
        people.dedup();
        if people.len() < self.roles.len() {
            return false;
        }

        // Bipartite matching of chain roles to distinct principals (augmenting paths).
        let mut assigned: Vec<Option<usize>> = vec![None; people.len()];
        for slot in 0..self.roles.len() {
            let mut visited = vec![false; people.len()];
            if !self.assign(slot, governance, &people, &mut assigned, &mut visited) {
                return false;
            }
        }
        true
    }

    fn assign(
        &self,
        slot: usize,
        governance: &Governance,
        people: &[&str],
        assigned: &mut [Option<usize>],
        visited: &mut [bool],
    ) -> bool {
        for (person, principal) in people.iter().enumerate() {
            if visited[person] || !governance.holds(principal, self.roles[slot]) {
                continue;
            }
            visited[person] = true;
            let free = match assigned[person] {
                None => true,
                Some(other) => self.assign(other, governance, people, assigned, visited),
            };
            if free {
                assigned[person] = Some(slot);
                return true;
            }
        }
        false
    }
}

/// A strategy pillar (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StrategyPillar {
    /// Short name.
    pub name: String,
    /// What it means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A goal and the KPIs that measure it (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    /// Goal id.
    pub id: String,
    /// The goal in plain language.
    pub name: String,
    /// Ids of the KPIs that measure it.
    #[serde(default)]
    pub kpis: Vec<String>,
}

/// A key performance indicator (spec 3.5 §7.6).
///
/// A KPI has no target until it has a baseline, and targets are set by the owner (A-I14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Kpi {
    /// KPI id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The metric, such as `leads.qualified`.
    pub metric: MetricRef,
    /// Where measurements come from.
    pub source: SourceRef,
    /// Which way is better.
    pub direction: Direction,
    /// The baseline measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Measurement>,
    /// The owner's target; `None` until a baseline exists (A-I14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// How often it is measured.
    pub cadence: Cadence,
}

/// A metric name, such as `leads.qualified` or `pages.answer_citations` (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct MetricRef(pub String);

impl From<&str> for MetricRef {
    fn from(metric: &str) -> Self {
        Self(metric.to_owned())
    }
}

/// Where a KPI's measurements come from (spec 3.5 §7.6).
///
/// Serialized as `beacon`, `host_logs`, `analytics:<driver>`, `commerce`, `spend`, `email`,
/// or another source's name.
///
/// # Example
///
/// ```
/// use awp_types::SourceRef;
///
/// assert_eq!(SourceRef::from("analytics:plausible"), SourceRef::Analytics("plausible".into()));
/// assert_eq!(SourceRef::Analytics("plausible".into()).to_string(), "analytics:plausible");
/// assert_eq!(SourceRef::from("host_logs"), SourceRef::HostLogs);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SourceRef {
    /// The edge's cookieless beacon.
    Beacon,
    /// Host access logs.
    HostLogs,
    /// An analytics or search-console driver, by name.
    Analytics(String),
    /// The commerce and action ledgers.
    Commerce,
    /// The spend ledger.
    Spend,
    /// The email driver's delivery statistics.
    Email,
    /// A source this version does not name.
    Other(String),
}

impl From<&str> for SourceRef {
    fn from(value: &str) -> Self {
        match value {
            "beacon" => SourceRef::Beacon,
            "host_logs" => SourceRef::HostLogs,
            "commerce" => SourceRef::Commerce,
            "spend" => SourceRef::Spend,
            "email" => SourceRef::Email,
            other => match other.strip_prefix("analytics:") {
                Some(driver) => SourceRef::Analytics(driver.to_owned()),
                None => SourceRef::Other(other.to_owned()),
            },
        }
    }
}

impl fmt::Display for SourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceRef::Beacon => f.write_str("beacon"),
            SourceRef::HostLogs => f.write_str("host_logs"),
            SourceRef::Analytics(driver) => write!(f, "analytics:{driver}"),
            SourceRef::Commerce => f.write_str("commerce"),
            SourceRef::Spend => f.write_str("spend"),
            SourceRef::Email => f.write_str("email"),
            SourceRef::Other(other) => f.write_str(other),
        }
    }
}

impl Serialize for SourceRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SourceRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|value| SourceRef::from(value.as_str()))
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for SourceRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SourceRef".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "examples": ["beacon", "host_logs", "analytics:search_console", "commerce", "spend", "email"]
        })
    }
}

/// Which way a KPI improves (spec 3.5 §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Direction {
    /// Higher is better.
    Up,
    /// Lower is better.
    Down,
}

/// A measured value over a window (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    /// The value.
    pub value: f64,
    /// The window it covers, in seconds on the wire.
    #[serde(with = "super::serde_util::duration_secs")]
    #[cfg_attr(feature = "json-schema", schemars(with = "u64"))]
    pub window: Duration,
    /// When it was measured.
    pub measured_at: DateTime<Utc>,
}

/// An owner-set target (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Target {
    /// The value to reach.
    pub value: f64,
    /// When to reach it.
    pub horizon: DateTime<Utc>,
}

/// How often something recurs (spec 3.5 §7.6, §7.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Cadence {
    /// Every day.
    Daily,
    /// Every week.
    Weekly,
    /// Every month.
    Monthly,
    /// Every quarter.
    Quarterly,
}

/// A guardrail: a metric limit whose breach pauses a job or alerts, overriding any growth
/// target (spec 3.5 §7.6, A-I15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Guardrail {
    /// Guardrail id.
    pub id: String,
    /// The metric watched.
    pub metric: MetricRef,
    /// The limit.
    pub threshold: f64,
    /// Which side of the threshold is a breach.
    pub breach: Breach,
    /// The window the metric is evaluated over, in seconds on the wire.
    #[serde(with = "super::serde_util::duration_secs")]
    #[cfg_attr(feature = "json-schema", schemars(with = "u64"))]
    pub window: Duration,
    /// What a breach does.
    pub action: GuardrailAction,
    /// The jobs a breach pauses; empty means every job that moves the metric.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<String>,
}

impl Guardrail {
    /// Returns `true` when `value` breaches the guardrail.
    pub fn is_breached_by(&self, value: f64) -> bool {
        match self.breach {
            Breach::Above => value > self.threshold,
            Breach::Below => value < self.threshold,
        }
    }
}

/// Which side of a guardrail's threshold is a breach (spec 3.5 §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Breach {
    /// A value above the threshold breaches.
    Above,
    /// A value below the threshold breaches.
    Below,
}

/// What a guardrail breach does (spec 3.5 §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GuardrailAction {
    /// Pause the affected jobs before their next action.
    PauseJob,
    /// Raise an alarm.
    Alert,
}

/// A job's autonomy level and limits (spec 3.5 §7.6, §7.11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobAuthority {
    /// Job id.
    pub job: String,
    /// Autonomy level.
    pub level: AutonomyLevel,
    /// Limits within the level; omitted when there are none.
    #[serde(default, skip_serializing_if = "JobLimits::is_unlimited")]
    pub limits: JobLimits,
}

/// Limits on a job's actions (spec 3.5 §7.6, §7.11).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobLimits {
    /// Most the job may spend per budget period, in the budget currency's minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_cap: Option<u64>,
    /// Most actions the job may take per day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions_per_day: Option<u32>,
}

impl JobLimits {
    /// Returns `true` when no limit is set.
    pub fn is_unlimited(&self) -> bool {
        self.spend_cap.is_none() && self.actions_per_day.is_none()
    }
}

/// A job's autonomy level (spec 3.5 §7.11).
///
/// The site may lower a job's level and never raise it; raising it is an owner decision
/// (A-I13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AutonomyLevel {
    /// Read-only; output goes to reports.
    Observe,
    /// Writes become drafts and proposals.
    Propose,
    /// Each non-read call is held for approval.
    ActWithApproval,
    /// Acts within policy, run budget and spend cap; anything beyond is held.
    ActWithinBudget,
    /// Acts only where the action is declared reversible.
    ActReversibleOnly,
}

/// The budget (spec 3.5 §7.6, §7.14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    /// The budget period.
    pub period: Cadence,
    /// ISO 4217 currency code.
    pub currency: String,
    /// Cap per category per period, in minor units.
    #[serde(default)]
    pub caps: BTreeMap<SpendCategory, u64>,
}

/// A spending category (spec 3.5 §7.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SpendCategory {
    /// Hosting.
    Hosting,
    /// Model tokens.
    Models,
    /// Advertising.
    Advertising,
    /// Domains.
    Domains,
    /// Everything else.
    Other,
}

/// The site's voice (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Voice {
    /// Tone.
    pub tone: String,
    /// Languages.
    #[serde(default)]
    pub languages: Vec<LanguageTag>,
    /// The disclosure line on agent-written messages (A-I17).
    pub disclosure: String,
}

/// A claim, topic or action never allowed (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RedLine {
    /// What kind of red line.
    pub kind: RedLineKind,
    /// The red line in plain language.
    pub text: String,
}

/// What a red line forbids (spec 3.5 §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RedLineKind {
    /// A claim.
    Claim,
    /// A topic.
    Topic,
    /// An action.
    Action,
}

/// Review cadence, urgency routing and quiet hours (spec 3.5 §7.6, §7.21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Reporting {
    /// How often the review is sent.
    pub review: Cadence,
    /// Owner channels per urgency.
    #[serde(default)]
    pub routes: Vec<UrgencyRoute>,
    /// Quiet hours; P1 alarms ignore them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<QuietHours>,
}

/// The owner channels for one urgency (spec 3.5 §7.21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UrgencyRoute {
    /// The urgency.
    pub urgency: Urgency,
    /// The channels.
    pub channels: Vec<Channel>,
}

/// Quiet hours in a time zone (spec 3.5 §7.21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct QuietHours {
    /// Start, local time.
    pub start: NaiveTime,
    /// End, local time.
    pub end: NaiveTime,
    /// IANA time zone, such as `Africa/Nairobi`.
    pub timezone: String,
}

/// An alarm's urgency (spec 3.5 §7.21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Urgency {
    /// Immediately, inside quiet hours, repeated until acknowledged.
    P1,
    /// Within the hour, outside quiet hours.
    P2,
    /// The weekly review.
    P3,
}

/// The charter's published part, served at `/.well-known/awp/charter.json` (spec 3.5 §7.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PublicTerms {
    /// Usage terms in the IETF AI preferences vocabulary, as sent in `Content-Usage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    /// Published policies.
    #[serde(default)]
    pub policies: Vec<PolicyRef>,
    /// Consent scopes the site asks for.
    #[serde(default)]
    pub consent_scopes: Vec<ConsentScope>,
    /// How to reach the site.
    #[serde(default)]
    pub contact: ContactLinks,
}

/// A published policy (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PolicyRef {
    /// Policy name, such as `privacy`, `terms` or `corrections`.
    pub name: String,
    /// URL.
    pub url: String,
    /// Version, as consent receipts name it.
    pub version: String,
    /// Digest of the policy text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
}

/// An owner's signature on a charter version (spec 3.5 §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OwnerSignature {
    /// The decision that committed the version.
    pub decision: String,
    /// The owner's principal.
    pub principal: String,
    /// The signed note sealing the commit in the audit log.
    pub note: String,
    /// When it was signed.
    pub signed_at: DateTime<Utc>,
}

/// Price changes that act within budget; larger ones are held (spec 3.5 §7.8.2, A-I25).
///
/// # Example
///
/// ```
/// use awp_types::PriceRule;
///
/// let rule = PriceRule::default();
/// assert!(rule.allows(10_000, 10_500));   // 5%
/// assert!(!rule.allows(10_000, 11_500));  // 15%: held
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PriceRule {
    /// Largest change, in percent of the old price, that acts within budget.
    pub auto_within_pct: f32,
}

impl Default for PriceRule {
    /// Changes within 10% act within budget.
    fn default() -> Self {
        Self { auto_within_pct: 10.0 }
    }
}

impl PriceRule {
    /// Returns `true` when a change from `old` to `new` minor units is within the rule. A
    /// change from zero is never within it.
    pub fn allows(&self, old: u64, new: u64) -> bool {
        if old == 0 {
            return new == 0;
        }
        // Compared without division so a change of exactly the percentage is within it.
        old.abs_diff(new) as f64 * 100.0 <= f64::from(self.auto_within_pct) * old as f64
    }
}
