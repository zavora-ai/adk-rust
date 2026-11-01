//! A bakery's website on local fixtures (spec 3.5 §11): its content on a `DirectoryHost` in
//! the workspace, its two partitions in a kernel partition table, in-memory facts, charters,
//! autonomy records, signals, standing rules, KPIs and a wallet, and the owner's desk over
//! them. Shared by `scripted_website` and the website API tests.
//!
//! | Site | What it holds |
//! |---|---|
//! | [`fresh`] | Nothing yet: a setup code, a recorded online presence for discovery, three services to connect |
//! | [`live`] | Launched: four signed releases, a week of KPIs, a job that earned a promotion and one lowered, signals, a wallet with a held payment, a held price change and a held feed delivery, an alarm, and last week's review |

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditLogStore, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{ActingContext, InMemorySpendLedger, SpendLedger, SubjectKind};
use adk_harness::website::{SitePartitions, SiteRef, SiteRuntime};
use adk_harness::{
    FactKind, FactScope, FactSource, FactStatus, FactStore, InMemoryFactStore,
    InMemoryStandingRuleStore, MemoryFact, Provenance,
};
use adk_kernel::channels::{ChannelSink, DecisionCard, StopNotice};
use adk_kernel::charter::{
    AutonomyLadder, CharterStore, CommitDecision, DecisionOutcome, InMemoryAutonomyStore,
    InMemoryCharterStore, InMemoryKpiStore, InMemorySignalStore, JobDecision, KpiSample, KpiStore,
    KpiTracker, RoleApproval,
};
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_kernel::{InMemoryAgendaStore, Partitions};
use adk_payments::wallet::{
    InMemoryBillingAccount, InMemoryUseBasedScheme, InMemoryWalletProvider, InMemoryWalletStore,
    Mandate, Payee, PayeeRule, PaymentChallenge, PaymentRequest, ProviderBillingRail, RailRef,
    UseBasedRail, Wallet, WalletCaller, WalletKpiSource, WalletPolicy, WalletStore,
};
use adk_site::content::{InMemoryContentHistory, InMemoryContentStore, SiteConfig, SiteProject};
use adk_site::feed::{FeedMapping, FeedRecord, FieldMapping, FieldType};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::DirectoryHost;
use adk_site::live::InMemoryLiveStore;
use adk_site::log::SiteLog;
use adk_site::migrate::{
    Fact, FactSource as FoundAt, SiteInventory, SiteSource, SourceContext, SourceKind,
};
use adk_site::pipeline::{Change, EntryPoint, PublishingPipeline};
use adk_site::signer::Ed25519ReleaseSigner;
use adk_site::updates::UpdateEngine;
use adk_website::alarms::{Alarm, AlarmRouter};
use adk_website::incident::{InMemoryStatusBoard, StatusRecord, StatusSink};
use adk_website::kinds::{CharterInputs, KindTemplate, template};
use adk_website::onboarding::SetupGate;
use adk_website::onboarding::build::{SitePlan, known_facts, stage_release_one};
use adk_website::onboarding::cost::{CostInputs, ModelPrice};
use adk_website::review::{ReviewComposer, ReviewInputs};
use adk_website::team::{AgentRole, SiteTeam};
use adk_website::themes::{ThemeId, theme};
use adk_website::ui::{
    ConnectMethod, ConnectStep, ConnectionKind, Connector, ReviewNotes, SiteAssembly, WalletParts,
    WebsiteDesk,
};
use adk_website::wallet::WalletKpi;
use async_trait::async_trait;
use awp_types::{
    Bound, Channel, ContentId, ContentItem, ContentKind, ContentState, FeedBounds, LanguageTag,
    Markdown, Measurement, PriceRule, Rate, Role, Signal, SignalKind, SiteKind, SiteState, Target,
    Urgency,
};
use chrono::{Datelike, Duration, Utc};
use serde_json::json;

/// The site's owner: every role until the charter names more people.
pub const OWNER: &str = "amina";
/// The site's domain.
pub const DOMAIN: &str = "juniper.example";
/// The site's name.
pub const NAME: &str = "Juniper Bakery";

/// The owner's channel: keeps what it was sent.
#[derive(Debug, Default)]
pub struct Phone {
    pub sent: Mutex<Vec<String>>,
}

#[async_trait]
impl ChannelSink for Phone {
    fn channel(&self) -> &str {
        "whatsapp"
    }

    async fn send_message(&self, _thread: &str, text: &str) -> adk_kernel::Result<()> {
        tracing::info!(text, "sent to the owner's channel");
        self.sent.lock().unwrap().push(text.to_string());
        Ok(())
    }

    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
        self.send_message(thread, &card.text()).await
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.send_message(thread, &notice.text()).await
    }
}

/// A service that connects through its own page or a code; connected once started, as the
/// person would finish it outside the site.
struct Scripted {
    id: &'static str,
    kind: ConnectionKind,
    service: &'static str,
    method: ConnectMethod,
    step: ConnectStep,
    account: &'static str,
    connected: Mutex<bool>,
}

#[async_trait]
impl Connector for Scripted {
    fn id(&self) -> &str {
        self.id
    }

    fn kind(&self) -> ConnectionKind {
        self.kind
    }

    fn service(&self) -> &str {
        self.service
    }

    fn method(&self) -> ConnectMethod {
        self.method
    }

    async fn start(&self, _person: &str) -> adk_core::Result<ConnectStep> {
        *self.connected.lock().unwrap() = true;
        Ok(self.step.clone())
    }

    async fn connected(&self) -> adk_core::Result<Option<String>> {
        Ok(self.connected.lock().unwrap().then(|| self.account.to_string()))
    }
}

fn connectors() -> Vec<Arc<dyn Connector>> {
    vec![
        Arc::new(Scripted {
            id: "domain",
            kind: ConnectionKind::Domain,
            service: "Registrar DNS for juniper.example",
            method: ConnectMethod::OAuth,
            step: ConnectStep::Open {
                url: "https://registrar.example/oauth/authorize?client=juniper".into(),
            },
            account: "juniper.example",
            connected: Mutex::new(false),
        }),
        Arc::new(Scripted {
            id: "telegram",
            kind: ConnectionKind::OwnerChannel,
            service: "Telegram",
            method: ConnectMethod::Pairing,
            step: ConnectStep::Code {
                code: "JNPR-4821".into(),
                how: "Send this code to @JuniperSiteBot in Telegram".into(),
            },
            account: "@amina",
            connected: Mutex::new(false),
        }),
        Arc::new(Scripted {
            id: "email",
            kind: ConnectionKind::Email,
            service: "Mail provider for hello@juniper.example",
            method: ConnectMethod::Broker,
            step: ConnectStep::Open {
                url: "https://broker.example/grants/new?scope=mail.send".into(),
            },
            account: "hello@juniper.example",
            connected: Mutex::new(false),
        }),
    ]
}

/// A recorded online presence: a listing and a social page (spec 3.5 §11, S2).
struct RecordedPresence;

#[async_trait]
impl SiteSource for RecordedPresence {
    async fn inventory(&self, ctx: &SourceContext) -> adk_site::migrate::Result<SiteInventory> {
        let now = ctx.clock.now();
        let mut inventory = SiteInventory::new(SourceKind::Presence, now);
        inventory.site_name = Some(NAME.into());
        inventory.description = Some("Bakery in Nakuru".into());
        let found = |id: &str, key: &str, value: &str, url: &str| Fact {
            id: id.into(),
            key: key.into(),
            value: value.into(),
            source: FoundAt { url: url.into(), retrieved_at: now, locator: "json-ld".into() },
            status: adk_site::gate::FactStatus::Assumed,
        };
        let listing = "https://maps.example/place/juniper-bakery-nakuru";
        let social = "https://social.example/juniperbakery";
        inventory.facts = vec![
            found("f1", "type", "LocalBusiness", listing),
            found("f2", "telephone", "+254 700 123 456", listing),
            found("f3", "address", "Kenyatta Avenue 12, Nakuru", listing),
            found("f4", "openingHours", "Mo-Sa 07:00-18:00", listing),
            found("f6", "description", "Rye and sourdough, baked every morning", social),
        ];
        Ok(inventory)
    }
}

/// Builds the runtime with a theme over the site's stores, host and log.
struct Assembly {
    site: SiteRef,
    template: KindTemplate,
    base: SiteProject,
    store: Arc<InMemoryContentStore>,
    history: Arc<InMemoryContentHistory>,
    host: Arc<DirectoryHost>,
    log: Arc<SiteLog>,
    work: PathBuf,
    charters: Arc<InMemoryCharterStore>,
    facts: Arc<InMemoryFactStore>,
    scope: FactScope,
    trail: AuditTrail,
    /// The site's approval requests: one service across every runtime a theme rebuilds, as
    /// a served site's kernel keeps them.
    approvals: Arc<dyn adk_core::ApprovalService>,
}

impl SiteAssembly for Assembly {
    fn runtime(&self, chosen: Option<ThemeId>) -> Arc<SiteRuntime> {
        let mut base = self.base.clone();
        if let Some(id) = chosen {
            base.theme = theme(id);
        }
        SiteRuntime::builder(
            self.site.clone(),
            base,
            self.store.clone(),
            Arc::new(BuiltinGenerator),
            self.host.clone(),
            self.log.clone(),
            &self.work,
        )
        .history(self.history.clone())
        .charters(self.charters.clone())
        .facts(self.facts.clone(), self.scope.clone())
        .gate(self.template.gate(&self.site.origin))
        .audit(self.trail.clone())
        .approvals(self.approvals.clone())
        .build()
        .expect("a runtime that need not be durable always builds")
    }
}

/// A site and its desk.
pub struct ScriptedSite {
    pub desk: WebsiteDesk,
    pub setup_code: Option<String>,
    pub phone: Arc<Phone>,
    pub audit: InMemoryAuditRecorder,
    pub host_dir: PathBuf,
}

struct Parts {
    site: SiteRef,
    template: KindTemplate,
    team: SiteTeam,
    assembly: Arc<Assembly>,
    partitions: SitePartitions,
    ladder: AutonomyLadder,
    rules: Arc<InMemoryStandingRuleStore>,
    signals: Arc<InMemorySignalStore>,
    agenda: Arc<InMemoryAgendaStore>,
    status: Arc<InMemoryStatusBoard>,
    audit: InMemoryAuditRecorder,
}

async fn parts(dir: &Path, kind: SiteKind) -> Parts {
    let site = SiteRef::new(uuid::Uuid::new_v4(), "juniper", format!("https://{DOMAIN}"));
    let template = template(kind);
    let team = SiteTeam::new(site.clone(), &template);
    let mut config = SiteConfig::new(NAME, DOMAIN, vec!["en".parse().unwrap()]);
    config.jurisdictions = vec!["KE".into()];
    config.security.contact = vec![format!("mailto:security@{DOMAIN}")];
    template.configure_site(&mut config);
    let base = SiteProject::new(config, Vec::new(), Utc::now());
    let host_dir = dir.join("host");
    let work = dir.join("work");
    std::fs::create_dir_all(&host_dir).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let host = Arc::new(DirectoryHost::new(&host_dir));
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed(DOMAIN, [7; 32]).unwrap());
    let log = Arc::new(
        SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), DOMAIN, signer).await.unwrap(),
    );
    let partitions = SitePartitions::new(&site);
    let table = Partitions::in_memory();
    partitions.install(&table, AuditActor::default()).await.unwrap();
    let charters = Arc::new(InMemoryCharterStore::new());
    let ladder =
        AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new())).with_charters(charters.clone());
    let facts = Arc::new(InMemoryFactStore::new());
    let audit = InMemoryAuditRecorder::new();
    let approvals =
        adk_core::StoreApprovalService::new(Arc::new(adk_core::InMemoryApprovalStore::new()))
            .with_directory(Arc::new(
                adk_kernel::charter::CharterDirectory::new(charters.clone()).for_site(site.id),
            ))
            .with_audit(AuditTrail::new(Arc::new(audit.clone())));
    let assembly = Arc::new(Assembly {
        site: site.clone(),
        template: template.clone(),
        base,
        store: Arc::new(InMemoryContentStore::default()),
        history: Arc::new(InMemoryContentHistory::default()),
        host,
        log,
        work,
        charters,
        facts,
        scope: partitions.fact_scope(OWNER),
        trail: AuditTrail::new(Arc::new(audit.clone())),
        approvals: Arc::new(approvals),
    });
    Parts {
        site,
        template,
        team,
        assembly,
        partitions,
        ladder,
        rules: Arc::new(InMemoryStandingRuleStore::new()),
        signals: Arc::new(InMemorySignalStore::new()),
        agenda: Arc::new(InMemoryAgendaStore::new()),
        status: Arc::new(InMemoryStatusBoard::new()),
        audit,
    }
}

fn desk(
    parts: &Parts,
    runtime: Arc<SiteRuntime>,
    phone: Arc<Phone>,
) -> adk_website::ui::WebsiteDeskBuilder {
    let assembly = parts.assembly.clone();
    let http = Arc::new(adk_site::migrate::http::HttpClient::builder().build().unwrap());
    let acting = ActingContext::derived(&parts.site.name, "steward", OWNER, SubjectKind::Human);
    let mut builder = WebsiteDesk::builder(parts.site.clone(), NAME, OWNER, runtime)
        .languages(vec!["en".parse().unwrap()])
        .currency("KES")
        .jurisdictions(vec!["KE".into()])
        .audit(assembly.trail.clone())
        .assembly(assembly.clone())
        .facts(assembly.facts.clone(), assembly.scope.clone())
        .charters(assembly.charters.clone())
        .ladder(parts.ladder.clone(), parts.rules.clone())
        .status(parts.status.clone())
        .signals(parts.signals.clone(), parts.partitions.back_office().id.clone())
        .channel(phone, "owner")
        .discovery(SourceContext::new(acting, http), Some(Arc::new(RecordedPresence)))
        .cost(CostInputs::small(
            150_000,
            180_000,
            ModelPrice { input_per_mtok: 40_000, output_per_mtok: 200_000 },
        ))
        .shape("Single server or container");
    for connector in connectors() {
        builder = builder.connector(connector);
    }
    builder
}

/// A freshly started site: it waits for its setup code (spec 3.5 §7.17, A-I20).
pub async fn fresh(dir: &Path, kind: SiteKind) -> ScriptedSite {
    let parts = parts(dir, kind).await;
    let gate = Arc::new(SetupGate::new(Pairing::new(Arc::new(InMemoryDeviceStore::new())), OWNER));
    let code = gate.issue().await.unwrap().code;
    let phone = Arc::new(Phone::default());
    let runtime = parts.assembly.runtime(None);
    let desk = desk(&parts, runtime, phone.clone()).setup(gate).build();
    ScriptedSite {
        desk,
        setup_code: Some(code),
        phone,
        audit: parts.audit.clone(),
        host_dir: dir.join("host"),
    }
}

fn item(kind: ContentKind, slug: &str, fields: serde_json::Value, body: &str) -> ContentItem {
    let fields: BTreeMap<String, serde_json::Value> = serde_json::from_value(fields).unwrap();
    ContentItem {
        id: ContentId(format!("{}/{slug}/en", adk_site::content::kind_directory(kind))),
        kind,
        slug: slug.into(),
        language: "en".parse().unwrap(),
        fields,
        volatile: Vec::new(),
        body: Some(Markdown(body.into())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Published,
    }
}

async fn publish(runtime: &SiteRuntime, item: ContentItem, job: &str) -> u64 {
    let outcome = runtime
        .pipeline()
        .run(Change {
            items: vec![item],
            media: Vec::new(),
            entry: EntryPoint::WebEditor,
            proposer: OWNER.into(),
            job: job.into(),
            action_class: "publish".into(),
        })
        .await
        .unwrap();
    outcome.published().map(|p| p.staged.manifest.seq).expect("published")
}

async fn known(facts: &dyn FactStore, scope: &FactScope, text: &str) {
    let mut provenance = Provenance::new(FactSource::UserStated, "seed", "seed");
    provenance.quote = Some(text.into());
    facts
        .put(MemoryFact::new(
            scope.clone(),
            FactKind::ProjectFact,
            text,
            FactStatus::Known,
            provenance,
        ))
        .await
        .unwrap();
}

struct Notes;

#[async_trait]
impl ReviewNotes for Notes {
    async fn notes(&self, _week: chrono::NaiveDate) -> adk_core::Result<ReviewInputs> {
        let mut inputs = ReviewInputs::default();
        inputs.work.insert(
            AgentRole::Steward,
            vec!["Planned the week and answered 3 of your messages".into()],
        );
        inputs.work.insert(
            AgentRole::Growth,
            vec![
                "Drafted an FAQ for gluten-free questions; it waits for you".into(),
                "Fixed two broken links on the menu page".into(),
            ],
        );
        inputs
            .work
            .insert(AgentRole::Guardian, vec!["Checked the site 2,016 times; no drift".into()]);
        inputs.work.insert(
            AgentRole::Revenue,
            vec!["Handed you one catering enquiry with its summary".into()],
        );
        inputs
            .experiments
            .push("Menu headline B: +9% enquiries (95% interval 1% to 17%); keeping B".into());
        inputs.handed_off.push("A café ordering 40 loaves on weekdays".into());
        inputs.questions.push("Should the site take orders for Saturday collection?".into());
        inputs.questions.push("May we list prices for wholesale customers?".into());
        Ok(inputs)
    }
}

/// A launched site with a week behind it (spec 3.5 §7.24, §7.21, §7.8.3).
pub async fn live(dir: &Path) -> ScriptedSite {
    let parts = parts(dir, SiteKind::Business).await;
    let assembly = parts.assembly.clone();
    let now = Utc::now();
    let scope = assembly.scope.clone();
    for text in [
        "Juniper Bakery bakes rye and sourdough every morning",
        "Juniper Bakery is run by Amina Odhiambo and her brother Tom",
        "Opening hours: Monday to Saturday, 07:00 to 18:00",
        "Email: hello@juniper.example",
        "Address: Kenyatta Avenue 12, Nakuru",
    ] {
        known(assembly.facts.as_ref(), &scope, text).await;
    }

    // Charter version 1, with baselines found and one target the owner set.
    let mut inputs = CharterInputs::new(
        parts.site.id,
        NAME,
        OWNER,
        "Fresh rye and sourdough for Nakuru, baked every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    inputs.p1_channels = vec![Channel::Sms];
    inputs.p2_channels = vec![Channel::Whatsapp];
    inputs.contact_email = Some("hello@juniper.example".into());
    inputs.caps = Some(BTreeMap::from([
        (awp_types::SpendCategory::Hosting, 150_000),
        (awp_types::SpendCategory::Models, 300_000),
        (awp_types::SpendCategory::Domains, 150_000),
        (awp_types::SpendCategory::Other, 50_000),
    ]));
    let mut charter = parts.template.draft_charter(&parts.team, &inputs);
    for kpi in &mut charter.kpis {
        if kpi.id == "qualified_leads" {
            kpi.baseline = Some(Measurement {
                value: 2.0,
                window: std::time::Duration::from_secs(14 * 86_400),
                measured_at: now - Duration::days(7),
            });
            kpi.target = Some(Target { value: 4.0, horizon: now + Duration::days(60) });
        }
    }
    let decision = CommitDecision::new("seed-charter", now).with_approval(RoleApproval::new(
        Role::Owner,
        SubjectRef::human(OWNER),
        now,
    ));
    let charter = assembly.charters.commit(charter, decision).await.unwrap().charter;
    adk_website::jobs::register(&parts.ladder, &charter).await.unwrap();

    // Release 1 and three more: a post, a product and a correction.
    let runtime = assembly.runtime(Some(ThemeId::Harbour));
    let facts = known_facts(assembly.facts.as_ref(), &scope).await.unwrap();
    let plan = SitePlan::new(&charter, NAME);
    let steward = AgentRole::Steward.principal(&parts.site);
    let staged =
        stage_release_one(&runtime, plan.items(&charter, &facts, &["KE".to_string()]), &steward)
            .await
            .unwrap();
    assert!(staged.held.is_empty(), "{}", staged.gate.summary());
    let home = runtime.store().get(&ContentId("pages/home/en".into())).await.unwrap().unwrap();
    publish(&runtime, home, "onboarding").await;
    let post = item(
        ContentKind::Post,
        "rye-week",
        json!({ "title": "Rye week", "summary": "Seven days of rye, from light to dark.", "byline": "Amina" }),
        "This week every loaf is rye. Come early: the dark rye sells out by ten.",
    );
    publish(&runtime, post, "web-editor").await;
    let loaf = item(
        ContentKind::Product,
        "sourdough-loaf",
        json!({ "title": "Sourdough loaf", "summary": "Our everyday loaf, 800 g.", "price": 450, "currency": "KES", "sku": "SD-800" }),
        "Slow-fermented for 36 hours. Keeps for four days.",
    );
    publish(&runtime, loaf, "web-editor").await;
    runtime
        .pipeline()
        .correct(
            &ContentId("posts/rye-week/en".into()),
            |item| {
                item.body = Some(Markdown(
                    "This week every loaf is rye. Come early: the dark rye sells out by nine."
                        .into(),
                ));
            },
            "We sell out an hour earlier than we first said.",
            OWNER,
            EntryPoint::WebEditor,
        )
        .await
        .unwrap();
    let latest = runtime.pipeline().previous().await.unwrap();

    // Three weeks of measures: this week and the two before it.
    let kpis = Arc::new(InMemoryKpiStore::new());
    let today = now.date_naive();
    let monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    for kpi in &charter.kpis {
        let weeks = match kpi.id.as_str() {
            "qualified_leads" => [2.0, 2.5, 3.0],
            "response_time" => [7.0, 6.0, 4.5],
            "bookings" => [5.0, 6.0, 5.5],
            "review_rating" => [4.5, 4.6, 4.7],
            "freshness" => [90.0, 92.0, 96.0],
            _ => [100.0, 100.0, 100.0],
        };
        for day in 0..21 {
            let at = (monday - Duration::days(14) + Duration::days(day))
                .and_hms_opt(9, 0, 0)
                .unwrap()
                .and_utc();
            if at > now {
                break;
            }
            let value = weeks[(day / 7) as usize];
            kpis.record(KpiSample::new(parts.site.id, &kpi.id, at, value, kpi.source.to_string()))
                .await
                .unwrap();
        }
    }
    let tracker = KpiTracker::new(parts.site.id, assembly.charters.clone(), kpis.clone());

    // Autonomy: twenty unchanged approvals earn freshness an offer; two rejections lower seo.
    let owner = SubjectRef::human(OWNER);
    for n in 0..20 {
        parts
            .ladder
            .record_decision(JobDecision::new(
                parts.site.id,
                "freshness",
                format!("fresh-{n}"),
                DecisionOutcome::ApprovedUnchanged,
                owner.clone(),
                Utc::now(),
            ))
            .await
            .unwrap();
    }
    for n in 0..2 {
        parts
            .ladder
            .record_decision(JobDecision::new(
                parts.site.id,
                "seo",
                format!("seo-{n}"),
                DecisionOutcome::Rejected,
                owner.clone(),
                Utc::now(),
            ))
            .await
            .unwrap();
    }

    // Signals from the public side, typed and bounded.
    let sink = parts.partitions.signal_sink(parts.signals.clone(), parts.agenda.clone());
    let concierge = AgentRole::Concierge.principal(&parts.site);
    let bounded = |text: &str| awp_types::Bounded::truncated(text, 280);
    for (kind, at) in [
        (
            SignalKind::ContentGap {
                topic: bounded("gluten-free bread"),
                question_count: 6,
                samples: vec![bounded("do you bake gluten free?")],
            },
            now - Duration::hours(5),
        ),
        (
            SignalKind::LeadCaptured {
                lead_ref: awp_types::LeadRef("lead-7".into()),
                source: Channel::Chat,
                consent: Vec::new(),
                answers: BTreeMap::new(),
            },
            now - Duration::hours(2),
        ),
        (
            SignalKind::ReviewSeen {
                platform: "Maps".into(),
                rating: 5,
                excerpt: bounded("Best rye in Nakuru"),
            },
            now - Duration::hours(26),
        ),
    ] {
        sink.emit(Signal {
            id: uuid::Uuid::new_v4(),
            kind,
            emitted_by: concierge.clone(),
            emitted_at: at,
        })
        .await
        .unwrap();
    }

    // The wallet: hosting and model tokens paid; a domain renewal over its cap is held.
    let policy = WalletPolicy::new("KES")
        .with_mandate(
            awp_types::SpendCategory::Hosting,
            Mandate::new(150_000, 150_000, RailRef::provider_billing("hostco"))
                .with_payee(PayeeRule::Id("hostco".into())),
        )
        .with_mandate(
            awp_types::SpendCategory::Models,
            Mandate::new(300_000, 20_000, RailRef::use_based())
                .with_payee(PayeeRule::Domain("models.example".into())),
        )
        .with_mandate(
            awp_types::SpendCategory::Domains,
            Mandate::new(150_000, 150_000, RailRef::provider_billing("registrar"))
                .with_payee(PayeeRule::Id("registrar".into())),
        );
    // Paid last Wednesday, so last week's review reports the spend.
    let paid_at = (monday - Duration::days(5)).and_hms_opt(10, 0, 0).unwrap().and_utc();
    let wallet_clock = adk_core::ManualClock::new(paid_at.min(now));
    let clock: Arc<dyn adk_core::Clock> = Arc::new(wallet_clock.clone());
    let ledger = InMemorySpendLedger::new(policy.spend_limits(&parts.site.name).unwrap())
        .with_clock(clock.clone());
    let store: Arc<dyn WalletStore> = Arc::new(InMemoryWalletStore::new());
    let wallet = Wallet::builder(
        &parts.site.name,
        policy,
        Arc::new(
            InMemoryWalletProvider::stored_value("treasury", "KES", 2_500_000)
                .with_clock(clock.clone()),
        ),
    )
    .rail(Arc::new(UseBasedRail::new(Arc::new(InMemoryUseBasedScheme::new()))))
    .rail(Arc::new(ProviderBillingRail::new(Arc::new(
        InMemoryBillingAccount::new("hostco", "KES").with_clock(clock.clone()),
    ))))
    .rail(Arc::new(ProviderBillingRail::new(Arc::new(
        InMemoryBillingAccount::new("registrar", "KES").with_clock(clock.clone()),
    ))))
    .spend_ledger(Arc::new(ledger) as Arc<dyn SpendLedger>)
    .store(store.clone())
    .owner(owner.clone())
    .clock(clock)
    .build()
    .unwrap();
    let finance =
        WalletCaller::agent(AgentRole::Finance.principal(&parts.site)).with_charter_version(1);
    wallet
        .pay(
            PaymentRequest::new(
                "hosting-month",
                awp_types::SpendCategory::Hosting,
                Payee::new("hostco"),
                120_000,
                "KES",
            ),
            &finance,
        )
        .await
        .unwrap();
    for n in 0..4 {
        let challenge = PaymentChallenge::new(
            "https://api.models.example/v1/generate",
            "acct-models",
            15_000,
            "KES",
        );
        wallet
            .pay_challenge(
                &format!("tokens-{n}"),
                awp_types::SpendCategory::Models,
                &challenge,
                &finance,
            )
            .await
            .unwrap();
    }
    let _ = wallet
        .pay(
            PaymentRequest::new(
                "domain-renewal",
                awp_types::SpendCategory::Domains,
                Payee::new("registrar"),
                180_000,
                "KES",
            )
            .with_description("Renew juniper.example for two years"),
            &finance,
        )
        .await;
    wallet_clock.set(Utc::now());
    let wallet_kpi = Arc::new(WalletKpi::new(WalletKpiSource::new(wallet.clone(), parts.site.id)));

    // Constant updates: a supplier feed whose delivery breaks its bounds, and a price change
    // beyond the price rule, both held for the owner.
    let feed_pipeline = Arc::new(
        PublishingPipeline::builder(
            assembly.base.clone(),
            assembly.store.clone(),
            Arc::new(BuiltinGenerator),
            assembly.host.clone(),
            assembly.log.clone(),
            dir.join("work/feed"),
        )
        .history(assembly.history.clone())
        .previous(latest)
        .build(),
    );
    let engine = Arc::new(
        UpdateEngine::builder(feed_pipeline, Arc::new(InMemoryLiveStore::default()))
            .build()
            .unwrap(),
    );
    engine
        .add_feed(
            FeedMapping::new("flour-supplier", ContentKind::Product, "en".parse().unwrap(), "sku")
                .id_prefix("products/")
                .field(
                    "price",
                    FieldMapping::new("price", FieldType::Number).bound(Bound::Range {
                        minimum: None,
                        exclusive_minimum: Some(0.0),
                        maximum: None,
                        exclusive_maximum: None,
                    }),
                ),
            FeedBounds {
                max_change_fraction: 0.2,
                price_rule: PriceRule::default(),
                field_bounds: BTreeMap::new(),
                max_rate: Rate { per_minute: 1_000 },
            },
        )
        .await
        .unwrap();
    engine
        .ingest(
            "flour-supplier",
            vec![FeedRecord::new([("sku", json!("sourdough-loaf/en")), ("price", json!(1200))])],
        )
        .await
        .unwrap();
    let mut dearer = runtime
        .store()
        .get(&ContentId("products/sourdough-loaf/en".into()))
        .await
        .unwrap()
        .unwrap();
    dearer.fields.insert("price".into(), json!(600));
    engine
        .submit(adk_site::batch::Update {
            item: dearer,
            at: now,
            entry: EntryPoint::Feed { feed: "pos".into() },
            proposer: "urn:feed:pos".into(),
            trigger: None,
            model_calls: 0,
        })
        .await
        .unwrap();
    engine.drain().await.unwrap();

    // Status, an alarm, and last week's review.
    parts
        .status
        .set(
            parts.site.id,
            StatusRecord {
                state: SiteState::Operational,
                since: now - Duration::days(9),
                reason: "operational".into(),
                release_seq: Some(4),
            },
        )
        .await
        .unwrap();
    let phone = Arc::new(Phone::default());
    let alarms = Arc::new(
        AlarmRouter::new(parts.site.id, &charter.reporting).with_sink(phone.clone(), "owner"),
    );
    alarms
        .raise(Alarm::new(
            parts.site.id,
            Urgency::P2,
            "renewal",
            "The domain renewal is over its monthly cap and waits for you",
            now - Duration::hours(3),
        ))
        .await;
    let composer = ReviewComposer::new(
        parts.site.id,
        tracker.clone(),
        parts.ladder.clone(),
        assembly.charters.clone(),
    )
    .with_wallet(wallet_kpi.clone())
    .with_alarms(alarms.clone())
    .with_audit(assembly.trail.clone());
    let notes = Arc::new(Notes);
    let last_week = composer
        .compose(monday - Duration::days(7), notes.notes(monday).await.unwrap())
        .await
        .unwrap();

    let desk = desk(&parts, runtime, phone.clone())
        .tracker(tracker)
        .wallet(WalletParts { wallet, store, kpi: wallet_kpi })
        .updates(engine)
        .alarms(alarms)
        .composer(composer)
        .notes(notes)
        .build();
    desk.record_review(last_week).await;
    ScriptedSite {
        desk,
        setup_code: None,
        phone,
        audit: parts.audit.clone(),
        host_dir: dir.join("host"),
    }
}

/// The language tag the site is in.
pub fn english() -> LanguageTag {
    "en".parse().unwrap()
}
