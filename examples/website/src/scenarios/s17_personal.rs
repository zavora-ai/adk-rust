//! S17 — a personal site (spec 3.5 §5 S17, §7.16.1 Personal, §7.18, §7.19, §7.21, §7.22).
//!
//! A writer's WordPress blog is discovered from its export, proposed as a personal site,
//! migrated and published; the budget is hosting plus a small model allowance.
//!
//! Pass condition: posts, archive and feeds keep their URLs; a newsletter sign-up with consent
//! replaces the plugin; the team runs Concierge, Steward, Growth and Guardian only; the weekly
//! review reports readers, subscribers and cost; the monthly running cost stays inside the
//! budget.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{InMemorySpendLedger, ManualClock, SpendLedger, SubjectRef};
use adk_kernel::charter::{InMemoryKpiStore, KpiSample, KpiStore, KpiTracker};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_payments::wallet::{
    InMemoryBillingAccount, InMemoryUseBasedScheme, InMemoryWalletProvider, InMemoryWalletStore,
    Mandate, Payee, PayeeRule, PaymentChallenge, PaymentRequest, ProviderBillingRail, RailRef,
    SpendCategory, UseBasedRail, Wallet, WalletCaller, WalletKpiSource, WalletPolicy,
};
use adk_site::host::SiteHost;
use adk_site::migrate::sources::Wxr;
use adk_website::kinds::{CharterInputs, KindRule};
use adk_website::onboarding::StartingPoint;
use adk_website::onboarding::cost::{CostInputs, ModelPrice, estimate};
use adk_website::onboarding::discovery::{Discovery, OwnerCard};
use adk_website::review::{ReviewComposer, ReviewInputs, Trend};
use adk_website::team::{AgentRole, TeamKit};
use adk_website::wallet::WalletKpi;
use anyhow::{Result, anyhow};
use awp_types::{ConsentScope, Role, SiteKind};
use axum::http::StatusCode;
use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::json;

use crate::checks::Checks;
use crate::fixture::edge::edge;
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "wanjiru.blog";
const MODELS_HOST: &str = "https://api.models.example/v1/generate";

fn wallet(clock: &ManualClock, site: &str) -> Result<Wallet> {
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
    let ledger = InMemorySpendLedger::new(policy.spend_limits(site)?).with_clock(shared.clone());
    Ok(Wallet::builder(
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
    .build()?)
}

/// Runs S17.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site = Site::new(SiteKind::Personal, "Wanjiru Writes", DOMAIN, &["en"]).await?;

    // Discover from the export: a personal site on a legacy runtime.
    let export = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../adk-website/tests/fixtures/blog.xml"),
    )?;
    let discovered = Discovery::new(
        StartingPoint::ExistingSite { url: format!("https://{DOMAIN}/") },
        "en".parse()?,
    )?
    .with_source(Arc::new(Wxr::from_bytes(export)))
    .run(&source_context(DOMAIN)?)
    .await?;
    let cards = discovered.cards();
    checks.check(
        "the blog is proposed as a personal site, with the freeze and the design choice offered",
        discovered.kind.as_ref().is_some_and(|k| k.kind == SiteKind::Personal)
            && discovered.legacy_runtime
            && cards.iter().any(|c| matches!(c, OwnerCard::Freeze { .. }))
            && cards.iter().any(|c| matches!(c, OwnerCard::Design { .. })),
        format!("{:?}", discovered.kind.as_ref().map(|k| (&k.kind, &k.reasons))),
    );
    let migration = discovered.migration.clone().ok_or_else(|| anyhow!("a migration"))?;
    let newsletter = migration.features.capability("subscribe_newsletter");
    checks.check(
        "the newsletter plugin becomes a consented capability",
        newsletter.is_some_and(|c| {
            c.consent.as_ref().is_some_and(|r| r.scope == ConsentScope::Newsletter)
        }) && site.template.capabilities.contains(&"subscribe_newsletter"),
        "subscribe_newsletter with newsletter consent",
    );

    // The charter: hosting and a small model allowance; no advertising or sales.
    let mut inputs = CharterInputs::new(
        site.site.id,
        "Wanjiru Writes",
        OWNER,
        "Essays on cities and the people who walk them.",
        vec!["en".parse()?],
        "USD",
    );
    inputs.caps = Some(BTreeMap::from([
        (awp_types::SpendCategory::Hosting, 500),
        (awp_types::SpendCategory::Models, 1_000),
    ]));
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await?;

    // Release 1: the migrated posts at their old URLs, the legacy redirects, the feeds.
    site.base.config.redirects.extend(migration.converted.redirects.clone());
    site.base.documents = migration.converted.documents.clone();
    site.runtime = site.assemble(None);
    let outcome = site
        .runtime
        .pipeline()
        .run(site.steward_change(migration.converted.items.clone(), "migration"))
        .await?;
    let seq = match outcome {
        adk_site::pipeline::PipelineOutcome::Published(p) => p.staged.manifest.seq,
        other => return Err(anyhow!("release 1 was not published: {other:?}")),
    };
    let mut kept = Vec::new();
    for path in [
        "/2026/09/12/city-walks/index.html",
        "/2026/10/03/markets-at-dawn/index.html",
        "/2026/10/17/the-last-matatu/index.html",
    ] {
        kept.push((path, site.host.fetch(path).await?.is_some()));
    }
    let manifest: awp_types::ReleaseManifest = serde_json::from_slice(
        &site
            .host
            .fetch(&adk_site::release::manifest_path(seq))
            .await?
            .ok_or_else(|| anyhow!("a manifest"))?,
    )?;
    let feeds: Vec<&String> =
        manifest.files.keys().filter(|p| p.ends_with(".xml") && p.contains("feed")).collect();
    let archives = migration.converted.redirects.iter().filter(|r| r.status == 301).count();
    checks.check(
        "posts, archive and feeds keep their URLs",
        kept.iter().all(|(_, served)| *served)
            && !feeds.is_empty()
            && migration.converted.redirects.iter().all(|r| r.status == 301),
        format!("{kept:?}; feeds {feeds:?}; {archives} archive and legacy URLs redirected 301"),
    );
    verify_served(&site.host, DOMAIN, &site.release_key).await?;

    // The newsletter sign-up on the edge: consent first, then one subscriber.
    let blog = edge(&site, DOMAIN).await?;
    let sign_up = json!({ "email": "reader@mail.example" });
    let (refused, _, body) =
        blog.send(blog.call("subscribe_newsletter", "n-1", None, &sign_up)).await;
    let receipt = blog.consent("newsletter").await?;
    let (status, _, _) =
        blog.send(blog.call("subscribe_newsletter", "n-1", Some(&receipt), &sign_up)).await;
    checks.check(
        "a newsletter sign-up with consent replaces the plugin",
        refused.as_u16() == 451
            && body["error"]["code"] == "consent_required"
            && status == StatusCode::OK,
        format!("without consent {refused}; with the receipt {status}"),
    );

    // The team: Concierge, Steward, Growth and Guardian only.
    let root = tempfile::tempdir()?;
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
        model: Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())),
        root: root.path().to_path_buf(),
        back_office: site.back_office_kit(&charter),
        public: Arc::new(public),
        standing_rules: site.rules.clone(),
        configure: Arc::new(|_, builder| {
            builder.project_instructions(no_user_file()).without_compaction()
        }),
    };
    let built = site.team.build(&kit).await?;
    checks.check(
        "the team runs Concierge, Steward, Growth and Guardian only",
        built.roles()
            == [AgentRole::Concierge, AgentRole::Steward, AgentRole::Growth, AgentRole::Guardian]
            && site.template.has(&KindRule::NoAdvertising)
            && site.template.has(&KindRule::NoSales),
        format!("{:?}, led by {}", built.roles(), built.back_office.coordinator),
    );

    // A week on duty: readers and subscribers measured, hosting and tokens paid.
    let price = ModelPrice { input_per_mtok: 30, output_per_mtok: 250 };
    let estimated = estimate(&site.team, &CostInputs::small(400, 0, price));
    let week = NaiveDate::from_ymd_opt(2026, 10, 19).ok_or_else(|| anyhow!("a date"))?;
    let clock = ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 21, 10, 0, 0).single().ok_or_else(|| anyhow!("a date"))?,
    );
    let wallet = wallet(&clock, &site.site.name)?;
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
        .await?;
    for n in 0..3 {
        let challenge = PaymentChallenge::new(MODELS_HOST, "acct-models", 100, "USD");
        wallet
            .pay_challenge(&format!("tokens-{n}"), SpendCategory::Models, &challenge, &finance)
            .await?;
    }
    let kpis = Arc::new(InMemoryKpiStore::new());
    for day in 0..14 {
        let at = Utc
            .with_ymd_and_hms(2026, 10, 12, 12, 0, 0)
            .single()
            .ok_or_else(|| anyhow!("a date"))?
            + chrono::Duration::days(day);
        let (readers, subscribers) = if day < 7 { (40.0, 10.0) } else { (55.0, 12.0) };
        kpis.record(KpiSample::new(site.site.id, "readers", at, readers, "beacon")).await?;
        kpis.record(KpiSample::new(site.site.id, "subscribers", at, subscribers, "email")).await?;
    }
    let tracker = KpiTracker::new(site.site.id, site.charters.clone(), kpis.clone())
        .with_clock(Arc::new(clock.clone()));
    let composer =
        ReviewComposer::new(site.site.id, tracker, site.ladder.clone(), site.charters.clone())
            .with_wallet(Arc::new(WalletKpi::new(WalletKpiSource::new(wallet, site.site.id))));
    let review = composer.compose(week, ReviewInputs::default()).await?;
    let readers = review.kpis.iter().find(|k| k.review.kpi == "readers");
    let subscribers = review.kpis.iter().find(|k| k.review.kpi == "subscribers");
    let document = review.to_markdown();
    checks.check(
        "the weekly review reports readers, subscribers and cost",
        readers.is_some_and(|r| r.review.week == Some(55.0) && r.trend == Trend::Up)
            && subscribers.is_some_and(|s| s.review.week == Some(12.0))
            && review.money.is_some()
            && ["Readers", "Newsletter subscribers", "## Money"]
                .iter()
                .all(|h| document.contains(h)),
        format!(
            "readers 55 (up from 40), subscribers 12, spend {:?}",
            review.money.as_ref().map(|m| m.spend)
        ),
    );
    checks.check(
        "the monthly running cost stays inside the budget",
        estimated.fits(&charter.budget) && review.money.as_ref().is_some_and(|m| m.within_budget),
        format!(
            "estimated before deployment {estimated:?}; spent {:?} of {:?} a month",
            review.money.as_ref().map(|m| m.spend),
            review.money.as_ref().map(|m| m.monthly_budget)
        ),
    );
    Ok(checks)
}
