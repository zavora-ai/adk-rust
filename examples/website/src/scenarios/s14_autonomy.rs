//! S14 — earned autonomy (spec 3.5 §5 S14, §7.11, §7.21, D-7, D-9, A-I13, A-I15).
//!
//! Over 30 simulated days the Growth agent proposes twenty review replies, which the owner
//! approves unchanged; then a reply goes out that sinks the review rating below the owner's
//! guardrail.
//!
//! Pass condition: the weekly review offers promotion; the owner accepts and a standing rule
//! is created by the owner's decision; the breach demotes the job one level automatically and
//! says why.

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditTrail;
use adk_core::principal::SubjectRef;
use adk_core::{Clock, ManualClock};
use adk_harness::StandingRuleStore;
use adk_harness::website::{apply_demotion, job_levels, promote_job};
use adk_kernel::charter::{
    AutonomyLadder, DecisionOutcome, InMemoryKpiStore, JobDecision, KpiSample, KpiStore,
    KpiTracker, OwnerDecision,
};
use adk_model::ScriptedResponse;
use adk_website::review::{ReviewComposer, ReviewInputs};
use adk_website::team::AgentRole;
use anyhow::{Result, anyhow};
use awp_types::{AutonomyLevel, Breach, Guardrail, GuardrailAction, Role, SiteKind};
use chrono::{Datelike, TimeZone, Utc};
use serde_json::json;

use crate::checks::Checks;
use crate::fixture::*;
use crate::live::Mode;

/// The review-reply job (process `site.reviews`, playbook `review-replies`).
const JOB: &str = "social";

/// Runs S14.
pub async fn run(mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let start =
        Utc.with_ymd_and_hms(2026, 9, 28, 9, 0, 0).single().ok_or_else(|| anyhow!("a date"))?;
    let clock = ManualClock::new(start);
    let shared: Arc<dyn Clock> = Arc::new(clock.clone());
    let mut site = Site::with(
        SiteKind::Business,
        "Juniper Bakery",
        "juniper.example",
        &["en"],
        Some(shared.clone()),
        |_| {},
    )
    .await?;
    site.ladder = AutonomyLadder::new(site.autonomy.clone())
        .with_charters(site.charters.clone())
        .with_clock(shared.clone())
        .with_audit(AuditTrail::new(Arc::new(site.audit.clone())));

    // The owner's charter adds a floor on the review rating for the review-reply job.
    let mut charter = site.charter("Juniper Bakery", "Fresh bread every morning.", "KES");
    let rating = charter
        .kpis
        .iter()
        .find(|k| k.id == "review_rating")
        .cloned()
        .ok_or_else(|| anyhow!("the business charter measures the review rating"))?;
    charter.guardrails.push(Guardrail {
        id: "rating_floor".into(),
        metric: rating.metric.clone(),
        threshold: 4.0,
        breach: Breach::Below,
        window: Duration::from_secs(2 * 24 * 3_600),
        action: GuardrailAction::PauseJob,
        jobs: vec![JOB.into()],
    });
    site.commit(&charter, &[(Role::Owner, OWNER)]).await?;
    let first = site.ladder.level(site.site.id, JOB).await?;
    checks.check(
        "the review-reply job works at Propose for the first week",
        first == Some(AutonomyLevel::Propose),
        format!("{JOB}: {first:?}"),
    );

    // Thirty days: a review a day and a half; the Growth agent drafts each reply as a proposal
    // and the owner approves it unchanged.
    let growth_prompter = ScriptedPrompter::new();
    let (growth, _) = site.back_office(
        &mode,
        "growth",
        &charter,
        vec![
            call("content_draft", json!({ "kind": "page", "slug": "reply-1", "language": "en", "fields": { "title": "Thank you, Wanjiru", "summary": "A reply to a review", "goal": "reputation" }, "body": "Thank you for the kind words about our sourdough.", "job": JOB })),
            ScriptedResponse::text("Drafted a reply for the owner."),
        ],
        |b| {
            b.instruction(format!(
                "{}\n\nA new review arrived: \"The sourdough is the best in Nakuru.\" — Wanjiru K. \
                 Draft a short, warm reply with content_draft (kind page, slug reply-1, language en, \
                 fields title and summary, goal reputation, job {JOB}) for the owner to approve. Do \
                 not publish it.",
                AgentRole::Growth.prompt()
            ))
            .prompter(growth_prompter.clone())
        },
    )?;
    let owner = user(OWNER);
    let session = growth.open_session(&owner, None).await?;
    let events =
        collect(growth.send(&owner, &session, "A review_seen signal: a new review.").await?)
            .await?;
    if mode.is_live() {
        trace(&events);
    }
    let drafted = response(&events, "content_draft", 0).unwrap_or_default();
    if !drafted["draft"].is_string() {
        checks.gap(format!(
            "the Growth agent cannot draft a review reply under `{JOB}`: the job covers no tool, \
             so `content_draft` is denied ({})",
            drafted["error"].as_str().unwrap_or_default()
        ));
    }
    checks.check(
        "at Propose the agent drafts the reply for the owner; nothing publishes",
        drafted["draft"].is_string() && site.log.size().await == 0,
        format!("draft {}", drafted["draft"]),
    );
    let owner_subject = SubjectRef::human(OWNER);
    for n in 0..20 {
        site.ladder
            .record_decision(JobDecision::new(
                site.site.id,
                JOB,
                format!("reply-{n}"),
                DecisionOutcome::ApprovedUnchanged,
                owner_subject.clone(),
                clock.now(),
            ))
            .await?;
        clock.advance(Duration::from_secs(36 * 3_600));
    }

    // The weekly review offers the promotion; it is an offer, nothing changes until the
    // owner decides.
    let kpis = Arc::new(InMemoryKpiStore::new());
    let tracker = || {
        KpiTracker::new(site.site.id, site.charters.clone(), kpis.clone())
            .with_clock(shared.clone())
            .with_ladder(site.ladder.clone())
            .with_audit(AuditTrail::new(Arc::new(site.audit.clone())))
    };
    let composer =
        ReviewComposer::new(site.site.id, tracker(), site.ladder.clone(), site.charters.clone());
    let today = clock.now().date_naive();
    let monday = today - chrono::Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let review = composer.compose(monday, ReviewInputs::default()).await?;
    let offer = review.promotions.iter().find(|p| p.job == JOB);
    checks.check(
        "the weekly review offers promotion",
        offer.is_some_and(|p| p.eligible)
            && review.to_markdown().contains(&format!("promote {JOB}"))
            && site.ladder.level(site.site.id, JOB).await? == Some(AutonomyLevel::Propose),
        format!("{offer:?}"),
    );

    // The site cannot raise its own level; the owner can, and the standing rule is theirs.
    let compiler = site.compiler(&charter);
    let agent = OwnerDecision::new(
        "dec-self",
        SubjectRef::service(AgentRole::Growth.principal(&site.site)),
        clock.now(),
    );
    let refused =
        promote_job(&site.ladder, &compiler, site.rules.as_ref(), JOB, &agent, OWNER).await;
    checks.check(
        "the site cannot promote itself",
        refused.is_err(),
        "an agent's decision is refused",
    );
    let accepted = OwnerDecision::new("dec-promote-social", owner_subject.clone(), clock.now());
    let promotion =
        promote_job(&site.ladder, &compiler, site.rules.as_ref(), JOB, &accepted, OWNER).await?;
    if promotion.rules.is_empty() {
        checks.gap(format!(
            "the review-reply job (`{JOB}`) binds no tools (`JobSpec::tools` is empty), and a \
             promotion to ActWithApproval grants no rule for any job: twenty approvals earn an \
             offer, but accepting it creates no standing rule"
        ));
    }
    let rules = site.rules.list(&site.site.name, OWNER).await?;
    checks.check(
        "the owner accepts and a standing rule is created by the owner's decision",
        promotion.job.level == AutonomyLevel::ActWithApproval
            && !promotion.rules.is_empty()
            && !rules.is_empty()
            && promotion.rules.iter().all(|r| r.created_by == owner_subject),
        format!(
            "{JOB} now {:?}; {} standing rule(s): {:?}",
            promotion.job.level,
            promotion.rules.len(),
            promotion.rules.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
        ),
    );
    let levels = job_levels(&compiler, site.autonomy.as_ref(), site.site.id).await?;
    let growth_mode = compiler.mode_for_agent("growth", &levels);

    // A reply goes out and the rating sinks below the floor: the tracker's next tick pauses
    // the job, then lowers it one rung, with the guardrail as the reason.
    for day in 0..3 {
        let at = clock.now() - chrono::Duration::hours(36 - 12 * day);
        kpis.record(KpiSample::new(site.site.id, &rating.id, at, 3.4, "reviews")).await?;
    }
    let report = tracker().tick().await?;
    let lowered = site.ladder.level(site.site.id, JOB).await?;
    let revoked = apply_demotion(
        &compiler,
        site.rules.as_ref(),
        JOB,
        lowered.unwrap_or(AutonomyLevel::Propose),
    )
    .await?;
    let review = composer.compose(monday, ReviewInputs::default()).await?;
    let demotion = review.demotions.iter().find(|d| d.job == JOB);
    checks.check(
        "the breach demotes the job one level automatically and says why",
        report.breached.contains(&"rating_floor".to_string())
            && lowered == Some(AutonomyLevel::Propose)
            && demotion.is_some_and(|d| d.reason.describe().contains("rating_floor")),
        format!(
            "breached {:?}; {JOB} back to {lowered:?}; the review says \"{}\"; {} rule(s) revoked",
            report.breached,
            demotion.map(|d| d.reason.describe()).unwrap_or_default(),
            revoked.len()
        ),
    );
    checks.check(
        "the demotion takes back the standing rules the level no longer warrants",
        !revoked.is_empty(),
        format!("{revoked:?}; the Growth agent worked in {growth_mode:?} while promoted"),
    );
    let actions = site.actions();
    checks.check(
        "promotion, pause and demotion are audited",
        actions.iter().any(|a| a.contains("promot"))
            && actions.iter().any(|a| a.contains("paused"))
            && actions.iter().any(|a| a.contains("demot") || a.contains("lowered")),
        format!(
            "{:?}",
            actions
                .iter()
                .filter(|a| a.contains("job") || a.contains("autonomy"))
                .collect::<Vec<_>>()
        ),
    );
    Ok(checks)
}
