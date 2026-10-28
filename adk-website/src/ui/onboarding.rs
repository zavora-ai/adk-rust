//! The onboarding views (spec 3.5 §7.18, §8.2): start, the discovery snapshot, the
//! interview with its live preview, the charter cards, the design directions, the launch
//! gate's report page by page, the connections, the deployment and the 30/60/90-day plan.
//!
//! The site does the work and the owner decides (D-12): every input here is a decision the
//! plan waits for, and the desk runs the plan's deterministic steps — discovery, the live
//! preview, building release 1, the launch gate, the launch — when the owner's input unblocks
//! them, through the same library functions the Steward's plan steps call. Each stage is
//! recorded with `website.onboarding.*` ([`OnboardingProgress`]).
//!
//! | Rule | Here |
//! |---|---|
//! | A-I20 | Every command and the view itself come only from the device the setup code paired ([`WebsiteDesk::admit`]) |
//! | A-I18 | Discovered facts are `Assumed` and say so ("A guess"); the interview's answers are the owner's own words and `Known`; only `Known` facts reach a page |
//! | A-I19 | The connections say [`CONNECTION_NOTICE`] first and never ask for a password: each is an OAuth grant, a credential broker or a pairing |
//! | A-I21 | Going live is the owner's decision, in every mode; under a publication chain it also waits for each role |
//! | A-I14 | The charter's KPIs carry no target until they have a baseline |

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_core::Result;
use adk_core::principal::SubjectRef;
use adk_harness::website::CONNECTION_NOTICE;
use adk_harness::{
    FACT_TEXT_MAX_CHARS, FactFilter, FactKind, FactSource, FactStatus, MemoryFact, Provenance,
    fact_digest, redact_fact_text,
};
use adk_site::gate::GateReport;
use adk_site::migrate::SourceKind;
use adk_site::pipeline::{Change, EntryPoint};
use awp_types::{Channel, ContentId, LanguageTag, RedLine, RedLineKind, Role, SiteKind};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::charter::{CharterCommand, CharterReview};
use super::editor::{ApprovalView, PendingRelease, ReleaseStep};
use super::words::{self, money};
use super::{
    ConnectMethod, ConnectStep, ConnectionKind, FieldKind, FieldOption, FieldSpec, FormValues,
    Line, Pill, Tone, WebsiteDesk, forbidden, invalid, not_found, optional, site, unavailable,
};
use crate::kinds::{CharterInputs, KindProposal, template};
use crate::onboarding::build::{
    LivePreview, PreviewSnapshot, SitePlan, known_facts, stage_release_one,
};
use crate::onboarding::discovery::{DesignChoice, Discovery, OwnerCard, record_facts};
use crate::onboarding::{OnboardingProgress, RunPlan, Stage, StartingPoint};
use crate::team::{AgentRole, SiteTeam};
use crate::themes::{ThemeId, tokens};

/// The session name the desk's facts are recorded under.
const DESK_SESSION: &str = "website-desk";

#[derive(Default)]
pub(crate) struct State {
    name: Option<String>,
    started: Option<Started>,
    progress: Option<Arc<OnboardingProgress>>,
    done: BTreeSet<Stage>,
    found: Option<Found>,
    kind: Option<SiteKind>,
    protect: Option<bool>,
    decisions: BTreeMap<String, String>,
    answers: BTreeMap<String, FormValues>,
    answer_facts: BTreeMap<String, Vec<String>>,
    preview: Option<(PreviewSnapshot, DateTime<Utc>)>,
    design: Option<DesignChoice>,
    built: Option<Built>,
    gate: Option<(GateReport, Vec<PageView>)>,
    skipped: BTreeSet<String>,
    pending: Option<PendingRelease>,
    live: Option<(u64, DateTime<Utc>)>,
}

struct Started {
    start: StartingPoint,
    phone: Option<String>,
}

struct Found {
    source: Option<String>,
    facts: usize,
    pages: usize,
    items: usize,
    legacy: bool,
    proposal: Option<KindProposal>,
    cards: Vec<OwnerCard>,
}

struct Built {
    held: Vec<String>,
    summary: String,
}

impl WebsiteDesk {
    pub(crate) async fn save_onboarding(&self) -> super::saved::SavedOnboarding {
        let state = self.inner.onboarding.lock().await;
        super::saved::SavedOnboarding {
            name: state.name.clone(),
            start: state.started.as_ref().map(|started| started.start.clone()),
            phone: state.started.as_ref().and_then(|started| started.phone.clone()),
            done: state.done.clone(),
            found: state.found.as_ref().map(|found| super::saved::SavedFound {
                source: found.source.clone(),
                facts: found.facts,
                pages: found.pages,
                items: found.items,
                legacy: found.legacy,
                proposal: found.proposal.clone(),
                cards: found.cards.clone(),
            }),
            kind: state.kind,
            protect: state.protect,
            decisions: state.decisions.clone(),
            answers: state.answers.clone(),
            answer_facts: state.answer_facts.clone(),
            design: state.design,
            built: state.built.as_ref().map(|built| (built.held.clone(), built.summary.clone())),
            skipped: state.skipped.clone(),
            live: state.live,
        }
    }

    pub(crate) async fn restore_onboarding(&self, saved: super::saved::SavedOnboarding) {
        let mut state = self.inner.onboarding.lock().await;
        state.progress = saved.start.as_ref().map(|_| {
            Arc::new(OnboardingProgress::resumed(
                self.inner.site.clone(),
                self.inner.audit.clone(),
                saved.done.iter().copied(),
            ))
        });
        state.started = saved.start.map(|start| Started { start, phone: saved.phone });
        state.name = saved.name;
        state.done = saved.done;
        state.found = saved.found.map(|found| Found {
            source: found.source,
            facts: found.facts,
            pages: found.pages,
            items: found.items,
            legacy: found.legacy,
            proposal: found.proposal,
            cards: found.cards,
        });
        state.kind = saved.kind;
        state.protect = saved.protect;
        state.decisions = saved.decisions;
        state.answers = saved.answers;
        state.answer_facts = saved.answer_facts;
        state.design = saved.design;
        state.built = saved.built.map(|(held, summary)| Built { held, summary });
        state.skipped = saved.skipped;
        state.live = saved.live;
    }
}

/// Where onboarding starts (spec 3.5 §7.18 "Starting point").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StartFrom {
    /// An existing site, by its URL.
    Site,
    /// Profiles, listings and reviews, and no site.
    Presence,
    /// Nothing: the interview and the charter.
    Nothing,
}

/// The owner's answer on a discovered fact (spec 3.3 §8.2 "That's right / Fix it / Forget it").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FactAnswer {
    /// It is right: it becomes `Known` by the owner's confirmation.
    Right,
    /// It is wrong: the owner's text replaces it, `Known`.
    Fix,
    /// Forget it.
    Forget,
}

/// An input to onboarding (spec 3.5 §7.18), from the paired device only (A-I20).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OnboardingCommand {
    /// Stage 1: a URL, profiles or nothing, the site's name, and a phone number for urgent
    /// alarms.
    Start {
        /// Where to start.
        from: StartFrom,
        /// The existing site's URL.
        #[serde(default)]
        url: Option<String>,
        /// The profiles' URLs.
        #[serde(default)]
        profiles: Vec<String>,
        /// The site's name.
        #[serde(default)]
        name: Option<String>,
        /// A phone number for urgent alarms.
        #[serde(default)]
        phone: Option<String>,
    },
    /// Stage 2: confirm, fix or forget a discovered fact.
    Fact {
        /// The fact.
        id: String,
        /// The answer.
        answer: FactAnswer,
        /// The corrected text, for `fix`.
        #[serde(default)]
        text: Option<String>,
    },
    /// Stage 2: confirm the site kind.
    Kind {
        /// The kind, such as `business`.
        kind: String,
    },
    /// Stage 3: freeze the legacy site or not.
    Protect {
        /// Whether to freeze it.
        freeze: bool,
    },
    /// Stage 2–3: decide a quarantined subject or a legacy feature.
    Decide {
        /// The card.
        card: String,
        /// The choice.
        choice: String,
    },
    /// Stage 4: answer one form; the live preview rebuilds.
    Answer {
        /// The form.
        form: String,
        /// The values, by field id.
        values: FormValues,
    },
    /// Stage 4 ends: the charter is drafted from the answers.
    FinishInterview,
    /// Stage 5: a charter card.
    Charter {
        /// What to do.
        command: CharterCommand,
    },
    /// Stage 6: keep the current design or pick a direction; release 1 is built.
    Design {
        /// `keep_current`, or a theme: `paper`, `harbour`, `meadow`.
        design: String,
    },
    /// Stage 7: run the launch gate.
    RunGate,
    /// Stage 8: start a connection.
    Connect {
        /// The connection.
        connection: String,
    },
    /// Stage 8: connect it later.
    Skip {
        /// The connection.
        connection: String,
    },
    /// Stage 8: go live (A-I21).
    GoLive,
    /// Stage 8: approve the launch in a role, under a publication chain (A-I23).
    Approve {
        /// The role.
        role: String,
    },
}

/// A stage's place in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StageState {
    /// Done.
    Done,
    /// The one to do now.
    Current,
    /// Still to come.
    Later,
}

/// One stage of onboarding (spec 3.5 §7.18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StageLine {
    /// The stage's id.
    pub id: String,
    /// 1 to 9.
    pub number: usize,
    /// Its name.
    pub title: String,
    /// Where it stands.
    pub state: StageState,
}

/// Stage 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartView {
    /// Whether onboarding started.
    pub started: bool,
    /// Where it started, in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Whether urgent alarms have a phone number.
    pub phone: bool,
}

/// A discovered fact (spec 3.5 §7.18 stage 2, A-I18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FactView {
    /// The fact.
    pub id: String,
    /// What it says.
    pub text: String,
    /// Where it was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// "A guess" until the owner confirms it.
    pub status: Pill,
    /// Whether the owner may still answer it.
    pub open: bool,
}

/// The kind of site: proposed from the evidence, confirmed by the owner (spec 3.5 §7.16.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KindView {
    /// The proposed kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<String>,
    /// Why.
    pub reasons: Vec<String>,
    /// The confirmed kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<String>,
    /// Every kind, the proposal and its alternatives first.
    pub options: Vec<FieldOption>,
}

/// A decision discovery found for the owner: a quarantined subject or a legacy feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionView {
    /// The card.
    pub id: String,
    /// What it is about.
    pub title: String,
    /// What was found.
    pub detail: String,
    /// Where.
    pub evidence: Vec<String>,
    /// The choices.
    pub options: Vec<FieldOption>,
    /// What applies until the owner decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The owner's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<String>,
}

/// Stage 2: what discovery found, each fact with its source (spec 3.5 §7.18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryView {
    /// Whether discovery ran.
    pub ran: bool,
    /// What it read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Counts of what it read.
    pub found: Vec<Line>,
    /// The facts.
    pub facts: Vec<FactView>,
    /// The site kind.
    pub kind: KindView,
    /// Quarantined subjects and legacy features.
    pub decisions: Vec<DecisionView>,
}

/// Stage 3: the freeze, for a site on a legacy runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtectView {
    /// The legacy site.
    pub url: String,
    /// What the freeze does.
    pub text: String,
    /// The owner's decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze: Option<bool>,
}

/// One interview form (spec 3.5 §7.18 stage 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FormView {
    /// The form.
    pub id: String,
    /// The question.
    pub question: String,
    /// Its fields, with the answers given.
    pub fields: Vec<FieldSpec>,
    /// Whether it is answered.
    pub answered: bool,
    /// Whether discovery's confirmed facts answered it already.
    pub from_discovery: bool,
}

/// A page of the live preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPage {
    /// The page's title.
    pub title: String,
    /// What it is for.
    pub summary: String,
}

/// The live preview, rebuilt after each answer (spec 3.5 §7.18 stage 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewView {
    /// The site's name.
    pub site_name: String,
    /// The pages release 1 will have.
    pub pages: Vec<PreviewPage>,
    /// The About page as built, in Markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// How many confirmed facts it shows.
    pub facts: usize,
    /// When it was rebuilt.
    pub rebuilt_at: DateTime<Utc>,
    /// Whether the preview is on the host.
    pub published: bool,
}

/// Stage 4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InterviewView {
    /// The forms, in order.
    pub forms: Vec<FormView>,
    /// Whether the interview is done.
    pub done: bool,
    /// The live preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewView>,
}

/// A theme's colours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Swatch {
    /// Page background.
    pub background: String,
    /// Body text.
    pub text: String,
    /// Buttons and highlights.
    pub accent: String,
    /// Links.
    pub link: String,
    /// Rules and outlines.
    pub border: String,
}

/// A design direction (spec 3.5 §7.18 stage 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DesignOption {
    /// `keep_current` or a theme's name.
    pub id: String,
    /// Its name.
    pub name: String,
    /// The direction in words.
    pub direction: String,
    /// Its light colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<Swatch>,
    /// Its dark colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<Swatch>,
}

/// Stage 6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DesignView {
    /// The directions, and keeping the current design for an existing site.
    pub options: Vec<DesignOption>,
    /// The owner's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<String>,
    /// What building release 1 did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built: Option<String>,
    /// Pages the per-change checks held.
    pub held: Vec<String>,
}

/// One launch-gate check (spec 3.5 §7.8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckView {
    /// The check, in words.
    pub name: String,
    /// How it ended.
    pub status: Pill,
    /// What to fix.
    pub findings: Vec<String>,
    /// What it could not measure here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One page of the preview, with how it changes (spec 3.3 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageView {
    /// The page's address.
    pub path: String,
    /// Its title.
    pub title: String,
    /// How it changes.
    pub change: Pill,
    /// The page as Markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
}

/// Stage 7: the launch gate's report, page by page (spec 3.5 §7.8.1, §7.18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateView {
    /// Whether every check passed.
    pub passed: bool,
    /// One line.
    pub text: String,
    /// The checks.
    pub checks: Vec<CheckView>,
    /// The preview's pages.
    pub pages: Vec<PageView>,
}

/// One connection (spec 3.5 §7.18 stage 8, A-I19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCard {
    /// The connection, as sent back.
    pub id: String,
    /// What it is for.
    pub kind: String,
    /// The service.
    pub service: String,
    /// How it connects, in words.
    pub method: String,
    /// Where it stands.
    pub status: Pill,
    /// The account it is connected to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Whether it can be connected here.
    pub can_connect: bool,
}

/// Stage 8: connections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionsView {
    /// What the site says before the first connection (A-I19).
    pub notice: String,
    /// The connections.
    pub cards: Vec<ConnectionCard>,
}

/// Stage 8: the deployment and going live (spec 3.5 §7.22, A-I21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeployView {
    /// The deployment shape.
    pub shape: String,
    /// The monthly running-cost estimate, by category.
    pub cost: Vec<Line>,
    /// Whether the estimate fits the budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fits: Option<bool>,
    /// Whether the owner can go live now.
    pub can_go_live: bool,
    /// The launch waiting for its approval chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<ApprovalView>,
    /// The live release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<String>,
}

/// Stage 9: the 30/60/90-day plan (spec 3.5 §7.18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    /// When the first weekly review comes.
    pub first_review: DateTime<Utc>,
    /// Until when every job suggests.
    pub propose_until: DateTime<Utc>,
    /// Days 1–30, 31–60 and 61–90.
    pub phases: Vec<Vec<String>>,
    /// The levels the jobs can earn, offered for your decision.
    pub promotions: Vec<Line>,
}

/// Onboarding as the owner sees it (spec 3.5 §7.18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingView {
    /// The site's name.
    pub site_name: String,
    /// The nine stages.
    pub stages: Vec<StageLine>,
    /// The stage to do now; `done` once the site runs.
    pub current: String,
    /// Stage 1.
    pub start: StartView,
    /// Stage 2.
    pub discovery: DiscoveryView,
    /// Stage 3, for a site on a legacy runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protect: Option<ProtectView>,
    /// Stage 4.
    pub interview: InterviewView,
    /// Stage 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charter: Option<CharterReview>,
    /// Stage 6.
    pub design: DesignView,
    /// Stage 7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateView>,
    /// Stage 8.
    pub connections: ConnectionsView,
    /// Stage 8.
    pub deploy: DeployView,
    /// Stage 9.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunView>,
}

/// What an onboarding command did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingOutcome {
    /// Onboarding after it.
    pub view: OnboardingView,
    /// The next step of a connection, for the person to take outside the site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<ConnectStep>,
    /// One line on what happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn stage_title(stage: Stage) -> &'static str {
    match stage {
        Stage::Start => "Start",
        Stage::Discover => "What we found",
        Stage::Protect => "Protect the old site",
        Stage::Interview => "A few questions",
        Stage::Charter => "Your charter",
        Stage::Build => "Design and build",
        Stage::LaunchGate => "Launch check",
        Stage::ConnectAndDeploy => "Connect and go live",
        Stage::Run => "Run",
    }
}

/// An interview form (spec 3.5 §7.18 stage 4): its question, fields, the facts its answer
/// states and the discovered facts that already answer it.
struct Form {
    id: &'static str,
    question: String,
    fields: Vec<FieldSpec>,
    covered_by: &'static [&'static str],
}

fn channel_options() -> Vec<(String, String)> {
    [
        ("sms", "Text message"),
        ("whatsapp", "WhatsApp"),
        ("telegram", "Telegram"),
        ("slack", "Slack"),
        ("discord", "Discord"),
        ("matrix", "Matrix"),
        ("email", "Email"),
    ]
    .into_iter()
    .map(|(id, label)| (id.to_string(), label.to_string()))
    .collect()
}

fn forms(kind: SiteKind, name: &str) -> Vec<Form> {
    let mut forms = vec![
        Form {
            id: "about",
            question: format!("What is {name} for, in a sentence or two?"),
            fields: vec![
                FieldSpec::new("mission", "In your words", FieldKind::LongText)
                    .required()
                    .hint("This becomes the mission on your home page"),
            ],
            covered_by: &[],
        },
        Form {
            id: "who",
            question: format!("Who is behind {name}?"),
            fields: vec![
                FieldSpec::new(
                    "who",
                    "Names and roles you are happy to publish",
                    FieldKind::LongText,
                )
                .required(),
            ],
            covered_by: &[],
        },
        Form {
            id: "contact",
            question: "How can people reach you?".into(),
            fields: vec![
                FieldSpec::new("email", "Email", FieldKind::Email).required(),
                FieldSpec::new("phone", "Telephone", FieldKind::Tel),
                FieldSpec::new("address", "Address", FieldKind::Text),
            ],
            covered_by: &[],
        },
    ];
    if matches!(
        kind,
        SiteKind::Business
            | SiteKind::Health
            | SiteKind::Community
            | SiteKind::Education
            | SiteKind::Government
            | SiteKind::Nonprofit
    ) {
        forms.push(Form {
            id: "hours",
            question: "When are you open?".into(),
            fields: vec![
                FieldSpec::new("hours", "Opening hours", FieldKind::Text)
                    .required()
                    .hint("For example: Monday to Saturday, 07:00 to 18:00"),
            ],
            covered_by: &["openinghours"],
        });
    }
    forms.push(Form {
        id: "alarms",
        question: "If something goes badly wrong, how should the site reach you?".into(),
        fields: vec![
            FieldSpec::new("urgent", "Urgent alarms", FieldKind::Choice)
                .required()
                .options(channel_options())
                .hint("Even in quiet time, until you acknowledge them"),
            FieldSpec::new("soon", "Within the hour", FieldKind::Choice).options(channel_options()),
        ],
        covered_by: &[],
    });
    forms.push(Form {
        id: "never",
        question: "Is there anything the site must never say or do?".into(),
        fields: vec![
            FieldSpec::new("never", "One per line", FieldKind::LongText)
                .hint("For example: never promise same-day delivery"),
        ],
        covered_by: &[],
    });
    forms
}

/// The facts an answer states, in the owner's words (A-I18).
fn answer_facts(form: &str, values: &FormValues, name: &str) -> Vec<String> {
    let value = |field: &str| optional(values, field).map(str::to_string);
    match form {
        "about" => {
            value("mission").map(|mission| format!("{name}: {mission}")).into_iter().collect()
        }
        "who" => value("who").map(|who| format!("{name} is run by {who}")).into_iter().collect(),
        // The email goes to the charter's public contact: fact text redacts addresses.
        "contact" => [
            value("phone").map(|v| format!("Telephone: {v}")),
            value("address").map(|v| format!("Address: {v}")),
        ]
        .into_iter()
        .flatten()
        .collect(),
        "hours" => value("hours").map(|v| format!("Opening hours: {v}")).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn swatch(colors: &adk_site::theme::ColorTokens) -> Swatch {
    Swatch {
        background: colors.background.to_string(),
        text: colors.text.to_string(),
        accent: colors.accent.to_string(),
        link: colors.link.to_string(),
        border: colors.border.to_string(),
    }
}

fn design_id(design: DesignChoice) -> String {
    match design {
        DesignChoice::KeepCurrent => "keep_current".into(),
        DesignChoice::NewTheme(theme) => theme.as_str().into(),
    }
}

fn source_words(kind: SourceKind, url: Option<&str>) -> String {
    let at = url.map(|url| format!(" of {url}")).unwrap_or_default();
    match kind {
        SourceKind::WordPressRest => format!("The WordPress site{at}, through its public API"),
        SourceKind::Wxr => "A WordPress export file".into(),
        SourceKind::Crawl => format!("A polite crawl{at}"),
        SourceKind::Presence => "Your profiles, listings and reviews".into(),
        SourceKind::IsolatedReplay => "A sealed copy of the old site's database".into(),
        _ => "Your existing site".into(),
    }
}

/// The slug of a page file: `/about/index.html` → `about`, `/index.html` → `home`.
fn slug_of(path: &str) -> String {
    let trimmed = path.trim_start_matches('/').trim_end_matches("index.html").trim_end_matches('/');
    if trimmed.is_empty() {
        "home".into()
    } else {
        trimmed.rsplit('/').next().unwrap_or(trimmed).into()
    }
}

const PAGE_MARKDOWN_MAX: usize = 6_000;
const PREVIEW_PAGES_MAX: usize = 40;

impl WebsiteDesk {
    /// The site's name: the one onboarding learned, else the desk's.
    pub async fn site_name(&self) -> String {
        self.inner.onboarding.lock().await.name.clone().unwrap_or_else(|| self.inner.name.clone())
    }

    fn steward(&self) -> String {
        AgentRole::Steward.principal(&self.inner.site)
    }

    async fn complete(&self, state: &mut State, stage: Stage, release: Option<u64>) {
        if state.done.insert(stage)
            && let Some(progress) = &state.progress
        {
            progress.complete(stage, release).await;
        }
    }

    fn inputs(&self, state: &State, name: &str) -> CharterInputs {
        let answer = |form: &str, field: &str| {
            state.answers.get(form).and_then(|values| optional(values, field)).map(str::to_string)
        };
        let mut inputs = CharterInputs::new(
            self.inner.site.id,
            name,
            self.inner.owner.clone(),
            answer("about", "mission").unwrap_or_else(|| name.to_string()),
            self.inner.languages.clone(),
            self.inner.currency.clone(),
        );
        let phone = state.started.as_ref().and_then(|s| s.phone.clone());
        let urgent = answer("alarms", "urgent").or_else(|| phone.map(|_| "sms".to_string()));
        inputs.p1_channels = urgent.map(|c| Channel::from(c.as_str())).into_iter().collect();
        inputs.p2_channels =
            answer("alarms", "soon").map(|c| Channel::from(c.as_str())).into_iter().collect();
        inputs.contact_email = answer("contact", "email");
        inputs
    }

    /// The charter draft the live preview builds against: the kind's template with the
    /// answers so far.
    fn preview_charter(&self, state: &State, kind: SiteKind, name: &str) -> awp_types::Charter {
        let template = template(kind);
        let team = SiteTeam::new(self.inner.site.clone(), &template);
        template.draft_charter(&team, &self.inputs(state, name))
    }

    async fn refresh_preview(&self, state: &mut State) -> Result<()> {
        let (Some(kind), Some((facts, scope))) = (state.kind, self.inner.facts.clone()) else {
            return Ok(());
        };
        let name = state.name.clone().unwrap_or_else(|| self.inner.name.clone());
        let charter = self.preview_charter(state, kind, &name);
        let preview = LivePreview::new(self.runtime().await, facts, scope, name, self.steward());
        let snapshot = preview.refresh(&charter).await?;
        state.preview = Some((snapshot, self.now()));
        Ok(())
    }

    /// Onboarding as the paired owner sees it (spec 3.5 §7.18, A-I20); once the charter is
    /// signed, as a person it names sees it.
    ///
    /// # Errors
    ///
    /// `Unauthorized` for anyone but the paired device; store errors.
    pub async fn onboarding_view(
        &self,
        person: &str,
        token: Option<&str>,
    ) -> Result<OnboardingView> {
        match self.admit(person, token).await {
            Ok(actor) => self.build_onboarding_view(&actor).await,
            // Once a charter is signed, the people it names follow the launch from their own
            // accounts, to approve it under a publication chain; they give no other input.
            Err(error) => {
                let committed = self.inner.charter.lock().await.committed.is_some();
                if committed && !self.roles(person).await?.is_empty() {
                    self.build_onboarding_view(person).await
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn build_onboarding_view(&self, actor: &str) -> Result<OnboardingView> {
        let charter = self.charter_review(actor).await?;
        let state = self.inner.onboarding.lock().await;
        let name = state.name.clone().unwrap_or_else(|| self.inner.name.clone());
        let current = Stage::ALL.into_iter().find(|stage| !state.done.contains(stage));
        let stages = Stage::ALL
            .into_iter()
            .map(|stage| StageLine {
                id: stage.as_str().into(),
                number: stage.number(),
                title: stage_title(stage).into(),
                state: if state.done.contains(&stage) {
                    StageState::Done
                } else if Some(stage) == current {
                    StageState::Current
                } else {
                    StageState::Later
                },
            })
            .collect();

        let start = StartView {
            started: state.started.is_some(),
            from: state.started.as_ref().map(|s| match &s.start {
                StartingPoint::ExistingSite { url } => format!("Your site at {url}"),
                StartingPoint::Presence { profiles } => {
                    format!(
                        "{} profile{} and listing{}",
                        profiles.len(),
                        plural(profiles.len()),
                        plural(profiles.len())
                    )
                }
                StartingPoint::Nothing => "Nothing yet: we start from your answers".into(),
            }),
            phone: state.started.as_ref().is_some_and(|s| s.phone.is_some()),
        };

        let discovery = self.discovery_view(&state).await?;
        let protect = match (&state.started, &state.found) {
            (Some(Started { start: StartingPoint::ExistingSite { url }, .. }), Some(found))
                if found.legacy =>
            {
                Some(ProtectView {
                    url: url.clone(),
                    text: "Your site runs on a server that runs code for every visitor. Freezing \
                           it serves a signed copy of every page instead, and takes the old \
                           server off the public path. Nothing is deleted; the old site and its \
                           data stay yours."
                        .into(),
                    freeze: state.protect,
                })
            }
            _ => None,
        };

        let interview = match state.kind {
            None => InterviewView { forms: Vec::new(), done: false, preview: None },
            Some(kind) => {
                let covered = self.confirmed_keys().await?;
                let forms = forms(kind, &name)
                    .into_iter()
                    .map(|form| {
                        let values = state.answers.get(form.id);
                        let from_discovery = values.is_none()
                            && !form.covered_by.is_empty()
                            && form.covered_by.iter().all(|key| covered.contains(*key));
                        FormView {
                            id: form.id.into(),
                            question: form.question,
                            answered: values.is_some() || from_discovery,
                            from_discovery,
                            fields: form
                                .fields
                                .into_iter()
                                .map(|field| {
                                    let value = values.and_then(|v| v.get(&field.id)).cloned();
                                    match value {
                                        Some(value) => field.value(value),
                                        None => field,
                                    }
                                })
                                .collect(),
                        }
                    })
                    .collect();
                let preview = state.preview.as_ref().map(|(snapshot, at)| {
                    let charter = self.preview_charter(&state, kind, &name);
                    PreviewView {
                        site_name: name.clone(),
                        pages: SitePlan::new(&charter, &name)
                            .pages
                            .into_iter()
                            .filter(|page| page.legal.is_none())
                            .map(|page| PreviewPage { title: page.title, summary: page.summary })
                            .collect(),
                        about: snapshot.about.clone(),
                        facts: snapshot.facts,
                        rebuilt_at: *at,
                        published: snapshot.published,
                    }
                });
                InterviewView { forms, done: state.done.contains(&Stage::Interview), preview }
            }
        };

        let mut options = Vec::new();
        if matches!(
            state.started.as_ref().map(|s| &s.start),
            Some(StartingPoint::ExistingSite { .. })
        ) {
            options.push(DesignOption {
                id: "keep_current".into(),
                name: "Keep your current design".into(),
                direction: "The new site looks like the old one; the launch check compares them \
                            page by page."
                    .into(),
                light: None,
                dark: None,
            });
        }
        for theme in ThemeId::ALL {
            let tokens = tokens(theme);
            let mut name = theme.as_str().to_string();
            name[..1].make_ascii_uppercase();
            options.push(DesignOption {
                id: theme.as_str().into(),
                name,
                direction: theme.direction().into(),
                light: Some(swatch(&tokens.light.colors)),
                dark: Some(swatch(&tokens.dark)),
            });
        }
        let design = DesignView {
            options,
            chosen: state.design.map(design_id),
            built: state.built.as_ref().map(|built| built.summary.clone()),
            held: state.built.as_ref().map(|built| built.held.clone()).unwrap_or_default(),
        };

        let gate = state.gate.as_ref().map(|(report, pages)| {
            let failed = report
                .checks
                .iter()
                .filter(|c| c.status == adk_site::gate::CheckStatus::Failed)
                .count();
            GateView {
                passed: report.passed(),
                text: if report.passed() {
                    "Every check passed. Review each page, then connect and go live.".into()
                } else {
                    format!(
                        "{failed} check{} need{} a fix before launch.",
                        plural(failed),
                        if failed == 1 { "s" } else { "" }
                    )
                },
                checks: report
                    .checks
                    .iter()
                    .map(|check| CheckView {
                        name: words::check(check.check).into(),
                        status: words::check_status(check.status),
                        findings: check
                            .findings
                            .iter()
                            .filter(|f| f.blocking)
                            .take(5)
                            .map(|f| f.message.clone())
                            .collect(),
                        note: check.note.clone(),
                    })
                    .collect(),
                pages: pages.clone(),
            }
        });

        let mut cards = Vec::new();
        for connector in &self.inner.connectors {
            let account = connector.connected().await?;
            let status = match (&account, state.skipped.contains(connector.id())) {
                (Some(_), _) => Pill::new("Connected", Tone::Good),
                (None, true) => Pill::new("Later", Tone::Neutral),
                (None, false) => Pill::new("Not connected", Tone::Waiting),
            };
            cards.push(ConnectionCard {
                id: connector.id().into(),
                kind: connection_kind(connector.kind()).into(),
                service: connector.service().into(),
                method: connect_method(connector.method()).into(),
                status,
                can_connect: account.is_none(),
                account,
            });
        }
        let offered: BTreeSet<ConnectionKind> =
            self.inner.connectors.iter().map(|c| c.kind()).collect();
        for kind in [
            ConnectionKind::Domain,
            ConnectionKind::Email,
            ConnectionKind::OwnerChannel,
            ConnectionKind::Analytics,
            ConnectionKind::Payment,
        ] {
            if !offered.contains(&kind) {
                cards.push(ConnectionCard {
                    id: format!("later-{}", connection_kind(kind).to_lowercase().replace(' ', "-")),
                    kind: connection_kind(kind).into(),
                    service: "Not set up on this site yet".into(),
                    method: "You can add it later".into(),
                    status: Pill::new("Later", Tone::Neutral),
                    account: None,
                    can_connect: false,
                });
            }
        }
        let connections = ConnectionsView { notice: CONNECTION_NOTICE.into(), cards };

        let committed = charter.as_ref().and_then(|c| c.committed.as_ref()).is_some();
        let draft = self.inner.charter.lock().await.draft.clone();
        let estimate = match &draft {
            Some(draft) => self.estimate(draft).await,
            None => None,
        };
        let currency = self.inner.currency.clone();
        let deploy = DeployView {
            shape: self.inner.shape.clone(),
            cost: estimate
                .as_ref()
                .map(|e| {
                    e.monthly
                        .iter()
                        .map(|(category, amount)| {
                            Line::new(words::category(*category), money(*amount, &currency))
                        })
                        .chain(std::iter::once(Line::new("Each month", money(e.total, &currency))))
                        .collect()
                })
                .unwrap_or_default(),
            fits: match (&estimate, &draft) {
                (Some(e), Some(d)) => Some(e.fits(&d.budget)),
                _ => None,
            },
            can_go_live: committed
                && state.gate.as_ref().is_some_and(|(report, _)| report.passed())
                && state.live.is_none()
                && state.pending.is_none(),
            waiting: match &state.pending {
                Some(pending) => Some(self.approval_view(pending, actor).await?),
                None => None,
            },
            live: state.live.map(|(seq, at)| {
                format!("Live since {} · release {seq}, signed", at.format("%-d %b %H:%M"))
            }),
        };

        let run = match (state.live, charter.as_ref().and_then(|c| c.committed.as_ref())) {
            (Some((_, at)), Some(_)) => {
                let committed = self.inner.charter.lock().await.committed.clone();
                committed.map(|version| {
                    let plan = RunPlan::new(&version.charter, at);
                    RunView {
                        first_review: plan.first_review,
                        propose_until: plan.propose_until,
                        phases: plan.phases.to_vec(),
                        promotions: plan
                            .promotions
                            .iter()
                            .map(|(job, level)| {
                                Line::new(
                                    crate::jobs::job(job).map_or(job.as_str(), |spec| spec.title),
                                    words::level(*level),
                                )
                            })
                            .collect(),
                    }
                })
            }
            _ => None,
        };

        Ok(OnboardingView {
            site_name: name,
            stages,
            current: current.map_or_else(|| "done".to_string(), |stage| stage.as_str().to_string()),
            start,
            discovery,
            protect,
            interview,
            charter,
            design,
            gate,
            connections,
            deploy,
            run,
        })
    }

    /// The keys of the discovered facts the owner confirmed, lowercased without spaces.
    async fn confirmed_keys(&self) -> Result<BTreeSet<String>> {
        let Some((facts, scope)) = &self.inner.facts else { return Ok(BTreeSet::new()) };
        Ok(facts
            .list(FactFilter::recall(scope))
            .await?
            .into_iter()
            .filter(|fact| !fact.provenance.source_ids.is_empty())
            .filter(|fact| fact.status == FactStatus::Known)
            .filter_map(|fact| {
                fact.text.split_once(':').map(|(key, _)| key.to_lowercase().replace(' ', ""))
            })
            .collect())
    }

    async fn discovery_view(&self, state: &State) -> Result<DiscoveryView> {
        let mut facts = Vec::new();
        if let Some((store, scope)) = &self.inner.facts {
            for fact in store.list(FactFilter::user(&scope.app, &scope.user)).await? {
                if fact.provenance.source_ids.is_empty() || !scope.admits(&fact.scope) {
                    continue;
                }
                let (status, open) = match fact.status {
                    FactStatus::Assumed => (Pill::new("A guess", Tone::Waiting), true),
                    // A confirmed guess reads as the owner's own words (spec 3.3 §8.1).
                    FactStatus::Known => (Pill::new("You said this", Tone::Good), false),
                    _ => continue,
                };
                facts.push(FactView {
                    id: fact.id.clone(),
                    text: fact_words(&fact.text),
                    source: fact.provenance.source_ids.first().cloned(),
                    status,
                    open,
                });
            }
            // A confirmed fact replaces its guess, so a stable order keeps each row in place.
            facts.sort_by(|a, b| a.text.cmp(&b.text));
        }
        let proposal = state.found.as_ref().and_then(|found| found.proposal.clone());
        let mut kinds: Vec<SiteKind> = Vec::new();
        if let Some(proposal) = &proposal {
            kinds.push(proposal.kind);
            kinds.extend(proposal.alternatives.iter().copied());
        }
        for kind in words::KINDS {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        let kind = KindView {
            proposed: proposal.as_ref().map(|p| words::kind_id(p.kind).to_string()),
            reasons: proposal.as_ref().map(|p| p.reasons.clone()).unwrap_or_default(),
            confirmed: state.kind.map(|kind| words::kind_id(kind).to_string()),
            options: kinds
                .into_iter()
                .map(|kind| FieldOption {
                    value: words::kind_id(kind).into(),
                    label: template(kind).title.into(),
                })
                .collect(),
        };
        let mut decisions = Vec::new();
        if let Some(found) = &state.found {
            for card in &found.cards {
                match card {
                    OwnerCard::Quarantined { entry } => {
                        let id = format!("quarantine:{}", entry.content_id);
                        let releasable = entry.findings.iter().all(|f| f.kind.releasable());
                        let mut options = vec![FieldOption {
                            value: "keep_out".into(),
                            label: "Keep it out of the site".into(),
                        }];
                        if releasable {
                            options.push(FieldOption {
                                value: "release".into(),
                                label: "It is mine: publish it".into(),
                            });
                        }
                        decisions.push(DecisionView {
                            title: entry
                                .path
                                .clone()
                                .or_else(|| entry.item.clone())
                                .unwrap_or_else(|| entry.content_id.to_string()),
                            detail: entry
                                .findings
                                .iter()
                                .map(|f| finding_words(f.kind))
                                .collect::<Vec<_>>()
                                .join("; "),
                            evidence: entry
                                .findings
                                .iter()
                                .flat_map(|f| f.evidence.iter())
                                .take(3)
                                .map(|e| match &e.url {
                                    Some(url) => format!("{} — {url}", e.detail),
                                    None => e.detail.clone(),
                                })
                                .collect(),
                            options,
                            default: Some("keep_out".into()),
                            chosen: state.decisions.get(&id).cloned(),
                            id,
                        });
                    }
                    OwnerCard::Feature { decision } => {
                        let id = format!("feature:{}", decision.id);
                        decisions.push(DecisionView {
                            title: decision.question.clone(),
                            detail: format!(
                                "Found on {} page{}",
                                decision.evidence.len(),
                                plural(decision.evidence.len())
                            ),
                            evidence: Vec::new(),
                            options: decision
                                .options
                                .iter()
                                .map(|o| FieldOption {
                                    value: o.id.clone(),
                                    label: o.label.clone(),
                                })
                                .collect(),
                            default: Some(decision.default.clone()),
                            chosen: state.decisions.get(&id).cloned(),
                            id,
                        });
                    }
                    OwnerCard::ConfirmKind { .. }
                    | OwnerCard::Freeze { .. }
                    | OwnerCard::Design { .. } => {}
                }
            }
        }
        // A found review is quoted only with its reviewer's public name and the owner's
        // approval (S2); an anonymous one has no card, since it is never quoted.
        if let Some(quotes) = &self.inner.quotes {
            for (index, review) in quotes.list().await.into_iter().enumerate() {
                let Some(author) = review.author.clone().filter(|a| !a.trim().is_empty()) else {
                    continue;
                };
                let id = format!("review:{index}");
                let excerpt: String = review.text.chars().take(160).collect();
                decisions.push(DecisionView {
                    title: format!("Quote {author}'s review on the site?"),
                    detail: format!(
                        "\"{excerpt}\" — a page quotes it with {author}'s name only if you say yes"
                    ),
                    evidence: review.source.clone().into_iter().collect(),
                    options: vec![
                        FieldOption {
                            value: "quote".into(),
                            label: "Yes, quote it with their name".into(),
                        },
                        FieldOption {
                            value: "do_not_quote".into(),
                            label: "Do not quote it".into(),
                        },
                    ],
                    default: Some("do_not_quote".into()),
                    chosen: state.decisions.get(&id).cloned(),
                    id,
                });
            }
        }
        Ok(DiscoveryView {
            ran: state.found.is_some(),
            source: state.found.as_ref().and_then(|f| f.source.clone()),
            found: state
                .found
                .as_ref()
                .map(|found| {
                    [
                        ("Facts to check", found.facts),
                        ("Pages", found.pages),
                        ("Posts, products and other items", found.items),
                    ]
                    .into_iter()
                    .filter(|(_, count)| *count > 0)
                    .map(|(label, count)| Line::new(label, count.to_string()))
                    .collect()
                })
                .unwrap_or_default(),
            facts,
            kind,
            decisions,
        })
    }

    /// Carries out an onboarding `command` from the paired device holding `token` (spec 3.5
    /// §7.18, A-I20).
    ///
    /// # Errors
    ///
    /// `Unauthorized` for anyone but the paired device; `InvalidInput` for a step taken out
    /// of order or a bad value; `Forbidden` for a step the owner's role is needed for; the
    /// stores' and the pipeline's errors.
    pub async fn onboarding_command(
        &self,
        person: &str,
        token: Option<&str>,
        command: OnboardingCommand,
    ) -> Result<OnboardingOutcome> {
        let result = self.onboarding_command_unsaved(person, token, command).await;
        if result.is_ok() {
            self.persist().await;
        }
        result
    }

    async fn onboarding_command_unsaved(
        &self,
        person: &str,
        token: Option<&str>,
        command: OnboardingCommand,
    ) -> Result<OnboardingOutcome> {
        // Approving the launch under a publication chain is a role holder's decision from
        // their own account (A-I23), not onboarding input; everything else comes only from the
        // paired device (A-I20).
        let actor = match &command {
            OnboardingCommand::Approve { .. } => {
                if !self.setup_view().await.paired {
                    return Err(self.admit(person, None).await.err().unwrap_or_else(|| {
                        forbidden("website.setup.unpaired", "the site has not been claimed")
                    }));
                }
                person.to_string()
            }
            _ => self.admit(person, token).await?,
        };
        let mut next = None;
        let mut note = None;
        match command {
            OnboardingCommand::Start { from, url, profiles, name, phone } => {
                self.require(&actor, &[Role::Owner], "Starting onboarding").await?;
                note = self.start(&actor, from, url, profiles, name, phone).await?;
            }
            OnboardingCommand::Fact { id, answer, text } => {
                self.require(&actor, &[Role::Owner], "Confirming a fact").await?;
                self.answer_fact(&actor, &id, answer, text.as_deref()).await?;
                let mut state = self.inner.onboarding.lock().await;
                self.refresh_preview(&mut state).await?;
            }
            OnboardingCommand::Kind { kind } => {
                self.require(&actor, &[Role::Owner], "Confirming the kind of site").await?;
                let kind =
                    words::KINDS.into_iter().find(|k| words::kind_id(*k) == kind).ok_or_else(
                        || invalid("website.onboarding.kind", format!("no kind '{kind}'")),
                    )?;
                let mut state = self.inner.onboarding.lock().await;
                if state.started.is_none() {
                    return Err(invalid("website.onboarding.order", "start first"));
                }
                if state.done.contains(&Stage::Interview) {
                    return Err(invalid(
                        "website.onboarding.order",
                        "the charter is drafted for this kind; a new kind is a charter change",
                    ));
                }
                state.kind = Some(kind);
                self.complete(&mut state, Stage::Discover, None).await;
                if !state.found.as_ref().is_some_and(|found| found.legacy) {
                    self.complete(&mut state, Stage::Protect, None).await;
                }
                if let Some(ladder) = &self.inner.ladder
                    && let Err(error) =
                        crate::jobs::register_onboarding(ladder, self.inner.site.id).await
                {
                    tracing::warn!(error = %error, "the onboarding job could not be registered");
                }
                self.refresh_preview(&mut state).await?;
            }
            OnboardingCommand::Protect { freeze } => {
                self.require(&actor, &[Role::Owner], "Deciding the freeze").await?;
                let mut state = self.inner.onboarding.lock().await;
                if !state.found.as_ref().is_some_and(|found| found.legacy) {
                    return Err(invalid(
                        "website.onboarding.order",
                        "there is no legacy site to freeze",
                    ));
                }
                state.protect = Some(freeze);
                if state.kind.is_some() {
                    self.complete(&mut state, Stage::Protect, None).await;
                }
                note = Some(if freeze {
                    "The Steward freezes the old site: a signed copy goes live and the old server leaves the public path.".into()
                } else {
                    "The old site stays as it is until you launch.".into()
                });
            }
            OnboardingCommand::Decide { card, choice } => {
                self.require(&actor, &[Role::Owner], "Deciding what the old site's parts become")
                    .await?;
                let mut state = self.inner.onboarding.lock().await;
                let view = self.discovery_view(&state).await?;
                let decision = view.decisions.iter().find(|d| d.id == card).ok_or_else(|| {
                    not_found("website.onboarding.card", format!("no card '{card}'"))
                })?;
                if !decision.options.iter().any(|o| o.value == choice) {
                    return Err(invalid(
                        "website.onboarding.choice",
                        format!("'{choice}' is not a choice of this card"),
                    ));
                }
                if let (Some(quotes), Some(index)) = (
                    &self.inner.quotes,
                    card.strip_prefix("review:").and_then(|n| n.parse::<usize>().ok()),
                ) {
                    let decision = format!("quote-review-{index}-{actor}");
                    quotes.approve(index, (choice == "quote").then_some(decision.as_str())).await?;
                }
                state.decisions.insert(card, choice);
            }
            OnboardingCommand::Answer { form, values } => {
                self.require(&actor, &[Role::Owner], "Answering the interview").await?;
                self.answer(&actor, &form, values).await?;
            }
            OnboardingCommand::FinishInterview => {
                self.require(&actor, &[Role::Owner], "Finishing the interview").await?;
                self.finish_interview().await?;
            }
            OnboardingCommand::Charter { command } => {
                if let Some(version) = self.charter_command(&actor, command).await? {
                    let mut state = self.inner.onboarding.lock().await;
                    self.complete(&mut state, Stage::Charter, None).await;
                    note = Some(format!(
                        "Charter version {} is signed and in force.",
                        version.version()
                    ));
                }
            }
            OnboardingCommand::Design { design } => {
                self.require(&actor, &[Role::Owner], "Choosing the design").await?;
                note = Some(self.choose_design(&design).await?);
            }
            OnboardingCommand::RunGate => {
                self.require(&actor, &[Role::Owner], "Running the launch check").await?;
                self.run_gate().await?;
            }
            OnboardingCommand::Connect { connection } => {
                self.require(&actor, &[Role::Owner], "Connecting a service").await?;
                let connector =
                    self.inner.connectors.iter().find(|c| c.id() == connection).ok_or_else(
                        || {
                            not_found(
                                "website.onboarding.connection",
                                format!("no connection '{connection}'"),
                            )
                        },
                    )?;
                next = Some(connector.start(&actor).await?);
                self.inner.onboarding.lock().await.skipped.remove(&connection);
            }
            OnboardingCommand::Skip { connection } => {
                self.require(&actor, &[Role::Owner], "Leaving a connection for later").await?;
                self.inner.onboarding.lock().await.skipped.insert(connection);
            }
            OnboardingCommand::GoLive => {
                self.require(&actor, &[Role::Owner], "Going live").await?;
                note = Some(self.go_live().await?);
            }
            OnboardingCommand::Approve { role } => {
                let mut state = self.inner.onboarding.lock().await;
                let mut pending = state.pending.take().ok_or_else(|| {
                    not_found("website.onboarding.no_launch", "no launch waits for approval")
                })?;
                match self.approve_release(&mut pending, &actor, &role).await {
                    Ok(Some(step)) => note = Some(self.launched(&mut state, step).await),
                    Ok(None) => {
                        state.pending = Some(pending);
                        note = Some(
                            "Your approval is recorded; the launch waits for the other roles."
                                .into(),
                        );
                    }
                    Err(error) => {
                        state.pending = Some(pending);
                        return Err(error);
                    }
                }
            }
        }
        Ok(OnboardingOutcome { view: self.build_onboarding_view(&actor).await?, next, note })
    }

    async fn start(
        &self,
        actor: &str,
        from: StartFrom,
        url: Option<String>,
        profiles: Vec<String>,
        name: Option<String>,
        phone: Option<String>,
    ) -> Result<Option<String>> {
        let mut state = self.inner.onboarding.lock().await;
        if state.started.is_some() {
            return Err(invalid("website.onboarding.started", "onboarding has started already"));
        }
        let start = match from {
            StartFrom::Site => StartingPoint::ExistingSite {
                url: url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).ok_or_else(
                    || invalid("website.onboarding.url", "enter your site's address"),
                )?,
            },
            StartFrom::Presence => {
                let profiles: Vec<String> = profiles
                    .into_iter()
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect();
                if profiles.is_empty() {
                    return Err(invalid(
                        "website.onboarding.profiles",
                        "add at least one profile or listing",
                    ));
                }
                StartingPoint::Presence { profiles }
            }
            StartFrom::Nothing => StartingPoint::Nothing,
        };
        let phone = phone.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        if let Some(phone) = &phone
            && !phone.chars().all(|c| c.is_ascii_digit() || " +-()".contains(c))
        {
            return Err(invalid(
                "website.onboarding.phone",
                "a phone number has digits, spaces and + only",
            ));
        }
        let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        let mut note = None;
        if !matches!(start, StartingPoint::Nothing) {
            let (context, source) = self
                .inner
                .discovery
                .clone()
                .ok_or_else(|| unavailable("Discovery of an existing site or presence"))?;
            let language = self
                .inner
                .languages
                .first()
                .cloned()
                .unwrap_or_else(|| LanguageTag::parse("en").expect("a valid tag"));
            let mut discovery = Discovery::new(start.clone(), language)?;
            if let Some(source) = source {
                discovery = discovery.with_source(source);
            }
            let discovered = discovery.run(&context).await?;
            let (store, scope) =
                self.inner.facts.clone().ok_or_else(|| unavailable("The site's fact store"))?;
            let recorded =
                record_facts(discovered.facts(), store.as_ref(), &scope, DESK_SESSION).await?;
            let inventory = discovered.inventory.as_ref();
            // The reviews it found, none approved, for the owner to decide (S2).
            if let (Some(quotes), Some(inventory)) = (&self.inner.quotes, inventory) {
                quotes.add_discovered(&inventory.reviews).await?;
            }
            note = Some(format!(
                "Found {} fact{} to check. Each is a guess until you confirm it.",
                recorded.len(),
                plural(recorded.len())
            ));
            state.found = Some(Found {
                source: inventory.map(|i| source_words(i.source, i.origin.as_deref())),
                facts: recorded.len(),
                pages: inventory.map_or(0, |i| i.urls.len()),
                items: inventory.map_or(0, |i| i.items.len()),
                legacy: discovered.legacy_runtime,
                proposal: discovered.kind.clone(),
                cards: discovered.cards(),
            });
            if name.is_none()
                && let Some(found_name) = inventory.and_then(|i| i.site_name.clone())
            {
                state.name = Some(found_name);
            }
        } else {
            state.found = None;
        }
        if name.is_some() {
            state.name = name;
        }
        let progress =
            OnboardingProgress::start(self.inner.site.clone(), &start, self.inner.audit.clone())
                .await;
        state.progress = Some(Arc::new(progress));
        state.started = Some(Started { start, phone });
        self.complete(&mut state, Stage::Start, None).await;
        tracing::info!(website.site = %self.inner.site.id, actor, "website.onboarding.started");
        Ok(note)
    }

    async fn answer_fact(
        &self,
        actor: &str,
        id: &str,
        answer: FactAnswer,
        text: Option<&str>,
    ) -> Result<()> {
        let (store, scope) =
            self.inner.facts.clone().ok_or_else(|| unavailable("The site's fact store"))?;
        let fact = store
            .get(id)
            .await?
            .filter(|fact| scope.admits(&fact.scope) && !fact.provenance.source_ids.is_empty())
            .ok_or_else(|| {
                not_found("website.onboarding.fact", format!("no discovered fact '{id}'"))
            })?;
        if fact.status != FactStatus::Assumed {
            return Err(invalid("website.onboarding.fact", "this fact is answered already"));
        }
        let by = SubjectRef::human(actor);
        // A confirmed fact is kept in the words the owner read, so the preview and the charter
        // quote it that way; a fix is kept in the owner's own words.
        let (text, source) = match answer {
            FactAnswer::Forget => return store.set_status(id, FactStatus::Forgotten, &by).await,
            FactAnswer::Right => (fact_words(&fact.text), FactSource::UserConfirmed),
            FactAnswer::Fix => {
                let text = text.map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| {
                    invalid("website.onboarding.fact", "write what is right instead")
                })?;
                (redact_fact_text(text, FACT_TEXT_MAX_CHARS)?, FactSource::UserStated)
            }
        };
        if text == fact.text {
            return store.set_status(id, FactStatus::Known, &by).await;
        }
        // The store keeps one live fact per digest, which ignores case and spacing. A live fact
        // other than the guess already says it: that one is confirmed.
        let digest = fact_digest(&text);
        let same = store.list(FactFilter::user(&scope.app, &scope.user)).await?.into_iter().find(
            |other| {
                other.id != fact.id
                    && other.status.is_live()
                    && other.scope == fact.scope
                    && other.digest == digest
            },
        );
        if let Some(other) = same {
            store.set_status(&other.id, FactStatus::Known, &by).await?;
            return store.set_status(id, FactStatus::Superseded { by: other.id }, &by).await;
        }
        let mut provenance = Provenance::new(source, DESK_SESSION, self.next_id("fact"));
        provenance.quote = Some(text.clone());
        provenance.source_ids = fact.provenance.source_ids.clone();
        let known = MemoryFact::new(scope.clone(), fact.kind, text, FactStatus::Known, provenance);
        // The guess goes first, so words that differ from it only in case are not taken for it.
        store.set_status(id, FactStatus::Superseded { by: known.id.clone() }, &by).await?;
        store.put(known).await.map(drop)
    }

    async fn answer(&self, actor: &str, form_id: &str, values: FormValues) -> Result<()> {
        let mut state = self.inner.onboarding.lock().await;
        let kind = state.kind.ok_or_else(|| {
            invalid("website.onboarding.order", "confirm what kind of site this is first")
        })?;
        if state.done.contains(&Stage::Interview) {
            return Err(invalid(
                "website.onboarding.order",
                "the interview is done; change the charter's cards instead",
            ));
        }
        let name = state.name.clone().unwrap_or_else(|| self.inner.name.clone());
        let form =
            forms(kind, &name).into_iter().find(|form| form.id == form_id).ok_or_else(|| {
                not_found("website.onboarding.form", format!("no question '{form_id}'"))
            })?;
        let mut clean = FormValues::new();
        for field in &form.fields {
            let value = values.get(&field.id).map(|v| v.trim().to_string()).unwrap_or_default();
            if field.required && value.is_empty() {
                return Err(invalid("website.ui.missing", format!("{} is needed", field.label)));
            }
            if value.is_empty() {
                continue;
            }
            match field.kind {
                FieldKind::Email if !value.contains('@') || value.contains(' ') => {
                    return Err(invalid(
                        "website.onboarding.email",
                        format!("{} is not an email address", value),
                    ));
                }
                FieldKind::Choice if !field.options.iter().any(|o| o.value == value) => {
                    return Err(invalid(
                        "website.onboarding.choice",
                        format!("choose one of the options for {}", field.label),
                    ));
                }
                _ => {}
            }
            // A-I19: no credential passes through the site; a secret in an answer is refused.
            redact_fact_text(&value, FACT_TEXT_MAX_CHARS * 4)?;
            clean.insert(field.id.clone(), value);
        }
        if let Some((store, scope)) = self.inner.facts.clone() {
            let by = SubjectRef::human(actor);
            for old in state.answer_facts.remove(form_id).unwrap_or_default() {
                store.set_status(&old, FactStatus::Forgotten, &by).await?;
            }
            let mut ids = Vec::new();
            for text in answer_facts(form_id, &clean, &name) {
                let text = redact_fact_text(&text, FACT_TEXT_MAX_CHARS)?;
                let mut provenance =
                    Provenance::new(FactSource::UserStated, DESK_SESSION, self.next_id("answer"));
                provenance.quote = Some(text.clone());
                let stored = store
                    .put(MemoryFact::new(
                        scope.clone(),
                        FactKind::ProjectFact,
                        text,
                        FactStatus::Known,
                        provenance,
                    ))
                    .await?;
                ids.push(stored.id);
            }
            state.answer_facts.insert(form_id.to_string(), ids);
        }
        state.answers.insert(form_id.to_string(), clean);
        self.refresh_preview(&mut state).await
    }

    async fn finish_interview(&self) -> Result<()> {
        let covered = self.confirmed_keys().await?;
        let mut state = self.inner.onboarding.lock().await;
        let kind = state.kind.ok_or_else(|| {
            invalid("website.onboarding.order", "confirm what kind of site this is first")
        })?;
        if state.done.contains(&Stage::Interview) {
            return Ok(());
        }
        let name = state.name.clone().unwrap_or_else(|| self.inner.name.clone());
        for form in forms(kind, &name) {
            let required = form.fields.iter().any(|f| f.required);
            let covered =
                !form.covered_by.is_empty() && form.covered_by.iter().all(|k| covered.contains(*k));
            if required && !covered && !state.answers.contains_key(form.id) {
                return Err(invalid(
                    "website.onboarding.unanswered",
                    format!("answer \"{}\" first", form.question),
                ));
            }
        }
        let template = template(kind);
        let team = SiteTeam::new(self.inner.site.clone(), &template);
        let inputs = self.inputs(&state, &name);
        let mut charter =
            crate::onboarding::draft_charter(&template, &team, &inputs, self.inner.audit.as_ref())
                .await;
        if let Some(never) = state.answers.get("never").and_then(|v| optional(v, "never")) {
            for line in never.lines().map(str::trim).filter(|l| !l.is_empty()) {
                charter
                    .red_lines
                    .push(RedLine { kind: RedLineKind::Claim, text: line.to_string() });
            }
        }
        self.inner.charter.lock().await.draft = Some(charter);
        self.complete(&mut state, Stage::Interview, None).await;
        Ok(())
    }

    async fn choose_design(&self, design: &str) -> Result<String> {
        let committed = self.inner.charter.lock().await.committed.clone().ok_or_else(|| {
            invalid(
                "website.onboarding.order",
                "sign the charter first: every page serves one of its goals",
            )
        })?;
        let mut state = self.inner.onboarding.lock().await;
        let choice = if design == "keep_current" {
            if !matches!(
                state.started.as_ref().map(|s| &s.start),
                Some(StartingPoint::ExistingSite { .. })
            ) {
                return Err(invalid(
                    "website.onboarding.design",
                    "there is no current design to keep",
                ));
            }
            DesignChoice::KeepCurrent
        } else {
            DesignChoice::NewTheme(
                design.parse::<ThemeId>().map_err(|e| invalid("website.onboarding.design", e))?,
            )
        };
        if state.live.is_some() {
            return Err(invalid(
                "website.onboarding.order",
                "the site is live; a new design is a release",
            ));
        }
        if matches!(
            state.started.as_ref().map(|s| &s.start),
            Some(StartingPoint::ExistingSite { .. })
        ) {
            // The owner's design choice for a migration is recorded in the migration audit.
            crate::audit::record(
                self.inner.audit.as_ref(),
                "site.migration.decided",
                adk_core::audit_log::AuditActor::default(),
                serde_json::json!({
                    "site": self.inner.site.id,
                    "migration": adk_site::migrate::parity::design_choice_fields(&choice.parity()),
                }),
            )
            .await;
        }
        if let Some(assembly) = &self.inner.assembly {
            let theme = match choice {
                DesignChoice::KeepCurrent => None,
                DesignChoice::NewTheme(theme) => Some(theme),
            };
            *self.inner.runtime.write().await = assembly.runtime(theme);
        }
        let runtime = self.runtime().await;
        let name = state.name.clone().unwrap_or_else(|| self.inner.name.clone());
        let charter = &committed.charter;
        let known = match &self.inner.facts {
            Some((facts, scope)) => known_facts(facts.as_ref(), scope).await?,
            None => Vec::new(),
        };
        let plan = SitePlan::new(charter, &name);
        let staged = stage_release_one(
            &runtime,
            plan.items(charter, &known, &self.inner.jurisdictions),
            &self.steward(),
        )
        .await?;
        let held: Vec<String> = staged.held.iter().map(|id| id.0.clone()).collect();
        let summary = if held.is_empty() {
            format!(
                "{} pages built from {} confirmed fact{}.",
                staged.written.len(),
                known.len(),
                plural(known.len())
            )
        } else {
            format!(
                "{} pages built; {} held: {}",
                staged.written.len(),
                held.len(),
                staged.gate.summary()
            )
        };
        state.design = Some(choice);
        state.gate = None;
        state.built = Some(Built { held: held.clone(), summary: summary.clone() });
        if held.is_empty() {
            self.complete(&mut state, Stage::Build, None).await;
        }
        Ok(summary)
    }

    async fn home_draft(&self) -> Result<adk_site::pipeline::Draft> {
        let runtime = self.runtime().await;
        let language = self.inner.languages.first().map_or("en", LanguageTag::as_str).to_string();
        let id = ContentId(format!(
            "{}/home/{language}",
            adk_site::content::kind_directory(awp_types::ContentKind::Page)
        ));
        let home = runtime.store().get(&id).await.map_err(site)?.ok_or_else(|| {
            invalid(
                "website.onboarding.order",
                "release 1 has no home page yet; choose a design first",
            )
        })?;
        let change = Change {
            items: vec![home],
            media: Vec::new(),
            entry: EntryPoint::SiteAgent { agent: self.steward() },
            proposer: self.steward(),
            job: crate::jobs::ONBOARDING.to_string(),
            action_class: "publish".into(),
        };
        runtime.pipeline().draft(change).await.map_err(site)
    }

    async fn run_gate(&self) -> Result<()> {
        let mut state = self.inner.onboarding.lock().await;
        if state.built.is_none() {
            return Err(invalid(
                "website.onboarding.order",
                "choose a design first: release 1 is built from it",
            ));
        }
        let runtime = self.runtime().await;
        let report = runtime.run_launch_gate().await?;
        let draft = self.home_draft().await?;
        let preview = runtime.pipeline().preview(&draft).await.map_err(site)?;
        let name = state.name.clone().unwrap_or_else(|| self.inner.name.clone());
        let titles: BTreeMap<String, String> = self
            .inner
            .charter
            .lock()
            .await
            .committed
            .as_ref()
            .map(|version| {
                SitePlan::new(&version.charter, &name)
                    .pages
                    .into_iter()
                    .map(|page| (page.slug, page.title))
                    .collect()
            })
            .unwrap_or_default();
        let mut pages = Vec::new();
        for diff in preview.diff.iter().take(PREVIEW_PAGES_MAX) {
            let twin =
                diff.path.trim_start_matches('/').trim_end_matches(".html").to_string() + ".md";
            let markdown = tokio::fs::read_to_string(preview.staged.root.join(&twin))
                .await
                .ok()
                .map(|text| text.chars().take(PAGE_MARKDOWN_MAX).collect());
            let slug = slug_of(&diff.path);
            pages.push(PageView {
                title: match diff.path.as_str() {
                    "/404.html" => "Page not found".to_string(),
                    "/410.html" => "Page withdrawn".to_string(),
                    path => titles.get(&slug).cloned().unwrap_or_else(|| path.to_string()),
                },
                path: diff.path.clone(),
                change: words::page_change(diff.change),
                markdown,
            });
        }
        pages.sort_by_key(|page| (page.path != "/index.html", page.path.clone()));
        let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
        let passed = report.passed();
        state.gate = Some((report, pages));
        if passed {
            self.complete(&mut state, Stage::LaunchGate, None).await;
        }
        Ok(())
    }

    async fn go_live(&self) -> Result<String> {
        let mut state = self.inner.onboarding.lock().await;
        if !state.gate.as_ref().is_some_and(|(report, _)| report.passed()) {
            return Err(invalid("website.onboarding.order", "the launch check has to pass first"));
        }
        if state.live.is_some() || state.pending.is_some() {
            return Err(invalid("website.onboarding.order", "the launch is under way already"));
        }
        let draft = self.home_draft().await?;
        let step = self.release(draft, None).await?;
        Ok(self.launched(&mut state, step).await)
    }

    /// Records how a launch step ended; returns the line for the owner.
    async fn launched(&self, state: &mut State, step: ReleaseStep) -> String {
        match step {
            ReleaseStep::Live { seq } => {
                let at = self.now();
                state.live = Some((seq, at));
                if let Some(status) = &self.inner.status {
                    let record = crate::incident::StatusRecord {
                        state: awp_types::SiteState::Operational,
                        since: at,
                        reason: "launched".into(),
                        release_seq: Some(seq),
                    };
                    if let Err(error) = status.set(self.inner.site.id, record).await {
                        tracing::warn!(error = %error, "the site's status could not be set at launch");
                    }
                }
                self.complete(state, Stage::ConnectAndDeploy, Some(seq)).await;
                self.complete(state, Stage::Run, Some(seq)).await;
                format!("The site is live: release {seq} is signed and published.")
            }
            ReleaseStep::Pending(next) => {
                state.pending = Some(*next);
                "The launch waits for every role of the publication chain.".into()
            }
            ReleaseStep::Held(summary) => format!("The launch is held: {summary}"),
            ReleaseStep::Rejected { by, reason } => format!("{by} refused the launch: {reason}"),
            ReleaseStep::Scheduled { at } => format!("The launch is scheduled for {at}"),
        }
    }
}

/// A discovered fact, `openingHours: Mo-Sa 07:00-18:00`, as "Opening hours: Mo-Sa 07:00-18:00".
fn fact_words(text: &str) -> String {
    let Some((key, value)) = text.split_once(':') else { return text.to_string() };
    let key = key.trim();
    if key.is_empty() || key.contains(' ') {
        return text.to_string();
    }
    let label = match key {
        "type" | "@type" => "Kind of place".to_string(),
        other => {
            let mut words = String::new();
            for (index, c) in other.trim_start_matches('@').chars().enumerate() {
                if index == 0 {
                    words.extend(c.to_uppercase());
                } else if c.is_uppercase() {
                    words.push(' ');
                    words.extend(c.to_lowercase());
                } else if c == '_' || c == '-' {
                    words.push(' ');
                } else {
                    words.push(c);
                }
            }
            words
        }
    };
    format!("{label}:{value}")
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn connection_kind(kind: ConnectionKind) -> &'static str {
    match kind {
        ConnectionKind::Domain => "Domain",
        ConnectionKind::Email => "Email",
        ConnectionKind::OwnerChannel => "Your channels",
        ConnectionKind::Analytics => "Analytics",
        ConnectionKind::Payment => "Payments",
    }
}

fn connect_method(method: ConnectMethod) -> &'static str {
    match method {
        ConnectMethod::OAuth => "Through the service's own sign-in",
        ConnectMethod::Broker => "Through your credential broker",
        ConnectMethod::Pairing => "With a pairing code",
    }
}

fn finding_words(kind: adk_site::migrate::detect::FindingKind) -> &'static str {
    use adk_site::migrate::detect::FindingKind;
    match kind {
        FindingKind::HiddenLinks => "Links hidden from visitors",
        FindingKind::SpamLinks => "Spam links",
        FindingKind::Cloaking => "Different content for search engines than for people",
        FindingKind::Executable => "A program uploaded as a file; it is never published",
        FindingKind::MimeMismatch => "A file that is not what its name says",
        FindingKind::ObfuscatedScript => "A disguised script",
        _ => "Something suspicious",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec 3.5 §10 X12: the alarms form offers every chat platform an adapter reaches, each a
    /// channel the charter names.
    #[test]
    fn alarms_offer_every_chat_platform() {
        let options = channel_options();
        for (id, label) in [("slack", "Slack"), ("discord", "Discord"), ("matrix", "Matrix")] {
            assert!(options.contains(&(id.to_string(), label.to_string())), "{id}");
        }
        for (id, _) in &options {
            assert!(!matches!(Channel::from(id.as_str()), Channel::Other(_)), "{id}");
        }
    }

    #[test]
    fn discovered_keys_read_as_words() {
        assert_eq!(
            fact_words("openingHours: Mo-Sa 07:00-18:00"),
            "Opening hours: Mo-Sa 07:00-18:00"
        );
        assert_eq!(fact_words("type: LocalBusiness"), "Kind of place: LocalBusiness");
        assert_eq!(fact_words("Juniper Bakery: bread"), "Juniper Bakery: bread");
    }

    #[test]
    fn page_slugs_come_from_their_files() {
        assert_eq!(slug_of("/index.html"), "home");
        assert_eq!(slug_of("/about/index.html"), "about");
        assert_eq!(slug_of("/sw/contact/index.html"), "contact");
    }

    #[test]
    fn answers_state_facts_in_the_owners_words() {
        let values = FormValues::from([
            ("email".to_string(), "hello@juniper.example".to_string()),
            ("phone".to_string(), "+254 700 000 000".to_string()),
        ]);
        assert_eq!(answer_facts("contact", &values, "Juniper"), ["Telephone: +254 700 000 000"]);
        assert!(answer_facts("alarms", &values, "Juniper").is_empty(), "channels are not claims");
    }

    #[test]
    fn hours_are_asked_only_of_places_people_visit() {
        let ids = |kind| forms(kind, "Site").into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert!(ids(SiteKind::Business).contains(&"hours"));
        assert!(!ids(SiteKind::Personal).contains(&"hours"));
    }
}
