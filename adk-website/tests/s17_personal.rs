//! S17 — a personal site (spec 3.5 §5 S17, §7.16.1 Personal, §7.18 stage 2–3, §7.19,
//! §7.21, §7.22): a writer's WordPress blog is discovered from its export, proposed as a
//! personal site, migrated with its URLs kept and its newsletter plugin replaced by a
//! consented capability; the team runs Concierge, Steward, Growth and Guardian only; the
//! weekly review reports readers, subscribers and cost; the running cost stays inside a
//! budget of hosting plus a small model allowance.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{InMemorySpendLedger, ManualClock, SpendLedger, SubjectRef};
use adk_kernel::charter::{InMemoryKpiStore, KpiSample, KpiStore, KpiTracker};
use adk_payments::wallet::{
    InMemoryBillingAccount, InMemoryUseBasedScheme, InMemoryWalletProvider, InMemoryWalletStore,
    Mandate, Payee, PayeeRule, PaymentChallenge, PaymentRequest, ProviderBillingRail, RailRef,
    SpendCategory, UseBasedRail, Wallet, WalletCaller, WalletKpiSource, WalletPolicy,
};
use adk_site::content::SiteProject;
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_site::migrate::sources::Wxr;
use adk_website::kinds::{CharterInputs, KindRule};
use adk_website::onboarding::StartingPoint;
use adk_website::onboarding::cost::{CostInputs, ModelPrice, estimate};
use adk_website::onboarding::discovery::{Discovery, OwnerCard};
use adk_website::review::{ReviewComposer, ReviewInputs, Trend};
use adk_website::team::{AgentRole, TeamKit};
use adk_website::wallet::WalletKpi;
use awp_types::{ConsentScope, Role, SiteKind};
use chrono::{NaiveDate, TimeZone, Utc};
use common::*;

const MODELS_HOST: &str = "https://api.models.example/v1/generate";

fn wallet(clock: &ManualClock, site: &str) -> Wallet {
    let shared: Arc<dyn adk_core::Clock> = Arc::new(clock.clone());
    let policy = WalletPolicy::new("USD")
        .with_mandate(
            SpendCategory::Hosting,
            Mandate::new(500, 500, RailRef::provider_billing("hostco"))
                .with_payee(PayeeRule::Id("hostco".into())),
        )
        .with_mandate(
            SpendCategory::Models,
            Mandate::new(1_000, 200, RailRef::use_based())
                .with_payee(PayeeRule::Domain("models.example".into())),
        );
    let ledger =
        InMemorySpendLedger::new(policy.spend_limits(site).unwrap()).with_clock(shared.clone());
    Wallet::builder(
        site,
        policy,
        Arc::new(
            InMemoryWalletProvider::stored_value("treasury", "USD", 100_000)
                .with_clock(shared.clone()),
        ),
    )
    .rail(Arc::new(UseBasedRail::new(Arc::new(InMemoryUseBasedScheme::new()))))
    .rail(Arc::new(ProviderBillingRail::new(Arc::new(
        InMemoryBillingAccount::new("hostco", "USD").with_clock(shared.clone()),
    ))))
    .spend_ledger(Arc::new(ledger) as Arc<dyn SpendLedger>)
    .store(Arc::new(InMemoryWalletStore::new()))
    .owner(SubjectRef::human(OWNER))
    .clock(shared)
    .build()
    .unwrap()
}

#[tokio::test]
async fn s17_a_writers_blog_becomes_a_personal_site_with_four_agents_inside_its_budget() {
    let site = Site::new(SiteKind::Personal, "Wanjiru Writes", "wanjiru.blog", &["en"]).await;

    // Discover from the export: a personal site on a legacy runtime.
    let export = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/blog.xml"),
    )
    .unwrap();
    let discovered = Discovery::new(
        StartingPoint::ExistingSite { url: "https://wanjiru.blog/".into() },
        "en".parse().unwrap(),
    )
    .unwrap()
    .with_source(Arc::new(Wxr::from_bytes(export)))
    .run(&source_context("wanjiru.blog"))
    .await
    .unwrap();
    let proposal = discovered.kind.clone().expect("a kind is proposed");
    assert_eq!(proposal.kind, SiteKind::Personal, "{proposal:?}");
    assert!(discovered.legacy_runtime);
    let cards = discovered.cards();
    assert!(cards.iter().any(|c| matches!(c, OwnerCard::Freeze { .. })), "the freeze is offered");
    assert!(
        cards.iter().any(|c| matches!(c, OwnerCard::Design { .. })),
        "keep the design or a new theme"
    );

    // The newsletter plugin becomes a consented capability; posts keep their URLs.
    let migration = discovered.migration.as_ref().unwrap();
    let newsletter =
        migration.features.capability("subscribe_newsletter").expect("the plugin's replacement");
    assert_eq!(newsletter.consent.as_ref().unwrap().scope, ConsentScope::Newsletter);
    assert!(site.template.capabilities.contains(&"subscribe_newsletter"));
    let mut project =
        SiteProject::new(site.base.config.clone(), migration.converted.items.clone(), Utc::now());
    project.config.redirects.extend(migration.converted.redirects.clone());
    let out = tempfile::tempdir().unwrap();
    let report = BuiltinGenerator.build(&project, out.path()).await.unwrap();
    for path in [
        "2026/09/12/city-walks/index.html",
        "2026/10/03/markets-at-dawn/index.html",
        "2026/10/17/the-last-matatu/index.html",
    ] {
        assert!(report.outputs.contains(path), "{path} keeps its URL: {:?}", report.outputs);
    }
    assert!(
        report.outputs.iter().any(|p| p.ends_with(".xml") && p.contains("feed")),
        "a feed: {:?}",
        report.outputs
    );
    // Content URLs move with 301s; only the legacy runtime's paths answer 410 Gone.
    assert!(migration.converted.redirects.iter().all(|r| {
        r.status == 301
            || (r.status == 410 && adk_site::migrate::convert::legacy_runtime_path(&r.from))
    }));
    assert!(migration.converted.redirects.iter().any(|r| r.from == "/wp-login.php"));

    // The charter: hosting and a small model allowance; no advertising or sales.
    let mut inputs = CharterInputs::new(
        site.site.id,
        "Wanjiru Writes",
        OWNER,
        "Essays on cities and the people who walk them.",
        vec!["en".parse().unwrap()],
        "USD",
    );
    inputs.caps = Some(BTreeMap::from([
        (awp_types::SpendCategory::Hosting, 500),
        (awp_types::SpendCategory::Models, 1_000),
    ]));
    let charter = site.template.draft_charter(&site.team, &inputs);
    charter.validate().unwrap();
    assert!(site.template.has(&KindRule::NoAdvertising) && site.template.has(&KindRule::NoSales));
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;

    // The team runs four agents, the back office led by the Steward.
    let root = tempfile::tempdir().unwrap();
    let public = {
        let (partitions, table, search) =
            (site.partitions.clone(), site.table.clone(), site.search.clone());
        let sink = site.partitions.signal_sink(site.signals.clone(), site.agenda.clone());
        move || adk_harness::website::PublicKit {
            partitions: partitions.clone(),
            table: table.clone(),
            search: search.clone(),
            signals: sink.clone(),
            reply: None,
            capabilities: Vec::new(),
        }
    };
    let kit = TeamKit {
        model: Arc::new(adk_model::ScriptedLlm::new(
            "scripted",
            Vec::<adk_model::ScriptedResponse>::new(),
        )),
        root: root.path().to_path_buf(),
        back_office: site.back_office_kit(&charter),
        public: Arc::new(public),
        standing_rules: site.rules.clone(),
        configure: Arc::new(|_, builder| {
            builder.project_instructions(no_user_file()).without_compaction()
        }),
        wallet: None,
    };
    let built = site.team.build(&kit).await.unwrap();
    assert_eq!(
        built.roles(),
        [AgentRole::Concierge, AgentRole::Steward, AgentRole::Growth, AgentRole::Guardian]
    );
    assert_eq!(built.back_office.coordinator, "steward");
    assert!(
        root.path().join("guardian/.skills/protect.md").exists(),
        "each agent carries its playbooks"
    );
    assert!(!root.path().join("concierge/.skills/protect.md").exists());

    // The running cost estimate fits the budget before deployment.
    let price = ModelPrice { input_per_mtok: 30, output_per_mtok: 250 };
    let estimated = estimate(&site.team, &CostInputs::small(400, 0, price));
    assert!(estimated.fits(&charter.budget), "{estimated:?}");

    // A week on duty: readers and subscribers measured, hosting and tokens paid.
    let week = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 21, 10, 0, 0).unwrap());
    let wallet = wallet(&clock, &site.site.name);
    let finance =
        WalletCaller::agent(AgentRole::Finance.principal(&site.site)).with_charter_version(1);
    wallet
        .pay(
            PaymentRequest::new(
                "hosting-oct",
                SpendCategory::Hosting,
                Payee::new("hostco"),
                400,
                "USD",
            ),
            &finance,
        )
        .await
        .unwrap();
    for n in 0..3 {
        let challenge = PaymentChallenge::new(MODELS_HOST, "acct-models", 100, "USD");
        wallet
            .pay_challenge(&format!("tokens-{n}"), SpendCategory::Models, &challenge, &finance)
            .await
            .unwrap();
    }
    let kpis = Arc::new(InMemoryKpiStore::new());
    for day in 0..14 {
        let at =
            Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0).unwrap() + chrono::Duration::days(day);
        let (readers, subscribers) = if day < 7 { (40.0, 10.0) } else { (55.0, 12.0) };
        kpis.record(KpiSample::new(site.site.id, "readers", at, readers, "beacon")).await.unwrap();
        kpis.record(KpiSample::new(site.site.id, "subscribers", at, subscribers, "email"))
            .await
            .unwrap();
    }
    let tracker = KpiTracker::new(site.site.id, site.charters.clone(), kpis.clone())
        .with_clock(Arc::new(clock.clone()));
    let composer =
        ReviewComposer::new(site.site.id, tracker, site.ladder.clone(), site.charters.clone())
            .with_wallet(Arc::new(WalletKpi::new(WalletKpiSource::new(wallet, site.site.id))));
    let mut inputs = ReviewInputs::default();
    inputs.work.insert(AgentRole::Growth, vec!["Fixed two broken links".into()]);
    let review = composer.compose(week, inputs).await.unwrap();
    let readers = review.kpis.iter().find(|k| k.review.kpi == "readers").unwrap();
    assert_eq!((readers.review.week, readers.review.previous_week), (Some(55.0), Some(40.0)));
    assert_eq!(readers.trend, Trend::Up);
    let subscribers = review.kpis.iter().find(|k| k.review.kpi == "subscribers").unwrap();
    assert_eq!(subscribers.review.week, Some(12.0));
    let money = review.money.as_ref().expect("the review reports cost");
    assert!((money.spend - 7.0).abs() < 1e-9, "{money:?}");
    assert!((money.monthly_budget - 15.0).abs() < 1e-9);
    assert!(money.within_budget, "the month's running cost stays inside the budget");
    let document = review.to_markdown();
    for heading in ["Readers", "Newsletter subscribers", "## Money", "Fixed two broken links"] {
        assert!(document.contains(heading), "{heading} in\n{document}");
    }
}
