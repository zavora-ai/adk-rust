//! Onboarding and owner views on the web home (spec 3.5 §7.16 `ui`, §7.8.2 "Web editor",
//! §7.18, §7.21, §7.24, §8.2; spec 3.3 §5.1, §8, §9).
//!
//! [`WebsiteDesk`] is what a surface reads and acts through: it turns the product's library
//! types — the setup gate, discovery, the charter template, the live preview, the launch
//! gate, the publishing pipeline, the autonomy ladder, the KPI tracker, the wallet, the
//! update engine and the weekly review — into views in plain words, and carries out the
//! decisions a person makes on them. `adk-surface`'s `website` feature serves it on the web
//! home; any other surface can read the same views.
//!
//! | View | Reads | A person decides | Spec 3.5 |
//! |---|---|---|---|
//! | [`setup`] | The setup gate | The setup code pairs the owner's device; nothing else is accepted before it | §7.17, A-I20 |
//! | [`onboarding`] | Discovery, the interview's facts, the live preview, the design directions, the launch gate, the connections, the deployment | Start, confirm or fix each found fact, the kind, the freeze, the interview's answers, the design, each connection, going live | §7.18, A-I18, A-I19, A-I21 |
//! | [`charter`] | The charter draft, section by section | Each section looks right or is changed; every required signer signs | §7.6, §7.18 stage 5, A-I14, A-I23 |
//! | [`site`] | Status, releases and their page changes, KPIs with trends, jobs and their levels, signals, the wallet, held batches | Promote or lower a job; release or discard a held item or feed delivery | §7.24, §7.11, §7.8.3, A-I13, A-I27 |
//! | [`review`] | The weekly review | Compose it; send its summary to the owner's channel | §7.21 |
//! | [`editor`] | Content per kind | Draft with a form per kind and Markdown, upload media, preview with page diffs, schedule, publish, approve under a chain, correct, withdraw | §7.8.2, A-I23, A-I24, Q13 |
//!
//! **Who may do what.** Every action names the person who takes it, as the server
//! authenticated them. Roles come from the charter in force (spec 3.5 §7.6); before the first
//! charter, the owner the desk was built for holds the owner, editor and publisher roles.
//! Onboarding input is accepted only from the device the setup code paired (A-I20); a
//! charter is signed and a job promoted only by a person who holds the role (A-I13, A-I23);
//! content is authored only by people holding an editing role (A-I24). No agent acts here:
//! an agent principal is refused every role.
//!
//! **What it keeps.** The views are rebuilt from the stores on every read. The desk holds the
//! state of the person's own work in progress — the paired device, the onboarding answers, the
//! charter under review, the editor's drafts — and, with a [`DeskStore`], saves it after every
//! command and reads it back with [`WebsiteDesk::restore`], so a restart loses none of it.

pub mod charter;
pub mod editor;
pub mod onboarding;
pub mod review;
mod saved;
pub mod setup;
pub mod site;
mod words;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use adk_core::audit_log::AuditTrail;
use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};
use adk_harness::website::{SiteRef, SiteRuntime};
use adk_harness::{FactScope, FactStore, StandingRuleStore};
use adk_kernel::PartitionId;
use adk_kernel::channels::ChannelSink;
use adk_kernel::charter::{AutonomyLadder, CharterStore, KpiTracker, SignalStore};
use adk_payments::wallet::{Wallet, WalletStore};
use adk_site::media::MediaPipeline;
use adk_site::migrate::{SiteSource, SourceContext};
use adk_site::updates::UpdateEngine;
use async_trait::async_trait;
use awp_types::{LanguageTag, Role};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::alarms::AlarmRouter;
use crate::incident::{IncidentPath, StatusSink};
use crate::onboarding::cost::CostInputs;
use crate::review::{ReviewComposer, ReviewInputs};
use crate::themes::ThemeId;
use crate::wallet::WalletKpi;

pub use charter::{CharterReview, SectionCard, SignerLine};
pub use editor::{DraftView, EditorCommand, EditorHome, EditorOutcome, KindForm};
pub use onboarding::{OnboardingCommand, OnboardingOutcome, OnboardingView};
pub use review::{ReviewCommand, ReviewPage, ReviewView};
pub use saved::{DeskStore, FileDeskStore, InMemoryDeskStore, SavedDesk};
pub use setup::{ClaimedView, SetupView};
pub use site::{SiteCommand, SiteOutcome, SiteViewModel};

/// The tone of a pill: a word always comes with it, so status is never colour alone (spec
/// 3.3 §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    /// Done, passed, up.
    Good,
    /// Waiting for someone.
    Waiting,
    /// Failed, held, down.
    Bad,
    /// Worth knowing.
    Info,
    /// Neither.
    Neutral,
}

/// A status word and its tone (spec 3.3 §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Pill {
    /// The word, in sentence case.
    pub text: String,
    /// Its tone.
    pub tone: Tone,
}

impl Pill {
    /// A pill saying `text` in `tone`.
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self { text: text.into(), tone }
    }
}

/// A label and its value, for a card's lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// The label.
    pub label: String,
    /// The value, in words.
    pub value: String,
}

impl Line {
    pub(crate) fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into() }
    }
}

/// What a form field takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FieldKind {
    /// One line of text.
    Text,
    /// Several lines of text.
    LongText,
    /// Markdown, with the editor's formatting buttons and preview (spec 3.5 §13 Q13).
    Markdown,
    /// An email address.
    Email,
    /// A telephone number.
    Tel,
    /// A web address.
    Url,
    /// A number.
    Number,
    /// A date and time.
    DateTime,
    /// A time of day, `HH:MM`.
    Time,
    /// One of `options`.
    Choice,
}

/// One choice of a [`FieldKind::Choice`] field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldOption {
    /// The value sent back.
    pub value: String,
    /// What the person reads.
    pub label: String,
}

/// One field of a form: the interview's, a charter section's or the web editor's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldSpec {
    /// The field's id, the key of its value.
    pub id: String,
    /// Its label.
    pub label: String,
    /// What it takes.
    pub kind: FieldKind,
    /// Whether it must be filled in.
    pub required: bool,
    /// A line under the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// The choices of a [`FieldKind::Choice`] field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<FieldOption>,
    /// The value it starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl FieldSpec {
    pub(crate) fn new(id: &str, label: &str, kind: FieldKind) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            required: false,
            hint: None,
            options: Vec::new(),
            value: None,
        }
    }

    pub(crate) fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub(crate) fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub(crate) fn value(mut self, value: impl Into<String>) -> Self {
        let value = value.into();
        self.value = (!value.is_empty()).then_some(value);
        self
    }

    pub(crate) fn options(mut self, options: impl IntoIterator<Item = (String, String)>) -> Self {
        self.options =
            options.into_iter().map(|(value, label)| FieldOption { value, label }).collect();
        self
    }
}

/// Builds the site runtime with the owner's chosen design (spec 3.5 §7.18 stage 6): the host
/// assembles the runtime over its stores, host and log with the theme the owner picked.
///
/// # Example
///
/// ```rust,ignore
/// struct Assembly { /* the site's stores, host and log */ }
/// impl SiteAssembly for Assembly {
///     fn runtime(&self, theme: Option<ThemeId>) -> Arc<SiteRuntime> { /* … */ }
/// }
/// ```
pub trait SiteAssembly: Send + Sync {
    /// The runtime with `theme`, or with the site's current look for `None`.
    fn runtime(&self, theme: Option<ThemeId>) -> Arc<SiteRuntime>;
}

/// What a connection is for (spec 3.5 §7.18 stage 8).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionKind {
    /// The site's domain and DNS.
    Domain,
    /// The site's email.
    Email,
    /// Where alarms and cards reach the owner.
    OwnerChannel,
    /// Analytics and search consoles.
    Analytics,
    /// Payments and the wallet.
    Payment,
}

/// How a connection is made; never by a password typed into the site (A-I19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectMethod {
    /// The service's own sign-in page.
    OAuth,
    /// A credential broker the owner already uses.
    Broker,
    /// Pairing a device or an account with a code.
    Pairing,
}

/// The next step of a connection, for the person to take outside the site (A-I19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ConnectStep {
    /// Open the service's own page: its sign-in or the broker's grant.
    Open {
        /// The page.
        url: String,
    },
    /// Send or enter this code where the text says.
    Code {
        /// The code.
        code: String,
        /// Where to use it.
        how: String,
    },
    /// Connected; nothing more to do.
    Done,
}

/// One service the site can connect to (spec 3.5 §7.18 stage 8, §7.13, A-I19): an OAuth
/// grant, a credential broker or a pairing. Implementations never take a password, an API key
/// or a card number from the person.
#[async_trait]
pub trait Connector: Send + Sync {
    /// A stable id.
    fn id(&self) -> &str;
    /// What it is for.
    fn kind(&self) -> ConnectionKind;
    /// The service's name, such as "Cloudflare DNS".
    fn service(&self) -> &str;
    /// How it connects.
    fn method(&self) -> ConnectMethod;
    /// Starts connecting for `person`.
    ///
    /// # Errors
    ///
    /// Returns the connector's error.
    async fn start(&self, person: &str) -> Result<ConnectStep>;
    /// The account it is connected to, or `None`.
    ///
    /// # Errors
    ///
    /// Returns the connector's error.
    async fn connected(&self) -> Result<Option<String>>;
}

/// What the jobs recorded for a week's review that no store keeps (spec 3.5 §7.21): what
/// each agent did, experiments, leads handed off and questions.
#[async_trait]
pub trait ReviewNotes: Send + Sync {
    /// The notes for the week starting `week_start`.
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    async fn notes(&self, week_start: chrono::NaiveDate) -> Result<ReviewInputs>;
}

/// The wallet as the site view reads it (spec 3.5 §7.14, §7.24).
#[derive(Clone)]
pub struct WalletParts {
    /// The wallet: its balance and policy.
    pub wallet: Wallet,
    /// The wallet's payments, for the held ones.
    pub store: Arc<dyn WalletStore>,
    /// Its spend as a KPI source.
    pub kpi: Arc<WalletKpi>,
}

pub(crate) struct Desk {
    pub(crate) site: SiteRef,
    pub(crate) name: String,
    pub(crate) owner: String,
    pub(crate) languages: Vec<LanguageTag>,
    pub(crate) currency: String,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) runtime: RwLock<Arc<SiteRuntime>>,
    pub(crate) assembly: Option<Arc<dyn SiteAssembly>>,
    pub(crate) setup: Option<Arc<crate::onboarding::SetupGate>>,
    pub(crate) facts: Option<(Arc<dyn FactStore>, FactScope)>,
    pub(crate) charters: Option<Arc<dyn CharterStore>>,
    pub(crate) ladder: Option<AutonomyLadder>,
    pub(crate) rules: Option<Arc<dyn StandingRuleStore>>,
    pub(crate) status: Option<Arc<dyn StatusSink>>,
    pub(crate) tracker: Option<KpiTracker>,
    pub(crate) signals: Option<(Arc<dyn SignalStore>, PartitionId)>,
    pub(crate) wallet: Option<WalletParts>,
    pub(crate) updates: Option<Arc<UpdateEngine>>,
    pub(crate) alarms: Option<Arc<AlarmRouter>>,
    pub(crate) incidents: Option<Arc<IncidentPath>>,
    pub(crate) composer: Option<ReviewComposer>,
    pub(crate) notes: Option<Arc<dyn ReviewNotes>>,
    pub(crate) store: Option<saved::SharedDeskStore>,
    pub(crate) quotes: Option<Arc<crate::quotes::ReviewQuotes>>,
    pub(crate) channel: Option<(Arc<dyn ChannelSink>, String)>,
    pub(crate) discovery: Option<(SourceContext, Option<Arc<dyn SiteSource>>)>,
    pub(crate) connectors: Vec<Arc<dyn Connector>>,
    pub(crate) cost: Option<CostInputs>,
    pub(crate) shape: String,
    pub(crate) jurisdictions: Vec<String>,
    pub(crate) media: MediaPipeline,
    pub(crate) paired: Mutex<Option<String>>,
    pub(crate) onboarding: Mutex<onboarding::State>,
    pub(crate) charter: Mutex<charter::State>,
    pub(crate) reviews: Mutex<review::State>,
    pub(crate) editor: Mutex<editor::State>,
    next: AtomicU64,
}

/// The owner's desk of one site: its views and the decisions people make on them (spec 3.5
/// §7.16 `ui`). Cheap to clone; every clone is the same desk.
///
/// # Example
///
/// ```rust,ignore
/// let desk = WebsiteDesk::builder(site, "Juniper Bakery", "amina", runtime)
///     .languages(vec!["en".parse()?])
///     .currency("KES")
///     .setup(gate)
///     .facts(facts, scope)
///     .charters(charters)
///     .ladder(ladder, rules)
///     .build();
/// let view = desk.site_view("amina").await?;
/// ```
#[derive(Clone)]
pub struct WebsiteDesk {
    pub(crate) inner: Arc<Desk>,
}

impl std::fmt::Debug for WebsiteDesk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebsiteDesk").field("site", &self.inner.site).finish_non_exhaustive()
    }
}

/// Builds a [`WebsiteDesk`].
pub struct WebsiteDeskBuilder {
    desk: Desk,
}

macro_rules! setter {
    ($(#[$doc:meta])* $name:ident, $field:ident: $ty:ty) => {
        $(#[$doc])*
        #[must_use]
        pub fn $name(mut self, value: $ty) -> Self {
            self.desk.$field = Some(value);
            self
        }
    };
}

impl WebsiteDeskBuilder {
    /// The site's languages; the first is the default (`en` unless set).
    #[must_use]
    pub fn languages(mut self, languages: Vec<LanguageTag>) -> Self {
        if !languages.is_empty() {
            self.desk.languages = languages;
        }
        self
    }

    /// The budget's ISO 4217 currency (`USD` unless set).
    #[must_use]
    pub fn currency(mut self, currency: impl Into<String>) -> Self {
        self.desk.currency = currency.into().to_ascii_uppercase();
        self
    }

    /// The clock.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.desk.clock = clock;
        self
    }

    /// Where onboarding and charter records go (spec 3.5 §9.3).
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.desk.audit = Some(trail);
        self
    }

    /// Rebuilds the runtime with the owner's chosen design (spec 3.5 §7.18 stage 6).
    #[must_use]
    pub fn assembly(mut self, assembly: Arc<dyn SiteAssembly>) -> Self {
        self.desk.assembly = Some(assembly);
        self
    }

    /// The setup gate of a fresh deployment (spec 3.5 §7.17, A-I20).
    #[must_use]
    pub fn setup(mut self, gate: Arc<crate::onboarding::SetupGate>) -> Self {
        self.desk.setup = Some(gate);
        self
    }

    /// The site's long-term facts and their scope (A-I18).
    #[must_use]
    pub fn facts(mut self, facts: Arc<dyn FactStore>, scope: FactScope) -> Self {
        self.desk.facts = Some((facts, scope));
        self
    }

    setter!(
        /// The charter store (spec 3.5 §7.11).
        charters, charters: Arc<dyn CharterStore>
    );

    /// The autonomy ladder and the standing-rule store a promotion grants rules in (spec 3.5
    /// §7.11, A-I13).
    #[must_use]
    pub fn ladder(mut self, ladder: AutonomyLadder, rules: Arc<dyn StandingRuleStore>) -> Self {
        self.desk.ladder = Some(ladder);
        self.desk.rules = Some(rules);
        self
    }

    setter!(
        /// The site's status (spec 3.5 §7.5).
        status, status: Arc<dyn StatusSink>
    );
    setter!(
        /// The KPI tracker (spec 3.5 §7.11).
        tracker, tracker: KpiTracker
    );

    /// The signal store and the back office's partition (spec 3.5 §7.10).
    #[must_use]
    pub fn signals(mut self, store: Arc<dyn SignalStore>, partition: PartitionId) -> Self {
        self.desk.signals = Some((store, partition));
        self
    }

    setter!(
        /// The wallet (spec 3.5 §7.14).
        wallet, wallet: WalletParts
    );
    setter!(
        /// The update engine whose held batches the site view shows (spec 3.5 §7.8.3).
        updates, updates: Arc<UpdateEngine>
    );
    setter!(
        /// The alarm router (spec 3.5 §7.21).
        alarms, alarms: Arc<AlarmRouter>
    );
    setter!(
        /// The incident path (spec 3.5 §7.20).
        incidents, incidents: Arc<IncidentPath>
    );
    setter!(
        /// The weekly review's composer (spec 3.5 §7.21).
        composer, composer: ReviewComposer
    );
    setter!(
        /// What the jobs recorded for each week's review.
        notes, notes: Arc<dyn ReviewNotes>
    );

    /// Where the desk saves the person's work in progress, so a restart keeps it (spec 3.5
    /// §8.2); restore it with [`WebsiteDesk::restore`].
    #[must_use]
    pub fn store(mut self, store: Arc<dyn DeskStore>) -> Self {
        self.desk.store = Some(store);
        self
    }

    /// The reviews the site may quote: discovery adds what it found, and the owner decides
    /// each from its card (spec 3.5 S2).
    #[must_use]
    pub fn quotes(mut self, quotes: Arc<crate::quotes::ReviewQuotes>) -> Self {
        self.desk.quotes = Some(quotes);
        self
    }

    /// The owner's channel and thread the review's summary goes to (spec 3.5 §7.21).
    #[must_use]
    pub fn channel(mut self, sink: Arc<dyn ChannelSink>, thread: impl Into<String>) -> Self {
        self.desk.channel = Some((sink, thread.into()));
        self
    }

    /// How discovery reads an existing site or online presence: the signed client of
    /// `context`, and `source` in place of the default crawl or presence reader, such as a
    /// WordPress export or a recorded inventory (spec 3.5 §7.18 stage 2).
    #[must_use]
    pub fn discovery(
        mut self,
        context: SourceContext,
        source: Option<Arc<dyn SiteSource>>,
    ) -> Self {
        self.desk.discovery = Some((context, source));
        self
    }

    /// Adds a service the owner can connect (spec 3.5 §7.18 stage 8).
    #[must_use]
    pub fn connector(mut self, connector: Arc<dyn Connector>) -> Self {
        self.desk.connectors.push(connector);
        self
    }

    /// What a month of the site looks like, for the running-cost estimate (spec 3.5 §7.22).
    #[must_use]
    pub fn cost(mut self, inputs: CostInputs) -> Self {
        self.desk.cost = Some(inputs);
        self
    }

    /// The deployment shape's name, such as "Single server or container" (spec 3.5 §7.22).
    #[must_use]
    pub fn shape(mut self, shape: impl Into<String>) -> Self {
        self.desk.shape = shape.into();
        self
    }

    /// The jurisdictions whose legal pages release 1 carries, such as `KE` (spec 3.5 §7.8.1
    /// "Legal").
    #[must_use]
    pub fn jurisdictions(mut self, jurisdictions: Vec<String>) -> Self {
        self.desk.jurisdictions = jurisdictions;
        self
    }

    /// The media pipeline uploads go through (spec 3.5 §7.8.2 "Media").
    #[must_use]
    pub fn media(mut self, media: MediaPipeline) -> Self {
        self.desk.media = media;
        self
    }

    /// The desk.
    pub fn build(self) -> WebsiteDesk {
        WebsiteDesk { inner: Arc::new(self.desk) }
    }
}

impl WebsiteDesk {
    /// A desk for `site`, named `name` on screen, whose onboarding `owner` holds every role
    /// until the first charter names its people, over `runtime`.
    pub fn builder(
        site: SiteRef,
        name: impl Into<String>,
        owner: impl Into<String>,
        runtime: Arc<SiteRuntime>,
    ) -> WebsiteDeskBuilder {
        WebsiteDeskBuilder {
            desk: Desk {
                site,
                name: name.into(),
                owner: owner.into(),
                languages: vec![LanguageTag::parse("en").expect("a valid tag")],
                currency: "USD".into(),
                clock: Arc::new(SystemClock),
                audit: None,
                runtime: RwLock::new(runtime),
                assembly: None,
                setup: None,
                facts: None,
                charters: None,
                ladder: None,
                rules: None,
                status: None,
                tracker: None,
                signals: None,
                wallet: None,
                updates: None,
                alarms: None,
                incidents: None,
                composer: None,
                notes: None,
                store: None,
                quotes: None,
                channel: None,
                discovery: None,
                connectors: Vec::new(),
                cost: None,
                shape: "Single server or container".into(),
                jurisdictions: Vec::new(),
                media: MediaPipeline::default(),
                paired: Mutex::new(None),
                onboarding: Mutex::new(onboarding::State::default()),
                charter: Mutex::new(charter::State::default()),
                reviews: Mutex::new(review::State::default()),
                editor: Mutex::new(editor::State::default()),
                next: AtomicU64::new(1),
            },
        }
    }

    /// The site.
    pub fn site(&self) -> &SiteRef {
        &self.inner.site
    }

    /// The site's name on screen.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The site runtime as it stands: the owner's chosen design replaces it during onboarding.
    pub async fn runtime(&self) -> Arc<SiteRuntime> {
        self.inner.runtime.read().await.clone()
    }

    pub(crate) fn bump_next(&self, at_least: u64) {
        self.inner.next.fetch_max(at_least, Ordering::Relaxed);
    }

    pub(crate) fn next_id(&self, prefix: &str) -> String {
        let n = self.inner.next.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{n}")
    }

    pub(crate) fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.inner.clock.now()
    }

    /// The roles `person` holds (spec 3.5 §7.6): the charter in force names them; before the
    /// first charter, the onboarding owner holds owner, editor and publisher. An agent holds
    /// none.
    ///
    /// # Errors
    ///
    /// Returns the charter store's error.
    pub async fn roles(&self, person: &str) -> Result<Vec<Role>> {
        if adk_kernel::charter::is_agent_principal(person) {
            return Ok(Vec::new());
        }
        if let Some(charters) = &self.inner.charters
            && let Some(current) = charters.current(self.inner.site.id).await.map_err(kernel)?
        {
            return Ok(current
                .charter
                .governance
                .roles
                .iter()
                .filter(|held| held.principal == person)
                .flat_map(|held| held.roles.iter().copied())
                .collect());
        }
        Ok(if person == self.inner.owner {
            vec![Role::Owner, Role::Editor, Role::Publisher]
        } else {
            Vec::new()
        })
    }

    /// Refuses `person` unless they hold one of `any`.
    pub(crate) async fn require(&self, person: &str, any: &[Role], what: &str) -> Result<()> {
        let held = self.roles(person).await?;
        if any.iter().any(|role| held.contains(role)) {
            return Ok(());
        }
        let names: Vec<&str> = any.iter().map(|role| words::role(*role)).collect();
        Err(forbidden(
            "website.ui.not_allowed",
            format!(
                "{what} needs the {} role in the site's charter; {person} does not hold it",
                names.join(" or ").to_lowercase()
            ),
        ))
    }
}

pub(crate) fn forbidden(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Server, ErrorCategory::Forbidden, code, message)
}

pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Server, ErrorCategory::InvalidInput, code, message)
}

pub(crate) fn not_found(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Server, ErrorCategory::NotFound, code, message)
}

pub(crate) fn unavailable(what: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Unsupported,
        "website.ui.not_configured",
        format!(
            "{what} is not set up on this site's desk; the host adds it to WebsiteDesk::builder"
        ),
    )
}

pub(crate) fn kernel(error: adk_kernel::KernelError) -> AdkError {
    let category = match &error {
        adk_kernel::KernelError::Unauthorized { .. } => ErrorCategory::Forbidden,
        adk_kernel::KernelError::Config { .. } => ErrorCategory::InvalidInput,
        _ => ErrorCategory::Internal,
    };
    AdkError::new(ErrorComponent::Server, category, "website.ui.kernel", error.to_string())
}

pub(crate) fn site(error: adk_site::SiteError) -> AdkError {
    let category = match &error {
        adk_site::SiteError::NotFound(_) | adk_site::SiteError::ReleaseNotFound { .. } => {
            ErrorCategory::NotFound
        }
        adk_site::SiteError::InvalidContent { .. }
        | adk_site::SiteError::GateFailed(_)
        | adk_site::SiteError::MediaRefused { .. } => ErrorCategory::InvalidInput,
        adk_site::SiteError::ServerCode { .. } => ErrorCategory::Forbidden,
        _ => ErrorCategory::Internal,
    };
    AdkError::new(ErrorComponent::Server, category, "website.ui.site", error.to_string())
}

/// Values a form sent back, by field id.
pub type FormValues = BTreeMap<String, String>;

/// A required value of `values`, trimmed.
pub(crate) fn required<'a>(values: &'a FormValues, field: &str, label: &str) -> Result<&'a str> {
    values
        .get(field)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("website.ui.missing", format!("{label} is needed")))
}

/// An optional value of `values`, trimmed; `None` when blank.
pub(crate) fn optional<'a>(values: &'a FormValues, field: &str) -> Option<&'a str> {
    values.get(field).map(|value| value.trim()).filter(|value| !value.is_empty())
}
