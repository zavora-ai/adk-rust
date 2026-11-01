//! Charter review as cards, one per section, signed by every required signer (spec 3.5 §7.6,
//! §7.18 stage 5, §8.2 "charter cards", A-I14, A-I23).
//!
//! The draft is the kind template's, filled from the interview. The owner reads each section
//! as a card and either says it looks right or changes it; a change asks for signatures anew,
//! because a signature is for the text that was read. Once every section has been read, each
//! person the approval chain for charter changes requires signs in their role — each a
//! distinct person, none an agent — on the charter's signing request in the site's approval
//! service, which the back office, the kernel's inbox and chat cards sign too; the version is
//! committed through the kernel's audited commit (`website.approval_chain.satisfied`,
//! `website.charter.committed`) with the request as the decision.

use std::collections::BTreeSet;

use adk_core::Result;
use adk_core::principal::SubjectRef;
use adk_kernel::charter::{CHARTER_ACTION, CharterVersion, RoleApproval};
use awp_types::{
    Charter, Direction, GuardrailAction, RedLine, RedLineKind, Role, RoleHolder, SpendCategory,
    Urgency,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::words::{self, money};
use super::{
    FieldKind, FieldSpec, FormValues, Line, Pill, Tone, WebsiteDesk, forbidden, invalid, kernel,
    optional, required, unavailable,
};

impl WebsiteDesk {
    pub(crate) async fn save_charter(&self) -> super::saved::SavedCharter {
        let state = self.inner.charter.lock().await;
        super::saved::SavedCharter {
            draft: state.draft.clone(),
            reviewed: state.reviewed.clone(),
            committed: state.committed.clone(),
        }
    }

    pub(crate) async fn restore_charter(&self, saved: super::saved::SavedCharter) {
        let mut state = self.inner.charter.lock().await;
        state.draft = saved.draft;
        state.reviewed = saved.reviewed;
        state.committed = saved.committed;
    }
}

/// The sections of a charter, in the order the cards show them.
pub const SECTIONS: [&str; 11] = [
    "purpose",
    "goals",
    "guardrails",
    "authority",
    "thresholds",
    "budget",
    "voice",
    "never",
    "reporting",
    "people",
    "public",
];

#[derive(Default)]
pub(crate) struct State {
    pub(crate) draft: Option<Charter>,
    pub(crate) reviewed: BTreeSet<String>,
    pub(crate) committed: Option<CharterVersion>,
}

/// One section of the charter as a card (spec 3.5 §7.18 stage 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SectionCard {
    /// The section's id.
    pub id: String,
    /// Its heading.
    pub title: String,
    /// What it decides, in one line.
    pub summary: String,
    /// What the draft says.
    pub lines: Vec<Line>,
    /// A rule that applies to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Whether the owner has read it.
    pub reviewed: bool,
    /// What can be changed here; empty when nothing can.
    pub fields: Vec<FieldSpec>,
}

/// A signature the charter needs (spec 3.5 A-I23).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignerLine {
    /// The role, as sent back to sign.
    pub role: String,
    /// The role's name.
    pub label: String,
    /// The people who hold it.
    pub holders: Vec<String>,
    /// Who signed in it, if anyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
    /// When.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The committed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommittedView {
    /// The version.
    pub version: u32,
    /// When it took effect.
    pub at: chrono::DateTime<chrono::Utc>,
    /// Who signed it.
    pub signed_by: Vec<String>,
}

/// The charter under review (spec 3.5 §7.18 stage 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CharterReview {
    /// The version it becomes.
    pub version: u32,
    /// The site kind's template it started from.
    pub kind: String,
    /// Where it stands.
    pub status: Pill,
    /// One card per section.
    pub sections: Vec<SectionCard>,
    /// Sections read so far.
    pub reviewed: usize,
    /// The signatures it needs, in the chain's order.
    pub signers: Vec<SignerLine>,
    /// The roles the viewer may sign in now.
    pub can_sign: Vec<String>,
    /// Why it cannot be signed yet.
    pub problems: Vec<String>,
    /// The committed version, once every signer has signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed: Option<CommittedView>,
}

/// A change to the charter under review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CharterCommand {
    /// The section looks right.
    Review {
        /// The section.
        section: String,
    },
    /// Change the section's fields; clears every signature.
    Edit {
        /// The section.
        section: String,
        /// The new values, by field id.
        values: FormValues,
    },
    /// Sign in `role` (A-I23).
    Sign {
        /// The role, such as `owner`.
        role: String,
    },
}

fn hours(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 86_400 && secs.is_multiple_of(86_400) {
        let days = secs / 86_400;
        if days == 1 { "a day".into() } else { format!("{days} days") }
    } else {
        format!("{} hours", secs / 3_600)
    }
}

fn channels(charter: &Charter, urgency: Urgency) -> String {
    let names: Vec<String> = charter
        .reporting
        .routes
        .iter()
        .filter(|route| route.urgency == urgency)
        .flat_map(|route| route.channels.iter().map(|c| c.as_str().to_string()))
        .collect();
    if names.is_empty() { "Not set".into() } else { names.join(", ") }
}

fn job_title(job: &str) -> String {
    crate::jobs::job(job).map_or_else(|| job.replace('-', " "), |spec| spec.title.to_string())
}

fn chain_words(roles: &[Role]) -> String {
    let names: Vec<&str> = roles.iter().map(|role| words::role(*role)).collect();
    names.join(", then ").to_lowercase()
}

/// The cards of `charter`.
pub(crate) fn cards(
    charter: &Charter,
    reviewed: &BTreeSet<String>,
    estimate: Option<&crate::onboarding::cost::RunningCostEstimate>,
) -> Vec<SectionCard> {
    let currency = charter.budget.currency.clone();
    let exponent = words::minor_exponent(&currency);
    let card = |id: &str, title: &str, summary: &str, lines: Vec<Line>| SectionCard {
        id: id.into(),
        title: title.into(),
        summary: summary.into(),
        lines,
        note: None,
        reviewed: reviewed.contains(id),
        fields: Vec::new(),
    };
    let mut out = Vec::new();

    let mut purpose = card(
        "purpose",
        "What the site is for",
        "The mission, in your words, and the pillars of the strategy",
        vec![
            Line::new("Mission", charter.mission.clone()),
            Line::new(
                "Strategy",
                charter.strategy.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(" · "),
            ),
        ],
    );
    purpose.fields = vec![
        FieldSpec::new("mission", "Mission", FieldKind::LongText)
            .required()
            .value(&charter.mission),
    ];
    out.push(purpose);

    let mut goals = card(
        "goals",
        "Goals and how they are measured",
        "What the site works towards, and the measure behind each goal",
        Vec::new(),
    );
    for goal in &charter.goals {
        let measures: Vec<String> = goal
            .kpis
            .iter()
            .filter_map(|id| charter.kpis.iter().find(|k| &k.id == id))
            .map(|k| k.name.clone())
            .collect();
        goals
            .lines
            .push(Line::new(goal.name.clone(), format!("Measured by {}", measures.join(", "))));
    }
    for kpi in &charter.kpis {
        let direction = match kpi.direction {
            Direction::Down => "lower is better",
            _ => "higher is better",
        };
        let target = match (&kpi.baseline, &kpi.target) {
            (Some(_), Some(target)) => format!("target {}", words::number(target.value)),
            _ => "no target until it has a baseline".into(),
        };
        goals.lines.push(Line::new(
            kpi.name.clone(),
            format!("From {}, {direction}; {target}", kpi.source.to_string().replace('_', " ")),
        ));
    }
    goals.note = Some(
        "Each measure runs for 14 days to find its baseline. Targets are yours to set after \
         that; the site only proposes ranges."
            .into(),
    );
    out.push(goals);

    let mut guardrails = card(
        "guardrails",
        "Guardrails",
        "Limits that stop a job, whatever the goals say",
        charter
            .guardrails
            .iter()
            .map(|g| {
                let breach = match g.breach {
                    awp_types::Breach::Below => "below",
                    _ => "above",
                };
                let action = match g.action {
                    GuardrailAction::PauseJob => "pause the job and tell you",
                    _ => "tell you",
                };
                Line::new(
                    g.metric.0.replace(['.', '_'], " "),
                    format!(
                        "If it goes {breach} {} over {}, {action}",
                        words::number(g.threshold),
                        hours(g.window)
                    ),
                )
            })
            .collect(),
    );
    guardrails.note = Some("Guardrails override growth targets.".into());
    out.push(guardrails);

    let mut authority = card(
        "authority",
        "What the site may do alone",
        "How much each job does without asking you first",
        charter
            .authority
            .iter()
            .map(|a| Line::new(job_title(&a.job), words::level(a.level)))
            .collect(),
    );
    authority.note = Some(
        "Every job starts by suggesting or just looking for the first week. Only you can let a \
         job do more; the site can only do less."
            .into(),
    );
    out.push(authority);
    out.push(thresholds_card(charter, card("thresholds", "When the site asks you", "When a lead comes to you, how long the site's own pages wait, and the amounts that wait for your yes", Vec::new())));

    let mut budget = card(
        "budget",
        "Budget",
        "What the site may spend each month, by category",
        charter
            .budget
            .caps
            .iter()
            .map(|(category, cap)| {
                Line::new(words::category(*category), format!("{} a month", money(*cap, &currency)))
            })
            .collect(),
    );
    if let Some(estimate) = estimate {
        let fits = estimate.fits(&charter.budget);
        budget.lines.push(Line::new(
            "Estimated running cost",
            format!(
                "{} a month — {}",
                money(estimate.total, &currency),
                if fits { "inside the budget" } else { "over the budget" }
            ),
        ));
    }
    budget.note = Some(
        "The site never raises a limit, adds a payment method or opens an account; those are \
         yours."
            .into(),
    );
    budget.fields = [
        SpendCategory::Hosting,
        SpendCategory::Models,
        SpendCategory::Domains,
        SpendCategory::Advertising,
        SpendCategory::Other,
    ]
    .into_iter()
    .map(|category| {
        let cap = charter.budget.caps.get(&category).copied().unwrap_or(0);
        let value = format!("{:.*}", exponent.max(0) as usize, cap as f64 / 10f64.powi(exponent));
        FieldSpec::new(
            category_id(category),
            &format!("{} ({currency} a month)", words::category(category)),
            FieldKind::Number,
        )
        .value(value)
    })
    .collect();
    out.push(budget);

    let mut voice = card(
        "voice",
        "How the site speaks",
        "Tone, languages and the line that says an AI agent wrote a message",
        vec![
            Line::new("Tone", charter.voice.tone.clone()),
            Line::new(
                "Languages",
                charter.voice.languages.iter().map(|l| l.as_str()).collect::<Vec<_>>().join(", "),
            ),
            Line::new("On every message it writes", charter.voice.disclosure.clone()),
        ],
    );
    voice.fields =
        vec![FieldSpec::new("tone", "Tone", FieldKind::Text).required().value(&charter.voice.tone)];
    out.push(voice);

    let mut never = card(
        "never",
        "Never",
        "Claims, topics and actions the site must never touch",
        charter
            .red_lines
            .iter()
            .map(|line| {
                let kind = match line.kind {
                    RedLineKind::Claim => "Never claim",
                    RedLineKind::Topic => "Never discuss",
                    _ => "Never do",
                };
                Line::new(kind, line.text.clone())
            })
            .collect(),
    );
    never.fields = vec![FieldSpec::new("never", "One per line", FieldKind::LongText).value(
        charter.red_lines.iter().map(|line| line.text.clone()).collect::<Vec<_>>().join("\n"),
    )];
    out.push(never);

    let quiet = charter.reporting.quiet_hours.as_ref();
    let mut reporting = card(
        "reporting",
        "Reports and alarms",
        "When the site reports, and how it reaches you when something is urgent",
        vec![
            Line::new("Review", "Every week, on the web home and on your channel"),
            Line::new("Urgent", channels(charter, Urgency::P1)),
            Line::new("Within the hour", channels(charter, Urgency::P2)),
            Line::new(
                "Quiet time",
                quiet.map_or_else(
                    || "None".to_string(),
                    |q| {
                        format!(
                            "{}–{} {}",
                            q.start.format("%H:%M"),
                            q.end.format("%H:%M"),
                            q.timezone
                        )
                    },
                ),
            ),
        ],
    );
    reporting.note =
        Some("Urgent alarms reach you even in quiet time, until you acknowledge them.".into());
    reporting.fields = vec![
        FieldSpec::new("quietStart", "Quiet time starts", FieldKind::Time)
            .value(quiet.map(|q| q.start.format("%H:%M").to_string()).unwrap_or_default()),
        FieldSpec::new("quietEnd", "Quiet time ends", FieldKind::Time)
            .value(quiet.map(|q| q.end.format("%H:%M").to_string()).unwrap_or_default()),
    ];
    out.push(reporting);

    let mut people = card(
        "people",
        "People and approvals",
        "Who holds each role, and who must approve what",
        Vec::new(),
    );
    for holder in &charter.governance.roles {
        let roles: Vec<&str> = holder.roles.iter().map(|r| words::role(*r)).collect();
        people.lines.push(Line::new(holder.principal.clone(), roles.join(", ")));
    }
    for chain in &charter.governance.approval_chains {
        let action = match chain.action.as_str() {
            "charter_change" => "Charter changes".to_string(),
            "publish" => "Publishing".to_string(),
            other => other.replace('_', " "),
        };
        people
            .lines
            .push(Line::new(action, format!("Approved by the {}", chain_words(&chain.roles))));
    }
    people.note = Some(
        "Each approval in a chain is a different person, and the person who proposes a change \
         cannot approve it. No agent holds a role."
            .into(),
    );
    people.fields = vec![
        FieldSpec::new("person", "Add a person", FieldKind::Text)
            .hint("The account they sign in with"),
        FieldSpec::new("role", "In the role", FieldKind::Choice).options(
            [Role::Publisher, Role::Editor, Role::Approver, Role::Owner, Role::Auditor]
                .into_iter()
                .map(|role| (words::role_id(role).to_string(), words::role(role).to_string())),
        ),
    ];
    out.push(people);

    let mut public = card(
        "public",
        "What the site publishes about itself",
        "Usage terms for AI and search, its policies, consent and how to reach you",
        vec![
            Line::new(
                "Use by AI and search",
                charter.public.usage.clone().unwrap_or_else(|| "The defaults".into()),
            ),
            Line::new(
                "Policies",
                charter
                    .public
                    .policies
                    .iter()
                    .map(|p| p.name.replace('-', " "))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Line::new(
                "Consent asked for",
                if charter.public.consent_scopes.is_empty() {
                    "Nothing yet".to_string()
                } else {
                    charter
                        .public
                        .consent_scopes
                        .iter()
                        .map(|s| s.as_str().replace('_', " "))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ),
            Line::new(
                "Contact",
                charter.public.contact.email.clone().unwrap_or_else(|| "The contact page".into()),
            ),
        ],
    );
    public.fields = vec![
        FieldSpec::new("email", "Public contact email", FieldKind::Email)
            .value(charter.public.contact.email.clone().unwrap_or_default()),
    ];
    out.push(public);
    out
}

/// The amounted capabilities `charter`'s kind declares, which an amount threshold can hold
/// (spec 3.5 §7.4).
fn amounted(charter: &Charter) -> Vec<&'static str> {
    crate::kinds::template(charter.kind)
        .capabilities
        .into_iter()
        .filter(|name| crate::packs::AMOUNTED.contains(name))
        .collect()
}

/// A capability's name in plain words: `place_order` is "Place order".
fn capability_words(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// The owner's settings as a card (spec 3.5 §5 S8, S10, §7.4, §7.6): the charter's
/// `handoffThreshold`, `publishDelayHours` and `amountThresholds`, or the site's defaults.
fn thresholds_card(charter: &Charter, mut card: SectionCard) -> SectionCard {
    let currency = &charter.budget.currency;
    let exponent = words::minor_exponent(currency);
    let default = crate::leads::DEFAULT_HANDOFF_THRESHOLD;
    card.lines.push(Line::new(
        "A lead comes to you",
        match charter.handoff_threshold {
            Some(threshold) => format!("When it scores {threshold} of 100"),
            None => format!("When it scores {} of 100, the site's default", words::number(default)),
        },
    ));
    card.lines.push(Line::new(
        "Pages the site writes on its own",
        match charter.publish_delay_hours {
            Some(hours) => format!("Wait {hours} hours before they can go live"),
            None => format!(
                "Wait {} hours before they can go live, the site's default",
                crate::publish_delay::DEFAULT_PUBLISH_DELAY_HOURS
            ),
        },
    ));
    let capabilities = amounted(charter);
    for name in &capabilities {
        let value = match charter.amount_thresholds.get(*name) {
            Some(amount) => format!("Above {} waits for you", money(*amount, currency)),
            None => "Not set: the site's defaults apply".to_string(),
        };
        card.lines.push(Line::new(capability_words(name), value));
    }
    card.note = Some(
        "A setting left empty keeps the site's default. Amount limits change what the site \
         publishes to agents, so they apply when the site next starts."
            .into(),
    );
    let mut fields = vec![
        FieldSpec::new("handoff", "Hand a lead to you at this score (0 to 100)", FieldKind::Number)
            .value(charter.handoff_threshold.map(|t| t.to_string()).unwrap_or_default()),
        FieldSpec::new("publishDelay", "Hours the site's own pages wait", FieldKind::Number)
            .value(charter.publish_delay_hours.map(|h| h.to_string()).unwrap_or_default()),
    ];
    for name in capabilities {
        let value = charter.amount_thresholds.get(name).map_or_else(String::new, |amount| {
            format!("{:.*}", exponent.max(0) as usize, *amount as f64 / 10f64.powi(exponent))
        });
        fields.push(
            FieldSpec::new(
                &format!("above_{name}"),
                &format!("{} above ({currency}) waits for you", capability_words(name)),
                FieldKind::Number,
            )
            .value(value),
        );
    }
    card.fields = fields;
    card
}

/// Applies the `thresholds` card's `values` to `charter`; an empty field clears its setting.
fn apply_thresholds(charter: &mut Charter, values: &FormValues) -> Result<()> {
    charter.handoff_threshold = match optional(values, "handoff") {
        None => None,
        Some(text) => Some(
            text.trim()
                .parse::<u8>()
                .ok()
                .filter(|threshold| *threshold <= 100)
                .ok_or_else(|| invalid("website.charter.handoff", "the score is 0 to 100"))?,
        ),
    };
    charter.publish_delay_hours = match optional(values, "publishDelay") {
        None => None,
        Some(text) => Some(text.trim().parse::<u32>().map_err(|_| {
            invalid("website.charter.publish_delay", "the wait is a whole number of hours")
        })?),
    };
    let exponent = words::minor_exponent(&charter.budget.currency);
    for name in amounted(charter) {
        match optional(values, &format!("above_{name}")) {
            None => {
                charter.amount_thresholds.remove(name);
            }
            Some(text) => {
                let amount: f64 = text.replace(',', "").trim().parse().map_err(|_| {
                    invalid(
                        "website.charter.amount",
                        format!("{} is an amount such as 250.00", capability_words(name)),
                    )
                })?;
                if !(0.0..=1e12).contains(&amount) {
                    return Err(invalid("website.charter.amount", "an amount is zero or more"));
                }
                let minor = (amount * 10f64.powi(exponent)).round() as u64;
                charter.amount_thresholds.insert(name.to_string(), minor);
            }
        }
    }
    Ok(())
}

fn category_id(category: SpendCategory) -> &'static str {
    match category {
        SpendCategory::Hosting => "hosting",
        SpendCategory::Models => "models",
        SpendCategory::Domains => "domains",
        SpendCategory::Advertising => "advertising",
        _ => "other",
    }
}

fn parse_time(text: &str, label: &str) -> Result<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(text.trim(), "%H:%M")
        .map_err(|_| invalid("website.charter.time", format!("{label} is a time such as 21:00")))
}

/// Applies `values` to `section` of `charter`.
pub(crate) fn apply_edit(charter: &mut Charter, section: &str, values: &FormValues) -> Result<()> {
    match section {
        "purpose" => charter.mission = required(values, "mission", "The mission")?.to_string(),
        "voice" => charter.voice.tone = required(values, "tone", "The tone")?.to_string(),
        "budget" => {
            let exponent = words::minor_exponent(&charter.budget.currency);
            for category in [
                SpendCategory::Hosting,
                SpendCategory::Models,
                SpendCategory::Domains,
                SpendCategory::Advertising,
                SpendCategory::Other,
            ] {
                let Some(text) = optional(values, category_id(category)) else { continue };
                let amount: f64 = text.replace(',', "").parse().map_err(|_| {
                    invalid(
                        "website.charter.amount",
                        format!("{} is an amount such as 25.00", words::category(category)),
                    )
                })?;
                if !(0.0..=1e12).contains(&amount) {
                    return Err(invalid("website.charter.amount", "an amount is zero or more"));
                }
                let minor = (amount * 10f64.powi(exponent)).round() as u64;
                if minor == 0 {
                    charter.budget.caps.remove(&category);
                } else {
                    charter.budget.caps.insert(category, minor);
                }
            }
        }
        "never" => {
            let lines: Vec<&str> = values
                .get("never")
                .map(|text| text.lines().map(str::trim).filter(|line| !line.is_empty()).collect())
                .unwrap_or_default();
            let mut kept: Vec<RedLine> = Vec::new();
            for line in lines {
                let kind = charter
                    .red_lines
                    .iter()
                    .find(|held| held.text == line)
                    .map_or(RedLineKind::Claim, |held| held.kind);
                kept.push(RedLine { kind, text: line.to_string() });
            }
            charter.red_lines = kept;
        }
        "reporting" => {
            let start = optional(values, "quietStart");
            let end = optional(values, "quietEnd");
            match (start, end) {
                (None, None) => charter.reporting.quiet_hours = None,
                (Some(start), Some(end)) => {
                    let start = parse_time(start, "The start")?;
                    let end = parse_time(end, "The end")?;
                    let timezone = charter
                        .reporting
                        .quiet_hours
                        .as_ref()
                        .map_or_else(|| "UTC".to_string(), |q| q.timezone.clone());
                    charter.reporting.quiet_hours =
                        Some(awp_types::QuietHours { start, end, timezone });
                }
                _ => {
                    return Err(invalid(
                        "website.charter.quiet",
                        "quiet time needs both a start and an end",
                    ));
                }
            }
        }
        "people" => {
            let person = required(values, "person", "The person's account")?;
            let role = optional(values, "role")
                .and_then(words::role_from)
                .ok_or_else(|| invalid("website.charter.role", "choose a role"))?;
            if adk_kernel::charter::is_agent_principal(person) {
                return Err(forbidden(
                    "website.charter.agent_role",
                    format!("{person} is an agent: no agent holds a role (spec 3.5 §7.6)"),
                ));
            }
            match charter.governance.roles.iter_mut().find(|held| held.principal == person) {
                Some(held) if !held.roles.contains(&role) => held.roles.push(role),
                Some(_) => {}
                None => charter
                    .governance
                    .roles
                    .push(RoleHolder { principal: person.to_string(), roles: vec![role] }),
            }
        }
        "public" => charter.public.contact.email = optional(values, "email").map(str::to_string),
        "thresholds" => apply_thresholds(charter, values)?,
        "goals" | "guardrails" | "authority" => {
            return Err(invalid(
                "website.charter.fixed",
                "this section changes later: targets after a baseline, levels by promotion",
            ));
        }
        other => {
            return Err(invalid("website.charter.section", format!("no section '{other}'")));
        }
    }
    Ok(())
}

/// The chain's signer lines with `signatures` placed on them, each person once.
fn signer_lines(
    charter: &Charter,
    governing: &awp_types::Governance,
    signatures: &[RoleApproval],
) -> Vec<SignerLine> {
    let roles = governing
        .approval_chains
        .iter()
        .find(|chain| chain.action == CHARTER_ACTION)
        .map(|chain| chain.roles.clone())
        .unwrap_or_else(|| vec![Role::Owner]);
    let _ = charter;
    let mut used: Vec<&str> = Vec::new();
    roles
        .iter()
        .map(|role| {
            let signature = signatures
                .iter()
                .find(|s| s.role == *role && !used.contains(&s.principal.id.as_str()));
            if let Some(signature) = signature {
                used.push(signature.principal.id.as_str());
            }
            SignerLine {
                role: words::role_id(*role).into(),
                label: words::role(*role).into(),
                holders: governing.holders(*role).map(str::to_string).collect(),
                signed_by: signature.map(|s| s.principal.id.clone()),
                signed_at: signature.map(|s| s.at),
            }
        })
        .collect()
}

impl WebsiteDesk {
    /// The governance that approves the next charter version: the charter in force, or the
    /// draft's for a site's first charter (spec 3.5 §7.6).
    async fn governing(&self, draft: &Charter) -> Result<awp_types::Governance> {
        if let Some(charters) = &self.inner.charters
            && let Some(current) = charters.current(self.inner.site.id).await.map_err(kernel)?
        {
            return Ok(current.charter.governance);
        }
        Ok(draft.governance.clone())
    }

    /// The charter under review as `person` sees it, `None` before a draft exists (spec 3.5
    /// §7.18 stage 5).
    ///
    /// # Errors
    ///
    /// Returns the charter store's error.
    pub async fn charter_review(&self, person: &str) -> Result<Option<CharterReview>> {
        let state = self.inner.charter.lock().await;
        let Some(draft) = state.draft.clone() else { return Ok(None) };
        let reviewed = state.reviewed.clone();
        let mut committed = state.committed.clone();
        drop(state);
        let runtime = self.runtime().await;
        let mut request = runtime.charter_request(&draft).await?;
        // People may have signed on another surface — the back office, the kernel's inbox, a
        // chat card — so a request they completed is committed now.
        if committed.is_none() && request.is_some() {
            for version in runtime.settle_charters().await? {
                self.adopt_charter(&version).await?;
            }
            committed = self.inner.charter.lock().await.committed.clone();
            if committed.is_none() {
                request = runtime.charter_request(&draft).await?;
            }
        }
        let signatures = request.map(|request| request.signatures).unwrap_or_default();
        let estimate = self.estimate(&draft).await;
        let sections = cards(&draft, &reviewed, estimate.as_ref());
        let governing = self.governing(&draft).await?;
        let signers = signer_lines(&draft, &governing, &signatures);
        let mut problems: Vec<String> = match draft.validate() {
            Ok(()) => Vec::new(),
            Err(error) => error.violations.iter().map(ToString::to_string).collect(),
        };
        if reviewed.len() < SECTIONS.len() {
            problems.push(format!(
                "Read every section first: {} of {} done",
                reviewed.len(),
                SECTIONS.len()
            ));
        }
        let can_sign = if committed.is_some() || !problems.is_empty() {
            Vec::new()
        } else {
            signers
                .iter()
                .filter(|line| line.signed_by.is_none())
                .filter(|line| line.holders.iter().any(|h| h == person))
                .filter(|_| !signatures.iter().any(|s| s.principal.id == person))
                .map(|line| line.role.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        let status = match (&committed, signatures.is_empty()) {
            (Some(_), _) => Pill::new("Signed", Tone::Good),
            (None, false) => Pill::new("Waiting for signatures", Tone::Waiting),
            (None, true) => Pill::new("Draft", Tone::Neutral),
        };
        Ok(Some(CharterReview {
            version: committed.as_ref().map_or(draft.version, CharterVersion::version),
            kind: crate::kinds::template(draft.kind).title.to_string(),
            status,
            reviewed: reviewed.len(),
            sections,
            signers,
            can_sign,
            problems,
            committed: committed.map(|version| CommittedView {
                version: version.version(),
                at: version.committed_at,
                signed_by: version
                    .decision
                    .approvals
                    .iter()
                    .map(|approval| approval.principal.id.clone())
                    .collect(),
            }),
        }))
    }

    /// Carries out `command` on the charter under review for `person` (spec 3.5 §7.18
    /// stage 5, A-I23). Edits come from the onboarding owner; a signature from any person the
    /// governing approval chain names, in a role they hold. Returns the committed version
    /// once every required signer has signed.
    ///
    /// # Errors
    ///
    /// `NotFound` without a draft; `Forbidden` for a person who may not edit or sign;
    /// `InvalidInput` for an unknown section, a bad value, or signing before every section
    /// was read or while the draft is invalid; the charter store's errors.
    pub async fn charter_command(
        &self,
        person: &str,
        command: CharterCommand,
    ) -> Result<Option<CharterVersion>> {
        let result = self.charter_command_unsaved(person, command).await;
        if result.is_ok() {
            self.persist().await;
        }
        result
    }

    async fn charter_command_unsaved(
        &self,
        person: &str,
        command: CharterCommand,
    ) -> Result<Option<CharterVersion>> {
        self.inner.charters.as_ref().ok_or_else(|| unavailable("The charter store"))?;
        let mut state = self.inner.charter.lock().await;
        if state.committed.is_some() {
            return Err(invalid(
                "website.charter.committed",
                "this version is signed; a change is a new version",
            ));
        }
        let Some(draft) = state.draft.clone() else {
            return Err(super::not_found(
                "website.charter.no_draft",
                "there is no charter draft yet: it is drafted when the interview is done",
            ));
        };
        match command {
            CharterCommand::Review { section } => {
                self.require(person, &[Role::Owner], "Reading the charter for signature").await?;
                if !SECTIONS.contains(&section.as_str()) {
                    return Err(invalid(
                        "website.charter.section",
                        format!("no section '{section}'"),
                    ));
                }
                state.reviewed.insert(section);
                Ok(None)
            }
            CharterCommand::Edit { section, values } => {
                self.require(person, &[Role::Owner], "Changing the charter").await?;
                let mut draft = draft;
                apply_edit(&mut draft, &section, &values)?;
                state.draft = Some(draft);
                state.reviewed.insert(section);
                Ok(None)
            }
            CharterCommand::Sign { role } => {
                let role = words::role_from(&role)
                    .ok_or_else(|| invalid("website.charter.role", format!("no role '{role}'")))?;
                if adk_kernel::charter::is_agent_principal(person) {
                    return Err(forbidden(
                        "website.charter.agent_signer",
                        format!("{person} is not a person: no agent signs a charter (A-I23)"),
                    ));
                }
                if state.reviewed.len() < SECTIONS.len() {
                    return Err(invalid(
                        "website.charter.unread",
                        "read every section before signing",
                    ));
                }
                if let Err(error) = draft.validate() {
                    return Err(invalid("website.charter.invalid", error.to_string()));
                }
                let governing = self.governing(&draft).await?;
                if !governing.holds(person, role) {
                    return Err(forbidden(
                        "website.charter.not_a_signer",
                        format!(
                            "{person} does not hold the {} role",
                            words::role(role).to_lowercase()
                        ),
                    ));
                }
                let runtime = self.runtime().await;
                // The Steward drafts the charter from the interview, so it asks; no agent signs.
                let steward = self.inner.site.principal("steward");
                let request = runtime.request_charter(&draft, None, &steward).await?;
                let signature = RoleApproval::new(role, SubjectRef::human(person), self.now());
                let Some(version) = runtime.sign_charter(&request.id, signature).await? else {
                    return Ok(None);
                };
                drop(state);
                self.adopt_charter(&version).await?;
                Ok(Some(version))
            }
        }
    }

    /// Takes `version` as the committed charter: the desk shows it signed, and its jobs join
    /// the autonomy ladder.
    async fn adopt_charter(&self, version: &CharterVersion) -> Result<()> {
        if let Some(ladder) = &self.inner.ladder {
            crate::jobs::register(ladder, &version.charter).await?;
        }
        self.inner.charter.lock().await.committed = Some(version.clone());
        Ok(())
    }

    /// The running-cost estimate of `charter`'s team, when the desk knows what a month
    /// looks like (spec 3.5 §7.22).
    pub(crate) async fn estimate(
        &self,
        charter: &Charter,
    ) -> Option<crate::onboarding::cost::RunningCostEstimate> {
        let inputs = self.inner.cost.as_ref()?;
        let template = crate::kinds::template(charter.kind);
        let team = crate::team::SiteTeam::new(self.inner.site.clone(), &template);
        Some(crate::onboarding::cost::estimate(&team, inputs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{CharterInputs, template};
    use crate::team::SiteTeam;
    use adk_harness::website::SiteRef;
    use awp_types::SiteKind;

    fn draft(kind: SiteKind) -> Charter {
        let id = uuid::Uuid::new_v4();
        let template = template(kind);
        let team = SiteTeam::new(SiteRef::new(id, "bakery", "https://bakery.example"), &template);
        let inputs = CharterInputs::new(
            id,
            "Juniper Bakery",
            "amina",
            "Fresh bread every morning.",
            vec!["en".parse().unwrap()],
            "KES",
        );
        template.draft_charter(&team, &inputs)
    }

    #[test]
    fn every_section_is_a_card_in_order() {
        let charter = draft(SiteKind::Business);
        let cards = cards(&charter, &BTreeSet::new(), None);
        let ids: Vec<&str> = cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, SECTIONS);
        let goals = cards.iter().find(|c| c.id == "goals").unwrap();
        assert!(
            goals
                .lines
                .iter()
                .filter(|l| charter.kpis.iter().any(|k| k.name == l.label))
                .all(|l| l.value.contains("no target until it has a baseline")),
            "A-I14: {goals:?}"
        );
    }

    #[test]
    fn edits_change_only_their_section_and_keep_the_charter_valid() {
        let mut charter = draft(SiteKind::Business);
        let values = FormValues::from([("models".to_string(), "1,500.00".to_string())]);
        apply_edit(&mut charter, "budget", &values).unwrap();
        assert_eq!(charter.budget.caps[&SpendCategory::Models], 150_000);
        apply_edit(
            &mut charter,
            "never",
            &FormValues::from([(
                "never".to_string(),
                "Never promise same-day delivery\n".to_string(),
            )]),
        )
        .unwrap();
        assert_eq!(charter.red_lines.len(), 1);
        assert!(charter.validate().is_ok());
        assert!(
            apply_edit(&mut charter, "goals", &FormValues::new()).is_err(),
            "targets wait for a baseline"
        );
        let agent = FormValues::from([
            ("person".to_string(), "urn:adk:agent:bakery/steward".to_string()),
            ("role".to_string(), "publisher".to_string()),
        ]);
        assert!(apply_edit(&mut charter, "people", &agent).is_err(), "no agent holds a role");
    }

    /// The owner's settings show on their card, defaults included, and an edit sets them as
    /// charter members (spec 3.5 §5 S8, S10, §7.4, §7.6).
    #[test]
    fn the_thresholds_card_shows_and_sets_the_owners_settings() {
        let mut charter = draft(SiteKind::Ecommerce);
        let card = |charter: &Charter| {
            cards(charter, &BTreeSet::new(), None)
                .into_iter()
                .find(|card| card.id == "thresholds")
                .unwrap()
        };
        let line = |card: &SectionCard, label: &str| {
            card.lines.iter().find(|line| line.label == label).unwrap().value.clone()
        };
        let before = card(&charter);
        assert_eq!(
            line(&before, "A lead comes to you"),
            "When it scores 70 of 100, the site's default"
        );
        assert!(line(&before, "Pages the site writes on its own").contains("24 hours"));
        assert_eq!(line(&before, "Place order"), "Not set: the site's defaults apply");
        assert!(before.fields.iter().any(|field| field.id == "above_place_order"));

        let values = FormValues::from([
            ("handoff".to_string(), "60".to_string()),
            ("publishDelay".to_string(), "48".to_string()),
            ("above_place_order".to_string(), "200.00".to_string()),
        ]);
        apply_edit(&mut charter, "thresholds", &values).unwrap();
        assert_eq!((charter.handoff_threshold, charter.publish_delay_hours), (Some(60), Some(48)));
        assert_eq!(charter.amount_thresholds.get("place_order"), Some(&20_000));
        assert!(charter.validate().is_ok());
        let after = card(&charter);
        assert_eq!(line(&after, "A lead comes to you"), "When it scores 60 of 100");
        assert_eq!(
            line(&after, "Pages the site writes on its own"),
            "Wait 48 hours before they can go live"
        );
        assert_eq!(line(&after, "Place order"), "Above KES 200.00 waits for you");

        let too_high = FormValues::from([("handoff".to_string(), "120".to_string())]);
        assert!(apply_edit(&mut charter, "thresholds", &too_high).is_err());
        apply_edit(&mut charter, "thresholds", &FormValues::new()).unwrap();
        assert_eq!(
            (
                charter.handoff_threshold,
                charter.publish_delay_hours,
                charter.amount_thresholds.len()
            ),
            (None, None, 0),
            "an empty field keeps the site's default"
        );
    }

    #[test]
    fn a_two_person_chain_names_a_signer_per_role() {
        let mut charter = draft(SiteKind::Government);
        charter.governance.approval_chains[0].roles = vec![Role::Owner, Role::Owner];
        charter
            .governance
            .roles
            .push(RoleHolder { principal: "baraka".into(), roles: vec![Role::Owner] });
        let signatures =
            vec![RoleApproval::new(Role::Owner, SubjectRef::human("amina"), chrono::Utc::now())];
        let lines = signer_lines(&charter, &charter.governance.clone(), &signatures);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].signed_by.as_deref(), Some("amina"));
        assert_eq!(lines[1].signed_by, None, "one person signs once");
    }
}
