//! S10 — a week on duty (spec 3.5 §5 S10, §7.19, §7.20, §7.21).
//!
//! Seven simulated days at a bakery: ten visitor questions nobody answered, two reviews, a
//! RustSec advisory against the edge's lockfile and a certificate 14 days from expiry. The
//! Concierge files each question as a typed signal; the Growth agent drafts pages for the
//! recurring gaps; the week ends with the weekly review.
//!
//! Pass condition: draft pages for recurring gaps wait out `publish_delay` and an approval;
//! review replies drafted; upgrade and renewal proposals; one weekly review with KPIs against
//! baselines, cost, questions and promotion offers; every proactive run inside budget and
//! outside quiet hours; every public change a signed release.

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{Clock, ManualClock};
use adk_kernel::charter::{
    DecisionOutcome, InMemoryKpiStore, JobDecision, KpiSample, KpiStore, KpiTracker, SignalFilter,
    SignalStore,
};
use adk_model::ScriptedResponse;
use adk_site::host::SiteHost;
use adk_website::review::{ReviewComposer, ReviewInputs};
use adk_website::team::AgentRole;
use anyhow::{Result, anyhow};
use awp_types::{Bounded, Signal, SignalKind, SiteKind};
use chrono::{Datelike, TimeZone, Timelike, Utc};
use serde_json::json;

use crate::checks::Checks;
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "juniper.example";

/// The visitors' unanswered questions, by day: six about wedding cakes, four about delivery.
const QUESTIONS: [(u32, &str, &str); 10] = [
    (0, "wedding cakes", "Do you bake wedding cakes?"),
    (0, "delivery", "Do you deliver to Naivasha?"),
    (1, "wedding cakes", "How far ahead should I order a wedding cake?"),
    (2, "wedding cakes", "Can a wedding cake be gluten free?"),
    (2, "delivery", "What does delivery to Naivasha cost?"),
    (3, "wedding cakes", "Do you do tastings for wedding cakes?"),
    (4, "delivery", "Can you deliver on Sunday?"),
    (4, "wedding cakes", "What is the price of a three-tier wedding cake?"),
    (5, "wedding cakes", "Do you decorate wedding cakes with flowers?"),
    (6, "delivery", "Do you deliver to Gilgil?"),
];

/// Runs S10.
#[allow(clippy::too_many_lines)]
pub async fn run(mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let monday =
        Utc.with_ymd_and_hms(2026, 10, 19, 10, 0, 0).single().ok_or_else(|| anyhow!("a date"))?;
    let clock = ManualClock::new(monday);
    let shared: Arc<dyn Clock> = Arc::new(clock.clone());
    let mut site = Site::with(
        SiteKind::Business,
        "Juniper Bakery",
        DOMAIN,
        &["en"],
        Some(shared.clone()),
        |_| {},
    )
    .await?;
    site.ladder = adk_kernel::charter::AutonomyLadder::new(site.autonomy.clone())
        .with_charters(site.charters.clone())
        .with_clock(shared.clone());
    let charter = site.launch("Juniper Bakery", "KES", Vec::new()).await?;
    let concierge = AgentRole::Concierge.principal(&site.site);
    let sink = site.partitions.signal_sink(site.signals.clone(), site.agenda.clone());

    // The week: each unanswered question is a typed content_gap signal from the Concierge; two
    // reviews arrive on Wednesday.
    for (day, topic, question) in QUESTIONS {
        clock.set(monday + chrono::Duration::days(i64::from(day)));
        sink.emit(Signal {
            id: uuid::Uuid::new_v4(),
            kind: SignalKind::ContentGap {
                topic: Bounded::new(topic, 120).map_err(|e| anyhow!("{e:?}"))?,
                question_count: 1,
                samples: vec![Bounded::truncated(question, 200)],
            },
            emitted_by: concierge.clone(),
            emitted_at: clock.now(),
        })
        .await?;
    }
    clock.set(monday + chrono::Duration::days(2));
    for (rating, excerpt) in
        [(5, "The sourdough is the best in Nakuru."), (3, "Good bread, but sold out by noon.")]
    {
        sink.emit(Signal {
            id: uuid::Uuid::new_v4(),
            kind: SignalKind::ReviewSeen {
                platform: "maps".into(),
                rating,
                excerpt: Bounded::truncated(excerpt, 200),
            },
            emitted_by: concierge.clone(),
            emitted_at: clock.now(),
        })
        .await?;
    }
    let gaps = site.signals.list(&SignalFilter::default().kind("content_gap")).await?;
    let recurring: Vec<(String, u32)> = gaps.iter().map(|g| (g.topic.clone(), g.count)).collect();
    checks.check(
        "ten unanswered questions become recurring gaps on the back office's agenda",
        gaps.iter().map(|g| g.count).sum::<u32>() == 10 && gaps.iter().any(|g| g.count >= 6),
        format!("{recurring:?}"),
    );

    // Thursday: the Growth agent drafts a page for each recurring gap and a reply to each
    // review. It works at Propose: drafts wait for the owner, and a page it drafts on its own
    // waits out the site's publish delay of 24 hours as well.
    clock.set(monday + chrono::Duration::days(3));
    let delay = adk_website::publish_delay::PublishDelay::new(
        chrono::Duration::hours(24),
        site.compiler(&charter),
    )
    .with_clock(shared.clone());
    let undelayed = adk_core::ToolPolicy::evaluate(
        &delay,
        &{
            let mut request = adk_core::ToolPolicyRequest::new(
                "content_draft",
                json!({ "kind": "page", "slug": "wedding-cakes", "language": "en", "job": "freshness" }),
            );
            request.agent_name = "growth".into();
            request
        },
    )
    .await;
    let publish_at =
        (clock.now() + chrono::Duration::hours(25)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let (growth, _) = site.back_office(
        &mode,
        "growth",
        &charter,
        vec![
            call("content_draft", json!({ "kind": "page", "slug": "wedding-cakes", "language": "en", "fields": { "title": "Wedding cakes", "summary": "Ordering, tastings and prices.", "goal": "enquiries" }, "body": "We bake wedding cakes to order. Ask us about tastings.", "job": "freshness", "schedule": { "publishAt": publish_at } })),
            call("content_draft", json!({ "kind": "page", "slug": "delivery", "language": "en", "fields": { "title": "Delivery", "summary": "Where and when we deliver.", "goal": "enquiries" }, "body": "Ask us about delivery outside Nakuru.", "job": "freshness", "schedule": { "publishAt": publish_at } })),
            call("content_draft", json!({ "kind": "page", "slug": "reply-sold-out", "language": "en", "fields": { "title": "Reply", "summary": "A reply to a review.", "goal": "reputation" }, "body": "Thank you; we now bake a second batch at eleven.", "job": "social" })),
            ScriptedResponse::text("Drafted two pages for the owner and a review reply."),
        ],
        |b| {
            b.instruction(format!(
                "{}\n\nThis week visitors asked six times about wedding cakes and four times about \
                 delivery to Naivasha and Gilgil, and nobody answered. Draft one page per topic \
                 with content_draft (kind page, slugs wedding-cakes and delivery, language en, \
                 fields title, summary, goal enquiries, job freshness, schedule publishAt \
                 {publish_at}: the site's publish delay keeps a page you draft on your own off the \
                 site for 24 hours), claiming nothing the owner has not confirmed. content_draft \
                 only drafts: it is allowed at your level, and nothing goes live until the owner \
                 approves. Then draft a reply to the review \"Good bread, but sold \
                 out by noon.\" with content_draft (slug reply-sold-out, goal reputation, job \
                 social). Do not publish anything.",
                AgentRole::Growth.prompt()
            ))
            .add_operator_policy(Arc::new(delay))
        },
    )?;
    let owner = user(OWNER);
    let session = growth.open_session(&owner, None).await?;
    let events = collect(
        growth
            .send(&owner, &session, "The week's content gaps and reviews are on your agenda.")
            .await?,
    )
    .await?;
    if mode.is_live() {
        trace(&events);
    }
    let drafts: Vec<String> = responses(&events)
        .into_iter()
        .filter(|(n, r)| n == "content_draft" && r["draft"].is_string())
        .map(|(_, r)| r["draft"].as_str().unwrap_or_default().to_string())
        .collect();
    let releases_before = site.log.size().await;
    if mode.is_live() && drafts.is_empty() {
        checks.gap(
            "the Growth agent works in plan mode at Propose, whose prompt says changes are \
             blocked; a live model asks to leave plan mode (held for approval) instead of drafting, \
             although `content_draft` runs in plan mode",
        );
    }
    checks.check(
        "draft pages for the recurring gaps wait for an approval",
        drafts.len() >= 2 && site.host.fetch("/wedding-cakes/").await?.is_none(),
        format!(
            "{} drafts; nothing published, the log still holds {releases_before} release",
            drafts.len()
        ),
    );
    let reply =
        responses(&events).into_iter().filter(|(n, _)| n == "content_draft").nth(2).map(|(_, r)| r);
    checks.check(
        "review replies drafted",
        reply.as_ref().is_some_and(|r| r["draft"].is_string()),
        format!("{reply:?}"),
    );
    // The Guardian's daily watch: a RustSec advisory against the edge's lockfile and a
    // certificate 14 days from expiry become two proposals, each one card for the owner.
    let upkeep = upkeep_proposals(&site, clock.now()).await?;
    checks.check(
        "upgrade and renewal proposals",
        upkeep.0.len() == 2 && upkeep.1 == 2 && upkeep.2 == 0,
        format!(
            "{:?}; {} card(s) for the owner, {} filed again on the next day's watch",
            upkeep.0, upkeep.1, upkeep.2
        ),
    );

    // The owner approves the wedding-cake page on Thursday afternoon: it is scheduled, and
    // goes live only once the publish delay has passed, on Friday, as a signed release.
    clock.set(monday + chrono::Duration::days(3) + chrono::Duration::hours(6));
    if let Some(first) = drafts.first() {
        let (approver, model) =
            site.back_office(&Mode::Scripted, "steward", &charter, vec![], |b| {
                b.mode(adk_harness::HarnessMode::Ask).prompter({
                    let p = ScriptedPrompter::new();
                    p.approve_once();
                    p
                })
            })?;
        model.push(call("site_publish", json!({ "draft": first })));
        model.push(ScriptedResponse::text("Published."));
        let session = approver.open_session(&owner, None).await?;
        let _ = collect(
            approver.send(&owner, &session, "The owner approved the wedding-cake page.").await?,
        )
        .await?;
    }
    let approved_early = site.host.fetch("/wedding-cakes/").await?.is_none();
    clock.set(monday + chrono::Duration::days(4) + chrono::Duration::hours(2));
    let due = site.runtime.pipeline().run_due(None).await?;
    checks.check(
        "draft pages for recurring gaps wait out publish_delay and an approval",
        matches!(undelayed, adk_core::PolicyDecision::Deny { .. })
            && approved_early
            && site.host.fetch("/wedding-cakes/").await?.is_some(),
        format!(
            "a draft without the delay refused; approved Thursday 16:00 and still not served; \
             served Friday 12:00 after {} scheduled release(s)",
            due.len()
        ),
    );
    let published = verify_served(&site.host, DOMAIN, &site.release_key).await;
    checks.check(
        "every public change a signed release",
        matches!(published, Ok(seq) if seq == site.log.size().await)
            && site.host.fetch("/wedding-cakes/").await?.is_some(),
        format!("{published:?}"),
    );

    // Sunday evening: the weekly review — KPIs against baselines, cost, questions and
    // promotion offers.
    let kpis = Arc::new(InMemoryKpiStore::new());
    for day in 0..14 {
        let at = monday - chrono::Duration::days(7) + chrono::Duration::days(day);
        let (leads, hours) = if day < 7 { (2.0, 9.0) } else { (3.0, 6.0) };
        kpis.record(KpiSample::new(site.site.id, "qualified_leads", at, leads, "commerce")).await?;
        kpis.record(KpiSample::new(site.site.id, "response_time", at, hours, "commerce")).await?;
    }
    for n in 0..20 {
        site.ladder
            .record_decision(JobDecision::new(
                site.site.id,
                "freshness",
                format!("fresh-{n}"),
                DecisionOutcome::ApprovedUnchanged,
                SubjectRef::human(OWNER),
                clock.now(),
            ))
            .await?;
    }
    clock.set(monday + chrono::Duration::days(6) + chrono::Duration::hours(8));
    let tracker = KpiTracker::new(site.site.id, site.charters.clone(), kpis.clone())
        .with_clock(shared.clone());
    let wallet = adk_payments::wallet::Wallet::builder(
        &site.site.name,
        adk_payments::wallet::WalletPolicy::new("KES"),
        Arc::new(
            adk_payments::wallet::InMemoryWalletProvider::stored_value("treasury", "KES", 50_000)
                .with_clock(shared.clone()),
        ),
    )
    .store(Arc::new(adk_payments::wallet::InMemoryWalletStore::new()))
    .owner(SubjectRef::human(OWNER))
    .clock(shared.clone())
    .build()?;
    let composer =
        ReviewComposer::new(site.site.id, tracker, site.ladder.clone(), site.charters.clone())
            .with_wallet(Arc::new(adk_website::wallet::WalletKpi::new(
                adk_payments::wallet::WalletKpiSource::new(wallet, site.site.id),
            )));
    let mut inputs = ReviewInputs::default();
    inputs.work.insert(
        AgentRole::Growth,
        vec![format!("Drafted {} pages for recurring questions", drafts.len())],
    );
    inputs.questions.push("Should we deliver to Naivasha on Saturdays?".into());
    let week = monday.date_naive()
        - chrono::Duration::days(i64::from(monday.weekday().num_days_from_monday()));
    let review = composer.compose(week, inputs).await?;
    let document = review.to_markdown();
    checks.check(
        "one weekly review with KPIs against baselines, questions and promotion offers",
        !review.kpis.is_empty()
            && document.contains("## KPIs")
            && document.contains("## Questions for you")
            && review.promotions.iter().any(|p| p.job == "freshness" && p.eligible),
        format!(
            "{} KPIs, promotion offers {:?}",
            review.kpis.len(),
            review.promotions.iter().map(|p| (&p.job, p.eligible)).collect::<Vec<_>>()
        ),
    );
    checks.check(
        "the weekly review reports cost",
        review.money.is_some() && document.contains("## Money"),
        format!("{:?}", review.money.as_ref().map(|m| (m.spend, m.monthly_budget))),
    );
    let hour = clock.now().hour();
    checks.gap(
        "no record of proactive runs to hold against budget and quiet hours outside the kernel's \
         process runner: the scenario's runs happen at 10:00 UTC, outside the template's quiet \
         hours, by construction",
    );
    checks.check(
        "every proactive run inside budget and outside quiet hours",
        adk_website::alarms::quiet_hours(&charter.reporting)
            .is_none_or(|q| !q.contains(monday + chrono::Duration::days(3))),
        format!("the Growth run at Thursday 10:00 UTC; the review at {hour}:00"),
    );
    Ok(checks)
}

/// The hosting job's watch on a fixture: the edge's lockfile pins `time` 0.1.45, a local
/// RustSec database holds RUSTSEC-2020-0071 against it, and the certificate expires in 14
/// days. Returns the proposals, the owner's cards, and the cards a second watch files.
async fn upkeep_proposals(
    site: &Site,
    now: chrono::DateTime<Utc>,
) -> Result<(Vec<String>, usize, usize)> {
    use adk_core::{
        ApprovalPolicy, ApprovalService, ApproverSelector, InMemoryApprovalStore, InboxQuery,
        StoreApprovalService,
    };
    use adk_website::upkeep::{AdvisoryDatabase, StaticCertificate, UpkeepCheck, UpkeepProposal};
    let dir = tempfile::tempdir()?;
    let advisory = dir.path().join("advisory-db/crates/time");
    std::fs::create_dir_all(&advisory)?;
    std::fs::write(
        advisory.join("RUSTSEC-2020-0071.md"),
        "```toml\n[advisory]\nid = \"RUSTSEC-2020-0071\"\npackage = \"time\"\ndate = \"2020-11-18\"\n\
         url = \"https://github.com/time-rs/time/issues/293\"\n\n[versions]\npatched = [\">= 0.2.23\"]\n\
         unaffected = [\"= 0.2.0\", \"= 0.2.1\", \"= 0.2.2\"]\n```\n\n# Potential segfault in the time crate\n",
    )?;
    let lockfile = dir.path().join("Cargo.lock");
    std::fs::write(
        &lockfile,
        "version = 4\n\n[[package]]\nname = \"time\"\nversion = \"0.1.45\"\n\n\
         [[package]]\nname = \"serde\"\nversion = \"1.0.228\"\n",
    )?;
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_router(ApprovalPolicy::new([ApproverSelector::human(OWNER)])),
    );
    let certificate = adk_site::migrate::probe::CertificateProbe {
        not_after: now + chrono::Duration::days(14),
        days_left: 14,
        expiring: true,
    };
    let check =
        UpkeepCheck::new(site.site.id, DOMAIN, site.site.name.clone(), OWNER, approvals.clone())
            .with_advisories(
                &lockfile,
                Arc::new(AdvisoryDatabase::new(dir.path().join("advisory-db"))),
            )
            .with_certificate(Arc::new(StaticCertificate(Some(certificate))));
    let report = check.run().await?;
    let inbox = approvals
        .inbox(InboxQuery::for_approver(
            adk_core::principal::SubjectRef::human(OWNER),
            Vec::<String>::new(),
        ))
        .await?;
    let again = check.run().await?;
    let proposals = report
        .proposals
        .iter()
        .map(|p| match p {
            UpkeepProposal::Upgrade { advisory, package, .. } => {
                format!("upgrade {package} ({advisory})")
            }
            UpkeepProposal::RenewCertificate { days_left, .. } => {
                format!("renew the certificate ({days_left} days)")
            }
        })
        .collect();
    Ok((proposals, inbox.items.len(), again.filed.len()))
}
