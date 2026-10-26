//! Onboarding and self-building (spec 3.5 §7.18, §7.17, D-12, A-I18, A-I19, A-I20, A-I21).
//!
//! Onboarding runs as a plan graph in the back office, led by the Steward; every step is a
//! checkpoint, so it survives restarts and the owner can leave and resume on any surface.
//! The plan is built from `adk-harness`' onboarding steps ([`plan`]), with the product's own
//! steps around them:
//!
//! | # | Stage | Plan steps | Gate |
//! |---|---|---|---|
//! | 1 | Start | `start` | — |
//! | 2 | Discover | `discover` | — |
//! | 3 | Protect | `protect` | the owner approves the freeze |
//! | 4 | Interview | `interview` | — |
//! | 5 | Charter | `charter_draft`, `charter_sign` | every required signer signs |
//! | 6 | Build | `build` | — |
//! | 7 | Launch gate | `gate` | — |
//! | 8 | Connect and deploy | `connect`, `deploy` | the owner grants each connection and approves going live |
//! | 9 | Run | `run` | — |
//!
//! | Part | What it does |
//! |---|---|
//! | [`SetupGate`] | A fresh deployment accepts no onboarding input until its setup code pairs the owner's device (A-I20) |
//! | [`StartingPoint`], [`discovery`] | An existing site, an online presence or nothing; sources, kind proposal, freeze offer, quarantine and feature decisions as owner cards |
//! | [`build`] | Release 1's structure — every page serves a goal — content from `Known` facts only, legal pages, every language; the live preview that rebuilds per answer |
//! | [`cost`] | The monthly running-cost estimate the charter's budget shows |
//! | [`OnboardingProgress`] | The stage reached, with `website.onboarding.*` records and spans |
//! | [`RunPlan`] | The 30/60/90-day plan: Propose for the first week, baselines from day 0, the first review on day 7, the starting levels offered for promotion |

pub mod build;
pub mod cost;
pub mod discovery;
pub mod scaffold;
mod setup;

use std::fmt;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_harness::PlanStep;
use adk_harness::website::{CONNECTION_NOTICE, OnboardingStage, SiteRef};
use awp_types::{AutonomyLevel, Charter};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

pub use setup::{SetupGate, holding_page};

use crate::audit;
use crate::kinds::{CharterInputs, KindTemplate};
use crate::team::SiteTeam;
use crate::themes::ThemeId;

/// A stage of onboarding (spec 3.5 §7.18).
///
/// # Example
///
/// ```rust
/// use adk_website::onboarding::Stage;
///
/// assert_eq!(Stage::ALL.len(), 9);
/// assert_eq!(Stage::Charter.steps(), ["charter_draft", "charter_sign"]);
/// assert_eq!(Stage::Run.span_name(), "website.onboarding.run");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Ask for a URL, a name or nothing, and a phone number for P1 alarms.
    Start,
    /// Research what exists; propose the site kind.
    Discover,
    /// Offer the freeze for a site on a legacy runtime.
    Protect,
    /// Ask only what discovery could not answer.
    Interview,
    /// Draft the charter from the kind's template; every required signer signs.
    Charter,
    /// Build release 1.
    Build,
    /// Run the launch gate.
    LaunchGate,
    /// Connect the owner's services and deploy.
    ConnectAndDeploy,
    /// The routine loop.
    Run,
}

impl Stage {
    /// Every stage, in order.
    pub const ALL: [Stage; 9] = [
        Stage::Start,
        Stage::Discover,
        Stage::Protect,
        Stage::Interview,
        Stage::Charter,
        Stage::Build,
        Stage::LaunchGate,
        Stage::ConnectAndDeploy,
        Stage::Run,
    ];

    /// The stage's number, 1 to 9.
    pub fn number(self) -> usize {
        Self::ALL.iter().position(|stage| *stage == self).map_or(0, |i| i + 1)
    }

    /// The stage's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Discover => "discover",
            Self::Protect => "protect",
            Self::Interview => "interview",
            Self::Charter => "charter",
            Self::Build => "build",
            Self::LaunchGate => "launch_gate",
            Self::ConnectAndDeploy => "connect_and_deploy",
            Self::Run => "run",
        }
    }

    /// The stage's span, `website.onboarding.{stage}` (spec 3.5 §7.24).
    pub fn span_name(self) -> &'static str {
        match self {
            Self::Start => "website.onboarding.start",
            Self::Discover => "website.onboarding.discover",
            Self::Protect => "website.onboarding.protect",
            Self::Interview => "website.onboarding.interview",
            Self::Charter => "website.onboarding.charter",
            Self::Build => "website.onboarding.build",
            Self::LaunchGate => "website.onboarding.launch_gate",
            Self::ConnectAndDeploy => "website.onboarding.connect_and_deploy",
            Self::Run => "website.onboarding.run",
        }
    }

    /// The stage's span on `site` (spec 3.5 §7.24).
    pub fn span(self, site: &SiteRef) -> tracing::Span {
        let id = site.id.to_string();
        match self {
            Self::Start => tracing::info_span!("website.onboarding.start", site.id = %id),
            Self::Discover => tracing::info_span!("website.onboarding.discover", site.id = %id),
            Self::Protect => tracing::info_span!("website.onboarding.protect", site.id = %id),
            Self::Interview => tracing::info_span!("website.onboarding.interview", site.id = %id),
            Self::Charter => tracing::info_span!("website.onboarding.charter", site.id = %id),
            Self::Build => tracing::info_span!("website.onboarding.build", site.id = %id),
            Self::LaunchGate => {
                tracing::info_span!("website.onboarding.launch_gate", site.id = %id)
            }
            Self::ConnectAndDeploy => {
                tracing::info_span!("website.onboarding.connect_and_deploy", site.id = %id)
            }
            Self::Run => tracing::info_span!("website.onboarding.run", site.id = %id),
        }
    }

    /// The ids of the stage's plan steps.
    pub fn steps(self) -> &'static [&'static str] {
        match self {
            Self::Start => &["start"],
            Self::Discover => &["discover"],
            Self::Protect => &["protect"],
            Self::Interview => &["interview"],
            Self::Charter => &["charter_draft", "charter_sign"],
            Self::Build => &["build"],
            Self::LaunchGate => &["gate"],
            Self::ConnectAndDeploy => &["connect", "deploy"],
            Self::Run => &["run"],
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where onboarding starts (spec 3.5 §7.18 "Starting point").
///
/// | Starting point | Source |
/// |---|---|
/// | Existing site | `WordPressRest`, `Wxr`, `Crawl` or `IsolatedReplay`, then the sanitiser |
/// | Online presence, no site | `Presence` |
/// | Nothing | The interview and the charter |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum StartingPoint {
    /// An existing site at `url`.
    ExistingSite {
        /// The site's URL.
        url: String,
    },
    /// Business profiles, listings, reviews and social pages, and no site.
    Presence {
        /// The profiles' URLs.
        profiles: Vec<String>,
    },
    /// Nothing: the interview and the charter.
    Nothing,
}

impl StartingPoint {
    /// What discovery reads for this starting point.
    pub fn sources(&self) -> &'static str {
        match self {
            Self::ExistingSite { .. } => {
                "the existing site, through its WordPress REST API, an export file, a crawl or an \
                 isolated replay of its database, then the sanitiser"
            }
            Self::Presence { .. } => "the business profiles, listings, reviews and social pages",
            Self::Nothing => "nothing: the interview and the charter",
        }
    }
}

fn step(
    id: &str,
    title: &str,
    instruction: String,
    after: &str,
    gate: adk_harness::PlanGate,
) -> PlanStep {
    PlanStep {
        id: id.to_string(),
        title: title.to_string(),
        instruction,
        depends_on: if after.is_empty() { Vec::new() } else { vec![after.to_string()] },
        gate,
    }
}

/// The nine-stage onboarding plan for `start` and the kind `template` once it is known (spec
/// 3.5 §7.18): the harness's onboarding steps for discover, interview, charter, build, gate,
/// connect and deploy, with the product's start, protect and run steps and the kind's
/// instructions.
///
/// # Example
///
/// ```rust
/// use adk_harness::PlanGate;
/// use adk_website::onboarding::{StartingPoint, plan};
///
/// let steps = plan(&StartingPoint::Nothing, None);
/// let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
/// assert_eq!(ids, ["start", "discover", "protect", "interview", "charter_draft", "charter_sign", "build", "gate", "connect", "deploy", "run"]);
/// let gated: Vec<&str> = steps.iter().filter(|s| s.gate == PlanGate::Approve).map(|s| s.id.as_str()).collect();
/// assert_eq!(gated, ["protect", "charter_sign", "connect", "deploy"]);
/// ```
pub fn plan(start: &StartingPoint, template: Option<&KindTemplate>) -> Vec<PlanStep> {
    use adk_harness::PlanGate;
    let harness = |stage: OnboardingStage, after: &str| stage.step([after]);
    let mut steps = vec![step(
        "start",
        "Start",
        "Ask the owner for their site's URL, its name, or nothing at all, and a phone number \
         for urgent alarms. Nothing else yet."
            .into(),
        "",
        PlanGate::None,
    )];
    let mut discover = harness(OnboardingStage::Discover, "start");
    discover.instruction = format!(
        "{} Read {}. Propose the site kind with the reasons for it; the owner confirms it.",
        discover.instruction,
        start.sources()
    );
    steps.push(discover);
    let protect = match start {
        StartingPoint::ExistingSite { url } => format!(
            "If {url} runs on a legacy runtime such as WordPress or PHP, offer the freeze as one \
             decision card: a crawled snapshot served as a signed release, with the old server \
             off the public path. Never delete the old site or its data."
        ),
        _ => "There is no existing site to protect; record that and continue.".into(),
    };
    steps.push(step("protect", "Protect", protect, "discover", PlanGate::Approve));
    steps.push(harness(OnboardingStage::Interview, "protect"));
    let mut charter = harness(OnboardingStage::CharterDraft, "interview");
    if let Some(template) = template {
        charter.instruction = format!(
            "{} Start from the {} template: its goals, KPIs and guardrails, every job at Observe \
             or Propose for the first week, the budget with its running-cost estimate.",
            charter.instruction, template.title
        );
    }
    steps.push(charter);
    steps.push(harness(OnboardingStage::CharterSign, "charter_draft"));
    let mut build = harness(OnboardingStage::Build, "charter_sign");
    let directions: Vec<String> =
        ThemeId::ALL.iter().map(|id| format!("{id} ({})", id.direction())).collect();
    build.instruction = format!(
        "{} Every page names its goal. Use the owner's brand assets, or let the owner pick one \
         of three directions: {}. Generated images only as labelled illustration, never the \
         owner's people or premises.",
        build.instruction,
        directions.join("; ")
    );
    steps.push(build);
    steps.push(harness(OnboardingStage::Gate, "build"));
    steps.push(harness(OnboardingStage::Connect, "gate"));
    steps.push(harness(OnboardingStage::Deploy, "connect"));
    steps.push(step(
        "run",
        "Run",
        "Propose the 30/60/90-day plan. Every job works at Propose for the first week; the \
         KPI tracker measures from day 0; the first weekly review goes out on day 7 with the \
         promotions the owner may decide."
            .into(),
        "deploy",
        PlanGate::None,
    ));
    steps
}

/// The Steward's instruction for onboarding (spec 3.5 §7.18): the plan in order, the rules,
/// and the connection notice (A-I19).
pub fn steward_instruction(
    site_name: &str,
    start: &StartingPoint,
    template: Option<&KindTemplate>,
) -> String {
    let mut text = format!(
        "You are onboarding the owner of {site_name}. Work through these stages in order and \
         tell the owner which stage you are in:\n"
    );
    for (number, step) in plan(start, template).iter().enumerate() {
        text.push_str(&format!("{}. {} — {}\n", number + 1, step.title, step.instruction));
    }
    text.push_str(&format!(
        "\nRules: evidence before questions; drafts with defaults, not blank forms; plain \
         language in, structured charter out; no target without a baseline, and targets are the \
         owner's; no invented facts, testimonials, team members, prices or credentials; nothing \
         copied from competitors. Before the first connection say: \"{CONNECTION_NOTICE}\""
    ));
    text
}

/// Drafts charter version 1 for `team` from `template` and `inputs` and records
/// `website.charter.drafted` (spec 3.5 §7.18 stage 5, §9.3).
pub async fn draft_charter(
    template: &KindTemplate,
    team: &SiteTeam,
    inputs: &CharterInputs,
    trail: Option<&AuditTrail>,
) -> Charter {
    let charter = template.draft_charter(team, inputs);
    audit::record(
        trail,
        audit::CHARTER_DRAFTED,
        AuditActor::default(),
        json!({
            "site": charter.site_id,
            "charterVersion": charter.version,
            "kind": charter.kind,
            "valid": charter.validate().is_ok(),
        }),
    )
    .await;
    charter
}

/// Where onboarding stands (spec 3.5 §7.18, §7.24, §9.3): each completed stage is recorded
/// as `website.onboarding.stage` in its span `website.onboarding.{stage}`.
#[derive(Debug)]
pub struct OnboardingProgress {
    site: SiteRef,
    audit: Option<AuditTrail>,
    done: Mutex<Vec<Stage>>,
}

impl OnboardingProgress {
    /// Starts onboarding `site` and records `website.onboarding.started`.
    pub async fn start(site: SiteRef, start: &StartingPoint, audit: Option<AuditTrail>) -> Self {
        audit::record(
            audit.as_ref(),
            audit::ONBOARDING_STARTED,
            AuditActor::default(),
            json!({ "site": site.id, "startingPoint": start }),
        )
        .await;
        Self { site, audit, done: Mutex::new(Vec::new()) }
    }

    /// Records `stage` as completed; completing [`Stage::Run`] completes onboarding
    /// (`website.onboarding.completed`).
    pub async fn complete(&self, stage: Stage, release: Option<u64>) {
        stage.span(&self.site).in_scope(|| {
            tracing::info!(
                stage = stage.as_str(),
                number = stage.number(),
                "website.onboarding.stage"
            );
        });
        let mut done = self.done.lock().await;
        if !done.contains(&stage) {
            done.push(stage);
        }
        drop(done);
        audit::record(
            self.audit.as_ref(),
            audit::ONBOARDING_STAGE,
            AuditActor::default(),
            json!({ "site": self.site.id, "stage": stage, "number": stage.number(), "releaseSeq": release }),
        )
        .await;
        if stage == Stage::Run {
            audit::record(
                self.audit.as_ref(),
                audit::ONBOARDING_COMPLETED,
                AuditActor::default(),
                json!({ "site": self.site.id, "releaseSeq": release }),
            )
            .await;
        }
    }

    /// Onboarding of `site` resumed after a restart, with `done` completed before it; records
    /// nothing, since each stage was recorded when it completed (spec 3.5 §7.18).
    pub fn resumed(
        site: SiteRef,
        audit: Option<AuditTrail>,
        done: impl IntoIterator<Item = Stage>,
    ) -> Self {
        Self { site, audit, done: Mutex::new(done.into_iter().collect()) }
    }

    /// The stages completed, in the order they completed.
    pub async fn completed(&self) -> Vec<Stage> {
        self.done.lock().await.clone()
    }

    /// The next stage, `None` once onboarding is complete.
    pub async fn next(&self) -> Option<Stage> {
        let done = self.done.lock().await;
        Stage::ALL.into_iter().find(|stage| !done.contains(stage))
    }
}

/// The 30/60/90-day plan the Run stage proposes (spec 3.5 §7.18 stage 9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPlan {
    /// The launch: the KPI tracker's first measurements start each KPI's baseline period.
    pub baselines_from: DateTime<Utc>,
    /// The first week ends: every job works at Propose until then.
    pub propose_until: DateTime<Utc>,
    /// The first weekly review.
    pub first_review: DateTime<Utc>,
    /// The promotions offered to the owner after the first week: each job to its §7.20
    /// starting level, decided only by the owner (A-I13).
    pub promotions: Vec<(String, AutonomyLevel)>,
    /// Days 1 to 30, 31 to 60 and 61 to 90.
    pub phases: [Vec<String>; 3],
}

impl RunPlan {
    /// The plan for a site launched at `launch` under `charter`.
    pub fn new(charter: &Charter, launch: DateTime<Utc>) -> Self {
        let kpis: Vec<&str> = charter.kpis.iter().map(|kpi| kpi.name.as_str()).collect();
        Self {
            baselines_from: launch,
            propose_until: launch + Duration::days(7),
            first_review: launch + Duration::days(7),
            promotions: crate::jobs::starting_levels(charter),
            phases: [
                vec![
                    format!("Measure {} from day 0", kpis.join(", ")),
                    "Every job proposes; the owner approves, edits or rejects each proposal".into(),
                    "Day 7: the first weekly review, with the promotions the jobs earned".into(),
                ],
                vec![
                    "Lock baselines when each KPI's measure-only period ends; the owner sets targets".into(),
                    "Fill content gaps the visitors' questions show".into(),
                ],
                vec![
                    "Review goals and strategy against the baselines; propose charter changes".into(),
                    "Propose experiments where a KPI has a baseline and a target".into(),
                ],
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_compiles_as_a_plan_graph_for_every_starting_point() {
        for start in [
            StartingPoint::Nothing,
            StartingPoint::Presence { profiles: vec!["https://maps.example/bakery".into()] },
            StartingPoint::ExistingSite { url: "https://bakery.example".into() },
        ] {
            let steps = plan(&start, Some(&crate::kinds::template(awp_types::SiteKind::Business)));
            adk_harness::compile_plan(&steps, "onboarding").unwrap();
            let covered: Vec<&str> =
                Stage::ALL.iter().flat_map(|s| s.steps().iter().copied()).collect();
            let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(ids, covered, "every stage's steps, in order");
        }
    }

    #[test]
    fn the_connect_step_says_the_notice_first() {
        let steps = plan(&StartingPoint::Nothing, None);
        let connect = steps.iter().find(|s| s.id == "connect").unwrap();
        assert!(connect.instruction.contains(CONNECTION_NOTICE), "A-I19");
        let instruction = steward_instruction("Juniper", &StartingPoint::Nothing, None);
        assert!(instruction.contains("11. Run"));
    }
}
