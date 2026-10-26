//! Site kinds: one charter template per kind (spec 3.5 §7.16.1, §7.6, D-15, G13).
//!
//! The kind is the first fact of a charter. Onboarding proposes one from discovery
//! ([`detect`]), the owner confirms it, and its [`KindTemplate`] sets the goals and KPIs, the
//! agents that run, the capabilities, and the rules the kind adds. The owner can change
//! anything a template proposes.
//!
//! | Kind | Agents | Capabilities | Rules the template adds |
//! |---|---|---|---|
//! | Personal | Concierge, Steward, Growth, Guardian | newsletter sign-up, contact | smallest budget; no advertising or sales jobs; private about the owner |
//! | Business | all eight | contact, quote, booking | the lead, nurture and handoff loop |
//! | E-commerce | all eight | catalogue, quote, order, payment, order status, returns | payment policy, product feeds, fraud and refund guardrails |
//! | Government | Concierge, Agent desk, Inbox, Steward, Growth, Guardian | service forms with verified identity, appointments, status lookups, notices | editor and publisher approval chain; WCAG 2.2 AA hard gate; every official language; no third-party scripts; no advertising or profit KPIs; records retention; data residency; AI disclosure on every reply |
//! | Nonprofit | all eight | donate, volunteer, register | donor consent and receipts; tax-receipt fields |
//! | Education | public three, Steward, Growth, Guardian, Revenue | enquiry, booking, prospectus | safeguarding; no profiling of minors; learning platforms outside |
//! | Media | Concierge, Steward, Growth, Guardian, Revenue | subscribe, newsletter, licensing | content-usage terms; corrections policy; never generated news |
//! | Health | public three, Steward, Growth, Guardian | appointments, practice information | no clinical advice; special-category data; emergency routing |
//! | Community | Concierge, Steward, Growth, Guardian, Revenue | events, donations, sign-ups | low budget; weekly review only |
//! | Events | all eight | registration, tickets, schedule | date-driven freshness; wind-down and archive |

mod detect;
mod gate;

use std::collections::BTreeMap;
use std::time::Duration;

use adk_site::content::SiteConfig;
use adk_site::gate::LaunchGate;
use adk_site::translation::TranslationPolicy;
use awp_types::{
    ActionClass, ApprovalChain, Breach, Budget, Cadence, CapabilityV2, Charter, ConsentScope,
    ContactLinks, Direction, Goal, Governance, Guardrail, GuardrailAction, Kpi, LanguageTag,
    PolicyRef, PriceRule, PublicTerms, QuietHours, RedLine, RedLineKind, Reporting, Role,
    RoleHolder, SiteKind, SourceRef, SpendCategory, StrategyPillar, Urgency, UrgencyRoute, Voice,
};
use chrono::NaiveTime;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use detect::{KindEvidence, KindProposal, detect};
pub use gate::ThirdPartyScripts;

use crate::jobs::{self, JOBS, JobNeed, JobSpec};
use crate::packs;
use crate::team::{AgentRole, SiteTeam};

/// A rule a site kind adds (spec 3.5 §7.16.1 "Rules the template adds").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
#[non_exhaustive]
pub enum KindRule {
    /// The smallest budget, for a person or a volunteer-run group.
    LowBudget,
    /// No advertising job or advertising budget.
    NoAdvertising,
    /// No sales jobs: no quotes, checkout or orders.
    NoSales,
    /// No profit, revenue or sales KPIs.
    NoProfitKpis,
    /// The site says nothing private about its owner beyond what the owner publishes.
    PrivateAboutOwner,
    /// The full lead, nurture and handoff loop.
    LeadNurtureHandoff,
    /// A payment policy: price rule, held refunds.
    PaymentPolicy,
    /// Product feeds for agents.
    ProductFeeds,
    /// Fraud and refund guardrails.
    FraudAndRefundGuardrails,
    /// Publication needs an editor's proposal and a publisher's approval, by two people.
    TwoPersonPublication,
    /// WCAG 2.2 AA as a hard launch gate.
    AccessibilityHardGate,
    /// Every official language is required, and they publish together.
    AllLanguagesRequired,
    /// No third-party scripts or trackers ship.
    NoThirdPartyScripts,
    /// Records are kept for this many years.
    RecordsRetention {
        /// Years.
        years: u32,
    },
    /// Data stays where the deployment shape puts it.
    DataResidency,
    /// Every agent reply discloses AI assistance.
    AiDisclosureOnEveryReply,
    /// Donor consent and receipts.
    DonorConsentAndReceipts,
    /// Gift-aid or tax-receipt fields by jurisdiction.
    TaxReceiptFields,
    /// Safeguarding red lines.
    Safeguarding,
    /// No profiling of minors.
    NoProfilingOfMinors,
    /// Learning platforms stay outside the site (N5).
    LearningPlatformsOutside,
    /// Content-usage terms for AI training and answer engines.
    ContentUsageTerms,
    /// A published corrections policy; corrections carry a visible note.
    CorrectionsPolicy,
    /// Drafts only from the newsroom's own reporting; never generated news.
    NeverGeneratedNews,
    /// No clinical advice from any agent.
    NoClinicalAdvice,
    /// Health data as a special category: consent and minimal retention.
    SpecialCategoryData,
    /// Urgent symptoms are routed to emergency information.
    EmergencyRouting,
    /// Only P1 alarms interrupt; everything else waits for the weekly review.
    WeeklyReviewOnly,
    /// Freshness follows the event's dates.
    DateDrivenFreshness,
    /// The site winds down after the event and archives.
    WindDownAfterEvent,
}

/// A kind's charter template (spec 3.5 §7.16.1).
#[derive(Debug, Clone, PartialEq)]
pub struct KindTemplate {
    /// The kind.
    pub kind: SiteKind,
    /// The kind in plain words.
    pub title: &'static str,
    /// Default goals, each naming its KPIs.
    pub goals: Vec<Goal>,
    /// Default KPIs, each with its baseline source and no baseline or target (A-I14).
    pub kpis: Vec<Kpi>,
    /// Default guardrails.
    pub guardrails: Vec<Guardrail>,
    /// The agents switched on.
    pub agents: Vec<AgentRole>,
    /// The capabilities, by name (see [`crate::packs`]).
    pub capabilities: Vec<&'static str>,
    /// The rules the kind adds.
    pub rules: Vec<KindRule>,
    /// Claims, topics and actions never allowed.
    pub red_lines: Vec<RedLine>,
    /// Default caps per category per month, in minor units of the owner's currency.
    pub caps: BTreeMap<SpendCategory, u64>,
    /// The tone of voice.
    pub tone: &'static str,
}

/// What the owner and discovery supply to a charter draft (spec 3.5 §7.18 stage 5).
#[derive(Debug, Clone, PartialEq)]
pub struct CharterInputs {
    /// The site's id.
    pub site_id: Uuid,
    /// The site's name.
    pub site_name: String,
    /// The owner's principal.
    pub owner: String,
    /// Other people and their roles, such as a publisher.
    pub people: Vec<RoleHolder>,
    /// The mission, in the owner's words.
    pub mission: String,
    /// The site's languages; the first is the default.
    pub languages: Vec<LanguageTag>,
    /// The budget's ISO 4217 currency.
    pub currency: String,
    /// Caps that replace the template's.
    pub caps: Option<BTreeMap<SpendCategory, u64>>,
    /// The owner's channels for P1 alarms, such as `sms` or `whatsapp`.
    pub p1_channels: Vec<awp_types::Channel>,
    /// The owner's chosen channel for P2 alarms.
    pub p2_channels: Vec<awp_types::Channel>,
    /// Quiet hours in the owner's time zone.
    pub quiet_hours: Option<QuietHours>,
    /// The public contact email.
    pub contact_email: Option<String>,
}

impl CharterInputs {
    /// Inputs for `owner`'s site `site_name` in `languages` and `currency`, with the default
    /// quiet hours (21:00 to 07:00 UTC) and no alarm channels yet.
    pub fn new(
        site_id: Uuid,
        site_name: impl Into<String>,
        owner: impl Into<String>,
        mission: impl Into<String>,
        languages: Vec<LanguageTag>,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            site_id,
            site_name: site_name.into(),
            owner: owner.into(),
            people: Vec::new(),
            mission: mission.into(),
            languages,
            currency: currency.into(),
            caps: None,
            p1_channels: Vec::new(),
            p2_channels: Vec::new(),
            quiet_hours: Some(QuietHours {
                start: NaiveTime::from_hms_opt(21, 0, 0).expect("a valid time"),
                end: NaiveTime::from_hms_opt(7, 0, 0).expect("a valid time"),
                timezone: "UTC".into(),
            }),
            contact_email: None,
        }
    }
}

/// The disclosure line on every agent-written message (spec 3.5 A-I17, §13 Q6).
pub fn disclosure(site_name: &str) -> String {
    format!("Written by {site_name}'s assistant, an AI agent, on behalf of the site's owner.")
}

impl KindTemplate {
    /// Whether the kind adds `rule`.
    pub fn has(&self, rule: &KindRule) -> bool {
        self.rules.contains(rule)
    }

    /// The capability declarations (spec 3.5 §7.16.1).
    pub fn capabilities(&self) -> Vec<CapabilityV2> {
        self.capabilities.iter().filter_map(|name| packs::capability(name)).collect()
    }

    /// The jobs the kind runs (spec 3.5 §7.20): every job one of whose agents runs and whose
    /// need the kind allows.
    pub fn jobs(&self) -> Vec<&'static JobSpec> {
        JOBS.iter()
            .filter(|job| job.agents.iter().any(|agent| self.agents.contains(agent)))
            .filter(|job| match job.needs {
                JobNeed::Nothing => true,
                JobNeed::Advertising => !self.has(&KindRule::NoAdvertising),
                JobNeed::Sales => !self.has(&KindRule::NoSales),
                JobNeed::Wallet => self.agents.contains(&AgentRole::Finance),
            })
            .collect()
    }

    /// The approval chains the kind needs (spec 3.5 §7.6, A-I23): a charter change needs an
    /// owner; under [`KindRule::TwoPersonPublication`] a publication needs an editor and a
    /// publisher.
    pub fn approval_chains(&self) -> Vec<ApprovalChain> {
        let mut chains =
            vec![ApprovalChain { action: ActionClass::CharterChange, roles: vec![Role::Owner] }];
        if self.has(&KindRule::TwoPersonPublication) {
            chains.push(ApprovalChain {
                action: ActionClass::Publish,
                roles: vec![Role::Editor, Role::Publisher],
            });
        }
        chains
    }

    /// Drafts charter version 1 from the template and `inputs` (spec 3.5 §7.18 stage 5): the
    /// template's goals, KPIs with baseline sources and no targets, guardrails, conservative
    /// first-week authority for the jobs `team` runs, roles and approval chains, the budget,
    /// the voice with its disclosure line, red lines, reporting and public terms.
    ///
    /// The draft is checked by `Charter::validate` when it is committed; a kind whose chain
    /// needs a second person fails it until `inputs.people` names one.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::website::SiteRef;
    /// use adk_website::kinds::{CharterInputs, template};
    /// use adk_website::team::SiteTeam;
    /// use awp_types::SiteKind;
    ///
    /// let id = uuid::Uuid::new_v4();
    /// let personal = template(SiteKind::Personal);
    /// let team = SiteTeam::new(SiteRef::new(id, "notes", "https://notes.example"), &personal);
    /// let inputs = CharterInputs::new(id, "Notes", "user:wanjiru", "Essays on cities.", vec!["en".parse().unwrap()], "USD");
    /// let charter = personal.draft_charter(&team, &inputs);
    /// assert!(charter.validate().is_ok());
    /// assert!(charter.kpis.iter().all(|kpi| kpi.baseline.is_none() && kpi.target.is_none()));
    /// ```
    pub fn draft_charter(&self, team: &SiteTeam, inputs: &CharterInputs) -> Charter {
        let chains = self.approval_chains();
        let mut owner_roles = vec![Role::Owner, Role::Editor];
        if !self.has(&KindRule::TwoPersonPublication) {
            owner_roles.push(Role::Publisher);
        }
        let mut roles = vec![RoleHolder { principal: inputs.owner.clone(), roles: owner_roles }];
        roles.extend(inputs.people.iter().cloned());
        let caps = inputs.caps.clone().unwrap_or_else(|| self.caps.clone());
        let mut routes = Vec::new();
        if !inputs.p1_channels.is_empty() {
            routes
                .push(UrgencyRoute { urgency: Urgency::P1, channels: inputs.p1_channels.clone() });
        }
        if !inputs.p2_channels.is_empty() && !self.has(&KindRule::WeeklyReviewOnly) {
            routes
                .push(UrgencyRoute { urgency: Urgency::P2, channels: inputs.p2_channels.clone() });
        }
        let mut policies = vec![
            policy("privacy", "/privacy/"),
            policy("terms", "/terms/"),
            policy("consent", "/consent/"),
        ];
        if self.has(&KindRule::CorrectionsPolicy) {
            policies.push(policy("corrections", "/corrections/"));
        }
        if let Some(years) = self.rules.iter().find_map(|rule| match rule {
            KindRule::RecordsRetention { years } => Some(*years),
            _ => None,
        }) {
            policies.push(PolicyRef {
                name: "records-retention".into(),
                url: "/records-retention/".into(),
                version: format!("{years}y"),
                digest: None,
            });
        }
        let mut consent_scopes: Vec<ConsentScope> = self
            .capabilities()
            .into_iter()
            .filter_map(|capability| capability.consent.map(|consent| consent.scope))
            .collect();
        consent_scopes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        consent_scopes.dedup();
        let jobs = self.jobs();
        let actions_per_day = if self.has(&KindRule::LowBudget) { 20 } else { 100 };
        Charter {
            version: 1,
            site_id: inputs.site_id,
            kind: self.kind,
            governance: Governance { roles, approval_chains: chains },
            mission: inputs.mission.clone(),
            strategy: self
                .goals
                .iter()
                .map(|goal| StrategyPillar { name: goal.name.clone(), description: None })
                .collect(),
            goals: self.goals.clone(),
            kpis: self.kpis.clone(),
            guardrails: self.guardrails.clone(),
            authority: jobs::first_week_authority(team, &jobs, actions_per_day),
            budget: Budget { period: Cadence::Monthly, currency: inputs.currency.clone(), caps },
            voice: Voice {
                tone: self.tone.to_string(),
                languages: inputs.languages.clone(),
                disclosure: disclosure(&inputs.site_name),
            },
            red_lines: self.red_lines.clone(),
            reporting: Reporting {
                review: Cadence::Weekly,
                routes,
                quiet_hours: inputs.quiet_hours.clone(),
            },
            public: PublicTerms {
                // Every launched site states its AI preferences in `robots.txt` (spec 3.5
                // §4.4); the owner reviews them with the charter. A newsroom licenses its
                // reporting, so it opts out of training; other kinds state only that search
                // and answer engines may use the site.
                usage: Some(
                    if self.has(&KindRule::ContentUsageTerms) {
                        "train-ai=n, search=y"
                    } else {
                        "search=y"
                    }
                    .to_string(),
                ),
                policies,
                consent_scopes,
                contact: ContactLinks {
                    security: Some("/.well-known/security.txt".into()),
                    email: inputs.contact_email.clone(),
                    page: Some("/contact/".into()),
                },
            },
            price_rule: PriceRule::default(),
            signed: None,
        }
    }

    /// Applies the kind's site rules to `config` (spec 3.5 §7.16.1, §7.8.2 "Translations"):
    /// every official language required and published together under
    /// [`KindRule::AllLanguagesRequired`]; the kind's capabilities on every page's machine
    /// view.
    pub fn configure_site(&self, config: &mut SiteConfig) {
        if self.has(&KindRule::AllLanguagesRequired) {
            config.translation = TranslationPolicy::official(config.languages.clone());
        }
        for name in &self.capabilities {
            if !config.site_capabilities.iter().any(|held| held == name) {
                config.site_capabilities.push((*name).to_string());
            }
        }
    }

    /// The launch gate the kind needs (spec 3.5 §7.8.1, §7.16.1): the charter's red lines and,
    /// under [`KindRule::NoThirdPartyScripts`], the third-party script check. The site
    /// runtime adds the claim check over its facts.
    pub fn gate(&self, origin: &str) -> LaunchGate {
        let gate = LaunchGate::new().with_red_lines(self.red_lines.clone());
        if self.has(&KindRule::NoThirdPartyScripts) {
            gate.with_extension(std::sync::Arc::new(ThirdPartyScripts::new(origin)))
        } else {
            gate
        }
    }

    /// The instruction public agents add for the kind (spec 3.5 §7.16.1): the AI disclosure
    /// on every reply, no clinical advice, emergency routing.
    pub fn public_instruction(&self, site_name: &str) -> String {
        let mut lines = Vec::new();
        if self.has(&KindRule::AiDisclosureOnEveryReply) {
            lines.push(format!("End every reply with: \"{}\"", disclosure(site_name)));
        }
        if self.has(&KindRule::NoClinicalAdvice) {
            lines.push("Never give clinical advice: no diagnosis, treatment or dosage.".into());
        }
        if self.has(&KindRule::EmergencyRouting) {
            lines.push(
                "When someone describes urgent symptoms, give the emergency number and the \
                 nearest emergency service from the site's pages before anything else."
                    .into(),
            );
        }
        if self.has(&KindRule::NoProfilingOfMinors) {
            lines.push("Never ask a child for personal details or build a profile of one.".into());
        }
        lines.join("\n")
    }
}

fn policy(name: &str, url: &str) -> PolicyRef {
    PolicyRef { name: name.into(), url: url.into(), version: "1".into(), digest: None }
}

fn goal(id: &str, name: &str, kpis: &[&str]) -> Goal {
    Goal { id: id.into(), name: name.into(), kpis: kpis.iter().map(|k| (*k).to_string()).collect() }
}

fn kpi(id: &str, name: &str, metric: &str, source: SourceRef, direction: Direction) -> Kpi {
    Kpi {
        id: id.into(),
        name: name.into(),
        metric: metric.into(),
        source,
        direction,
        baseline: None,
        target: None,
        cadence: Cadence::Daily,
    }
}

fn guardrail(
    id: &str,
    metric: &str,
    threshold: f64,
    breach: Breach,
    days: u64,
    action: GuardrailAction,
    jobs: &[&str],
) -> Guardrail {
    Guardrail {
        id: id.into(),
        metric: metric.into(),
        threshold,
        breach,
        window: Duration::from_secs(days * 24 * 60 * 60),
        action,
        jobs: jobs.iter().map(|j| (*j).to_string()).collect(),
    }
}

fn red_line(kind: RedLineKind, text: &str) -> RedLine {
    RedLine { kind, text: text.into() }
}

fn caps(entries: &[(SpendCategory, u64)]) -> BTreeMap<SpendCategory, u64> {
    entries.iter().copied().collect()
}

/// The site-internal KPI source: measurements the site's own jobs take, such as freshness.
fn site_source() -> SourceRef {
    SourceRef::Other("site".into())
}

/// The template of `kind` (spec 3.5 §7.16.1).
///
/// # Example
///
/// ```rust
/// use adk_website::kinds::{KindRule, template};
/// use adk_website::team::AgentRole;
/// use awp_types::SiteKind;
///
/// let ministry = template(SiteKind::Government);
/// assert!(ministry.has(&KindRule::TwoPersonPublication));
/// assert!(!ministry.agents.contains(&AgentRole::Finance));
/// assert!(ministry.jobs().iter().all(|job| job.id != "advertising"));
/// ```
#[allow(clippy::too_many_lines)]
pub fn template(kind: SiteKind) -> KindTemplate {
    use AgentRole::{AgentDesk, Concierge, Finance, Growth, Guardian, Inbox, Revenue, Steward};
    use Direction::{Down, Up};
    use GuardrailAction::{Alert, PauseJob};
    use KindRule as R;
    use RedLineKind::{Action, Claim};
    use SpendCategory::{Advertising, Domains, Hosting, Models, Other};

    let all = vec![Concierge, AgentDesk, Inbox, Steward, Growth, Revenue, Guardian, Finance];
    let public_and_core = vec![Concierge, AgentDesk, Inbox, Steward, Growth, Guardian];
    let trust_goal =
        goal("trust", "Stay trustworthy, accessible and current", &["freshness", "accessibility"]);
    let trust_kpis = vec![
        kpi("freshness", "Pages current", "content.fresh_share", site_source(), Up),
        kpi("accessibility", "Pages passing WCAG 2.2 AA", "a11y.passing_share", site_source(), Up),
    ];
    let universal = vec![
        red_line(Action, "Never invent facts, testimonials, team members, prices or credentials"),
        red_line(
            Action,
            "Generated images only as labelled illustration, never the owner's people or premises",
        ),
        red_line(Action, "Nothing copied from competitors"),
    ];
    let with = |mut lines: Vec<RedLine>, extra: Vec<RedLine>| {
        lines.extend(extra);
        lines
    };
    let unsubscribes = guardrail(
        "unsubscribes",
        "email.unsubscribe_rate",
        0.02,
        Breach::Above,
        7,
        PauseJob,
        &["email", "nurture"],
    );

    let (
        title,
        mut goals,
        mut kpis,
        guardrails,
        agents,
        capabilities,
        rules,
        red_lines,
        budget,
        tone,
    ) = match kind {
        SiteKind::Personal => (
            "A personal site: a blog, portfolio or CV",
            vec![
                goal("audience", "Grow an audience of readers", &["readers", "subscribers"]),
                goal("enquiries", "Receive enquiries", &["enquiries"]),
            ],
            vec![
                kpi("readers", "Readers", "visits.readers", SourceRef::Beacon, Up),
                kpi(
                    "subscribers",
                    "Newsletter subscribers",
                    "newsletter.subscribers",
                    SourceRef::Email,
                    Up,
                ),
                kpi("enquiries", "Enquiries", "leads.enquiries", SourceRef::Commerce, Up),
            ],
            vec![guardrail(
                "unsubscribes",
                "email.unsubscribe_rate",
                0.02,
                Breach::Above,
                7,
                Alert,
                &[],
            )],
            vec![Concierge, Steward, Growth, Guardian],
            vec!["subscribe_newsletter", "submit_inquiry", "search_site"],
            vec![R::LowBudget, R::NoAdvertising, R::NoSales, R::NoProfitKpis, R::PrivateAboutOwner],
            with(
                universal.clone(),
                vec![red_line(
                    Action,
                    "Say nothing about the owner's private life beyond what the owner publishes",
                )],
            ),
            caps(&[(Hosting, 500), (Models, 1_000), (Domains, 200)]),
            "Personal and plain",
        ),
        SiteKind::Business => (
            "A business: services, a local firm or a professional",
            vec![
                goal("enquiries", "Win qualified enquiries", &["qualified_leads", "response_time"]),
                goal("bookings", "Fill the calendar", &["bookings"]),
                goal("reputation", "Earn good reviews", &["review_rating"]),
            ],
            vec![
                kpi(
                    "qualified_leads",
                    "Qualified enquiries",
                    "leads.qualified",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi(
                    "response_time",
                    "Hours to first reply",
                    "leads.response_hours",
                    SourceRef::Commerce,
                    Down,
                ),
                kpi("bookings", "Bookings", "bookings.count", SourceRef::Commerce, Up),
                kpi(
                    "review_rating",
                    "Average review rating",
                    "reviews.rating",
                    SourceRef::Other("reviews".into()),
                    Up,
                ),
            ],
            vec![unsubscribes.clone()],
            all.clone(),
            vec!["submit_inquiry", "get_quote", "check_availability", "book", "search_site"],
            vec![R::LeadNurtureHandoff],
            universal.clone(),
            caps(&[
                (Hosting, 2_000),
                (Models, 5_000),
                (Advertising, 0),
                (Domains, 300),
                (Other, 1_000),
            ]),
            "Warm, clear and practical",
        ),
        SiteKind::Ecommerce => (
            "A shop",
            vec![
                goal(
                    "sales",
                    "Sell more, profitably",
                    &["orders", "conversion", "average_order_value"],
                ),
                goal("service", "Keep customers happy", &["refund_rate", "stock_accuracy"]),
            ],
            vec![
                kpi("orders", "Orders", "orders.count", SourceRef::Commerce, Up),
                kpi("conversion", "Conversion rate", "orders.conversion", SourceRef::Beacon, Up),
                kpi(
                    "average_order_value",
                    "Average order value",
                    "orders.average_value",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi("refund_rate", "Refund rate", "orders.refund_rate", SourceRef::Commerce, Down),
                kpi(
                    "stock_accuracy",
                    "Stock accuracy",
                    "catalog.stock_accuracy",
                    site_source(),
                    Up,
                ),
            ],
            vec![
                guardrail(
                    "refunds",
                    "orders.refund_rate",
                    0.05,
                    Breach::Above,
                    7,
                    PauseJob,
                    &["sell", "advertising"],
                ),
                guardrail(
                    "chargebacks",
                    "payments.chargeback_rate",
                    0.01,
                    Breach::Above,
                    30,
                    PauseJob,
                    &["sell"],
                ),
                unsubscribes.clone(),
            ],
            all.clone(),
            vec![
                "list_products",
                "get_quote",
                "place_order",
                "pay_order",
                "order_status",
                "request_return",
                "search_site",
            ],
            vec![
                R::PaymentPolicy,
                R::ProductFeeds,
                R::FraudAndRefundGuardrails,
                R::LeadNurtureHandoff,
            ],
            universal.clone(),
            caps(&[
                (Hosting, 5_000),
                (Models, 10_000),
                (Advertising, 0),
                (Domains, 300),
                (Other, 2_000),
            ]),
            "Helpful and precise",
        ),
        SiteKind::Government => (
            "A government or public-sector site",
            vec![
                goal("services", "Complete services online", &["service_completion"]),
                goal(
                    "accuracy",
                    "Keep public information accurate and current",
                    &["notice_update_hours"],
                ),
            ],
            vec![
                kpi(
                    "service_completion",
                    "Service completion rate",
                    "services.completion_rate",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi(
                    "notice_update_hours",
                    "Hours to update a notice",
                    "notices.update_hours",
                    site_source(),
                    Down,
                ),
            ],
            vec![],
            public_and_core.clone(),
            vec![
                "submit_service_form",
                "book_appointment",
                "check_availability",
                "lookup_status",
                "list_notices",
                "search_site",
            ],
            vec![
                R::TwoPersonPublication,
                R::AccessibilityHardGate,
                R::AllLanguagesRequired,
                R::NoThirdPartyScripts,
                R::NoAdvertising,
                R::NoSales,
                R::NoProfitKpis,
                R::RecordsRetention { years: 7 },
                R::DataResidency,
                R::AiDisclosureOnEveryReply,
            ],
            with(
                universal.clone(),
                vec![
                    red_line(Action, "No third-party scripts or trackers on any page"),
                    red_line(Action, "No advertising and no profit targets"),
                    red_line(
                        Action,
                        "Keep superseded notices as records; delete no record within its retention period",
                    ),
                ],
            ),
            caps(&[(Hosting, 10_000), (Models, 10_000), (Domains, 500)]),
            "Plain, accurate and respectful",
        ),
        SiteKind::Nonprofit => (
            "A nonprofit or charity",
            vec![
                goal("donations", "Raise donations", &["donations", "donor_retention"]),
                goal("volunteers", "Recruit volunteers", &["volunteers"]),
                goal("events", "Fill events", &["event_signups"]),
            ],
            vec![
                kpi("donations", "Donations", "donations.total", SourceRef::Commerce, Up),
                kpi(
                    "donor_retention",
                    "Returning donors",
                    "donations.retention",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi(
                    "volunteers",
                    "Volunteer sign-ups",
                    "volunteers.signups",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi("event_signups", "Event sign-ups", "events.signups", SourceRef::Commerce, Up),
            ],
            vec![unsubscribes.clone()],
            all.clone(),
            vec!["donate", "volunteer", "register", "submit_inquiry", "search_site"],
            vec![R::DonorConsentAndReceipts, R::TaxReceiptFields, R::NoProfitKpis],
            with(
                universal.clone(),
                vec![red_line(Action, "Use donor details only within the consent the donor gave")],
            ),
            caps(&[(Hosting, 1_500), (Models, 3_000), (Advertising, 0), (Domains, 300)]),
            "Hopeful, honest and specific",
        ),
        SiteKind::Education => (
            "A school, college or training provider",
            vec![
                goal(
                    "admissions",
                    "Grow admissions enquiries",
                    &["admissions_enquiries", "open_day_bookings"],
                ),
                goal("information", "Answer families' questions", &["information_requests"]),
            ],
            vec![
                kpi(
                    "admissions_enquiries",
                    "Admissions enquiries",
                    "leads.admissions",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi(
                    "open_day_bookings",
                    "Open-day bookings",
                    "bookings.open_day",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi(
                    "information_requests",
                    "Prospectus requests",
                    "leads.prospectus",
                    SourceRef::Commerce,
                    Up,
                ),
            ],
            vec![],
            vec![Concierge, AgentDesk, Inbox, Steward, Growth, Guardian, Revenue],
            vec![
                "submit_inquiry",
                "check_availability",
                "book",
                "request_prospectus",
                "search_site",
            ],
            vec![R::Safeguarding, R::NoProfilingOfMinors, R::LearningPlatformsOutside],
            with(
                universal.clone(),
                vec![
                    red_line(
                        Action,
                        "Never contact a child directly; speak with parents, guardians or the school",
                    ),
                    red_line(Action, "No profiling of minors"),
                ],
            ),
            caps(&[(Hosting, 2_000), (Models, 4_000), (Domains, 300)]),
            "Welcoming and clear",
        ),
        SiteKind::Media => (
            "A newsroom or media site",
            vec![
                goal(
                    "readers",
                    "Grow readers and subscribers",
                    &["readers", "newsletter_subscribers", "subscriptions"],
                ),
                goal("corrections", "Correct fast and in the open", &["correction_hours"]),
            ],
            vec![
                kpi("readers", "Readers", "visits.readers", SourceRef::Beacon, Up),
                kpi(
                    "newsletter_subscribers",
                    "Newsletter subscribers",
                    "newsletter.subscribers",
                    SourceRef::Email,
                    Up,
                ),
                kpi(
                    "subscriptions",
                    "Paid subscriptions",
                    "subscriptions.active",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi(
                    "correction_hours",
                    "Hours to publish a correction",
                    "corrections.hours",
                    site_source(),
                    Down,
                ),
            ],
            vec![unsubscribes.clone()],
            vec![Concierge, Steward, Growth, Guardian, Revenue],
            vec!["subscribe", "subscribe_newsletter", "licensing_enquiry", "search_site"],
            vec![
                R::ContentUsageTerms,
                R::CorrectionsPolicy,
                R::NeverGeneratedNews,
                R::TwoPersonPublication,
            ],
            with(
                universal.clone(),
                vec![red_line(
                    Action,
                    "Never generate news; drafts only from the newsroom's own reporting",
                )],
            ),
            caps(&[(Hosting, 5_000), (Models, 5_000), (Advertising, 0), (Domains, 300)]),
            "Precise and fair",
        ),
        SiteKind::Health => (
            "A clinic or practitioner",
            vec![
                goal("appointments", "Fill appointments", &["appointment_bookings", "no_shows"]),
                goal("service", "Answer patients quickly", &["response_time"]),
            ],
            vec![
                kpi(
                    "appointment_bookings",
                    "Appointment bookings",
                    "bookings.appointments",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi("no_shows", "No-show rate", "bookings.no_show_rate", SourceRef::Commerce, Down),
                kpi(
                    "response_time",
                    "Hours to first reply",
                    "leads.response_hours",
                    SourceRef::Commerce,
                    Down,
                ),
            ],
            vec![],
            public_and_core.clone(),
            vec!["check_availability", "book_appointment", "practice_information", "search_site"],
            vec![R::NoClinicalAdvice, R::SpecialCategoryData, R::EmergencyRouting],
            with(
                universal.clone(),
                vec![
                    red_line(Action, "No clinical advice from any agent"),
                    red_line(
                        Action,
                        "Health data only with explicit consent, kept no longer than needed",
                    ),
                    red_line(Claim, "guaranteed cure"),
                    red_line(Claim, "miracle cure"),
                ],
            ),
            caps(&[(Hosting, 2_000), (Models, 4_000), (Domains, 300)]),
            "Calm, careful and kind",
        ),
        SiteKind::Community => (
            "A club, association or place of worship",
            vec![
                goal(
                    "attendance",
                    "Bring people together",
                    &["event_attendance", "members_reached"],
                ),
                goal("donations", "Receive donations", &["donations"]),
            ],
            vec![
                kpi(
                    "event_attendance",
                    "Event sign-ups",
                    "events.signups",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi(
                    "members_reached",
                    "Members reached",
                    "newsletter.subscribers",
                    SourceRef::Email,
                    Up,
                ),
                kpi("donations", "Donations", "donations.total", SourceRef::Commerce, Up),
            ],
            vec![],
            vec![Concierge, Steward, Growth, Guardian, Revenue],
            vec!["list_events", "register", "donate", "subscribe_newsletter", "search_site"],
            vec![R::LowBudget, R::WeeklyReviewOnly, R::NoAdvertising, R::NoProfitKpis],
            universal.clone(),
            caps(&[(Hosting, 500), (Models, 1_000), (Domains, 200)]),
            "Friendly and simple",
        ),
        SiteKind::Events => (
            "A conference or festival",
            vec![
                goal("registrations", "Fill the event", &["registrations", "ticket_sales"]),
                goal("attendees", "Answer attendees' questions", &["questions_answered"]),
            ],
            vec![
                kpi(
                    "registrations",
                    "Registrations",
                    "events.registrations",
                    SourceRef::Commerce,
                    Up,
                ),
                kpi("ticket_sales", "Ticket sales", "tickets.sold", SourceRef::Commerce, Up),
                kpi(
                    "questions_answered",
                    "Questions answered",
                    "conversations.answered_share",
                    SourceRef::Beacon,
                    Up,
                ),
            ],
            vec![unsubscribes.clone()],
            all.clone(),
            vec!["register", "buy_ticket", "get_schedule", "submit_inquiry", "search_site"],
            vec![R::DateDrivenFreshness, R::WindDownAfterEvent, R::LeadNurtureHandoff],
            with(
                universal.clone(),
                vec![red_line(
                    Action,
                    "After the event, wind the site down to an archive; sell nothing for a past date",
                )],
            ),
            caps(&[
                (Hosting, 2_000),
                (Models, 5_000),
                (Advertising, 0),
                (Domains, 300),
                (Other, 1_000),
            ]),
            "Energetic and exact about dates",
        ),
        other => {
            tracing::warn!(site.kind = ?other, "no template for this site kind; using the business template");
            return template(SiteKind::Business);
        }
    };
    goals.push(trust_goal);
    kpis.extend(trust_kpis);
    KindTemplate {
        kind,
        title,
        goals,
        kpis,
        guardrails,
        agents,
        capabilities,
        rules,
        red_lines,
        caps: budget,
        tone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn charter_for(kind: SiteKind) -> Charter {
        let id = Uuid::new_v4();
        let template = template(kind);
        let team = SiteTeam::new(
            adk_harness::website::SiteRef::new(id, "site", "https://site.example"),
            &template,
        );
        let mut inputs = CharterInputs::new(
            id,
            "Site",
            "user:owner",
            "A mission.",
            vec!["en".parse().unwrap()],
            "USD",
        );
        inputs
            .people
            .push(RoleHolder { principal: "user:publisher".into(), roles: vec![Role::Publisher] });
        template.draft_charter(&team, &inputs)
    }

    #[test]
    fn every_template_drafts_a_valid_charter_with_baseline_sources_and_no_targets() {
        for kind in SiteKind::ALL {
            let charter = charter_for(kind);
            charter.validate().unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            assert_eq!(charter.kind, kind);
            assert!(!charter.kpis.is_empty());
            for kpi in &charter.kpis {
                assert!(kpi.target.is_none() && kpi.baseline.is_none(), "{kind:?} {}", kpi.id);
            }
            for goal in &charter.goals {
                assert!(!goal.kpis.is_empty(), "{kind:?} goal {} has a KPI", goal.id);
            }
            assert!(charter.voice.disclosure.contains("AI agent"), "A-I17");
        }
    }

    #[test]
    fn non_commercial_kinds_carry_no_sales_or_profit_machinery() {
        for kind in [SiteKind::Personal, SiteKind::Government, SiteKind::Community] {
            let template = template(kind);
            let jobs: Vec<&str> = template.jobs().iter().map(|job| job.id).collect();
            assert!(!jobs.contains(&"advertising"), "{kind:?} runs advertising");
            // A community site's Revenue takes donations; the others sell nothing.
            if kind != SiteKind::Community {
                assert!(!jobs.contains(&"sell"), "{kind:?} sells");
            }
            assert!(!template.caps.contains_key(&SpendCategory::Advertising), "{kind:?}");
            for kpi in &template.kpis {
                for word in ["profit", "revenue", "orders", "sales"] {
                    assert!(!kpi.metric.0.contains(word), "{kind:?} measures {}", kpi.metric.0);
                }
            }
        }
    }

    #[test]
    fn the_agents_follow_the_kind_table() {
        use AgentRole::*;
        assert_eq!(template(SiteKind::Personal).agents, [Concierge, Steward, Growth, Guardian]);
        assert_eq!(template(SiteKind::Business).agents.len(), 8);
        assert_eq!(template(SiteKind::Ecommerce).agents.len(), 8);
        assert_eq!(template(SiteKind::Nonprofit).agents.len(), 8);
        assert_eq!(template(SiteKind::Events).agents.len(), 8);
        for kind in [SiteKind::Government, SiteKind::Health] {
            let agents = template(kind).agents;
            assert!(!agents.contains(&Revenue) && !agents.contains(&Finance), "{kind:?}");
        }
        assert!(template(SiteKind::Education).agents.contains(&Revenue));
        for kind in [SiteKind::Media, SiteKind::Community] {
            assert_eq!(template(kind).agents, [Concierge, Steward, Growth, Guardian, Revenue]);
        }
    }

    #[test]
    fn government_requires_two_people_every_language_and_no_trackers() {
        let template = template(SiteKind::Government);
        let chain = template
            .approval_chains()
            .into_iter()
            .find(|chain| chain.action == ActionClass::Publish)
            .unwrap();
        assert_eq!(chain.roles, [Role::Editor, Role::Publisher]);
        let mut config = SiteConfig::new(
            "Kaunti",
            "kaunti.go.ke",
            vec!["en".parse().unwrap(), "sw".parse().unwrap()],
        );
        template.configure_site(&mut config);
        assert_eq!(config.translation.required.len(), 2);
        assert!(config.translation.simultaneous);
        assert!(format!("{:?}", template.gate("https://kaunti.go.ke")).contains("extensions: 1"));

        let id = Uuid::new_v4();
        let team =
            SiteTeam::new(adk_harness::website::SiteRef::new(id, "k", "https://k"), &template);
        let alone = CharterInputs::new(
            id,
            "K",
            "user:clerk",
            "Serve residents.",
            vec!["en".parse().unwrap()],
            "KES",
        );
        let violations = template.draft_charter(&team, &alone).validate().unwrap_err().violations;
        assert!(
            violations.iter().any(|v| matches!(
                v,
                awp_types::CharterViolation::UnsatisfiableApprovalChain { .. }
            )),
            "a public body needs a second person to publish: {violations:?}"
        );
    }
}
