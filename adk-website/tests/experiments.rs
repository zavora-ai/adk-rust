//! Experiments on the site (spec 3.5 §7.12, A-I15): the charter mappings, the plan's
//! analysis, the record stores, variants as static files, and the runner's looks, stops and
//! review entries.

mod common;
mod experiment_fixture;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::ManualClock;
use adk_eval::experiments::{Decision, MetricDirection, StopReason, Tolerance};
use adk_kernel::charter::{InMemoryCharterStore, JobControl, JobPauses};
use adk_server::awp::{BeaconCounts, BeaconStore, InMemoryBeaconStore};
use adk_server::background::WorkflowExecutor;
use adk_site::content::SiteProject;
use adk_site::generator::{BuiltinGenerator, ChangeSet, SiteGenerator};
use adk_website::experiments::{
    Arm, EXPERIMENTS_WORKFLOW, ExperimentGenerator, ExperimentPlan, ExperimentRecord,
    ExperimentRunner, ExperimentStatus, ExperimentStore, FileExperimentStore,
    InMemoryExperimentStore, SCRIPT, assign, bucket_hash, guardrail_direction, guardrail_spec,
    guardrail_tolerance, metric_direction,
};
use adk_website::measurement::BeaconKpi;
use awp_types::{Breach, Charter, Direction, Guardrail, GuardrailAction, Role, SiteKind};
use chrono::{NaiveDate, TimeZone, Utc};
use common::*;
use experiment_fixture::{charter, headline};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn guardrail(threshold: f64, breach: Breach) -> Guardrail {
    Guardrail {
        id: "g".into(),
        metric: "m".into(),
        threshold,
        breach,
        window: Duration::from_secs(86_400),
        action: GuardrailAction::PauseJob,
        jobs: Vec::new(),
    }
}

fn relative(tolerance: Tolerance) -> f64 {
    match tolerance {
        Tolerance::Relative(share) => share,
        other => panic!("expected a relative tolerance, got {other:?}"),
    }
}

/// The charter's directions and limits become the analyzer's (spec 3.5 §7.6, §7.12).
#[test]
fn charter_kpis_and_guardrails_map_onto_the_analysis() {
    assert_eq!(metric_direction(Direction::Up), MetricDirection::HigherIsBetter);
    assert_eq!(metric_direction(Direction::Down), MetricDirection::LowerIsBetter);
    assert_eq!(guardrail_direction(Breach::Above), MetricDirection::LowerIsBetter);
    assert_eq!(guardrail_direction(Breach::Below), MetricDirection::HigherIsBetter);

    // A limit to stay under: the headroom from the baseline up to the threshold.
    let above = guardrail(0.02, Breach::Above);
    assert!((relative(guardrail_tolerance(&above, Some(0.015))) - 1.0 / 3.0).abs() < 1e-12);
    // A limit to stay over: the headroom down to it.
    let below = guardrail(0.03, Breach::Below);
    assert!((relative(guardrail_tolerance(&below, Some(0.04))) - 0.25).abs() < 1e-12);
    // No headroom left, or none known: any proven harm breaches.
    assert_eq!(relative(guardrail_tolerance(&above, Some(0.025))), 0.0);
    assert_eq!(relative(guardrail_tolerance(&below, Some(0.02))), 0.0);
    assert_eq!(guardrail_tolerance(&above, None), Tolerance::Absolute(0.0));
    assert_eq!(guardrail_tolerance(&above, Some(0.0)), Tolerance::Absolute(0.0));
    // A floor of zero can never be crossed; the drop stays below 100% so the analyzer accepts it.
    assert!(relative(guardrail_tolerance(&guardrail(0.0, Breach::Below), Some(0.04))) < 1.0);

    let spec = guardrail_spec(&above, Some(0.015));
    assert_eq!(spec.metric.id, "m");
    assert_eq!(spec.metric.direction, MetricDirection::LowerIsBetter);
}

/// The shipped script's hash and the edge's agree, past ASCII too: the vectors were computed
/// by running `experiments.js`'s own `hash` under Node.js.
#[test]
fn the_script_and_the_edge_hash_buckets_alike() {
    for (key, expected) in [
        ("", 0x811c_9dc5),
        ("a", 0xe40c_292c),
        ("foobar", 0xbf9c_f968),
        ("headline:9f2c41d0", 0x3368_99c6),
        ("héllo", 0xf1e5_b55f),
        ("exp:\u{1F600}", 0x79d1_f8e3),
        ("checkout-button:ffffffff00000000", 0x09a7_7d16),
    ] {
        assert_eq!(bucket_hash(key), expected, "{key}");
    }
    // `headline:9f2c41d0` sits at 0.2008 of the range.
    assert_eq!(assign("headline", "9f2c41d0", 0.21), Arm::Variant);
    assert_eq!(assign("headline", "9f2c41d0", 0.20), Arm::Control);
    for fragment in ["0x811c9dc5", "0x01000193", "charCodeAt", "Math.imul", "sessionStorage"] {
        assert!(SCRIPT.contains(fragment), "{fragment}");
    }
    assert!(!SCRIPT.contains("document.cookie"), "the script sets no cookie");
}

async fn committed() -> (Site, Charter) {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let charter = charter(&site);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;
    (site, charter)
}

fn plan(id: &str) -> ExperimentPlan {
    ExperimentPlan::new(id, "/", "signups")
        .replace("<h1>Juniper Bakery</h1>", "<h1>Bread out of the oven at 5am</h1>")
}

/// The plan fixes its analysis from the charter in force (spec 3.5 §7.12).
#[tokio::test]
async fn a_plan_fixes_its_analysis_from_the_charter() {
    let (_site, charter) = committed().await;
    let baselines = BTreeMap::from([("email.unsubscribe_rate".to_string(), 0.015)]);

    // By default every guardrail of the job; the template's guardrail names `experiments`.
    let (config, guarded) = plan("headline").config(&charter, &baselines).unwrap();
    assert_eq!(config.primary.id, "newsletter.signups");
    assert_eq!(config.primary.direction, MetricDirection::HigherIsBetter);
    assert_eq!(guarded.len(), 1);
    assert_eq!(guarded[0].guardrail, "unsubscribes");
    assert_eq!(guarded[0].threshold, 0.02);
    assert_eq!(config.guardrails[0].metric.direction, MetricDirection::LowerIsBetter);
    // Another job's experiment monitors no guardrail of this one.
    let (config, guarded) = plan("h2").job("seo").config(&charter, &baselines).unwrap();
    assert!(config.guardrails.is_empty() && guarded.is_empty());
    // Without a baseline any proven harm breaches.
    let (config, _) = plan("h3").config(&charter, &BTreeMap::new()).unwrap();
    assert_eq!(config.guardrails[0].tolerance, Tolerance::Absolute(0.0));

    let refused = |plan: ExperimentPlan| plan.config(&charter, &baselines).unwrap_err().message;
    assert!(
        refused(ExperimentPlan::new("h", "/", "nope").replace("a", "b")).contains("no KPI 'nope'")
    );
    assert!(refused(plan("h").guardrail("nope")).contains("no guardrail 'nope'"));
    let mut guarding_itself = charter.clone();
    guarding_itself.guardrails[0].metric = "newsletter.signups".into();
    assert!(
        plan("h")
            .config(&guarding_itself, &baselines)
            .unwrap_err()
            .message
            .contains("primary metric")
    );
    let mut unnamed = charter.clone();
    unnamed.kpis.iter_mut().find(|k| k.id == "signups").unwrap().metric =
        "Newsletter Signups".into();
    assert!(
        plan("h")
            .config(&unnamed, &baselines)
            .unwrap_err()
            .message
            .contains("cannot be counted per arm")
    );
    let long = "a".repeat(24);
    let mut long_metric = charter.clone();
    long_metric.kpis.iter_mut().find(|k| k.id == "signups").unwrap().metric =
        "newsletter.signups.from.the.home.page".into();
    assert!(plan(&long).config(&long_metric, &baselines).unwrap_err().message.contains("shorten"));

    for bad in [
        plan("Headline"),
        plan(&"a".repeat(25)),
        ExperimentPlan::new("h", "/", "signups"),
        plan("h").allocation(1.0),
        plan("h").replace("<p>", "<p onclick=\"steal()\">"),
        plan("h").replace("<p>", "<script src=//evil.example/x.js></script>"),
        plan("h").replace("<a href=\"/\">", "<a href=\"javascript:alert(1)\">"),
        plan("h").goal_page("/thanks"),
    ] {
        assert_eq!(bad.validate().unwrap_err().code, "website.experiment.invalid", "{bad:?}");
    }
    assert!(plan("h").replace("<p>Data:", "<p>Our data: fresh").validate().is_ok());
}

fn record(site: uuid::Uuid, id: &str, charter: &Charter) -> ExperimentRecord {
    let baselines = BTreeMap::new();
    let plan = plan(id);
    let (config, guardrails) = plan.config(charter, &baselines).unwrap();
    let analyzer = adk_eval::experiments::ExperimentAnalyzer::new(config.clone()).unwrap();
    ExperimentRecord {
        site,
        plan,
        config,
        guardrails,
        charter_version: charter.version,
        status: ExperimentStatus::Running,
        started_at: Utc.with_ymd_and_hms(2026, 9, 7, 9, 0, 0).unwrap(),
        counted_to: NaiveDate::from_ymd_opt(2026, 9, 7).unwrap(),
        counts: Default::default(),
        state: analyzer.state().clone(),
        latest: None,
    }
}

/// One contract for every record store: replace by id, list per site, round-trip whole.
async fn store_contract(store: &dyn ExperimentStore, charter: &Charter) {
    let (one, two) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    assert!(store.get(one, "headline").await.unwrap().is_none());
    assert!(store.list(one).await.unwrap().is_empty());
    let mut first = record(one, "headline", charter);
    store.put(&first).await.unwrap();
    store.put(&record(one, "about", charter)).await.unwrap();
    store.put(&record(two, "headline", charter)).await.unwrap();
    assert_eq!(store.get(one, "headline").await.unwrap(), Some(first.clone()));
    let ids: Vec<String> = store.list(one).await.unwrap().into_iter().map(|r| r.plan.id).collect();
    assert_eq!(ids, ["about", "headline"]);
    assert_eq!(store.list(two).await.unwrap().len(), 1, "sites stay apart");
    first.status = ExperimentStatus::Stopped {
        decision: Decision::KeepControl { reason: StopReason::VariantWorse },
        at: Utc::now(),
        paused_job: None,
    };
    first.counts.exposures.variant = 7;
    store.put(&first).await.unwrap();
    assert_eq!(store.get(one, "headline").await.unwrap(), Some(first));
    assert_eq!(store.list(one).await.unwrap().len(), 2, "a put replaces");
    assert!(store.get(one, "../etc/passwd").await.unwrap().is_none());
}

#[tokio::test]
async fn record_stores_keep_experiments_between_looks() {
    let (_site, charter) = committed().await;
    store_contract(&InMemoryExperimentStore::new(), &charter).await;
    let dir = tempfile::tempdir().unwrap();
    store_contract(&FileExperimentStore::new(dir.path()), &charter).await;
    // The files outlive the store that wrote them.
    let site = uuid::Uuid::new_v4();
    FileExperimentStore::new(dir.path()).put(&record(site, "headline", &charter)).await.unwrap();
    let reopened = FileExperimentStore::new(dir.path());
    assert_eq!(reopened.list(site).await.unwrap().len(), 1);
}

async fn built_home(out: &std::path::Path, project: &SiteProject) -> String {
    BuiltinGenerator.build(project, out).await.unwrap();
    std::fs::read_to_string(out.join("index.html")).unwrap()
}

/// A running experiment's variant is its own static file; a stopped one leaves the build
/// (spec 3.5 §7.12).
#[tokio::test]
async fn variants_are_static_files_while_their_experiment_runs() {
    let (site, charter) = committed().await;
    let scratch = tempfile::tempdir().unwrap();
    let control = headline(&built_home(scratch.path(), &site.base).await);
    let store = Arc::new(InMemoryExperimentStore::new());
    let mut running = record(site.site.id, "headline", &charter);
    running.plan = ExperimentPlan::new("headline", "/", "signups")
        .replace(control.clone(), "<h1>Bread out of the oven at 5am</h1>")
        .allocation(0.3);
    store.put(&running).await.unwrap();
    let generator = ExperimentGenerator::new(
        Arc::new(BuiltinGenerator),
        store.clone(),
        site.site.id,
        "https://edge.example/awp/v2/beacon?a=1&b=2",
    );
    let out = tempfile::tempdir().unwrap();
    let report = generator.build(&site.base, out.path()).await.unwrap();
    for file in ["index.html", "_exp/headline/index.html", "_awp/experiments.js"] {
        assert!(report.written.contains(file) && report.outputs.contains(file), "{file}");
    }
    assert!(!report.pages.iter().any(|page| page.html.starts_with("_exp")), "no page record");
    let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    let variant = std::fs::read_to_string(out.path().join("_exp/headline/index.html")).unwrap();
    assert_eq!(home.matches("data-awp-experiment=\"headline\"").count(), 1);
    assert!(home.contains("data-allocation=\"0.3\" data-variant=\"/_exp/headline/\""));
    assert!(home.contains("data-beacon=\"https://edge.example/awp/v2/beacon?a=1&amp;b=2\""));
    assert!(
        variant.contains("<h1>Bread out of the oven at 5am</h1>") && !variant.contains(&control)
    );
    assert!(variant.contains("data-arm=\"b\"") && variant.contains("content=\"noindex\""));
    assert_eq!(std::fs::read_to_string(out.path().join("_awp/experiments.js")).unwrap(), SCRIPT);

    // An incremental build changes nothing it does not need to, and never adds a second tag.
    let changes = ChangeSet { items: Default::default(), previous: report.graph.clone() };
    let again = generator.build_incremental(&site.base, out.path(), &changes).await.unwrap();
    assert!(again.written.is_empty(), "{:?}", again.written);
    assert!(again.outputs.contains("_exp/headline/index.html"));
    let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert_eq!(home.matches("data-awp-experiment=").count(), 1);

    // Stopped: the variant and the script leave, and the control loses its tag.
    let mut stopped = running.clone();
    stopped.status = ExperimentStatus::Stopped {
        decision: Decision::AdoptVariant,
        at: Utc::now(),
        paused_job: None,
    };
    store.put(&stopped).await.unwrap();
    let changes = ChangeSet { items: Default::default(), previous: again.graph.clone() };
    let after = generator.build_incremental(&site.base, out.path(), &changes).await.unwrap();
    assert!(
        after.removed.contains("_exp/headline/index.html")
            && after.removed.contains("_awp/experiments.js")
    );
    assert!(after.written.contains("index.html"));
    assert!(!out.path().join("_exp").exists(), "empty variant directories go too");
    let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert_eq!(
        home,
        std::fs::read_to_string(scratch.path().join("index.html")).unwrap(),
        "the control as built"
    );

    // A control page that no longer holds a change's text carries no experiment.
    let mut stale = running.clone();
    stale.plan =
        ExperimentPlan::new("headline", "/", "signups").replace("<h1>Gone</h1>", "<h1>x</h1>");
    store.put(&stale).await.unwrap();
    let report = generator.build(&site.base, out.path()).await.unwrap();
    assert!(!report.outputs.contains("_exp/headline/index.html"));
    assert!(
        !std::fs::read_to_string(out.path().join("index.html"))
            .unwrap()
            .contains("data-awp-experiment")
    );
}

struct Fixture {
    site: Site,
    store: Arc<InMemoryExperimentStore>,
    counts: Arc<InMemoryBeaconStore>,
    clock: ManualClock,
    pauses: Arc<JobPauses>,
}

impl Fixture {
    async fn new() -> Self {
        let (site, _) = committed().await;
        Self {
            site,
            store: Arc::new(InMemoryExperimentStore::new()),
            counts: Arc::new(InMemoryBeaconStore::new()),
            clock: ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 7, 9, 0, 0).unwrap()),
            pauses: Arc::new(JobPauses::new()),
        }
    }

    fn runner(&self) -> ExperimentRunner {
        ExperimentRunner::new(self.site.site.id, self.site.charters.clone(), self.store.clone())
            .with_source(Arc::new(BeaconKpi::new(
                self.site.site.id,
                BeaconCounts::new(self.counts.clone()),
            )))
            .with_control(self.pauses.clone())
            .with_clock(Arc::new(self.clock.clone()))
    }

    /// Counts one day of each arm: exposures, sign-ups and unsubscribes.
    async fn day(&self, date: NaiveDate, arms: [(u64, u64, u64); 2]) {
        for (arm, (exposures, signups, unsubscribes)) in ["a", "b"].into_iter().zip(arms) {
            for (event, count) in [
                ("exposure", exposures),
                ("newsletter.signups", signups),
                ("email.unsubscribe_rate", unsubscribes),
            ] {
                let metric = format!("events:exp.headline.{arm}.{event}");
                self.counts.add(date, &metric, count).await.unwrap();
            }
        }
    }

    fn at(&self, date: NaiveDate) {
        self.clock.set(Utc.from_utc_datetime(&date.and_hms_opt(3, 30, 0).unwrap()));
    }
}

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, day).unwrap()
}

/// What `start` refuses (spec 3.5 §7.12, A-I15).
#[tokio::test]
async fn start_refuses_a_paused_job_a_reused_id_and_a_taken_page() {
    let fixture = Fixture::new().await;
    let runner = fixture.runner();
    let no_charter = ExperimentRunner::new(
        uuid::Uuid::new_v4(),
        Arc::new(InMemoryCharterStore::new()),
        fixture.store.clone(),
    );
    assert_eq!(
        no_charter.start(plan("headline")).await.unwrap_err().code,
        "website.experiment.no_charter"
    );

    runner.start(plan("headline")).await.unwrap();
    assert_eq!(runner.start(plan("headline")).await.unwrap_err().code, "website.experiment.exists");
    let taken = runner.start(plan("other")).await.unwrap_err();
    assert!(taken.message.contains("one experiment runs on a page at a time"), "{taken}");
    let elsewhere = ExperimentPlan::new("pricing", "/pricing/", "signups").replace("a", "b");
    runner.start(elsewhere).await.unwrap();

    fixture.pauses.pause(fixture.site.site.id, "experiments", "a breach").await.unwrap();
    let paused = ExperimentPlan::new("about", "/about/", "signups").replace("a", "b");
    assert_eq!(runner.start(paused).await.unwrap_err().code, "website.experiment.paused");
    assert_eq!(runner.list().await.unwrap().len(), 2);
}

/// Each look adds the whole days since the last one, once (spec 3.5 §7.12).
#[tokio::test]
async fn looks_add_each_whole_day_once() {
    let fixture = Fixture::new().await;
    let runner = fixture.runner();
    runner.start(plan("headline")).await.unwrap();
    assert!(runner.look("headline").await.unwrap().is_none(), "no whole day yet");

    fixture.day(date(7), [(500, 20, 3), (500, 25, 3)]).await;
    fixture.day(date(8), [(400, 16, 2), (410, 21, 2)]).await;
    fixture.day(date(9), [(600, 24, 3), (590, 30, 4)]).await;
    fixture.at(date(9));
    let first = runner.look("headline").await.unwrap().unwrap();
    assert_eq!(first.days, 2);
    assert_eq!(first.record.counts.exposures.control, 900);
    assert_eq!(first.record.counts.events["newsletter.signups"].variant, 46);
    assert!(runner.look("headline").await.unwrap().is_none(), "the same day again adds nothing");
    fixture.at(date(11));
    let second = runner.look("headline").await.unwrap().unwrap();
    assert_eq!(second.days, 2);
    assert_eq!(second.record.counts.exposures.variant, 1500);
    assert_eq!(second.record.counts.events["email.unsubscribe_rate"].variant, 9);
    let report = second.record.latest.as_ref().unwrap();
    assert_eq!(report.look, 2);
    assert_eq!(report.primary.control_units, 1500);
    assert_eq!(report.decision, Decision::Continue);

    // A source that fails changes nothing; the next look reads the same days again.
    let broken = ExperimentRunner::new(
        fixture.site.site.id,
        fixture.site.charters.clone(),
        fixture.store.clone(),
    )
    .with_clock(Arc::new(fixture.clock.clone()));
    fixture.at(date(12));
    assert_eq!(broken.look("headline").await.unwrap_err().code, "website.experiment.no_source");
    assert_eq!(runner.get("headline").await.unwrap().unwrap(), second.record);
    assert_eq!(runner.look("ghost").await.unwrap_err().code, "website.experiment.unknown");
}

/// A clear lift with no harm adopts the variant: no pause, and the review says so.
#[tokio::test]
async fn a_lift_without_harm_adopts_the_variant() {
    let fixture = Fixture::new().await;
    let runner = fixture.runner();
    runner.start(plan("headline").planned_units_per_arm(20_000)).await.unwrap();
    let mut stopped = None;
    for day in 7..30 {
        fixture.day(date(day), [(2_000, 80, 12), (2_000, 110, 11)]).await;
        fixture.at(date(day + 1));
        let look = runner.look("headline").await.unwrap().unwrap();
        if look.stopped {
            stopped = Some(look);
            break;
        }
    }
    let stopped = stopped.expect("the lift is found within the month");
    assert_eq!(stopped.record.latest.as_ref().unwrap().decision, Decision::AdoptVariant);
    assert!(matches!(&stopped.record.status, ExperimentStatus::Stopped { paused_job: None, .. }));
    assert!(fixture.pauses.paused(fixture.site.site.id, "experiments").await.unwrap().is_none());
    let from = Utc.with_ymd_and_hms(2026, 9, 7, 0, 0, 0).unwrap();
    let entries = runner.review(from, from + chrono::Duration::days(30)).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].contains("adopt the variant"), "{}", entries[0]);
    assert!(!entries[0].contains("paused"), "{}", entries[0]);
    // A week before it started, or after it stopped, has no entry.
    assert!(runner.review(from - chrono::Duration::days(7), from).await.unwrap().is_empty());
    let after = stopped.record.stopped_at().unwrap() + chrono::Duration::days(1);
    assert!(runner.review(after, after + chrono::Duration::days(7)).await.unwrap().is_empty());
}

/// The daily look as a kernel run (spec 3.5 §7.12).
#[tokio::test]
async fn the_daily_look_runs_as_a_kernel_workflow() {
    let fixture = Fixture::new().await;
    let runner = fixture.runner();
    runner.start(plan("headline")).await.unwrap();
    fixture.day(date(7), [(500, 20, 3), (500, 25, 3)]).await;
    fixture.at(date(8));
    assert!(runner.has_workflow(EXPERIMENTS_WORKFLOW));
    let input = json!({ "site": fixture.site.site.id });
    let output = runner
        .execute(
            EXPERIMENTS_WORKFLOW,
            input.as_object().unwrap().clone().into_iter().collect(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output["looks"][0]["experiment"], "headline");
    assert_eq!(output["looks"][0]["decision"]["outcome"], "continue");
    let other = json!({ "site": uuid::Uuid::new_v4() });
    assert!(
        runner
            .execute(
                EXPERIMENTS_WORKFLOW,
                other.as_object().unwrap().clone().into_iter().collect(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let trigger = runner.trigger("juniper", "0 30 3 * * *");
    let json = serde_json::to_value(&trigger).unwrap().to_string();
    assert!(json.contains(EXPERIMENTS_WORKFLOW) && json.contains("0 30 3 * * *"), "{json}");
}

/// The shipped client script in a stubbed browser under Node.js: arms, the redirect to the
/// variant's file, one exposure per tab session, goals once, attribution from a fragment.
///
/// ```text
/// cargo nextest run -p adk-website --test experiments --run-ignored all the_client_script
/// ```
#[tokio::test]
#[ignore = "needs Node.js on PATH"]
async fn the_client_script_follows_its_protocol_in_a_browser() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = tokio::process::Command::new("node")
        .arg(dir.join("tests/fixtures/experiments_script_check.js"))
        .arg(dir.join("src/experiments/experiments.js"))
        .kill_on_drop(true)
        .output()
        .await
        .expect("node runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(stdout.lines().filter(|line| line.starts_with("ok")).count(), 13, "{stdout}");
}
