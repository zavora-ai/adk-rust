//! S11 — an experiment that respects its guardrails (spec 3.5 §5 S11, §7.12, A-I15, §7.21).
//!
//! The bakery's Growth agent tests two headlines on the home page. The variant is its own
//! static file in a signed release behind the site's edge; the page's script reports
//! exposures and goals to the edge's beacon; the runner reads the beacon each day. Variant B
//! lifts newsletter sign-ups from 4% to 5% of exposed sessions but doubles unsubscribes.
//!
//! Pass condition: the analysis reports the lift with its interval; the unsubscribe guardrail
//! breach pauses the experiment and keeps A; the decision and evidence appear in the weekly
//! review.

use std::sync::Arc;
use std::time::Duration;

use adk_core::ManualClock;
use adk_core::audit_log::AuditTrail;
use adk_eval::experiments::{Decision, Evidence, StopReason};
use adk_kernel::charter::{InMemoryKpiStore, JobControl, KpiTracker};
use adk_server::awp::{BeaconStore, InMemoryBeaconStore};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::SiteHost;
use adk_website::experiments::{
    ExperimentGenerator, ExperimentPlan, ExperimentRunner, ExperimentStatus,
    InMemoryExperimentStore, RepublishOnStop,
};
use adk_website::measurement::BeaconKpi;
use adk_website::review::{ReviewComposer, ReviewInputs};
use adk_website::team::AgentRole;
use anyhow::{Result, anyhow};
use awp_types::{Cadence, Direction, Kpi, Measurement, Role, SiteKind, SourceRef};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Datelike, TimeZone, Utc};
use serde_json::json;
use tower::ServiceExt;

use crate::checks::Checks;
use crate::fixture::edge::{EdgeOptions, edge_with};
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "juniper.example";
const VISITS: u64 = 2_000;

async fn served(site: &Site, path: &str) -> Option<String> {
    site.host.fetch(path).await.ok().flatten().map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// The first `<h1>…</h1>` of a page.
fn headline(html: &str) -> String {
    let start = html.find("<h1").unwrap_or(0);
    let end = html[start..].find("</h1>").map_or(html.len(), |e| start + e + 5);
    html[start..end].to_string()
}

/// Runs S11.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site = Site::new(SiteKind::Business, "Juniper Bakery", DOMAIN, &["en"]).await?;

    // The owner's charter measures sign-ups (a 4% baseline) and the unsubscribe rate (1.5%),
    // and puts the experiments job under the template's unsubscribe guardrail.
    let mut charter =
        site.charter("Juniper Bakery", "Fresh bread for Nakuru every morning.", "KES");
    let baseline = |value: f64| Measurement {
        value,
        window: Duration::from_secs(14 * 86_400),
        measured_at: Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).single().expect("a date"),
    };
    for (id, name, metric, source, direction, value) in [
        (
            "signups",
            "Newsletter sign-ups",
            "newsletter.signups",
            SourceRef::Beacon,
            Direction::Up,
            0.04,
        ),
        (
            "unsubscribe_rate",
            "Unsubscribe rate",
            "email.unsubscribe_rate",
            SourceRef::Email,
            Direction::Down,
            0.015,
        ),
    ] {
        charter.kpis.push(Kpi {
            id: id.into(),
            name: name.into(),
            metric: metric.into(),
            source,
            direction,
            baseline: Some(baseline(value)),
            target: None,
            cadence: Cadence::Daily,
        });
    }
    if let Some(unsubscribes) = charter.guardrails.iter_mut().find(|g| g.id == "unsubscribes") {
        unsubscribes.jobs.push("experiments".into());
    }
    site.commit(&charter, &[(Role::Owner, OWNER)]).await?;

    // The site's generator carries its experiments; the edge's beacon counts them.
    let experiments = Arc::new(InMemoryExperimentStore::new());
    site.generator = Arc::new(ExperimentGenerator::new(
        Arc::new(BuiltinGenerator),
        experiments.clone(),
        site.site.id,
        format!("{}/awp/v2/beacon", site.site.origin),
    ));
    site.publish_signed(&charter, "Juniper Bakery", Vec::new()).await?;
    let beacon_store = Arc::new(InMemoryBeaconStore::new());
    let edge = edge_with(
        &site,
        DOMAIN,
        EdgeOptions {
            beacon: Some(beacon_store.clone() as Arc<dyn BeaconStore>),
            ..EdgeOptions::default()
        },
    )
    .await?;
    let control_html =
        served(&site, "index.html").await.ok_or_else(|| anyhow!("release 1 serves a home page"))?;
    let control_headline = headline(&control_html);

    // The runner reads the edge's beacon each day; a stop republishes the control.
    let growth = AgentRole::Growth.principal(&site.site);
    let beacon = Arc::new(BeaconKpi::new(site.site.id, edge.edge.edge.beacon()));
    let start =
        Utc.with_ymd_and_hms(2026, 9, 7, 9, 0, 0).single().ok_or_else(|| anyhow!("a date"))?;
    let clock = ManualClock::new(start);
    let kit = site.back_office_kit(&charter);
    let runner = Arc::new(
        ExperimentRunner::new(site.site.id, site.charters.clone(), experiments.clone())
            .with_source(beacon.clone())
            .with_control(kit.pauses.clone())
            .with_ladder(site.ladder.clone())
            .with_audit(AuditTrail::new(Arc::new(site.audit.clone())))
            .with_stop_hook(Arc::new(RepublishOnStop::new(site.runtime.clone(), growth.clone())))
            .with_clock(Arc::new(clock.clone())),
    );
    let variant_headline = "<h1>Bread out of the oven at 5am</h1>";
    let plan = ExperimentPlan::new("headline", "/", "signups")
        .hypothesis("Saying when the bread is baked lifts sign-ups")
        .replace(control_headline.clone(), variant_headline)
        .item("pages/home/en")
        .planned_units_per_arm(20_000);
    let record = runner.start(plan).await?;
    checks.check(
        "the experiment's guardrail comes from the charter",
        record.guardrails.iter().any(|g| g.metric == "email.unsubscribe_rate"),
        format!(
            "primary {}, guardrails {:?}",
            record.config.primary.id,
            record.guardrails.iter().map(|g| &g.metric).collect::<Vec<_>>()
        ),
    );

    // Release 2 carries the variant as its own static file behind the edge.
    let mut home = adk_site::content::ContentStore::get(
        site.store.as_ref(),
        &awp_types::ContentId("pages/home/en".into()),
    )
    .await?
    .ok_or_else(|| anyhow!("the home page"))?;
    home.fields.insert("summary".into(), json!("Rye and sourdough, baked every morning."));
    let mut change = site.steward_change(vec![home], "experiments");
    change.entry = adk_site::pipeline::EntryPoint::SiteAgent { agent: growth.clone() };
    change.proposer = growth.clone();
    site.runtime.pipeline().run(change).await?;
    let (variant_status, _, variant) = edge.send(edge.get("/_exp/headline/")).await;
    let (_, _, control) = edge.send(edge.get("/")).await;
    let (control, variant) = (
        control.as_str().unwrap_or_default().to_string(),
        variant.as_str().unwrap_or_default().to_string(),
    );
    checks.check(
        "both arms are static files of a signed release behind the edge",
        variant_status == StatusCode::OK
            && variant.contains(variant_headline)
            && control.contains(&control_headline)
            && control.contains(r#"data-awp-experiment="headline" data-arm="a""#)
            && variant.contains(r#"<meta name="robots" content="noindex">"#),
        "control at /, variant at /_exp/headline/ (noindex, canonical to /)",
    );
    verify_served(&site.host, DOMAIN, &site.release_key).await?;

    // The page's events reach the edge's beacon through the site's router.
    for name in
        ["exp.headline.a.exposure", "exp.headline.b.exposure", "exp.headline.b.newsletter.signups"]
    {
        let request = Request::post(format!("{}/awp/v2/beacon", site.site.origin))
            .header("content-type", "text/plain;charset=UTF-8")
            .body(Body::from(json!({ "kind": "event", "name": name }).to_string()))?;
        let response = edge.router.clone().oneshot(request).await?;
        anyhow::ensure!(response.status() == StatusCode::NO_CONTENT, "the beacon took {name}");
    }

    // Day by day: 2,000 exposed sessions per arm; sign-ups 4% against 5%, unsubscribes 0.6%
    // against 1.2%.
    let mut significant_but_held = None;
    let mut outcome = None;
    for day in 0..30_i64 {
        let date = (start + chrono::Duration::days(day)).date_naive();
        for (event, count) in [
            ("exp.headline.a.exposure", VISITS),
            ("exp.headline.b.exposure", VISITS),
            ("exp.headline.a.newsletter.signups", 80),
            ("exp.headline.b.newsletter.signups", 100),
            ("exp.headline.a.email.unsubscribe_rate", 12),
            ("exp.headline.b.email.unsubscribe_rate", 24),
        ] {
            beacon_store.add(date, &format!("events:{event}"), count).await?;
        }
        clock.set(
            Utc.from_utc_datetime(
                &(date + chrono::Duration::days(1))
                    .and_hms_opt(3, 30, 0)
                    .ok_or_else(|| anyhow!("a time"))?,
            ),
        );
        let Some(look) = runner.look("headline").await? else { continue };
        let report = look.record.latest.clone().ok_or_else(|| anyhow!("a report"))?;
        if report.decision == Decision::Continue
            && let Evidence::Sequential { p_value, alpha, .. } = report.primary.evidence
            && p_value <= alpha
        {
            significant_but_held = significant_but_held.or(Some(report.look));
        }
        if look.stopped {
            outcome = Some(look);
            break;
        }
    }
    let outcome = outcome.ok_or_else(|| anyhow!("the experiment stops within 30 days"))?;
    let report = outcome.record.latest.clone().ok_or_else(|| anyhow!("a report"))?;
    println!("{report}");
    let effect = &report.primary.effect;
    let interval = effect.difference_interval;
    checks.check(
        "the analysis reports the lift with its interval",
        effect.relative_lift.is_some_and(|l| (l - 0.25).abs() < 1e-9)
            && interval.is_some_and(|i| i.lower > 0.0 && i.contains(0.01))
            && significant_but_held.is_some_and(|look| look < report.look),
        format!(
            "relative lift {:?}, difference interval {interval:?}; significant from look {significant_but_held:?}, held back until the breach at look {}",
            effect.relative_lift, report.look
        ),
    );
    let paused = kit.pauses.paused(site.site.id, "experiments").await?;
    let kept = served(&site, "index.html").await.unwrap_or_default();
    checks.check(
        "the unsubscribe guardrail breach pauses the experiment and keeps A",
        report.decision
            == Decision::KeepControl {
                reason: StopReason::GuardrailBreach { guardrails: vec!["email.unsubscribe_rate".into()] },
            }
            && matches!(&outcome.record.status, ExperimentStatus::Stopped { paused_job: Some(job), .. } if job == "experiments")
            && paused.as_deref().is_some_and(|r| r.contains("unsubscribes"))
            && !kept.contains("data-awp-experiment")
            && served(&site, "_exp/headline/index.html").await.is_none(),
        format!("{:?}; the job is paused: {paused:?}; the control is served alone", report.decision),
    );
    verify_served(&site.host, DOMAIN, &site.release_key).await?;

    // The weekly review carries the decision and its evidence.
    let stopped_on = outcome.record.stopped_at().ok_or_else(|| anyhow!("a stop"))?.date_naive();
    let week =
        stopped_on - chrono::Duration::days(i64::from(stopped_on.weekday().num_days_from_monday()));
    let composer = ReviewComposer::new(
        site.site.id,
        KpiTracker::new(site.site.id, site.charters.clone(), Arc::new(InMemoryKpiStore::new())),
        site.ladder.clone(),
        site.charters.clone(),
    )
    .with_experiments(runner.clone());
    let review = composer.compose(week, ReviewInputs::default()).await?;
    let document = review.to_markdown();
    checks.check(
        "the decision and evidence appear in the weekly review",
        document.contains("## Experiments")
            && document.contains("keep the control — guardrail breached: email.unsubscribe_rate")
            && document.contains("relative lift +25.0%"),
        format!("{} experiment entries in the review", review.experiments.len()),
    );
    Ok(checks)
}
