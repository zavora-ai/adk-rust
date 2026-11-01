//! S11 — an experiment that respects its guardrails (spec 3.5 §5 S11, §7.12, A-I15, §7.21).
//!
//! Two headline variants on a bakery's home page; variant B lifts newsletter sign-ups from 4%
//! to 5% of exposed sessions but doubles unsubscribes. The variant is its own static file in
//! a signed release; exposures and goals reach the edge's beacon under the event names the
//! client script sends; each day the runner feeds the cumulative counts to the analyzer. The
//! sign-up lift becomes significant first, yet B is never adopted: the unsubscribe estimate
//! holds it back until the breach is proven, which pauses the experiments job before its next
//! action, lowers it one level, keeps A, republishes the home page without the variant, and
//! puts the decision and its evidence in the weekly review.

mod common;
mod experiment_fixture;

use std::sync::Arc;
use std::time::Duration;

use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
use adk_awp::v2::capability::CapabilityRegistry;
use adk_core::audit_log::AuditTrail;
use adk_core::principal::SubjectRef;
use adk_core::{ManualClock, Url};
use adk_eval::experiments::{Decision, Evidence, StopReason, Tolerance};
use adk_harness::website::SiteRuntime;
use adk_kernel::charter::{
    AutonomyReason, AutonomyStore, CharterEvent, InMemoryKpiStore, JobControl, KpiSource,
    KpiTracker, OwnerDecision, ladder_rank,
};
use adk_server::awp::{AwpEdge, BeaconStore, EdgeKeys, InMemoryBeaconStore};
use adk_site::content::ContentStore;
use adk_site::generator::BuiltinGenerator;
use adk_site::host::SiteHost;
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use adk_website::experiments::{
    ExperimentGenerator, ExperimentPlan, ExperimentRunner, ExperimentStatus,
    InMemoryExperimentStore, RepublishOnStop,
};
use adk_website::measurement::BeaconKpi;
use adk_website::onboarding::build::SitePlan;
use adk_website::review::{ReviewComposer, ReviewInputs};
use adk_website::team::AgentRole;
use awp_types::{Role, SiteKind};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use common::*;
use experiment_fixture::{Events, charter, headline};
use serde_json::json;
use tower::ServiceExt;

const EDGE: &str = "https://edge.juniper.example";
const VISITS: u64 = 2_000;

async fn served(site: &Site, path: &str) -> Option<String> {
    site.host.fetch(path).await.unwrap().map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn s11_variant_b_lifts_signups_but_doubles_unsubscribes() {
    let mut site =
        Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let charter = charter(&site);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;

    // The site's generator carries its experiments; the edge's beacon counts them.
    let experiments = Arc::new(InMemoryExperimentStore::new());
    let generator = Arc::new(ExperimentGenerator::new(
        Arc::new(BuiltinGenerator),
        experiments.clone(),
        site.site.id,
        format!("{EDGE}/awp/v2/beacon"),
    ));
    let work = tempfile::tempdir().unwrap();
    site.runtime = SiteRuntime::builder(
        site.site.clone(),
        site.base.clone(),
        site.store.clone(),
        generator,
        site.host.clone(),
        site.log.clone(),
        work.path(),
    )
    .history(site.history.clone())
    .charters(site.charters.clone())
    .facts(site.facts.clone(), site.partitions.fact_scope(OWNER))
    .gate(site.template.gate(&site.site.origin))
    .audit(AuditTrail::new(Arc::new(site.audit.clone())))
    .announcer(site.announcer.clone())
    .build()
    .unwrap();
    let growth = AgentRole::Growth.principal(&site.site);

    // Release 1: the site as onboarding builds it.
    let items = SitePlan::new(&charter, "Juniper Bakery").items(&charter, &[], &["KE".into()]);
    let release = site
        .runtime
        .pipeline()
        .run(Change {
            items,
            media: Vec::new(),
            entry: EntryPoint::SiteAgent { agent: AgentRole::Steward.principal(&site.site) },
            proposer: AgentRole::Steward.principal(&site.site),
            job: "onboarding".into(),
            action_class: "publish".into(),
        })
        .await
        .unwrap();
    assert!(matches!(release, PipelineOutcome::Published(_)), "release 1");
    let control_html = served(&site, "index.html").await.unwrap();
    let control_headline = headline(&control_html);

    // The edge with its beacon, and the runner reading it.
    let beacon_store = Arc::new(InMemoryBeaconStore::new());
    let keys = EdgeKeys {
        card: Arc::new(Ed25519DocumentSigner::from_seed("edge-card", [7; 32]).unwrap()),
        receipt: Arc::new(Ed25519DocumentSigner::from_seed("edge-receipt", [8; 32]).unwrap())
            as Arc<dyn DocumentSigner>,
    };
    let edge = AwpEdge::builder(
        CapabilityRegistry::new("juniper.example"),
        keys,
        Url::parse(EDGE).unwrap(),
    )
    .beacon_store(beacon_store.clone())
    .build()
    .unwrap();
    let beacon = Arc::new(BeaconKpi::new(site.site.id, edge.beacon()));
    let start = Utc.with_ymd_and_hms(2026, 9, 7, 9, 0, 0).unwrap();
    let clock = ManualClock::new(start);
    let kit = site.back_office_kit(&charter);
    let events = Arc::new(Events::default());
    let runner = Arc::new(
        ExperimentRunner::new(site.site.id, site.charters.clone(), experiments.clone())
            .with_source(beacon.clone())
            .with_control(kit.pauses.clone())
            .with_ladder(site.ladder.clone())
            .with_events(events.clone())
            .with_audit(AuditTrail::new(Arc::new(site.audit.clone())))
            .with_stop_hook(Arc::new(RepublishOnStop::new(site.runtime.clone(), growth.clone())))
            .with_clock(Arc::new(clock.clone())),
    );

    // The Growth agent's plan: a headline that says when the bread is baked.
    let variant_headline = "<h1>Bread out of the oven at 5am</h1>";
    let plan = ExperimentPlan::new("headline", "/", "signups")
        .hypothesis("Saying when the bread is baked lifts sign-ups")
        .replace(control_headline.clone(), variant_headline)
        .item("pages/home/en")
        .planned_units_per_arm(20_000);
    let record = runner.start(plan).await.unwrap();
    assert_eq!(record.config.primary.id, "newsletter.signups");
    assert_eq!(record.guardrails.len(), 1, "{:?}", record.guardrails);
    let guarded = &record.guardrails[0];
    assert_eq!(
        (guarded.guardrail.as_str(), guarded.metric.as_str()),
        ("unsubscribes", "email.unsubscribe_rate")
    );
    // From a 1.5% baseline to the charter's 2% threshold is a third.
    let Tolerance::Relative(share) = guarded.tolerance else { panic!("{:?}", guarded.tolerance) };
    assert!((share - 1.0 / 3.0).abs() < 1e-9, "{share}");

    // Release 2 carries the variant as its own static file; both pages load the script.
    let mut home =
        site.store.get(&awp_types::ContentId("pages/home/en".into())).await.unwrap().unwrap();
    home.fields.insert("summary".into(), json!("Rye and sourdough, baked every morning."));
    let release = site
        .runtime
        .pipeline()
        .run(Change {
            items: vec![home],
            media: Vec::new(),
            entry: EntryPoint::SiteAgent { agent: growth.clone() },
            proposer: growth.clone(),
            job: "experiments".into(),
            action_class: "publish".into(),
        })
        .await
        .unwrap();
    let PipelineOutcome::Published(published) = release else { panic!("release 2: {release:?}") };
    assert!(published.staged.files.contains_key("/_exp/headline/index.html"));
    assert!(published.staged.files.contains_key("/_awp/experiments.js"));
    verify_served(&site.host, "juniper.example", &site.release_key).await;
    let control = served(&site, "index.html").await.unwrap();
    let variant = served(&site, "_exp/headline/index.html").await.unwrap();
    assert!(control.contains(&control_headline) && !control.contains(variant_headline));
    assert!(variant.contains(variant_headline) && !variant.contains(&control_headline));
    assert!(control.contains(r#"data-awp-experiment="headline" data-arm="a""#));
    assert!(control.contains(r#"data-variant="/_exp/headline/""#));
    assert!(variant.contains(r#"data-awp-experiment="headline" data-arm="b""#));
    assert!(variant.contains(r#"<meta name="robots" content="noindex">"#));
    assert!(
        variant.contains(r#"<link rel="canonical" href="https://juniper.example/""#),
        "the variant's canonical page is the control"
    );
    assert!(control.contains(&format!(r#"data-beacon="{EDGE}/awp/v2/beacon""#)));

    // The client's events reach the edge's beacon under the names the runner reads.
    for name in [
        "exp.headline.a.exposure",
        "exp.headline.b.exposure",
        "exp.headline.b.newsletter.signups",
        "exp.headline.b.email.unsubscribe_rate",
    ] {
        let request = Request::post(format!("{EDGE}/awp/v2/beacon"))
            .header("content-type", "text/plain;charset=UTF-8")
            .body(Body::from(json!({ "kind": "event", "name": name }).to_string()))
            .unwrap();
        let response = edge.router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{name}");
    }
    let today = Utc::now();
    let tomorrow = today + chrono::Duration::days(1);
    for name in ["exp.headline.b.exposure", "exp.headline.b.email.unsubscribe_rate"] {
        let count =
            beacon.read(site.site.id, &format!("events:{name}"), today, tomorrow).await.unwrap();
        assert_eq!(count, Some(1.0), "{name}");
    }

    // Day by day: 2,000 exposed sessions per arm; sign-ups 4% against 5%, unsubscribes 0.6%
    // against 1.2%. Each look reads the whole days since the last one.
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
            beacon_store.add(date, &format!("events:{event}"), count).await.unwrap();
        }
        clock.set(
            Utc.from_utc_datetime(
                &(date + chrono::Duration::days(1)).and_hms_opt(3, 30, 0).unwrap(),
            ),
        );
        let look = runner.look("headline").await.unwrap().expect("a whole day passed");
        assert_eq!(look.days, 1);
        let report = look.record.latest.clone().unwrap();
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
    let outcome = outcome.expect("the experiment stops within 30 days");
    let report = outcome.record.latest.clone().unwrap();
    // The evidence block the weekly review shows, in the test log.
    println!("{report}");
    let held_from =
        significant_but_held.expect("the sign-up lift was significant before the breach");
    assert!(
        held_from < report.look,
        "B was held back from look {held_from} to the breach at look {}",
        report.look
    );
    assert_eq!(
        report.decision,
        Decision::KeepControl {
            reason: StopReason::GuardrailBreach {
                guardrails: vec!["email.unsubscribe_rate".into()]
            }
        }
    );
    // The lift with its interval.
    let effect = &report.primary.effect;
    let interval = effect.difference_interval.unwrap();
    assert!(interval.lower > 0.0 && interval.contains(0.01), "{interval:?}");
    let lift = effect.relative_interval.unwrap();
    assert!((effect.relative_lift.unwrap() - 0.25).abs() < 1e-9 && lift.contains(0.25), "{lift:?}");
    assert!(report.guardrails[0].breached && report.guardrails[0].harm_lower_bound.unwrap() > 0.0);

    // A-I15: the job is paused before its next action, lowered one level, and the owner hears
    // of it as a P2.
    let paused = kit.pauses.paused(site.site.id, "experiments").await.unwrap();
    assert!(paused.as_deref().is_some_and(|reason| reason.contains("unsubscribes")), "{paused:?}");
    let ExperimentStatus::Stopped { paused_job, .. } = &outcome.record.status else {
        panic!("stopped: {:?}", outcome.record.status)
    };
    assert_eq!(paused_job.as_deref(), Some("experiments"));
    let history = site.autonomy.history(site.site.id, "experiments").await.unwrap();
    let lowered = history.last().unwrap();
    assert!(
        matches!(&lowered.reason, AutonomyReason::GuardrailBreach { guardrail } if guardrail == "unsubscribes"),
        "{history:?}"
    );
    assert!(ladder_rank(lowered.to) < ladder_rank(lowered.from.unwrap()), "lowered one level");
    assert!(matches!(
        events.0.lock().unwrap().as_slice(),
        [CharterEvent::JobPaused { job, .. }] if job == "experiments"
    ));
    assert_eq!(events.0.lock().unwrap()[0].urgency(), awp_types::Urgency::P2);
    assert!(
        site.actions().iter().any(|action| action == "website.job.paused"),
        "{:?}",
        site.actions()
    );
    let paused_record = site
        .audit
        .records()
        .into_iter()
        .find(|record| record.action == "website.job.paused")
        .unwrap();
    assert_eq!(paused_record.ext["guardrail"], "unsubscribes");
    assert_eq!(paused_record.ext["experiment"], "headline");

    // A is kept: the stop republished the home page without the variant, in a signed release.
    let control = served(&site, "index.html").await.unwrap();
    assert!(!control.contains("data-awp-experiment"), "the control is served alone");
    assert!(
        served(&site, "_exp/headline/index.html").await.is_none(),
        "the variant's file is gone"
    );
    assert!(served(&site, "_awp/experiments.js").await.is_none());
    verify_served(&site.host, "juniper.example", &site.release_key).await;
    // Later looks change nothing.
    clock.advance(Duration::from_secs(2 * 86_400));
    let again = runner.look("headline").await.unwrap().unwrap();
    assert!(!again.stopped && again.record.latest == outcome.record.latest);

    // The Growth agent's next action is refused while the job is paused, even a read the
    // ladder allows at every level; the job's next experiment is refused too.
    let rules = site.rules.clone();
    let script = vec![
        call("launch_gate_run", json!({})),
        adk_model::ScriptedResponse::text("The experiments job is paused."),
    ];
    let (agent, model) =
        site.harness("growth", script, |b| b.website_back_office(&kit).standing_rules(rules));
    let owner = user(OWNER);
    let session = agent.open_session(&owner, None).await.unwrap();
    let turn = "Check the site, then publish the 5am headline for everyone.";
    let paused_run = collect(agent.send(&owner, &session, turn).await.unwrap()).await;
    let refused = response(&paused_run, "launch_gate_run", 0).to_string();
    assert!(refused.contains("paused by a guardrail breach"), "{refused}");
    let next = ExperimentPlan::new("headline-2", "/", "signups")
        .replace(control_headline, "<h1>Hot rye at 5am</h1>");
    let error = runner.start(next).await.unwrap_err();
    assert_eq!(error.code, "website.experiment.paused");

    // Only a person resumes the job; then its agent acts again.
    let service = OwnerDecision::new(
        "dec-resume",
        SubjectRef::service("urn:adk:agent:juniper/growth"),
        Utc::now(),
    );
    assert!(kit.pauses.resume(site.site.id, "experiments", &service).await.is_err());
    let owner_decision = OwnerDecision::new("dec-resume", SubjectRef::human(OWNER), Utc::now());
    assert!(kit.pauses.resume(site.site.id, "experiments", &owner_decision).await.unwrap());
    model.push(call("launch_gate_run", json!({})));
    model.push(adk_model::ScriptedResponse::text("The gate ran."));
    let resumed_run = collect(agent.send(&owner, &session, turn).await.unwrap()).await;
    let answered = response(&resumed_run, "launch_gate_run", 0).to_string();
    assert!(!answered.contains("paused"), "{answered}");

    // The weekly review carries the decision and its evidence.
    let stopped_on: NaiveDate = outcome.record.stopped_at().unwrap().date_naive();
    let week_start =
        stopped_on - chrono::Duration::days(i64::from(stopped_on.weekday().num_days_from_monday()));
    let composer = ReviewComposer::new(
        site.site.id,
        KpiTracker::new(site.site.id, site.charters.clone(), Arc::new(InMemoryKpiStore::new())),
        site.ladder.clone(),
        site.charters.clone(),
    )
    .with_experiments(runner.clone());
    let review = composer.compose(week_start, ReviewInputs::default()).await.unwrap();
    assert_eq!(review.experiments.len(), 1, "{:?}", review.experiments);
    let entry = &review.experiments[0];
    for expected in [
        "Experiment `headline` on /",
        "keep the control — guardrail breached: email.unsubscribe_rate",
        "the experiments job is paused until you resume it",
        "Primary newsletter.signups (higher is better)",
        "relative lift +25.0%",
        "Guardrail email.unsubscribe_rate (lower is better, tolerance 33.3% relative)",
        "breached",
    ] {
        assert!(entry.contains(expected), "missing {expected:?} in\n{entry}");
    }
    let document = review.to_markdown();
    assert!(document.contains("## Experiments\n\n- Experiment `headline`"), "{document}");
    assert!(document.contains("  ```text\n  Experiment 'headline', look "), "{document}");
    let json = serde_json::to_value(&outcome.record).unwrap();
    assert_eq!(json["latest"]["decision"]["outcome"], "keepControl");
    assert_eq!(json["status"]["pausedJob"], "experiments");
}
