//! Learning (spec 3.5 §7.15, §7.11, A-I13, A-I14): a decided experiment and a KPI short of
//! the owner's target each propose one playbook revision on the owner's decision card;
//! declining leaves the playbook as it was, accepting or editing makes its next version, no
//! autonomy level applies a revision without the owner, at Observe a proposal is only
//! reported, and too little evidence proposes nothing. The experiment runs on scripted
//! beacon counts; the KPIs are recorded samples. A served site runs the learning job in its
//! kernel.

mod common;
mod experiment_fixture;

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalDecision, ApprovalRequest, ApprovalService, InMemoryApprovalStore, InboxQuery,
    ManualClock, StoreApprovalService,
};
use adk_eval::experiments::Decision;
use adk_kernel::charter::{
    AutonomyLadder, CharterStore, InMemoryKpiStore, KpiSample, KpiStore, KpiTracker, OwnerDecision,
};
use adk_server::awp::{BeaconCounts, BeaconStore, InMemoryBeaconStore};
use adk_website::experiments::{ExperimentPlan, ExperimentRunner, InMemoryExperimentStore};
use adk_website::learning::{
    LEARNING_JOB, LearningParts, OutcomeKind, PLAYBOOK_REVISION, PlaybookLearning, ProposalState,
    REVIEW_PATH,
};
use adk_website::measurement::BeaconKpi;
use adk_website::review::ReviewComposer;
use awp_types::{AutonomyLevel, Charter, Role, SiteKind, Target};
use chrono::{NaiveDate, TimeZone, Utc};
use common::*;
use serde_json::json;

/// The Monday of the week the learning run reviews.
fn week() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()
}

fn date(month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, month, day).unwrap()
}

struct Fixture {
    site: Site,
    kpis: Arc<InMemoryKpiStore>,
    counts: Arc<InMemoryBeaconStore>,
    clock: ManualClock,
    runner: Arc<ExperimentRunner>,
    approvals: Arc<StoreApprovalService>,
    learning: PlaybookLearning,
    data: tempfile::TempDir,
}

impl Fixture {
    /// The experiments site with the owner's `targets` on its KPIs, each beside a baseline
    /// (A-I14).
    async fn new(targets: &[(&str, f64)]) -> Self {
        let site =
            Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
        let mut charter: Charter = experiment_fixture::charter(&site);
        for (id, value) in targets {
            let kpi = charter.kpis.iter_mut().find(|kpi| kpi.id == *id).unwrap();
            kpi.target = Some(Target {
                value: *value,
                horizon: Utc.with_ymd_and_hms(2026, 12, 31, 0, 0, 0).unwrap(),
            });
        }
        site.commit(&charter, &[(Role::Owner, OWNER)]).await;
        let kpis = Arc::new(InMemoryKpiStore::new());
        let counts = Arc::new(InMemoryBeaconStore::new());
        let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 7, 9, 0, 0).unwrap());
        let charters: Arc<dyn CharterStore> = site.charters.clone();
        let runner = Arc::new(
            ExperimentRunner::new(
                site.site.id,
                charters.clone(),
                Arc::new(InMemoryExperimentStore::new()),
            )
            .with_source(Arc::new(BeaconKpi::new(site.site.id, BeaconCounts::new(counts.clone()))))
            .with_clock(Arc::new(clock.clone())),
        );
        let tracker = KpiTracker::new(site.site.id, charters.clone(), kpis.clone());
        let composer =
            ReviewComposer::new(site.site.id, tracker, site.ladder.clone(), charters.clone());
        let approvals = Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
        let data = tempfile::tempdir().unwrap();
        let learning = PlaybookLearning::open(LearningParts {
            site: site.site.clone(),
            team: site.team.clone(),
            owner: OWNER.into(),
            composer,
            experiments: runner.clone(),
            charters,
            ladder: site.ladder.clone(),
            approvals: approvals.clone(),
            data: data.path().to_path_buf(),
        })
        .unwrap()
        .with_clock(Arc::new(clock.clone()));
        Self { site, kpis, counts, clock, runner, approvals, learning, data }
    }

    /// One sample a day of `kpi`: the week before the reviewed one at `previous`, the
    /// reviewed week at `current`.
    async fn kpi_weeks(&self, kpi: &str, previous: f64, current: f64) {
        for day in 0..14 {
            let at = (week() - chrono::Duration::days(7) + chrono::Duration::days(day))
                .and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc();
            let value = if day < 7 { previous } else { current };
            self.kpis
                .record(KpiSample::new(self.site.site.id, kpi, at, value, "beacon"))
                .await
                .unwrap();
        }
    }

    /// The `headline` experiment, counted daily until its test decides: a clear lift with
    /// no harm adopts the variant.
    async fn decided_experiment(&self) {
        self.runner
            .start(
                ExperimentPlan::new("headline", "/", "signups")
                    .hypothesis("Saying when the bread is baked lifts sign-ups")
                    .replace("<h1>Juniper Bakery</h1>", "<h1>Bread out of the oven at 5am</h1>")
                    .planned_units_per_arm(20_000),
            )
            .await
            .unwrap();
        for day in 7..30 {
            self.day(date(9, day), [(2_000, 80, 12), (2_000, 110, 11)]).await;
            self.clock.set(Utc.from_utc_datetime(&date(9, day + 1).and_hms_opt(3, 30, 0).unwrap()));
            if self.runner.look("headline").await.unwrap().unwrap().stopped {
                break;
            }
        }
        let record = self.runner.get("headline").await.unwrap().unwrap();
        assert_eq!(record.latest.unwrap().decision, Decision::AdoptVariant);
    }

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

    async fn cards(&self) -> Vec<ApprovalRequest> {
        let inbox = self
            .approvals
            .inbox(InboxQuery::for_approver(SubjectRef::human(OWNER), Vec::<String>::new()))
            .await
            .unwrap();
        inbox.items.into_iter().filter(|card| card.tool_name == PLAYBOOK_REVISION).collect()
    }

    async fn decide(&self, card: &str, decision: ApprovalDecision) {
        let request = self.approvals.get(card).await.unwrap().unwrap();
        self.approvals.decide(card, request.version, decision).await.unwrap();
    }

    fn workspace(&self, agent: &str, playbook: &str) -> std::path::PathBuf {
        self.data.path().join("agents").join(agent).join(".skills").join(format!("{playbook}.md"))
    }

    fn builtin(&self, playbook: &str) -> String {
        adk_website::playbooks::playbook(playbook).unwrap().text.to_string()
    }
}

/// A scripted week: the experiment's winner and the KPI miss propose one revision each, with
/// the playbook, the diff, the evidence and its links, and the expected effect; the owner
/// declines one, which leaves its playbook as it was, and accepts the other, its next
/// version (spec 3.5 §7.15).
#[tokio::test]
async fn an_experiment_winner_and_a_kpi_miss_propose_one_revision_each() {
    let fixture = Fixture::new(&[("signups", 0.06)]).await;
    fixture.decided_experiment().await;
    fixture.kpi_weeks("signups", 0.045, 0.05).await;

    let proposals = fixture.learning.learn(week()).await.unwrap();
    let playbooks: Vec<&str> = proposals.iter().map(|p| p.playbook.as_str()).collect();
    assert_eq!(playbooks, ["experiments", "nurture"], "{proposals:#?}");
    let (experiment, kpi) = (&proposals[0], &proposals[1]);
    for proposal in &proposals {
        assert_eq!((proposal.state, proposal.base_version), (ProposalState::Proposed, 1));
        assert_eq!(proposal.card.as_deref(), Some(proposal.id.as_str()));
        let header = format!("--- a/{0}.md\n+++ b/{0}.md\n", proposal.playbook);
        assert!(proposal.diff.starts_with(&header), "{}", proposal.diff);
        assert!(proposal.diff.contains(&format!("+{}", proposal.addition)), "{}", proposal.diff);
        assert!(proposal.evidence[0].link.starts_with(&format!("{REVIEW_PATH}?week=")));
        assert!(!proposal.expected_effect.is_empty());
    }
    assert_eq!(experiment.evidence[0].kind, OutcomeKind::Experiment);
    assert!(
        experiment.addition.starts_with("- Prefer what experiment `headline` tested on /"),
        "{}",
        experiment.addition
    );
    assert!(experiment.expected_effect.contains("newsletter.signups"));
    assert_eq!(kpi.evidence[0].kind, OutcomeKind::Kpi);
    assert_eq!(kpi.evidence[0].link, format!("{REVIEW_PATH}?week=2026-09-28#kpis"));
    assert!(kpi.evidence[0].summary.contains("against a target of 0.06"));
    assert!(kpi.addition.contains("Newsletter sign-ups missed its target"), "{}", kpi.addition);

    // Each is the owner's decision card, showing the diff and the evidence.
    let cards = fixture.cards().await;
    assert_eq!(cards.len(), 2);
    let card = cards.iter().find(|c| c.id == kpi.id).unwrap();
    assert_eq!(card.args_redacted["diff"], json!(kpi.diff));
    assert_eq!(card.args_redacted["evidence"][0]["link"], json!(kpi.evidence[0].link));
    assert!(card.summary.starts_with("Revise the nurture playbook"), "{}", card.summary);

    // The same outcomes propose nothing again.
    assert!(fixture.learning.learn(week()).await.unwrap().is_empty());
    assert_eq!(fixture.cards().await.len(), 2);

    // Declining leaves the playbook unchanged.
    fixture.decide(&kpi.id, ApprovalDecision::reject(SubjectRef::human(OWNER), "not now")).await;
    let decided = fixture.learning.step().await.unwrap();
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0].state, ProposalState::Declined);
    let nurture = fixture.learning.versions().current("nurture").await.unwrap();
    assert_eq!((nurture.version, nurture.text), (1, fixture.builtin("nurture")));
    assert!(!fixture.workspace("revenue", "nurture").exists());

    // Accepting bumps the version and writes it where the Growth agent reads it.
    fixture.decide(&experiment.id, ApprovalDecision::approve(SubjectRef::human(OWNER))).await;
    let decided = fixture.learning.step().await.unwrap();
    assert_eq!((decided[0].state, decided[0].version), (ProposalState::Accepted, Some(2)));
    let current = fixture.learning.versions().current("experiments").await.unwrap();
    assert_eq!(current.version, 2);
    assert_eq!(current.text, experiment.text);
    assert!(current.text.ends_with(&format!("## Learned\n\n{}\n", experiment.addition)));
    assert_eq!(current.accepted_by.as_deref(), Some(OWNER));
    let installed = std::fs::read_to_string(fixture.workspace("growth", "experiments")).unwrap();
    assert_eq!(installed, experiment.text);
    assert_eq!(fixture.learning.versions().history("experiments").await.unwrap().len(), 1);

    // The decisions count on the ladder for the learning job: an approval after a rejection.
    let eligibility =
        fixture.site.ladder.eligibility(fixture.site.site.id, LEARNING_JOB).await.unwrap();
    assert_eq!((eligibility.level, eligibility.streak), (AutonomyLevel::Propose, 1));
    let journal =
        std::fs::read_to_string(fixture.data.path().join("learning/proposals.json")).unwrap();
    assert!(journal.contains("\"accepted\"") && journal.contains("\"declined\""), "{journal}");
}

/// No autonomy level applies a revision without the owner: promoted to "act within budget",
/// the learning job still only proposes, and the owner's edit is the next version (spec 3.5
/// §7.15, A-I13).
#[tokio::test]
async fn no_level_applies_a_revision_and_an_edit_is_the_next_version() {
    let fixture = Fixture::new(&[("signups", 0.06)]).await;
    let ladder: &AutonomyLadder = &fixture.site.ladder;
    let decision = OwnerDecision::new("promote-learning", SubjectRef::human(OWNER), Utc::now());
    ladder
        .promote_to(fixture.site.site.id, LEARNING_JOB, AutonomyLevel::ActWithinBudget, &decision)
        .await
        .unwrap();
    fixture.kpi_weeks("signups", 0.03, 0.035).await;

    let proposals = fixture.learning.learn(week()).await.unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].state, ProposalState::Proposed);
    assert!(fixture.learning.step().await.unwrap().is_empty(), "nothing applies undecided");
    assert_eq!(fixture.learning.versions().current("nurture").await.unwrap().version, 1);
    assert!(!fixture.workspace("revenue", "nurture").exists());

    let text = format!(
        "{}\n\n## Learned\n\n- Ask for the sign-up after the first order.\n",
        fixture.builtin("nurture").trim_end()
    );
    let edit = ApprovalDecision::edit(SubjectRef::human(OWNER), json!({ "text": text }));
    fixture.decide(&proposals[0].id, edit).await;
    let decided = fixture.learning.step().await.unwrap();
    assert_eq!(
        (decided[0].state, decided[0].edited, decided[0].version),
        (ProposalState::Accepted, true, Some(2))
    );
    let current = fixture.learning.versions().current("nurture").await.unwrap();
    assert_eq!((current.version, current.text.as_str()), (2, text.as_str()));
    assert_eq!(std::fs::read_to_string(fixture.workspace("revenue", "nurture")).unwrap(), text);
}

/// At Observe the learning job's output goes to reports: a proposal is drafted and reported,
/// with no card, and the playbook is unchanged (spec 3.5 §7.11).
#[tokio::test]
async fn at_observe_a_proposal_is_reported_only() {
    let fixture = Fixture::new(&[("unsubscribe_rate", 0.01)]).await;
    let site = fixture.site.site.id;
    fixture.site.ladder.lower(site, LEARNING_JOB, "owner rejections").await.unwrap();
    let level = fixture.site.ladder.level(site, LEARNING_JOB).await.unwrap();
    assert_eq!(level, Some(AutonomyLevel::Observe));
    fixture.kpi_weeks("unsubscribe_rate", 0.02, 0.018).await;

    let proposals = fixture.learning.learn(week()).await.unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!((proposals[0].state, proposals[0].card.as_deref()), (ProposalState::Reported, None));
    assert_eq!(proposals[0].playbook, "email", "the unsubscribe rate is the email job's");
    assert!(fixture.cards().await.is_empty());
    assert!(fixture.learning.step().await.unwrap().is_empty());
    assert_eq!(fixture.learning.versions().current("email").await.unwrap().version, 1);
}

/// Too little evidence proposes nothing (spec 3.5 §7.12, A-I14): a KPI short of its target
/// for one week only, a KPI that meets it, a KPI with no target, and an experiment its test
/// has not decided.
#[tokio::test]
async fn too_little_evidence_proposes_nothing() {
    let fixture = Fixture::new(&[("signups", 0.06), ("unsubscribe_rate", 0.01)]).await;
    fixture.kpi_weeks("signups", 0.07, 0.05).await;
    fixture.kpi_weeks("unsubscribe_rate", 0.008, 0.009).await;
    let plan = ExperimentPlan::new("headline", "/", "signups")
        .replace("<h1>Juniper Bakery</h1>", "<h1>Fresh</h1>");
    fixture.runner.start(plan).await.unwrap();
    fixture.day(date(9, 7), [(500, 20, 3), (500, 25, 3)]).await;
    fixture.clock.set(Utc.from_utc_datetime(&date(9, 8).and_hms_opt(3, 30, 0).unwrap()));
    let look = fixture.runner.look("headline").await.unwrap().unwrap();
    assert!(!look.stopped, "one day decides nothing");

    assert!(fixture.learning.learn(week()).await.unwrap().is_empty());
    assert!(fixture.learning.proposals().await.is_empty());
    assert!(fixture.cards().await.is_empty());
}

/// A served site runs the learning job in its kernel: a run of its workflow reads last week's
/// review and answers with the proposals it drafted (spec 3.5 §7.15, §7.20).
#[tokio::test]
async fn a_served_site_runs_the_learning_job_in_its_kernel() {
    use adk_core::{DurableRunStore, NewRun, RunFilter};
    use adk_website::deploy::{ServeOptions, serve};
    use adk_website::learning::LEARNING_WORKFLOW;
    use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};

    let dir = tempfile::tempdir().unwrap();
    let options = ScaffoldOptions {
        name: "Juniper Bakery".into(),
        domain: "juniper.example".into(),
        kind: SiteKind::Business,
        languages: vec!["en".parse().unwrap()],
        theme: adk_website::themes::ThemeId::Paper,
        currency: "KES".into(),
    };
    scaffold(dir.path(), &options).await.unwrap();
    let mut options = ServeOptions::new(dir.path(), OWNER);
    options.edge = "127.0.0.1:0".parse().unwrap();
    options.kernel = "127.0.0.1:0".parse().unwrap();
    options.site.idle = Some(std::time::Duration::from_millis(200));
    let served = serve(options).await.unwrap();
    let site = &served.sites[0];
    let run = NewRun::new(
        adk_kernel::QueueClass::BACKGROUND,
        site.site.name.clone(),
        "website:learning",
        LEARNING_WORKFLOW,
        json!({ "site": site.site.id }),
    );
    let id = match served.stores.runs.enqueue(run).await.unwrap() {
        adk_core::run_store::Enqueued::Created(record) => record.run_id,
        other => panic!("{other:?}"),
    };
    let mut finished = None;
    for _ in 0..200 {
        let runs = served.stores.runs.list(RunFilter::default()).await.unwrap();
        finished = runs.into_iter().find(|r| r.run_id == id && r.state.is_terminal());
        if finished.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let run = finished.expect("the learning run ends");
    assert_eq!(run.error, None, "{run:#?}");
    let result = run.result.unwrap();
    assert_eq!(result["proposals"], json!([]), "a new site has no outcomes yet: {result}");
    assert!(site.learning.proposals().await.is_empty());
    served.shutdown().await.unwrap();
}
